//! 在会话里同步剪贴板：一条专用的可靠流，双向复用（协议见 `peerdesk_core::clipboard`）。
//!
//! 只有握手协商出 [`peerdesk_core::proto::Caps::CLIPBOARD`] 才开启：控制端打开这条流，被控端接受。
//!
//! 粘贴键和剪贴板内容的先后（[`gate_pastes`]）：输入走数据报，剪贴板内容走这条可靠流，两者之间没有顺序保证。
//! 控制端本机复制后马上在远程按粘贴，粘贴键会比内容先到，粘贴出来的是对方剪贴板里的旧内容
//! （复制文件时最明显：内容大，要读盘、传输、解包）。所以控制端看到粘贴键时先让同步循环马上检查
//! 本机剪贴板，等新内容到了对方并写进剪贴板（对方回 [`ClipMsg::Applied`]）再把粘贴键发出去。

use std::collections::{BTreeSet, VecDeque};
use std::sync::atomic::AtomicU64;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use peerdesk_core::clipboard::{
    Clip, ClipKind, ClipMsg, Deframer, Limits, Offered, Received, Receiver, Sender,
};
use peerdesk_core::input::InputEvent;
use quinn::{RecvStream, SendStream};
use tokio::sync::{mpsc, watch, Notify};

use crate::NetError;

/// 多久看一次本机剪贴板有没有变（只读一个计数，开销可以忽略）。
pub const POLL: Duration = Duration::from_millis(100);

/// 粘贴键最多等这么久；还没同步完也照发（和没有这个等待时一样，粘贴出来的可能是旧内容）。
/// 20 MB 的文件在 16 Mbps 的链路上要 10 秒。
pub const PASTE_WAIT_MAX: Duration = Duration::from_secs(10);
/// 粘贴键等了这么久还没发出去，就告诉用户在等什么（很快就好的不提示，免得一闪而过）。
pub const PASTE_NOTE_AFTER: Duration = Duration::from_millis(300);

/// 给界面看的剪贴板事件。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClipNote {
    /// 本机复制的内容太大，没有同步过去。
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
    /// 用户按了粘贴，但刚复制的内容还在发给对方：粘贴键等内容到了再发。
    PasteWaiting,
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

/// 本机剪贴板。真实实现在平台层；测试用 [`MemClipboard`]（测试不碰开发者电脑的剪贴板）。
pub trait SystemClipboard: Send + Sync {
    /// 剪贴板的变化计数：内容一变就不同。
    fn change_count(&self) -> u64;
    /// 读当前内容（文件、文本或图片）；空的、读不了或是别的格式时为 `None`。
    fn read(&self) -> Option<Copied>;
    /// 写入。
    fn write(&self, clip: &Clip) -> Result<(), String>;
    /// 剪贴板事件（给界面提示用）。默认不处理。
    fn note(&self, _note: ClipNote) {}
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
    failing: std::sync::atomic::AtomicU32,
}

impl MemClipboard {
    /// 模拟用户复制。
    pub fn copy(&self, clip: Clip) {
        let mut s = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        s.0 = s.0.wrapping_add(1);
        s.1 = Some(clip);
        s.3 = None;
    }

    /// 模拟用户复制了太大的内容（例如超过 20 MB 的文件）。
    pub fn copy_too_large(&self, kind: ClipKind, size: u64) {
        let mut s = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        s.0 = s.0.wrapping_add(1);
        s.3 = Some(Copied::TooLarge { kind, size });
    }

