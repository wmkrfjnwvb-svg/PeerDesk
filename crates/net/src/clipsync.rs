//! 在会话里同步剪贴板：一条专用的可靠流，双向复用（协议见 `peerdesk_core::clipboard`）。
//!
//! 只有握手协商出 [`peerdesk_core::proto::Caps::CLIPBOARD`] 才开启：控制端打开这条流，被控端接受。
//!
//! # 粘贴时才传
//!
//! 以前本机剪贴板一变就把整份内容读出来传给对方（文件最多 20 MB）。用户只是复制、还没粘贴，
//! 传输就和画面、输入抢带宽：复制完把鼠标移到远程窗口，远程就卡。现在的做法和 RDP 的剪贴板重定向
//! 一样（复制时只说有什么，粘贴时才传数据），对方声明了
//! [`Caps::CLIPBOARD_LAZY`](peerdesk_core::proto::Caps::CLIPBOARD_LAZY) 时启用：
//!
//! - 复制时：任何内容（文字、图片、文件）都不读盘、不打包、不发数据，只记下“有这种内容”
//!   并告诉对方（[`ClipMsg::Held`]）。
//! - 控制端在远程窗口按粘贴键时（[`gate_pastes`]）：这一下按键先扣住，同步循环看本机剪贴板，
//!   有还没给对方的内容就现在读出来、限速传过去（[`crate::throttle`]），对方写进剪贴板、
//!   回 [`ClipMsg::Applied`] 以后，再让对方执行粘贴（[`InputEvent::Paste`]）。
//!   同一份内容只传一次：剪贴板没变就不再读；变了但内容一样（哈希相同）读了也不传。
//!   等的时候鼠标、键盘照常发，只推迟这一次粘贴。界面显示进度（[`ClipNote::Sending`]），可以取消。
//! - 收到对方的 `Held`：在本机系统剪贴板上放一个“承诺”（[`SystemClipboard::promise`]，只声明格式，
//!   不放数据）。本机任何程序来取（快捷键、右键菜单“粘贴”、拖放）时系统回调，回调里向对方发
//!   [`ClipMsg::Want`]，对方这时才读出来传；取到的留着，同一份不重传。两个方向一样，所以被控端复制、
//!   控制端本机粘贴也是这条路。系统不支持放承诺时（目前是 Linux）退回只有上一条，反方向不同步并提示。
//!   各系统的做法和限制见 docs/research/clipboard-lazy.md。
//!
//! 对方是老版本时保持原来的行为：复制后马上整份同步，粘贴键原样发。
//!
//! 顺序为什么要靠确认：输入走数据报，剪贴板内容走这条可靠流，两者之间没有顺序保证。

use std::collections::{BTreeSet, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use peerdesk_core::clipboard::{
    packed_count, packed_top_level, Clip, ClipKind, ClipMsg, Deframer, Limits, Offered, Received,
    Receiver, Sender,
};
use peerdesk_core::input::{InputEvent, MODIFIER_FIRST, MODIFIER_LAST};
use quinn::{RecvStream, SendStream};
use tokio::sync::{mpsc, oneshot};

use crate::throttle::Throttle;
use crate::NetError;

/// 多久看一次本机剪贴板有没有变（只读一个计数，开销可以忽略）。
pub const POLL: Duration = Duration::from_millis(100);

/// 剪贴板流的优先级：比控制流、文件流（默认 0）低。数据报（画面、输入）在 quinn 里本来就排在
/// 所有流的数据前面（quinn-proto 0.11 `Connection::populate_packet` 先写 DATAGRAM 再写 STREAM）。
pub const STREAM_PRIORITY: i32 = -1;

/// 粘贴时才传的那次传输为什么没成。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SendFail {
    /// 用户取消了。
    Cancelled,
    /// 本机剪贴板里的内容读不出来（空了、格式不认识、文件读不了）。
    Unreadable,
    /// 传到了，但对方的剪贴板写不进去。
    RemoteWrite,
}

/// 给界面看的剪贴板事件。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClipNote {
    /// 本机复制的内容太大，没有同步给对方。
    LocalTooLarge {
        /// 种类。
        kind: ClipKind,
        /// 大小（字节）。
        size: u64,
    },
    /// 对方复制的内容太大，没有同步过来。
    RemoteTooLarge {
        /// 种类。
        kind: ClipKind,
        /// 大小（字节）。
        size: u64,
    },
    /// 本机复制了文件，但对方的版本不支持复制文件。
    FilesUnsupported,
    /// 本机复制的文件里有读不了的（没有权限，或者文件名在对方系统上不能用），这些没有同步。
    FilesSkipped {
        /// 没有同步的文件数。
        skipped: u32,
        /// 同步了的文件数。
        sent: u32,
    },
    /// 用户按了粘贴，正在把本机剪贴板里的内容传给对方，传完自动粘贴（进度，每 100 ms 一次）。
    Sending {
        /// 种类。
        kind: ClipKind,
        /// 文件数（不是文件时为 0）。
        files: u32,
        /// 已经发出的字节数。
        sent: u64,
        /// 总字节数。
        total: u64,
    },
    /// 传完了，对方已经放进剪贴板，粘贴已执行。
    SendDone,
    /// 没传成，这次粘贴没有执行。
    SendFailed(SendFail),
    /// 本机的程序来取对方的内容，没取到。
    FetchFailed(FetchError),
    /// 对方复制了内容，但本机系统不支持延迟提供数据，没有同步过来。
    RemoteHeld {
        /// 种类。
        kind: ClipKind,
    },
    /// 收到对方的内容，已写进本机剪贴板。
    Received {
        /// 种类。
        kind: ClipKind,
        /// 大小（字节）。
        size: u64,
    },
}

/// 读本机剪贴板的结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Copied {
    /// 内容。
    Clip(Clip),
    /// 太大（例如复制了超过 20 MB 的文件），没有读出内容，只有大小。
    TooLarge {
        /// 种类。
        kind: ClipKind,
        /// 大小（字节，可能只算到超过上限为止）。
        size: u64,
    },
}

/// 看一眼本机剪贴板的结果（[`SystemClipboard::peek`]）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Peeked {
    /// 文字（已经在内存里，读它不花什么）。
    Text(Clip),
    /// 这种内容，没有读出来（文件要读盘、图片要编码），和它有几项（文件：最上层的文件和文件夹数；其他为 1）。
    Later(ClipKind, u16),
}

/// 本机剪贴板。真实实现在平台层；测试用 [`MemClipboard`]（测试不碰开发者电脑的剪贴板）。
pub trait SystemClipboard: Send + Sync {
    /// 剪贴板的变化计数：内容一变就不同。
    fn change_count(&self) -> u64;
    /// 读当前内容（文件、文本或图片）；空的、读不了或是别的格式时为 `None`。
    fn read(&self) -> Option<Copied>;
    /// 看一眼当前是什么内容：文字直接给出，文件和图片只说种类，**不读文件、不编码图片**。
    /// 默认实现整份读出来再看（对内存里的假剪贴板没有区别）；真实的剪贴板要自己实现。
    fn peek(&self) -> Option<Peeked> {
        Some(match self.read()? {
            Copied::Clip(clip) if clip.kind == ClipKind::Text => Peeked::Text(clip),
            Copied::Clip(clip) => Peeked::Later(clip.kind, 1),
            Copied::TooLarge { kind, .. } => Peeked::Later(kind, 1),
        })
    }
    /// 写入。
    fn write(&self, clip: &Clip) -> Result<(), String>;
    /// 剪贴板事件（给界面提示用）。默认不处理。
    fn note(&self, _note: ClipNote) {}
    /// 用户有没有点“取消”（取走：返回 `true` 以后复位）。传输期间每 100 ms 问一次。默认没有。
    fn take_cancel(&self) -> bool {
        false
    }
    /// 系统剪贴板里现在是不是 [`SystemClipboard::promise`] 放的承诺（没被本机的复制顶掉）。
    /// 有的系统在程序来取数据时变化计数也会变，同步循环靠它区分“还是承诺”和“用户复制了新东西”。
    fn promise_in_place(&self) -> bool {
        false
    }
    /// 在系统剪贴板上放一个“承诺”：有这种内容，但数据不在这里；本机任何程序来取时（快捷键、
    /// 右键菜单、拖放）系统回调，回调里用 [`Promise::get`] 向对方要。放上去以后变化计数会变。
    /// 默认不支持（返回原因）：这时对方复制的内容不会出现在本机剪贴板里。
    fn promise(&self, _promise: Promise) -> Result<(), String> {
        Err("这个系统上还不支持延迟提供剪贴板数据".into())
    }
}

