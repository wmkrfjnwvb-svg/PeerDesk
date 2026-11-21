//! 在会话里同步剪贴板：一条专用的可靠流，双向复用（协议见 `peerdesk_core::clipboard`）。
//!
//! 只有握手协商出 [`peerdesk_core::proto::Caps::CLIPBOARD`] 才开启：控制端打开这条流，被控端接受。

use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use peerdesk_core::clipboard::{
    Clip, ClipKind, Deframer, Limits, Offered, Received, Receiver, Sender,
};
use quinn::{RecvStream, SendStream};

use crate::NetError;

/// 多久看一次本机剪贴板有没有变（只读一个计数，开销可以忽略）。
pub const POLL: Duration = Duration::from_millis(100);

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
    /// 收到对方的内容，已写进本机剪贴板。
    Received {
        /// 种类。
        kind: ClipKind,
        /// 大小（字节）。
        size: u64,
    },
}

/// 本机剪贴板。真实实现在平台层；测试用 [`MemClipboard`]（测试不碰开发者电脑的剪贴板）。
pub trait SystemClipboard: Send + Sync {
    /// 剪贴板的变化计数：内容一变就不同。
    fn change_count(&self) -> u64;
    /// 读当前内容（文本或图片）；空的、读不了或是别的格式（例如文件）时为 `None`。
    fn read(&self) -> Option<Clip>;
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

/// 内存里的剪贴板，用于测试和仿真。
#[derive(Debug, Default)]
pub struct MemClipboard {
    state: Mutex<(u64, Option<Clip>, Vec<ClipNote>)>,
}

impl MemClipboard {
    /// 模拟用户复制。
    pub fn copy(&self, clip: Clip) {
        let mut s = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        s.0 = s.0.wrapping_add(1);
        s.1 = Some(clip);
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
    fn read(&self) -> Option<Clip> {
        self.get()
    }
    fn write(&self, clip: &Clip) -> Result<(), String> {
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

/// 在一条流上双向同步，直到流断开。开始时本机剪贴板里已有的内容不发（只同步之后复制的）。
pub async fn run(
    mut send: SendStream,
    mut recv: RecvStream,
    clipboard: Arc<dyn SystemClipboard>,
    limits: Limits,
) -> Result<(), NetError> {
    let mut sender = Sender::new(limits);
    let mut receiver = Receiver::new(limits);
    let mut deframer = Deframer::default();
    let mut seen = clipboard.change_count();
    let mut tick = tokio::time::interval(POLL);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        // 有待发的消息时一条一条地发，每条之间也能读对方的消息、发现新的复制（新的复制会作废旧的）。
        let next = sender.peek();
        tokio::select! {
            biased;
            read = recv.read(&mut buf) => {
                let Some(n) = read.map_err(|e| NetError::Closed(e.to_string()))? else {
                    return Ok(());
                };
                for msg in deframer.push(buf.get(..n).unwrap_or_default())? {
                    match receiver.on_msg(msg)? {
                        Some(Received::Clip(clip)) => {
                            sender.applied_remote(&clip);
                            clipboard.write(&clip).map_err(|e| NetError::Io(format!("写剪贴板失败：{e}")))?;
                            seen = clipboard.change_count();
                            crate::note!(info, "剪贴板：收到 {:?} {} 字节", clip.kind, clip.data.len());
                            clipboard.note(ClipNote::Received { kind: clip.kind, size: len64(&clip) });
                        }
                        Some(Received::TooLarge { kind, size }) => {
                            crate::note!(info, "剪贴板：对方复制的 {kind:?} {size} 字节超过上限，没有同步");
                            clipboard.note(ClipNote::RemoteTooLarge { kind, size });
                        }
                        None => {}
                    }
                }
            }
            _ = tick.tick() => {
                let count = clipboard.change_count();
                if count != seen {
                    seen = count;
                    if let Some(clip) = clipboard.read() {
                        if sender.offer(&clip) == Offered::TooLarge {
                            crate::note!(info, "剪贴板：本机复制的 {:?} {} 字节超过上限，没有同步", clip.kind, clip.data.len());
                            clipboard.note(ClipNote::LocalTooLarge { kind: clip.kind, size: len64(&clip) });
                        }
                    }
                }
            }
            written = async {
                match &next {
                    Some(msg) => send.write_all(&msg.frame()).await,
                    None => std::future::pending().await,
                }
            } => {
                written.map_err(|e| NetError::Closed(e.to_string()))?;
                let _ = sender.next_msg();
            }
        }
    }
}

fn len64(clip: &Clip) -> u64 {
    u64::try_from(clip.data.len()).unwrap_or(u64::MAX)
}