    /// 接下来 `n` 次写入失败。
    pub fn fail_next_writes(&self, n: u32) {
        self.failing.store(n, std::sync::atomic::Ordering::Relaxed);
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
        let st = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        st.3.clone().or_else(|| st.1.clone().map(Copied::Clip))
    }
    fn write(&self, clip: &Clip) -> Result<(), String> {
        let failing = &self.failing;
        if failing
            .fetch_update(
                std::sync::atomic::Ordering::Relaxed,
                std::sync::atomic::Ordering::Relaxed,
                |n| n.checked_sub(1),
            )
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
}

/// 同步的选项（由握手协商出的能力决定）。
#[derive(Debug, Clone, Default)]
pub struct SyncOptions {
    /// 对方支持复制文件（[`Caps::CLIPBOARD_FILES`](peerdesk_core::proto::Caps::CLIPBOARD_FILES)）。
    pub files: bool,
    /// 对方支持确认（[`Caps::CLIPBOARD_ACK`](peerdesk_core::proto::Caps::CLIPBOARD_ACK)）：
    /// 收到的内容处理完后回 [`ClipMsg::Applied`]，自己发出去的内容等对方的确认。
    pub ack: bool,
    /// 控制端：粘贴键等内容同步完再发（见 [`gate_pastes`]）。
    pub paste: Option<PasteSync>,
}

/// 粘贴键和剪贴板同步之间的协调：输入那边调 [`PasteSync::settle`]，同步循环在本机剪贴板里
/// 现在的内容已经到了对方以后放行。
#[derive(Debug, Clone)]
pub struct PasteSync(Arc<PasteShared>);

#[derive(Debug)]
struct PasteShared {
    /// 叫同步循环马上检查本机剪贴板（不等下一次轮询）。
    kick: Notify,
    /// 发出过的请求数。
    requested: AtomicU64,
    /// 已经放行到第几个请求。
    settled: watch::Sender<u64>,
}

impl Default for PasteSync {
    fn default() -> Self {
        Self::new()
    }
}

impl PasteSync {
    /// 创建。
    #[must_use]
    pub fn new() -> Self {
        Self(Arc::new(PasteShared {
            kick: Notify::new(),
            requested: AtomicU64::new(0),
            settled: watch::Sender::new(0),
        }))
    }

    /// 等本机剪贴板现在的内容同步到对方（没有新内容时马上返回）。同步已经结束时也马上返回。
    pub async fn settle(&self) {
        let mine = self
            .0
            .requested
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
            .saturating_add(1);
        let mut settled = self.0.settled.subscribe();
        self.0.kick.notify_one();
        let _ = settled.wait_for(|done| *done >= mine).await;
    }