/// 系统回调里等对方数据最多等这么久。回调期间来粘贴的那个程序在等（Windows 上它的界面没有响应），
/// 所以不能长；超时以后传输还在继续，传完再粘贴一次就能拿到。
pub const FETCH_TIMEOUT: Duration = Duration::from_secs(10);

/// 承诺的数据为什么没取到。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FetchError {
    /// 等了 [`FETCH_TIMEOUT`] 还没传完（传输在继续）。
    Timeout,
    /// 对方给不了：剪贴板已经换了内容、读不出来或者太大。
    Failed,
    /// 会话已经结束。
    Gone,
}

/// 向对方要数据：阻塞到数据到了、失败或者过了给定的时间。可以在任何线程上调用（不能在会话的异步线程上）。
pub type Fetch = Arc<dyn Fn(Duration) -> Result<Clip, FetchError> + Send + Sync>;

/// 放在系统剪贴板上的承诺（[`SystemClipboard::promise`]）。
#[derive(Clone)]
pub struct Promise {
    /// 种类：决定在系统剪贴板上声明哪些格式。
    pub kind: ClipKind,
    /// 有几项（文件：最上层的文件和文件夹数；其他为 1）。
    pub items: u16,
    /// 取数据。
    pub fetch: Fetch,
}

impl std::fmt::Debug for Promise {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Promise({:?}, {})", self.kind, self.items)
    }
}

impl Promise {
    /// 系统回调里用：取数据，取不到时通过 `note` 告诉用户原因（会话结束除外）并返回 `None`
    /// （回调给系统空结果）。
    pub fn get(&self, timeout: Duration, note: &dyn Fn(ClipNote)) -> Option<Clip> {
        match (self.fetch)(timeout) {
            Ok(clip) => Some(clip),
            Err(why) => {
                crate::note!(
                    warn,
                    "剪贴板：本机程序来取对方的 {:?}，没取到：{why:?}",
                    self.kind
                );
                if why != FetchError::Gone {
                    note(ClipNote::FetchFailed(why));
                }
                None
            }
        }
    }
}

/// 本机剪贴板的句柄，可以放进会话选项里。
#[derive(Clone)]
pub struct ClipboardHandle(pub Arc<dyn SystemClipboard>);

impl std::fmt::Debug for ClipboardHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ClipboardHandle")
    }
}

/// 内存剪贴板的状态：（变化计数，内容，收到的事件，模拟“太大”的复制）。
type MemState = (u64, Option<Clip>, Vec<ClipNote>, Option<Copied>);

/// 内存里的剪贴板，用于测试和仿真。
#[derive(Debug, Default)]
pub struct MemClipboard {
    state: Mutex<MemState>,
    /// 接下来这么多次写入失败（模拟剪贴板被别的程序占着）。
    failing: AtomicU32,
    /// 整份读出过多少次（真实的剪贴板上这是读盘、编码图片）。
    reads: AtomicU32,
    /// 用户点了取消。
    cancel: AtomicBool,
    /// 内容读不出来（模拟文件没有权限读）：看得到种类，整份读时失败。
    unreadable: AtomicBool,
    /// 支持放承诺（模拟有延迟提供数据机制的系统）；默认不支持。
    promises: AtomicBool,
    /// 现在放着的承诺。
    promised: Mutex<Option<Promise>>,
}

impl MemClipboard {
    /// 之后支持放承诺（模拟 Windows、macOS 这样有延迟提供数据机制的系统）。
    pub fn enable_promises(&self) {
        self.promises.store(true, Ordering::Relaxed);
    }

    fn promised(&self) -> std::sync::MutexGuard<'_, Option<Promise>> {
        self.promised.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// 现在放着的承诺是什么种类、几项。
    #[must_use]
    pub fn promise_on_board(&self) -> Option<(ClipKind, u16)> {
        self.promised().as_ref().map(|p| (p.kind, p.items))
    }

    /// 模拟本机的一个程序来粘贴：剪贴板上是承诺时向对方取（最多等 `timeout`，取不到为 `None`），
    /// 否则就是剪贴板里的内容。会阻塞，不能在异步线程上直接调用。
    #[must_use]
    pub fn paste(&self, timeout: Duration) -> Option<Clip> {
        let promise = self.promised().clone();
        match promise {
            Some(promise) => promise.get(timeout, &|note| self.note(note)),
            None => self.get(),
        }
    }

    /// 模拟用户复制。
    pub fn copy(&self, clip: Clip) {
        *self.promised() = None;
        let mut s = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        s.0 = s.0.wrapping_add(1);
        s.1 = Some(clip);
        s.3 = None;
    }

    /// 模拟用户复制了太大的内容（例如超过 20 MB 的文件）。
    pub fn copy_too_large(&self, kind: ClipKind, size: u64) {
        *self.promised() = None;
        let mut s = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        s.0 = s.0.wrapping_add(1);
        s.1 = None;
        s.3 = Some(Copied::TooLarge { kind, size });
    }

    /// 模拟剪贴板被清空（或者换成了读不了的格式）。
    pub fn clear(&self) {
        *self.promised() = None;
        let mut s = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        s.0 = s.0.wrapping_add(1);
        s.1 = None;
        s.3 = None;
    }

    /// 接下来 `n` 次写入失败。
    pub fn fail_next_writes(&self, n: u32) {
        self.failing.store(n, Ordering::Relaxed);
    }

    /// 之后整份读都失败（`false` 恢复）：模拟复制的文件没有权限读。
    pub fn set_unreadable(&self, unreadable: bool) {
        self.unreadable.store(unreadable, Ordering::Relaxed);
    }

    /// 变化计数加一、内容不变：模拟系统在程序来取承诺的数据时动了计数（Windows 是这样）。
    pub fn touch(&self) {
        let mut s = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        s.0 = s.0.wrapping_add(1);
    }

    /// 模拟用户点“取消”。
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }

    /// 整份读出过多少次。
    #[must_use]
    pub fn reads(&self) -> u32 {
        self.reads.load(Ordering::Relaxed)
    }

    /// 当前内容。
    #[must_use]
    pub fn get(&self) -> Option<Clip> {
        self.state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .1
            .clone()
    }

    /// 收到过的事件。
    #[must_use]
    pub fn notes(&self) -> Vec<ClipNote> {
        self.state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .2
            .clone()
    }
}

impl SystemClipboard for MemClipboard {
    fn change_count(&self) -> u64 {
        self.state.lock().unwrap_or_else(PoisonError::into_inner).0
    }
    fn read(&self) -> Option<Copied> {
        let _ = self.reads.fetch_add(1, Ordering::Relaxed);
        if self.unreadable.load(Ordering::Relaxed) {
            return None;
        }
        let st = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        st.3.clone().or_else(|| st.1.clone().map(Copied::Clip))
    }
    fn peek(&self) -> Option<Peeked> {
        let st = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(Copied::TooLarge { kind, .. }) = &st.3 {
            return Some(Peeked::Later(*kind, 1));
        }
        st.1.as_ref().map(|clip| match clip.kind {
            ClipKind::Text => Peeked::Text(clip.clone()),
            ClipKind::Files => Peeked::Later(clip.kind, packed_top_level(&clip.data)),
            ClipKind::Png => Peeked::Later(clip.kind, 1),
        })
    }
    fn promise_in_place(&self) -> bool {
        self.promised().is_some()
    }
    fn promise(&self, promise: Promise) -> Result<(), String> {
        if !self.promises.load(Ordering::Relaxed) {
            return Err("这个假剪贴板没有打开承诺".into());
        }
        // 承诺顶掉原来的内容（真实系统上也是：放承诺前先清空）
        self.clear();
        *self.promised() = Some(promise);
        Ok(())
    }
    fn write(&self, clip: &Clip) -> Result<(), String> {
        let failing = &self.failing;
        if failing
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |n| n.checked_sub(1))
            .is_ok()
        {
            return Err("剪贴板被占用".into());
        }
        self.copy(clip.clone());
        Ok(())
    }
    fn note(&self, note: ClipNote) {
        self.state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .2
            .push(note);
    }
    fn take_cancel(&self) -> bool {
        self.cancel.swap(false, Ordering::Relaxed)
    }
}

/// 现在的往返时间（限速用，见 [`crate::throttle`]）。
#[derive(Clone)]
pub struct RttProbe(pub Arc<dyn Fn() -> Duration + Send + Sync>);

impl std::fmt::Debug for RttProbe {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("RttProbe")
    }
}

/// 同步的选项（由握手协商出的能力决定）。
#[derive(Debug, Default)]
pub struct SyncOptions {
    /// 对方支持复制文件（[`Caps::CLIPBOARD_FILES`](peerdesk_core::proto::Caps::CLIPBOARD_FILES)）。
    pub files: bool,
    /// 对方支持“粘贴时才传”（[`Caps::CLIPBOARD_LAZY`](peerdesk_core::proto::Caps::CLIPBOARD_LAZY)）。
    pub lazy: bool,
    /// 控制端：粘贴键来的请求（[`paste_channel`] 的接收端）。被控端没有。
    pub paste: Option<PasteRequests>,
    /// 往返时间；没有时传输速率停在 [`crate::throttle::START`]。
    pub rtt: Option<RttProbe>,
}

/// 同步循环对一次粘贴的答复。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// 本机剪贴板里的内容已经在对方那里：执行粘贴。
    Go,
    /// 没传成（原因已经告诉用户）：这次粘贴不执行，免得粘贴出旧内容。
    Drop,
}

/// 粘贴键这一侧：向同步循环提出请求。
#[derive(Debug, Clone)]
pub struct PasteSync(mpsc::UnboundedSender<oneshot::Sender<Verdict>>);

/// 同步循环这一侧：收粘贴请求。
#[derive(Debug)]
pub struct PasteRequests(mpsc::UnboundedReceiver<oneshot::Sender<Verdict>>);

/// 建一对：粘贴键那边用 [`PasteSync`]，同步循环用 [`PasteRequests`]（放进 [`SyncOptions::paste`]）。
#[must_use]
pub fn paste_channel() -> (PasteSync, PasteRequests) {
    let (tx, rx) = mpsc::unbounded_channel();
    (PasteSync(tx), PasteRequests(rx))
}

impl PasteSync {
    /// 提出一次粘贴：同步循环把本机剪贴板里现在的内容送到对方以后答复。
    /// 同步已经结束（流断了、对方不让同步剪贴板）时答复端直接失效，按可以粘贴处理。
    fn request(&self) -> oneshot::Receiver<Verdict> {
        let (tx, rx) = oneshot::channel();
        let _ = self.0.send(tx);
        rx
    }
}

/// 认出粘贴键：按着 Ctrl 或 Win/Command 时按 V（Ctrl+V、Cmd+V、Ctrl+Shift+V、Win+V），或 Shift+Insert。
#[derive(Debug, Default)]
struct PasteChord {
    modifiers: BTreeSet<u16>,
}

impl PasteChord {
    const V: u16 = 0x19;
    const INSERT: u16 = 0x49;

    /// 现在按着的修饰键，按 [`InputEvent::Paste`] 的位表示。
    fn mask(&self) -> u8 {
        self.modifiers.iter().fold(0u8, |mask, m| {
            let bit = u32::from(m.saturating_sub(MODIFIER_FIRST));
            mask | 1u8.checked_shl(bit).unwrap_or(0)
        })
    }

    /// 记下修饰键的状态；这个事件是按下粘贴键时返回 `true`。
    fn on(&mut self, event: &InputEvent) -> bool {
        match event {
            InputEvent::Key { code, down } if (MODIFIER_FIRST..=MODIFIER_LAST).contains(code) => {
                if *down {
                    self.modifiers.insert(*code);
                } else {
                    self.modifiers.remove(code);
                }
                false
            }
            InputEvent::Key { code, down: true } => {
                let held = |codes: [u16; 2]| codes.iter().any(|c| self.modifiers.contains(c));
                let command = held([0xE0, 0xE4]) || held([0xE3, 0xE7]);
                (*code == Self::V && command) || (*code == Self::INSERT && held([0xE1, 0xE5]))
            }
            InputEvent::ReleaseAll => {
                self.modifiers.clear();
                false
            }
            InputEvent::Key { .. }
            | InputEvent::MouseMove { .. }
            | InputEvent::MouseRelative { .. }
            | InputEvent::MouseButton { .. }
            | InputEvent::Wheel { .. }
            | InputEvent::Text(_)
            | InputEvent::Paste { .. } => false,
        }
    }
}

/// 一次被扣住的粘贴。
struct HeldPaste {
    /// 扣住的键（V 或 Insert）和当时按着的修饰键。
    code: u16,
    modifiers: u8,
    /// 之后这个键抬起了（抬起也扣住），或者修饰键变了：不能再原样补发按下，要用原子的粘贴事件。
    released: bool,
    changed: bool,
    verdict: oneshot::Receiver<Verdict>,
}

/// 控制端：把界面来的输入转给会话，粘贴键等剪贴板里的内容到了对方再执行（原因见本模块开头）。
///
/// **只推迟这一次粘贴**，其他输入（鼠标、别的键）照常转发、不排队：
/// - 按下粘贴键：扣住这一下，问同步循环；这个键之后的抬起和重复也扣住；
/// - 同步循环答复可以粘贴时：键还按着、修饰键没变（通常是马上答复：没有要传的）就原样补发按下；
///   否则发 [`InputEvent::Paste`]，由被控端原子地按一次**用户当时按的那个组合键**
///   （Ctrl+V 还是 Command+V、带不带 Shift 都照原样，不按系统换键）；
/// - 没传成（用户取消、读不了、对方写不进去）：这次粘贴作废。
pub async fn gate_pastes(
    mut input: mpsc::Receiver<InputEvent>,
    out: mpsc::Sender<InputEvent>,
    sync: PasteSync,
) {
    let mut chord = PasteChord::default();
    let mut held: Option<HeldPaste> = None;
    // 粘贴已经处理完，但那个键还没抬起：抬起来的时候不用发（按下没有原样发过）
    let mut swallow_up: Option<u16> = None;
    loop {
        tokio::select! {
            biased;
            verdict = async {
                match held.as_mut() {
                    Some(h) => (&mut h.verdict).await,
                    None => std::future::pending().await,
                }
            } => {
                let Some(h) = held.take() else { continue };
                // 答复端失效：同步已经结束，按可以粘贴处理
                let go = verdict.unwrap_or(Verdict::Go) == Verdict::Go;
                let event = match (go, h.released || h.changed) {
                    (true, false) => Some(InputEvent::Key { code: h.code, down: true }),
                    (true, true) => Some(InputEvent::Paste { modifiers: h.modifiers, code: h.code }),
                    (false, _) => None,
                };
                let plain = matches!(event, Some(InputEvent::Key { .. }));
                if !h.released && !plain {
                    swallow_up = Some(h.code);
                }
                if let Some(event) = event {
                    if out.send(event).await.is_err() {
                        return;
                    }
                }
            }
            event = input.recv() => {
                let Some(event) = event else { return };
                let is_paste = chord.on(&event);
                let forward = match (&event, held.as_mut()) {
                    (InputEvent::Key { code, down }, Some(h)) if *code == h.code => {
                        h.released |= !*down;
                        false
                    }
                    (InputEvent::Key { code, .. }, Some(h))
                        if (MODIFIER_FIRST..=MODIFIER_LAST).contains(code) =>
                    {
                        h.changed = true;
                        true
                    }
                    (InputEvent::ReleaseAll, Some(h)) => {
                        h.changed = true;
                        h.released = true;
                        true
                    }
                    (InputEvent::ReleaseAll, None) => {
                        swallow_up = None;
                        true
                    }
                    (InputEvent::Key { code, down: false }, None) if swallow_up == Some(*code) => {
                        swallow_up = None;
                        false
                    }
                    (InputEvent::Key { code, down: true }, None) if is_paste => {
                        swallow_up = None;
                        held = Some(HeldPaste {
                            code: *code,
                            modifiers: chord.mask(),
                            released: false,
                            changed: false,
                            verdict: sync.request(),
                        });
                        false
                    }
                    _ => true,
                };
                if forward && out.send(event).await.is_err() {
                    return;
                }
            }
        }
    }
}

/// 粘贴触发的那次传输（界面显示进度用）。
struct Transfer {
    kind: ClipKind,
    files: u32,
    total: u64,
    /// 这份内容切成了多少条消息。
    msgs: usize,
}

/// 同步循环里“本机的内容给对方”这一半的状态。
struct Lazy {
    /// 本机剪贴板里有这种内容，还没给对方（对方来要、或者在远程按粘贴时才传）。
    held: Option<ClipKind>,
    /// 告诉对方“有这种内容”的那条 [`ClipMsg::Held`] 的序号和项数（对方用这个序号来要）。
    held_seq: u32,
    held_items: u16,
    /// 自己发出去、还在等对方确认的那一份的序号。
    awaiting: Option<u32>,
    /// 等这一份到了对方再执行的粘贴。
    waiters: Vec<oneshot::Sender<Verdict>>,
    /// 这一份是粘贴触发的传输。
    transfer: Option<Transfer>,
}

impl Lazy {
    fn answer(&mut self, verdict: Verdict) {
        for waiter in self.waiters.drain(..) {
            let _ = waiter.send(verdict);
        }
    }
}