    /// 同步循环用：目前发出过的请求数。
    #[must_use]
    pub fn requested(&self) -> u64 {
        self.0.requested.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// 同步循环用：等下一次“马上检查”。
    pub async fn kicked(&self) {
        self.0.kick.notified().await;
    }

    /// 同步循环用：到第 `upto` 个为止的请求都可以放行了。
    pub fn mark_settled(&self, upto: u64) {
        self.0.settled.send_if_modified(|done| {
            let newer = upto > *done;
            if newer {
                *done = upto;
            }
            newer
        });
    }
}

/// 同步循环结束时（流断了、出错）放行所有等着的粘贴键：没有同步了，等也没用。
struct SettleOnExit(Option<PasteSync>);

impl Drop for SettleOnExit {
    fn drop(&mut self) {
        if let Some(paste) = &self.0 {
            paste.mark_settled(u64::MAX);
        }
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

    /// 记下修饰键的状态；这个事件是按下粘贴键时返回 `true`。
    fn on(&mut self, event: &InputEvent) -> bool {
        match event {
            InputEvent::Key { code, down } if (0xE0..=0xE7).contains(code) => {
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
            | InputEvent::Text(_) => false,
        }
    }
}

/// 控制端：把界面来的输入转给会话，粘贴键等剪贴板同步完再发（原因见本模块开头）。
///
/// 等的时候后面的输入按顺序排在粘贴键后面，一起放行（顺序不能乱，否则修饰键先抬起，粘贴变成打字）。
/// 最多等 [`PASTE_WAIT_MAX`]；等得久时通过 [`ClipNote::PasteWaiting`] 告诉用户。
pub async fn gate_pastes(
    mut input: mpsc::Receiver<InputEvent>,
    out: mpsc::Sender<InputEvent>,
    sync: PasteSync,
    clipboard: Arc<dyn SystemClipboard>,
) {
    let mut chord = PasteChord::default();
    while let Some(event) = input.recv().await {
        if !chord.on(&event) {
            if out.send(event).await.is_err() {
                return;
            }
            continue;
        }
        let mut held = vec![event];
        let mut open = true;
        {
            let started = std::time::Instant::now();
            let settle = sync.settle();
            let limit = tokio::time::sleep(PASTE_WAIT_MAX);
            let note = tokio::time::sleep(PASTE_NOTE_AFTER);
            tokio::pin!(settle, limit, note);
            let mut noted = false;
            loop {
                tokio::select! {
                    biased;
                    () = &mut settle => {
                        if noted {
                            crate::note!(info, "剪贴板：内容到了对方，粘贴键等了 {} ms 后发出", started.elapsed().as_millis());
                        }
                        break;
                    }
                    () = &mut limit => {
                        crate::note!(warn, "剪贴板：等了 {} 秒内容还没同步完，粘贴键照发（粘贴出来的可能是旧内容）", PASTE_WAIT_MAX.as_secs());
                        break;
                    }
                    () = &mut note, if !noted => {
                        noted = true;
                        crate::note!(info, "剪贴板：按了粘贴，但刚复制的内容还在发给对方，粘贴键等内容到了再发");
                        clipboard.note(ClipNote::PasteWaiting);
                    }
                    next = input.recv(), if open => match next {
                        Some(next) => {
                            let _ = chord.on(&next);
                            held.push(next);
                        }
                        None => open = false,
                    },
                }
            }
        }
        for event in held {
            if out.send(event).await.is_err() {
                return;
            }
        }
        if !open {
            return;
        }
    }
}

/// 在一条流上双向同步，直到流断开。开始时本机剪贴板里已有的内容不发（只同步之后复制的）。
pub async fn run(
    mut send: SendStream,
    mut recv: RecvStream,
    clipboard: Arc<dyn SystemClipboard>,
    limits: Limits,
    opts: SyncOptions,
) -> Result<(), NetError> {
    let SyncOptions { files, ack, paste } = opts;
    let _settle_on_exit = SettleOnExit(paste.clone());
    // 要回给对方的确认（不放进 sender 的队列：新的复制会清掉那个队列）
    let mut acks: VecDeque<ClipMsg> = VecDeque::new();
    // 自己发出去、还在等对方确认的那一份的序号
    let mut awaiting: Option<u32> = None;
    // 到第几个粘贴请求为止，本机剪贴板已经在请求之后检查过
    let mut checked = 0u64;
    let mut sender = Sender::new(limits);
    let mut receiver = Receiver::new(limits);
    let mut deframer = Deframer::default();
    let mut seen = blocking(&clipboard, |c| c.change_count()).await?;
    let mut tick = tokio::time::interval(POLL);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut buf = vec![0u8; 64 * 1024];
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
        // 没有要发的、对方也确认了：本机剪贴板里的内容已经在对方那里，等着的粘贴键可以发了
        if let Some(paste) = paste
            .as_ref()
            .filter(|_| outgoing.is_none() && awaiting.is_none())
        {
            paste.mark_settled(checked);
        }
        tokio::select! {
            biased;
            read = recv.read(&mut buf) => {
                let Some(n) = read.map_err(|e| NetError::Closed(e.to_string()))? else {
                    return Ok(());
                };
                for msg in deframer.push(buf.get(..n).unwrap_or_default())? {
                    if let ClipMsg::Applied { seq } = msg {
                        if awaiting == Some(seq) {
                            awaiting = None;
                        }
                        continue;
                    }
                    let done = if let ClipMsg::Done { seq, .. } = &msg { Some(*seq) } else { None };
                    match receiver.on_msg(msg)? {
                        Some(Received::Clip(clip)) => {
                            // 处理完（写进去、和本机相同不用写、写不进去）都回确认。确认在这一段跑完以后
                            // 才会写到流里，所以对方收到时内容已经在剪贴板里了。
                            if let Some(seq) = done.filter(|_| ack) {
                                acks.push_back(ClipMsg::Applied { seq });
                            }
                            let (kind, size) = (clip.kind, len64(&clip));
                            // 和本机剪贴板现在的内容比一下（对方的内容可能比本机这边的轮询先到）
                            let hash = clip.hash();
                            let current = blocking(&clipboard, |c| c.read()).await?;
                            let same_now = matches!(&current, Some(Copied::Clip(c)) if c.hash() == hash);
                            if same_now || sender.is_local(&clip) {
                                // 本机已经是这份内容（例如另一个同步工具已经同步过来），不再写
                                crate::note!(info, "剪贴板：收到 {kind:?} {size} 字节，和本机相同，不再写入");
                                continue;
                            }
                            sender.applied_remote(&clip);
                            let started = std::time::Instant::now();
                            let written = blocking(&clipboard, move |c| write_with_retry(c, &clip)).await?;
                            let took = started.elapsed().as_millis();
                            match written {
                                Ok(()) => {
                                    crate::note!(info, "剪贴板：收到 {kind:?} {size} 字节，写入用时 {took} ms");
                                    clipboard.note(ClipNote::Received { kind, size });
                                }
                                // 写不进去（剪贴板被别的程序占着）：记下原因，继续同步后面的内容
                                Err(e) => crate::note!(warn, "剪贴板：收到 {kind:?} {size} 字节，写入失败（{took} ms）：{e}"),
                            }
                            seen = blocking(&clipboard, |c| c.change_count()).await?;
                        }
                        Some(Received::TooLarge { kind, size }) => {
                            crate::note!(info, "剪贴板：对方复制的 {kind:?} {size} 字节超过上限，没有同步");
                            clipboard.note(ClipNote::RemoteTooLarge { kind, size });
                        }
                        None => {}
                    }
                }
            }
            // 定时看一次；用户按了粘贴时马上看（不等下一次轮询）
            () = async {
                tokio::select! {
                    _ = tick.tick() => {}
                    () = kicked(paste.as_ref()) => {}
                }
            } => {
                let asked = paste.as_ref().map_or(0, PasteSync::requested);
                let count = blocking(&clipboard, |c| c.change_count()).await?;
                if count != seen {
                    seen = count;
                    let started = std::time::Instant::now();
                    match blocking(&clipboard, |c| c.read()).await? {
                        None => {}
                        Some(Copied::TooLarge { kind, size }) => {
                            crate::note!(info, "剪贴板：本机复制的 {kind:?} {size} 字节超过上限，没有同步");
                            sender.announce_too_large(kind, size);
                            awaiting = None;
                            clipboard.note(ClipNote::LocalTooLarge { kind, size });
                        }
                        Some(Copied::Clip(clip)) if clip.kind == ClipKind::Files && !files => {
                            crate::note!(info, "剪贴板：本机复制了文件，对方版本不支持复制文件");
                            clipboard.note(ClipNote::FilesUnsupported);
                        }
                        Some(Copied::Clip(clip)) => {
                        let took = started.elapsed().as_millis();
                        match sender.offer(&clip) {
                            Offered::TooLarge => {
                                awaiting = None;
                                crate::note!(info, "剪贴板：本机复制的 {:?} {} 字节超过上限，没有同步", clip.kind, clip.data.len());
                                clipboard.note(ClipNote::LocalTooLarge { kind: clip.kind, size: len64(&clip) });
                            }
                            Offered::Sending => {
                                awaiting = ack.then_some(sender.seq());
                                crate::note!(info, "剪贴板：发送 {:?} {} 字节（读取用时 {took} ms）", clip.kind, clip.data.len());
                            }
                            Offered::Skip => {}
                        }
                        }
                    }
                }
                checked = asked;
            }
            // 用可以取消的 write（没写进去就是一个字节都没写），记着写到哪里：
            // 其他分支先就绪时这次写被取消，下一轮接着写。原来用 write_all，被取消时已经写了一部分，
            // 下一轮又整条重写，流里的数据错位，剪贴板同步就断了（慢网络、大图片、文件时出现）。
            written = async {
                match &outgoing {
                    Some((bytes, at)) => send.write(bytes.get(*at..).unwrap_or_default()).await,
                    None => std::future::pending().await,
                }
            } => {
                let n = written.map_err(|e| NetError::Closed(e.to_string()))?;
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

async fn kicked(paste: Option<&PasteSync>) {
    match paste {
        Some(paste) => paste.kicked().await,
        None => std::future::pending().await,
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

    /// 只实现必需方法的剪贴板：`note` 用默认实现。
    struct Plain;
    impl SystemClipboard for Plain {
        fn change_count(&self) -> u64 {
            0
        }
        fn read(&self) -> Option<Copied> {
            None
        }
        fn write(&self, _clip: &Clip) -> Result<(), String> {
            Ok(())
        }
    }

    #[test]
    fn handle_and_default_note() {
        let handle = ClipboardHandle(Arc::new(Plain));
        assert_eq!(format!("{handle:?}"), "ClipboardHandle");
        handle.0.note(ClipNote::Received {
            kind: ClipKind::Text,
            size: 1,
        });
        assert_eq!(handle.0.change_count(), 0);
        assert!(handle.0.read().is_none());
        assert!(handle.0.write(&Clip::text("x")).is_ok());
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
        // 修饰键抬起、全部松开以后不再算；鼠标和文字不算
        let mut chord = PasteChord::default();
        assert!(!chord.on(&key(0xE0, true)));
        assert!(!chord.on(&key(0xE0, false)));
        assert!(!chord.on(&key(0x19, true)));
        assert!(!chord.on(&key(0xE3, true)));
        assert!(!chord.on(&InputEvent::ReleaseAll));
        assert!(!chord.on(&key(0x19, true)));
        assert!(!chord.on(&key(0xE0, true)));
        assert!(!chord.on(&InputEvent::MouseMove { x: 1, y: 2 }));
        assert!(!chord.on(&InputEvent::Text("v".into())));
        assert!(chord.on(&key(0x19, true)));
    }

    #[tokio::test]
    async fn paste_sync_settles_requests_in_order_and_on_exit() {
        let sync = PasteSync::default();
        assert_eq!(sync.requested(), 0);
        let waiter = tokio::spawn({
            let sync = sync.clone();
            async move { sync.settle().await }
        });
        // 同步循环：被叫醒，看到一个请求，放行
        sync.kicked().await;
        assert_eq!(sync.requested(), 1);
        assert!(!waiter.is_finished());
        sync.mark_settled(0);
        tokio::task::yield_now().await;
        assert!(!waiter.is_finished());
        sync.mark_settled(1);
        waiter.await.unwrap();
        // 放行的编号不会倒退
        sync.mark_settled(0);
        // 同步循环结束：以后的请求马上放行
        drop(SettleOnExit(Some(sync.clone())));
        drop(SettleOnExit(None));
        sync.settle().await;
        assert_eq!(sync.requested(), 2);
    }

    /// 粘贴键的闸门加上两头的通道。
    struct Gate {
        tx: mpsc::Sender<InputEvent>,
        rx: mpsc::Receiver<InputEvent>,
        sync: PasteSync,
        clip: Arc<MemClipboard>,
        task: tokio::task::JoinHandle<()>,
    }

    fn gate() -> Gate {
        let (tx, input) = mpsc::channel(64);
        let (out, rx) = mpsc::channel(64);
        let sync = PasteSync::new();
        let clip = Arc::new(MemClipboard::default());
        let task = tokio::spawn(gate_pastes(input, out, sync.clone(), clip.clone()));
        Gate {
            tx,
            rx,
            sync,
            clip,
            task,
        }
    }

    #[tokio::test(start_paused = true)]
    async fn the_paste_key_waits_for_the_sync_and_keeps_the_order() {
        let mut g = gate();
        // 不是粘贴的输入直接过
        g.tx.send(key(0xE0, true)).await.unwrap();
        assert_eq!(g.rx.recv().await, Some(key(0xE0, true)));
        // 按下 V：等同步；后面的抬起排在它后面，顺序不变
        for event in [key(0x19, true), key(0x19, false), key(0xE0, false)] {
            g.tx.send(event).await.unwrap();
        }
        g.sync.kicked().await;
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(g.rx.try_recv().is_err(), "内容还没到对方，粘贴键不能先发");
        assert!(g.clip.notes().is_empty(), "很快就好的不提示");
        // 等得久了告诉用户在等什么（只说一次）
        tokio::time::sleep(PASTE_NOTE_AFTER * 3).await;
        assert_eq!(g.clip.notes(), vec![ClipNote::PasteWaiting]);
        assert!(g.rx.try_recv().is_err());
        // 内容到了：按原来的顺序放行
        g.sync.mark_settled(g.sync.requested());
        for want in [key(0x19, true), key(0x19, false), key(0xE0, false)] {
            assert_eq!(g.rx.recv().await, Some(want));
        }
        // 没有新内容时（同步循环马上放行）粘贴键不耽搁，也不提示
        g.tx.send(key(0xE3, true)).await.unwrap();
        g.tx.send(key(0x19, true)).await.unwrap();
        assert_eq!(g.rx.recv().await, Some(key(0xE3, true)));
        g.sync.kicked().await;
        g.sync.mark_settled(g.sync.requested());
        assert_eq!(g.rx.recv().await, Some(key(0x19, true)));
        assert_eq!(g.clip.notes().len(), 1);
        // 输入那头关了：闸门结束
        drop(g.tx);
        g.task.await.unwrap();
        assert_eq!(g.rx.recv().await, None);
    }

    #[tokio::test(start_paused = true)]
    async fn the_paste_key_is_sent_anyway_after_the_longest_wait() {
        let mut g = gate();
        g.tx.send(key(0xE0, true)).await.unwrap();
        g.tx.send(key(0x19, true)).await.unwrap();
        assert_eq!(g.rx.recv().await, Some(key(0xE0, true)));
        let started = tokio::time::Instant::now();
        // 同步一直没完成：到时间照发
        assert_eq!(g.rx.recv().await, Some(key(0x19, true)));
        assert_eq!(started.elapsed(), PASTE_WAIT_MAX);
        assert_eq!(g.clip.notes(), vec![ClipNote::PasteWaiting]);
        // 等的时候输入那头关了：把等着的发完再结束
        g.tx.send(key(0x19, true)).await.unwrap();
        g.tx.send(key(0x19, false)).await.unwrap();
        g.sync.kicked().await;
        drop(g.tx);
        tokio::time::sleep(Duration::from_millis(10)).await;
        g.sync.mark_settled(u64::MAX);
        assert_eq!(g.rx.recv().await, Some(key(0x19, true)));
        assert_eq!(g.rx.recv().await, Some(key(0x19, false)));
        g.task.await.unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn the_gate_stops_when_the_session_is_gone() {
        // 会话那头关了：普通输入发不出去就结束
        let g = gate();
        drop(g.rx);
        g.tx.send(key(0x04, true)).await.unwrap();
        g.task.await.unwrap();
        // 等着的粘贴键放行时会话那头已经关了
        let g = gate();
        g.tx.send(key(0xE0, true)).await.unwrap();
        g.tx.send(key(0x19, true)).await.unwrap();
        g.sync.kicked().await;
        drop(g.rx);
        g.sync.mark_settled(u64::MAX);
        g.task.await.unwrap();
    }
}