/// 系统来取数据的一次请求（[`Promise`] 的取数函数发给同步循环）。
struct FetchRequest {
    /// 承诺对应的那条 `Held` 的序号。
    seq: u32,
    reply: std::sync::mpsc::SyncSender<Result<Clip, FetchError>>,
}

/// 已经从对方取到的内容：（承诺的序号，内容）。取数函数先看这里，有就不用等同步循环。
type Fetched = Arc<Mutex<Option<(u32, Clip)>>>;

/// 同步循环里“对方的内容给本机”这一半的状态：本机系统剪贴板上的承诺。
struct Remote {
    /// 本机系统剪贴板上放着对方这条 `Held`（序号）的承诺。`None`：没有（系统不支持，或者被本机的复制顶掉了）。
    promised: Option<u32>,
    fetched: Fetched,
    /// 上一份取到的内容：对方说“和上次一样”（[`ClipMsg::Same`]）时接着用。
    previous: Option<Clip>,
    /// 正在等数据的程序。
    fetchers: Vec<std::sync::mpsc::SyncSender<Result<Clip, FetchError>>>,
    /// 已经向对方要过这一份了。
    wanted: bool,
    requests: mpsc::UnboundedSender<FetchRequest>,
}

impl Remote {
    fn lock(&self) -> std::sync::MutexGuard<'_, Option<(u32, Clip)>> {
        self.fetched.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// 等数据的程序都得到这个结果。
    fn answer(&mut self, result: &Result<Clip, FetchError>) {
        self.wanted = false;
        for fetcher in self.fetchers.drain(..) {
            let _ = fetcher.try_send(result.clone());
        }
    }

    /// 承诺的内容到了：记下来，交给等着的程序。
    fn deliver(&mut self, seq: u32, clip: Clip) {
        *self.lock() = Some((seq, clip.clone()));
        self.answer(&Ok(clip));
    }

    /// 对方说这条 `Held` 的内容和上次发的一样：接着用上一份。返回内容是不是在本机可用。
    fn same(&mut self, seq: u32) -> bool {
        match (self.promised, self.previous.take()) {
            (Some(promised), Some(clip)) if promised == seq => {
                self.deliver(seq, clip);
                true
            }
            // 上一份已经不在了（不应该发生：本机换过内容的话对方不会说“一样”）：让对方重新传
            (Some(_), _) => {
                self.answer(&Err(FetchError::Failed));
                false
            }
            // 没有承诺（系统不支持）：上一份当时已经写进了本机剪贴板
            (None, _) => true,
        }
    }

    /// 对方的剪贴板换了内容：取到的那份留作“上一份”，等着的程序拿不到了。
    fn superseded(&mut self) {
        let taken = self.lock().take();
        if let Some((_, clip)) = taken {
            self.previous = Some(clip);
        }
        self.promised = None;
        self.answer(&Err(FetchError::Failed));
    }

    /// 本机剪贴板被本机的复制顶掉了：承诺没了，取到的内容也不用留。
    fn dropped(&mut self) {
        *self.lock() = None;
        self.previous = None;
        self.promised = None;
        self.answer(&Err(FetchError::Failed));
    }

    /// 给系统剪贴板用的承诺：取数函数先看有没有取到过，没有就向同步循环要，等到超时。
    fn promise(&self, seq: u32, kind: ClipKind, items: u16) -> Promise {
        let fetched = Arc::clone(&self.fetched);
        let requests = self.requests.clone();
        Promise {
            kind,
            items,
            fetch: Arc::new(move |timeout| {
                let ready = fetched
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .as_ref()
                    .filter(|(s, _)| *s == seq)
                    .map(|(_, clip)| clip.clone());
                if let Some(clip) = ready {
                    return Ok(clip);
                }
                let (reply, result) = std::sync::mpsc::sync_channel(1);
                if requests.send(FetchRequest { seq, reply }).is_err() {
                    return Err(FetchError::Gone);
                }
                match result.recv_timeout(timeout) {
                    Ok(result) => result,
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout) => Err(FetchError::Timeout),
                    Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => Err(FetchError::Gone),
                }
            }),
        }
    }
}

/// 在一条流上双向同步，直到流断开。开始时本机剪贴板里已有的内容不发（只同步之后复制的）。
#[allow(clippy::too_many_lines)]
pub async fn run(
    mut send: SendStream,
    mut recv: RecvStream,
    clipboard: Arc<dyn SystemClipboard>,
    limits: Limits,
    opts: SyncOptions,
) -> Result<(), NetError> {
    let SyncOptions {
        files,
        lazy,
        mut paste,
        rtt,
    } = opts;
    // 流已经关了时设不上，随后的读写会报出来
    let _ = send.set_priority(STREAM_PRIORITY);
    // 要回给对方的确认和请求（不放进 sender 的队列：新的复制会清掉那个队列）
    let mut acks: VecDeque<ClipMsg> = VecDeque::new();
    let mut state = Lazy {
        held: None,
        held_seq: 0,
        held_items: 1,
        awaiting: None,
        waiters: Vec::new(),
        transfer: None,
    };
    let (requests, mut fetches) = mpsc::unbounded_channel();
    let mut remote = Remote {
        promised: None,
        fetched: Arc::default(),
        previous: None,
        fetchers: Vec::new(),
        wanted: false,
        requests,
    };
    let mut sender = Sender::new(limits);
    let mut receiver = Receiver::new(limits);
    let mut deframer = Deframer::default();
    let mut seen = blocking(&clipboard, |c| c.change_count()).await?;
    let mut tick = tokio::time::interval(POLL);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut buf = vec![0u8; 64 * 1024];
    let started = tokio::time::Instant::now();
    let mut throttle = Throttle::new();
    // 正在写的一条消息（编码后的字节，已写到第几个字节）。一条消息一旦开始写就要写完，
    // 否则流里的数据就错位了；新的复制只作废还没开始写的消息。
    let mut outgoing: Option<(Vec<u8>, usize)> = None;
    loop {
        if outgoing.is_none() {
            outgoing = acks
                .pop_front()
                .or_else(|| sender.next_msg())
                .map(|msg| (msg.frame(), 0));
        }
        // 只在“粘贴时才传”时限速（传的是大内容）；对方是老版本时和原来一样
        let wait = if lazy {
            throttle.wait(started.elapsed())
        } else {
            Duration::ZERO
        };
        let quota = if lazy { throttle.quota() } else { usize::MAX };
        tokio::select! {
            biased;
            read = recv.read(&mut buf) => {
                let Some(n) = read.map_err(|e| NetError::Closed(e.to_string()))? else {
                    return Ok(());
                };
                for msg in deframer.push(buf.get(..n).unwrap_or_default())? {
                    match msg {
                        ClipMsg::Applied { seq, ok } => {
                            if state.awaiting == Some(seq) {
                                state.awaiting = None;
                                let transfer = state.transfer.take();
                                if ok {
                                    state.held = None;
                                    crate::note!(info, "剪贴板：内容已经在对方那里");
                                    if transfer.is_some() {
                                        clipboard.note(ClipNote::SendDone);
                                    }
                                    state.answer(Verdict::Go);
                                } else {
                                    // 没进对方的剪贴板：忘掉这一份，用户再按粘贴时重新传
                                    crate::note!(warn, "剪贴板：内容传到了，但对方的剪贴板写不进去，这次粘贴不执行");
                                    sender.forget();
                                    clipboard.note(ClipNote::SendFailed(SendFail::RemoteWrite));
                                    state.answer(Verdict::Drop);
                                }
                            }
                            continue;
                        }
                        // 对方的某个程序来取数据了（右键菜单“粘贴”、快捷键、拖放）：现在读出来传过去
                        ClipMsg::Want { seq } => {
                            let current = state.held.filter(|_| state.held_seq == seq);
                            if let Some(kind) = current {
                                if state.awaiting.is_none() {
                                    crate::note!(info, "剪贴板：对方的程序来取 {kind:?}，现在传");
                                    pull(&clipboard, &mut sender, &mut state, &mut acks, files).await?;
                                }
                            }
                            // 给不了（已经换了内容、读不出来、太大）：告诉对方，别让那个程序干等
                            if current.is_none() || state.awaiting.is_none() {
                                acks.push_back(ClipMsg::Unavailable { seq });
                            }
                            continue;
                        }
                        ClipMsg::Unavailable { seq } => {
                            if remote.promised == Some(seq) {
                                crate::note!(warn, "剪贴板：对方给不了承诺的内容（已经换了、读不出来或太大）");
                                remote.answer(&Err(FetchError::Failed));
                            }
                            continue;
                        }
                        // 对方又复制了一遍同样的内容：接着用上一份
                        ClipMsg::Same { seq } => {
                            let ok = remote.same(seq);
                            acks.push_back(ClipMsg::Applied { seq, ok });
                            continue;
                        }
                        ClipMsg::Offer { .. }
                        | ClipMsg::Chunk { .. }
                        | ClipMsg::Done { .. }
                        | ClipMsg::TooLarge { .. }
                        | ClipMsg::Held { .. } => {}
                    }
                    let done = if let ClipMsg::Done { seq, .. } = &msg { Some(*seq) } else { None };
                    match receiver.on_msg(msg)? {
                        // 本机系统剪贴板上放着承诺：内容不写进剪贴板，交给来取的程序（没有人在取就先留着）
                        Some(Received::Clip(clip)) if remote.promised.is_some() => {
                            crate::note!(info, "剪贴板：承诺的 {:?} {} 字节到了", clip.kind, clip.data.len());
                            if let Some(promised) = remote.promised {
                                remote.deliver(promised, clip);
                            }
                            if let Some(seq) = done {
                                acks.push_back(ClipMsg::Applied { seq, ok: true });
                            }
                        }
                        Some(Received::Clip(clip)) => {
                            let (kind, size) = (clip.kind, len64(&clip));
                            // 和本机剪贴板现在的内容比一下（对方的内容可能比本机这边的轮询先到）。
                            // “粘贴时才传”时只看一眼：本机是文件、图片就不去读它
                            let hash = clip.hash();
                            let current = if lazy {
                                match blocking(&clipboard, |c| c.peek()).await? {
                                    Some(Peeked::Text(text)) => Some(text),
                                    Some(Peeked::Later(..)) | None => None,
                                }
                            } else {
                                match blocking(&clipboard, |c| c.read()).await? {
                                    Some(Copied::Clip(clip)) => Some(clip),
                                    Some(Copied::TooLarge { .. }) | None => None,
                                }
                            };
                            let same_now = current.is_some_and(|c| c.hash() == hash);
                            let written = if same_now || sender.is_local(&clip) {
                                // 本机已经是这份内容（例如另一个同步工具已经同步过来），不再写
                                crate::note!(info, "剪贴板：收到 {kind:?} {size} 字节，和本机相同，不再写入");
                                true
                            } else {
                                let started = std::time::Instant::now();
                                let (written, clip) = blocking(&clipboard, move |c| (write_with_retry(c, &clip), clip)).await?;
                                let took = started.elapsed().as_millis();
                                match &written {
                                    Ok(()) => {
                                        crate::note!(info, "剪贴板：收到 {kind:?} {size} 字节，写入用时 {took} ms");
                                        clipboard.note(ClipNote::Received { kind, size });
                                    }
                                    // 写不进去（剪贴板被别的程序占着）：记下原因，继续同步后面的内容
                                    Err(e) => crate::note!(warn, "剪贴板：收到 {kind:?} {size} 字节，写入失败（{took} ms）：{e}"),
                                }
                                seen = blocking(&clipboard, |c| c.change_count()).await?;
                                if written.is_ok() {
                                    // 写进去了才记：本机剪贴板现在是对方的内容，不发回去，也没有要给对方的了。
                                    // （原来写之前就记，写失败以后同一份内容再来会被当成“本机已经有”而不写。）
                                    sender.applied_remote(&clip);
                                    state.held = None;
                                }
                                written.is_ok()
                            };
                            // 处理完再回确认：对方收到确认时内容已经在剪贴板里（或者知道没写进去）
                            if let Some(seq) = done.filter(|_| lazy) {
                                acks.push_back(ClipMsg::Applied { seq, ok: written });
                            }
                        }
                        Some(Received::TooLarge { kind, size }) => {
                            crate::note!(info, "剪贴板：对方复制的 {kind:?} {size} 字节超过上限，没有同步");
                            clipboard.note(ClipNote::RemoteTooLarge { kind, size });
                            remote.answer(&Err(FetchError::Failed));
                        }
                        Some(Received::Held { seq, kind, items }) => {
                            // 对方的剪贴板换了内容：上次发过去的那份已经不在了，再粘贴要重新传
                            sender.peer_changed();
                            remote.superseded();
                            // 在本机系统剪贴板上放承诺：本机任何程序来取时才向对方要
                            let promise = remote.promise(seq, kind, items);
                            match blocking(&clipboard, move |c| c.promise(promise)).await? {
                                Ok(()) => {
                                    crate::note!(info, "剪贴板：对方复制了 {kind:?}（{items} 项），本机放了承诺，有程序来取时再传");
                                    remote.promised = Some(seq);
                                    seen = blocking(&clipboard, |c| c.change_count()).await?;
                                    // 本机剪贴板现在是对方的承诺，没有自己的内容要给对方了
                                    state.held = None;
                                }
                                Err(why) => {
                                    crate::note!(info, "剪贴板：对方复制了 {kind:?}，本机放不了承诺（{why}），没有同步过来");
                                    clipboard.note(ClipNote::RemoteHeld { kind });
                                }
                            }
                        }
                        None => {}
                    }
                }
            }
            // 用户在远程窗口按了粘贴
            request = next_paste(paste.as_mut()) => {
                let Some(reply) = request else {
                    // 粘贴键那一头没了（会话在结束）：不再等
                    paste = None;
                    continue;
                };
                // 马上看一眼本机剪贴板（不等下一次轮询）；正在传的时候不看，传完再说
                if state.transfer.is_none() {
                    refresh(&clipboard, &mut seen, &mut sender, &mut state, &mut remote, files, lazy).await?;
                }
                if state.awaiting.is_some() {
                    // 正在发的那一份（上一次粘贴或者对方来取触发的传输）到了再粘贴
                    state.waiters.push(reply);
                } else if state.held.is_some() {
                    state.waiters.push(reply);
                    pull(&clipboard, &mut sender, &mut state, &mut acks, files).await?;
                } else {
                    let _ = reply.send(Verdict::Go);
                }
            }
            // 本机的某个程序来取承诺的数据
            Some(request) = fetches.recv() => {
                let ready = remote
                    .lock()
                    .as_ref()
                    .filter(|(seq, _)| *seq == request.seq)
                    .map(|(_, clip)| clip.clone());
                if let Some(clip) = ready {
                    let _ = request.reply.try_send(Ok(clip));
                } else if remote.promised == Some(request.seq) {
                    remote.fetchers.push(request.reply);
                    if !remote.wanted {
                        remote.wanted = true;
                        acks.push_back(ClipMsg::Want { seq: request.seq });
                    }
                } else {
                    // 过时的承诺（对方已经换了内容）
                    let _ = request.reply.try_send(Err(FetchError::Failed));
                }
            }
            _ = tick.tick() => {
                if let Some(rtt) = &rtt {
                    throttle.on_rtt(started.elapsed(), (rtt.0)());
                }
                match &state.transfer {
                    Some(t) if clipboard.take_cancel() => {
                        crate::note!(info, "剪贴板：用户取消了传输，这次粘贴不执行");
                        let kind = t.kind;
                        // 没发完的作废，告诉对方丢掉收到的那一半；内容还在本机，下次粘贴重新传
                        sender.forget();
                        sender.announce_held(kind, state.held_items);
                        state.held_seq = sender.seq();
                        state.transfer = None;
                        state.awaiting = None;
                        state.held = Some(kind);
                        clipboard.note(ClipNote::SendFailed(SendFail::Cancelled));
                        state.answer(Verdict::Drop);
                    }
                    Some(t) => {
                        let left = u64::try_from(sender.pending()).unwrap_or(u64::MAX);
                        let msgs = u64::try_from(t.msgs).unwrap_or(u64::MAX).max(1);
                        let sent = t.total.saturating_mul(msgs.saturating_sub(left)) / msgs;
                        clipboard.note(ClipNote::Sending { kind: t.kind, files: t.files, sent, total: t.total });
                    }
                    None => {
                        // 没在传的时候点的取消不留到下一次
                        let _ = clipboard.take_cancel();
                        refresh(&clipboard, &mut seen, &mut sender, &mut state, &mut remote, files, lazy).await?;
                    }
                }
            }
            // 限速：还不到能写的时候
            () = tokio::time::sleep(wait), if outgoing.is_some() && !wait.is_zero() => {}
            // 用可以取消的 write（没写进去就是一个字节都没写），记着写到哪里：
            // 其他分支先就绪时这次写被取消，下一轮接着写。原来用 write_all，被取消时已经写了一部分，
            // 下一轮又整条重写，流里的数据错位，剪贴板同步就断了（慢网络、大图片、文件时出现）。
            written = async {
                match &outgoing {
                    Some((bytes, at)) => {
                        let rest = bytes.get(*at..).unwrap_or_default();
                        send.write(rest.get(..quota).unwrap_or(rest)).await
                    }
                    None => std::future::pending().await,
                }
            }, if wait.is_zero() => {
                let n = written.map_err(|e| NetError::Closed(e.to_string()))?;
                throttle.sent(started.elapsed(), n);
                if let Some((bytes, at)) = outgoing.as_mut() {
                    *at += n;
                    if *at >= bytes.len() {
                        outgoing = None;
                    }
                }
            }
        }
    }
}

async fn next_paste(paste: Option<&mut PasteRequests>) -> Option<oneshot::Sender<Verdict>> {
    match paste {
        Some(paste) => paste.0.recv().await,
        None => std::future::pending().await,
    }
}

/// 本机剪贴板变了没有；变了就按内容决定：马上发（对方是老版本），还是记下来等对方来要。
async fn refresh(
    clipboard: &Arc<dyn SystemClipboard>,
    seen: &mut u64,
    sender: &mut Sender,
    state: &mut Lazy,
    remote: &mut Remote,
    files: bool,
    lazy: bool,
) -> Result<(), NetError> {
    let count = blocking(clipboard, |c| c.change_count()).await?;
    if count == *seen {
        return Ok(());
    }
    *seen = count;
    // 计数变了，但剪贴板里还是我们放的承诺（Windows 在程序来取数据时序号会变）：不是本机复制了东西
    if remote.promised.is_some() && blocking(clipboard, |c| c.promise_in_place()).await? {
        return Ok(());
    }
    if !lazy {
        // 对方是老版本：和原来一样，整份读出来马上发
        let started = std::time::Instant::now();
        match blocking(clipboard, |c| c.read()).await? {
            None => {}
            Some(Copied::TooLarge { kind, size }) => too_large(clipboard, sender, kind, size),
            Some(Copied::Clip(clip)) => {
                let took = started.elapsed().as_millis();
                let _ = offer(clipboard, sender, &clip, files, took);
            }
        }
        return Ok(());
    }
    // 本机复制了新东西：之前放的承诺（对方的内容）被顶掉了
    if remote.promised.is_some() {
        crate::note!(info, "剪贴板：本机复制了新内容，对方内容的承诺作废");
    }
    remote.dropped();
    // 任何内容都一样：复制时不传，只告诉对方有什么
    match blocking(clipboard, |c| c.peek()).await? {
        Some(Peeked::Text(_)) => hold(sender, state, ClipKind::Text, 1),
        Some(Peeked::Later(kind, items)) => hold(sender, state, kind, items),
        None => state.held = None,
    }
    Ok(())
}

/// 记下本机有这种内容、等对方来要时才传，并告诉对方。
fn hold(sender: &mut Sender, state: &mut Lazy, kind: ClipKind, items: u16) {
    crate::note!(
        info,
        "剪贴板：本机复制了 {kind:?}（{items} 项），先不传，对方来取或者在远程按粘贴时再传"
    );
    state.held = Some(kind);
    state.held_items = items;
    state.awaiting = None;
    sender.announce_held(kind, items);
    state.held_seq = sender.seq();
}

/// 本机有还没给对方的内容，现在要用了（在远程按了粘贴，或者对方的程序来取）：读出来开始传。
/// 开始传了（或者和上次发的一样、不用传）以后 `state.awaiting` 有值；传不了时答复等着的粘贴。
async fn pull(
    clipboard: &Arc<dyn SystemClipboard>,
    sender: &mut Sender,
    state: &mut Lazy,
    acks: &mut VecDeque<ClipMsg>,
    files: bool,
) -> Result<(), NetError> {
    let started = std::time::Instant::now();
    let sending = match blocking(clipboard, |c| c.read()).await? {
        None => {
            crate::note!(
                warn,
                "剪贴板：要传本机剪贴板里的内容，但读不出来，这次粘贴不执行"
            );
            clipboard.note(ClipNote::SendFailed(SendFail::Unreadable));
            None
        }
        Some(Copied::TooLarge { kind, size }) => {
            too_large(clipboard, sender, kind, size);
            None
        }
        Some(Copied::Clip(clip)) => {
            let took = started.elapsed().as_millis();
            let before = sender.seq();
            if offer(clipboard, sender, &clip, files, took) {
                Some(Transfer {
                    kind: clip.kind,
                    files: if clip.kind == ClipKind::Files {
                        packed_count(&clip.data)
                    } else {
                        0
                    },
                    total: len64(&clip),
                    msgs: sender.pending(),
                })
            } else if sender.seq() == before && sender.is_local(&clip) {
                // 和上次发过去的是同一份（哈希相同）：不重传，让对方接着用上一份，等它确认
                crate::note!(info, "剪贴板：这份内容和上次发的一样，不再传");
                acks.push_back(ClipMsg::Same {
                    seq: state.held_seq,
                });
                state.awaiting = Some(state.held_seq);
                return Ok(());
            } else {
                None
            }
        }
    };
    match sending {
        Some(transfer) => {
            clipboard.note(ClipNote::Sending {
                kind: transfer.kind,
                files: transfer.files,
                sent: 0,
                total: transfer.total,
            });
            state.awaiting = Some(sender.seq());
            state.transfer = Some(transfer);
        }
        None => state.answer(Verdict::Drop),
    }
    Ok(())
}

/// 本机的内容超过上限：告诉对方和用户。
fn too_large(clipboard: &Arc<dyn SystemClipboard>, sender: &mut Sender, kind: ClipKind, size: u64) {
    crate::note!(
        info,
        "剪贴板：本机复制的 {kind:?} {size} 字节超过上限，没有同步"
    );
    sender.announce_too_large(kind, size);
    clipboard.note(ClipNote::LocalTooLarge { kind, size });
}

/// 把一份内容交给发送方；开始发了返回 `true`。不发的原因（太大、对方不支持文件）告诉用户。
fn offer(
    clipboard: &Arc<dyn SystemClipboard>,
    sender: &mut Sender,
    clip: &Clip,
    files: bool,
    took_ms: u128,
) -> bool {
    if clip.kind == ClipKind::Files && !files {
        crate::note!(info, "剪贴板：本机复制了文件，对方版本不支持复制文件");
        clipboard.note(ClipNote::FilesUnsupported);
        return false;
    }
    match sender.offer(clip) {
        Offered::TooLarge => {
            crate::note!(
                info,
                "剪贴板：本机复制的 {:?} {} 字节超过上限，没有同步",
                clip.kind,
                clip.data.len()
            );
            clipboard.note(ClipNote::LocalTooLarge {
                kind: clip.kind,
                size: len64(clip),
            });
            false
        }
        Offered::Sending => {
            crate::note!(
                info,
                "剪贴板：发送 {:?} {} 字节（读取用时 {took_ms} ms）",
                clip.kind,
                clip.data.len()
            );
            true
        }
        Offered::Skip => false,
    }
}

/// 读写系统剪贴板会阻塞（Windows 上剪贴板被别的程序占着时要等），放到阻塞线程里做，
/// 不卡住会话（否则画面和输入的反馈被拖住，码率控制误以为网络拥塞）。
async fn blocking<T: Send + 'static>(
    clipboard: &Arc<dyn SystemClipboard>,
    f: impl FnOnce(&dyn SystemClipboard) -> T + Send + 'static,
) -> Result<T, NetError> {
    let c = Arc::clone(clipboard);
    // 只有运行时关闭（会话正在结束）或剪贴板实现崩溃时才失败：结束剪贴板同步
    tokio::task::spawn_blocking(move || f(c.as_ref()))
        .await
        .map_err(|e| NetError::Io(format!("剪贴板线程失败：{e}")))
}

/// 写剪贴板，被占用时重试几次（每次间隔 50 ms）。
fn write_with_retry(clipboard: &dyn SystemClipboard, clip: &Clip) -> Result<(), String> {
    let mut last = String::new();
    for attempt in 0..5 {
        match clipboard.write(clip) {
            Ok(()) => return Ok(()),
            Err(e) => last = e,
        }
        if attempt < 4 {
            std::thread::sleep(Duration::from_millis(50));
        }
    }
    Err(last)
}

fn len64(clip: &Clip) -> u64 {
    u64::try_from(clip.data.len()).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    /// 只实现必需方法的剪贴板：`note`、`peek`、`take_cancel` 用默认实现。
    struct Plain(Option<Copied>);
    impl SystemClipboard for Plain {
        fn change_count(&self) -> u64 {
            0
        }
        fn read(&self) -> Option<Copied> {
            self.0.clone()
        }
        fn write(&self, _clip: &Clip) -> Result<(), String> {
            Ok(())
        }
    }

    #[test]
    fn handle_and_default_methods() {
        let handle = ClipboardHandle(Arc::new(Plain(None)));
        assert_eq!(format!("{handle:?}"), "ClipboardHandle");
        handle.0.note(ClipNote::Received {
            kind: ClipKind::Text,
            size: 1,
        });
        assert_eq!(handle.0.change_count(), 0);
        assert!(handle.0.read().is_none());
        assert!(handle.0.write(&Clip::text("x")).is_ok());
        assert!(!handle.0.take_cancel());
        // 默认的“看一眼”：整份读出来，文字给出内容，其他只说种类
        assert_eq!(handle.0.peek(), None);
        let text = Plain(Some(Copied::Clip(Clip::text("t"))));
        assert_eq!(text.peek(), Some(Peeked::Text(Clip::text("t"))));
        let png = Plain(Some(Copied::Clip(Clip {
            kind: ClipKind::Png,
            data: vec![1],
        })));
        assert_eq!(png.peek(), Some(Peeked::Later(ClipKind::Png, 1)));
        let big = Plain(Some(Copied::TooLarge {
            kind: ClipKind::Files,
            size: 1,
        }));
        assert_eq!(big.peek(), Some(Peeked::Later(ClipKind::Files, 1)));
        let probe = RttProbe(Arc::new(|| Duration::from_millis(3)));
        assert_eq!(format!("{probe:?}"), "RttProbe");
        assert_eq!((probe.0)(), Duration::from_millis(3));
    }

    #[test]
    fn the_memory_clipboard_peeks_without_reading() {
        let clip = MemClipboard::default();
        assert_eq!(clip.peek(), None);
        clip.copy(Clip::text("t"));
        assert_eq!(clip.peek(), Some(Peeked::Text(Clip::text("t"))));
        clip.copy(Clip {
            kind: ClipKind::Files,
            data: vec![0, 0],
        });
        assert_eq!(clip.peek(), Some(Peeked::Later(ClipKind::Files, 0)));
        clip.copy_too_large(ClipKind::Png, 9);
        assert_eq!(clip.peek(), Some(Peeked::Later(ClipKind::Png, 1)));
        assert_eq!(clip.reads(), 0);
        assert!(matches!(clip.read(), Some(Copied::TooLarge { .. })));
        assert_eq!(clip.reads(), 1);
        clip.set_unreadable(true);
        assert_eq!(clip.read(), None);
        clip.set_unreadable(false);
        clip.clear();
        assert_eq!((clip.peek(), clip.read()), (None, None));
        assert!(!clip.take_cancel());
        clip.cancel();
        assert!(clip.take_cancel());
        assert!(!clip.take_cancel());
    }

    #[test]
    fn the_memory_clipboard_holds_a_promise_until_the_next_copy() {
        let clip = MemClipboard::default();
        let promise = |result: Result<Clip, FetchError>| Promise {
            kind: ClipKind::Png,
            items: 1,
            fetch: Arc::new(move |_| result.clone()),
        };
        // 默认不支持（没有延迟提供数据机制的系统）
        assert!(clip.promise(promise(Err(FetchError::Gone))).is_err());
        assert!(Plain(None).promise(promise(Err(FetchError::Gone))).is_err());
        clip.enable_promises();
        clip.copy(Clip::text("本机原来的"));
        let before = clip.change_count();
        let png = Clip {
            kind: ClipKind::Png,
            data: vec![1, 2],
        };
        clip.promise(promise(Ok(png.clone()))).unwrap();
        assert_eq!(format!("{:?}", promise(Ok(png.clone()))), "Promise(Png, 1)");
        // 放上承诺：原来的内容没了，计数变了；看和读都是空的（不会把承诺当成本机复制的）
        assert_ne!(clip.change_count(), before);
        assert_eq!(clip.promise_on_board(), Some((ClipKind::Png, 1)));
        assert!(clip.promise_in_place() && !Plain(None).promise_in_place());
        let count = clip.change_count();
        clip.touch();
        assert!(clip.promise_in_place() && clip.change_count() != count);
        assert_eq!((clip.get(), clip.peek(), clip.read()), (None, None, None));
        // 程序来粘贴：取到
        assert_eq!(clip.paste(Duration::ZERO), Some(png));
        // 取不到：告诉用户原因；会话已经结束就不说了
        for (why, noted) in [
            (FetchError::Timeout, true),
            (FetchError::Failed, true),
            (FetchError::Gone, false),
        ] {
            let had = clip.notes().len();
            clip.promise(promise(Err(why))).unwrap();
            assert_eq!(clip.paste(Duration::ZERO), None);
            let notes = clip.notes();
            assert_eq!(notes.len() > had, noted);
            if noted {
                assert_eq!(notes.last(), Some(&ClipNote::FetchFailed(why)));
            }
        }
        // 本机复制、清空、复制太大的内容：承诺被顶掉
        clip.copy(Clip::text("新的"));
        assert_eq!(clip.promise_on_board(), None);
        assert_eq!(clip.paste(Duration::ZERO), Some(Clip::text("新的")));
        clip.promise(promise(Err(FetchError::Gone))).unwrap();
        clip.copy_too_large(ClipKind::Files, 9);
        assert_eq!(clip.promise_on_board(), None);
        clip.promise(promise(Err(FetchError::Gone))).unwrap();
        clip.clear();
        assert_eq!(clip.promise_on_board(), None);
    }

    fn remote() -> (Remote, mpsc::UnboundedReceiver<FetchRequest>) {
        let (requests, fetches) = mpsc::unbounded_channel();
        let remote = Remote {
            promised: None,
            fetched: Arc::default(),
            previous: None,
            fetchers: Vec::new(),
            wanted: false,
            requests,
        };
        (remote, fetches)
    }

    #[test]
    fn a_promise_fetches_once_and_reuses_what_it_got() {
        let (mut r, mut fetches) = remote();
        let text = Clip::text("对方的内容");
        r.promised = Some(7);
        let promise = r.promise(7, ClipKind::Text, 1);
        // 没取到过：向同步循环要，等到超时
        assert_eq!(
            (promise.fetch)(Duration::from_millis(5)),
            Err(FetchError::Timeout)
        );
        let request = fetches.try_recv().unwrap();
        assert_eq!(request.seq, 7);
        // 同步循环答复了：等着的程序拿到
        let waiting = std::thread::spawn({
            let promise = promise.clone();
            move || (promise.fetch)(Duration::from_secs(5))
        });
        let request = fetches.blocking_recv().unwrap();
        r.fetchers.push(request.reply);
        r.wanted = true;
        r.deliver(7, text.clone());
        assert_eq!(waiting.join().unwrap(), Ok(text.clone()));
        assert!(!r.wanted);
        // 取到过：不再问同步循环
        assert_eq!((promise.fetch)(Duration::ZERO), Ok(text.clone()));
        assert!(fetches.try_recv().is_err());
        // 对方换了内容：取到的留作上一份；说“和上次一样”时接着用
        r.superseded();
        assert_eq!(r.promised, None);
        r.promised = Some(8);
        assert!(r.same(8));
        let again = r.promise(8, ClipKind::Text, 1);
        assert_eq!((again.fetch)(Duration::ZERO), Ok(text));
        // 上一份不在了、序号对不上：让对方重新传；没有承诺（系统不支持）：算在
        assert!(!r.same(8));
        r.superseded();
        r.promised = Some(9);
        assert!(!r.same(10));
        r.promised = None;
        assert!(r.same(11));
        // 本机复制了新内容：什么都不留，等着的程序拿到失败
        let (reply, result) = std::sync::mpsc::sync_channel(1);
        r.fetchers.push(reply);
        r.dropped();
        assert_eq!(result.recv().unwrap(), Err(FetchError::Failed));
        assert!(r.previous.is_none() && r.lock().is_none());
        // 同步循环没了：会话已经结束
        drop(fetches);
        assert_eq!((promise.fetch)(Duration::ZERO), Err(FetchError::Gone));
        let (r, fetches) = remote();
        let promise = r.promise(1, ClipKind::Png, 1);
        let waiting = std::thread::spawn(move || (promise.fetch)(Duration::from_secs(5)));
        let mut fetches = fetches;
        drop(fetches.blocking_recv());
        assert_eq!(waiting.join().unwrap(), Err(FetchError::Gone));
    }

    const fn key(code: u16, down: bool) -> InputEvent {
        InputEvent::Key { code, down }
    }

    #[test]
    fn paste_chords_are_recognised() {
        // (按着的修饰键, 按下的键, 是不是粘贴)
        let cases: [(&[u16], u16, bool); 10] = [
            (&[0xE0], 0x19, true),       // Ctrl+V
            (&[0xE4], 0x19, true),       // 右 Ctrl+V
            (&[0xE3], 0x19, true),       // Command/Win+V
            (&[0xE7], 0x19, true),       // 右 Command+V
            (&[0xE0, 0xE1], 0x19, true), // Ctrl+Shift+V
            (&[0xE1], 0x49, true),       // Shift+Insert
            (&[0xE5], 0x49, true),       // 右 Shift+Insert
            (&[], 0x19, false),          // 单按 V 是打字
            (&[0xE2], 0x19, false),      // Alt+V
            (&[0xE0], 0x06, false),      // Ctrl+C
        ];
        for (modifiers, code, want) in cases {
            let mut chord = PasteChord::default();
            for m in modifiers {
                assert!(!chord.on(&key(*m, true)));
            }
            assert_eq!(
                chord.on(&key(code, true)),
                want,
                "{modifiers:?} + {code:#x}"
            );
            // 抬起不算
            assert!(!chord.on(&key(code, false)));
        }
        // 修饰键抬起、全部松开以后不再算；鼠标、文字、粘贴事件不算
        let mut chord = PasteChord::default();
        assert!(!chord.on(&key(0xE0, true)));
        assert_eq!(chord.mask(), 0b0000_0001);
        assert!(!chord.on(&key(0xE0, false)));
        assert!(!chord.on(&key(0x19, true)));
        assert!(!chord.on(&key(0xE3, true)));
        assert!(!chord.on(&key(0xE5, true)));
        assert_eq!(chord.mask(), 0b0010_1000);
        assert!(!chord.on(&InputEvent::ReleaseAll));
        assert_eq!(chord.mask(), 0);
        assert!(!chord.on(&key(0x19, true)));
        assert!(!chord.on(&key(0xE0, true)));
        assert!(!chord.on(&InputEvent::MouseMove { x: 1, y: 2 }));
        assert!(!chord.on(&InputEvent::Text("v".into())));
        assert!(!chord.on(&InputEvent::Paste {
            modifiers: 1,
            code: 0x19
        }));
        assert!(chord.on(&key(0x19, true)));
    }

    /// 粘贴键的闸门加上两头的通道；`requests` 是同步循环那一侧。
    struct Gate {
        tx: mpsc::Sender<InputEvent>,
        rx: mpsc::Receiver<InputEvent>,
        requests: PasteRequests,
        task: tokio::task::JoinHandle<()>,
    }

    fn gate() -> Gate {
        let (tx, input) = mpsc::channel(64);
        let (out, rx) = mpsc::channel(64);
        let (sync, requests) = paste_channel();
        let task = tokio::spawn(gate_pastes(input, out, sync));
        Gate {
            tx,
            rx,
            requests,
            task,
        }
    }

    impl Gate {
        async fn send(&self, events: &[InputEvent]) {
            for event in events {
                self.tx.send(event.clone()).await.unwrap();
            }
        }
        async fn expect(&mut self, events: &[InputEvent]) {
            for event in events {
                assert_eq!(self.rx.recv().await.as_ref(), Some(event));
            }
        }
        /// 让闸门把已经收到的输入处理完，然后确认没有多发。
        async fn quiet(&mut self) {
            for _ in 0..20 {
                tokio::task::yield_now().await;
            }
            assert!(self.rx.try_recv().is_err());
        }
    }

    const MOUSE: InputEvent = InputEvent::MouseMove { x: 7, y: 9 };
    const CLICK: InputEvent = InputEvent::MouseButton {
        button: 0,
        down: true,
    };

    #[tokio::test]
    async fn a_paste_answered_at_once_is_sent_as_the_plain_key() {
        let mut g = gate();
        // 不是粘贴的输入直接过
        g.send(&[key(0xE0, true), key(0x04, true)]).await;
        g.expect(&[key(0xE0, true), key(0x04, true)]).await;
        // 按下 V：扣住，问同步循环
        g.send(&[key(0x19, true)]).await;
        let reply = g.requests.0.recv().await.unwrap();
        g.quiet().await;
        // 马上答复（没有要传的）：键还按着、修饰键没变，原样补发；之后的抬起照常
        reply.send(Verdict::Go).unwrap();
        g.expect(&[key(0x19, true)]).await;
        g.send(&[key(0x19, false), key(0xE0, false)]).await;
        g.expect(&[key(0x19, false), key(0xE0, false)]).await;
        // 输入那头关了：闸门结束
        drop(g.tx);
        g.task.await.unwrap();
        assert_eq!(g.rx.recv().await, None);
    }

    #[tokio::test]
    async fn only_the_paste_waits_and_other_input_keeps_flowing() {
        let mut g = gate();
        g.send(&[key(0xE0, true), key(0x19, true)]).await;
        g.expect(&[key(0xE0, true)]).await;
        let reply = g.requests.0.recv().await.unwrap();
        // 传输期间：鼠标、别的键照常；V 的重复和抬起扣住；修饰键照常抬起
        g.send(&[
            MOUSE,
            key(0x19, true),
            key(0x19, false),
            key(0xE0, false),
            CLICK,
            key(0x04, true),
        ])
        .await;
        g.expect(&[MOUSE, key(0xE0, false), CLICK, key(0x04, true)])
            .await;
        g.quiet().await;
        // 内容到了：原子的粘贴事件，带着用户当时按的组合键（Ctrl+V）
        reply.send(Verdict::Go).unwrap();
        g.expect(&[InputEvent::Paste {
            modifiers: 0b0000_0001,
            code: 0x19,
        }])
        .await;
        g.quiet().await;

        // 修饰键变了但 V 还没抬起：也用粘贴事件，之后 V 的抬起不发（按下没有原样发过）
        g.send(&[key(0xE3, true), key(0xE1, true), key(0x19, true)])
            .await;
        g.expect(&[key(0xE3, true), key(0xE1, true)]).await;
        let reply = g.requests.0.recv().await.unwrap();
        g.send(&[key(0xE1, false)]).await;
        g.expect(&[key(0xE1, false)]).await;
        reply.send(Verdict::Go).unwrap();
        g.expect(&[InputEvent::Paste {
            modifiers: 0b0000_1010,
            code: 0x19,
        }])
        .await;
        g.send(&[key(0x19, false), key(0x19, true)]).await;
        // 抬起被扣住；再按一次是新的一次粘贴
        let again = g.requests.0.recv().await.unwrap();
        g.quiet().await;
        drop(again);
        // 同步循环没了（答复端失效）：按可以粘贴处理
        g.expect(&[key(0x19, true)]).await;
    }

    #[tokio::test]
    async fn a_failed_or_cancelled_transfer_drops_the_paste() {
        let mut g = gate();
        g.send(&[key(0xE1, true), key(0x49, true)]).await;
        g.expect(&[key(0xE1, true)]).await;
        let reply = g.requests.0.recv().await.unwrap();
        reply.send(Verdict::Drop).unwrap();
        g.quiet().await;
        // 那个键之后的抬起也不发；再往后一切照常
        g.send(&[key(0x49, false), key(0xE1, false), key(0x49, true)])
            .await;
        g.expect(&[key(0xE1, false), key(0x49, true)]).await;

        // 等的时候失去焦点（全部松开）：照常转发，粘贴事件带着原来的组合键
        g.send(&[key(0x49, false), key(0xE4, true), key(0x19, true)])
            .await;
        g.expect(&[key(0x49, false), key(0xE4, true)]).await;
        let reply = g.requests.0.recv().await.unwrap();
        g.send(&[InputEvent::ReleaseAll]).await;
        g.expect(&[InputEvent::ReleaseAll]).await;
        reply.send(Verdict::Go).unwrap();
        g.expect(&[InputEvent::Paste {
            modifiers: 0b0001_0000,
            code: 0x19,
        }])
        .await;
        // 作废以后、键还没抬起时失去焦点：不再记着要扣住抬起
        g.send(&[key(0xE0, true), key(0x19, true)]).await;
        g.expect(&[key(0xE0, true)]).await;
        g.requests
            .0
            .recv()
            .await
            .unwrap()
            .send(Verdict::Drop)
            .unwrap();
        g.quiet().await;
        g.send(&[InputEvent::ReleaseAll, key(0x19, false)]).await;
        g.expect(&[InputEvent::ReleaseAll, key(0x19, false)]).await;
    }

    #[tokio::test]
    async fn the_gate_stops_when_the_session_is_gone() {
        // 会话那头关了：普通输入发不出去就结束
        let g = gate();
        drop(g.rx);
        g.tx.send(key(0x04, true)).await.unwrap();
        g.task.await.unwrap();
        // 粘贴放行时会话那头已经关了
        let mut g = gate();
        g.send(&[key(0xE0, true), key(0x19, true)]).await;
        g.expect(&[key(0xE0, true)]).await;
        let reply = g.requests.0.recv().await.unwrap();
        drop(g.rx);
        reply.send(Verdict::Go).unwrap();
        g.task.await.unwrap();
    }
}
