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
    /// 接下来这么多次写入失败（模拟剪贴板被别的程序占着）。
    failing: std::sync::atomic::AtomicU32,
}

impl MemClipboard {
    /// 模拟用户复制。
    pub fn copy(&self, clip: Clip) {
        let mut s = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        s.0 = s.0.wrapping_add(1);
        s.1 = Some(clip);
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
    fn read(&self) -> Option<Clip> {
        self.get()
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
    let mut seen = blocking(&clipboard, |c| c.change_count()).await?;
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
                            let (kind, size) = (clip.kind, len64(&clip));
                            if sender.is_local(&clip) {
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
            _ = tick.tick() => {
                let count = blocking(&clipboard, |c| c.change_count()).await?;
                if count != seen {
                    seen = count;
                    let started = std::time::Instant::now();
                    if let Some(clip) = blocking(&clipboard, |c| c.read()).await? {
                        let took = started.elapsed().as_millis();
                        match sender.offer(&clip) {
                            Offered::TooLarge => {
                                crate::note!(info, "剪贴板：本机复制的 {:?} {} 字节超过上限，没有同步", clip.kind, clip.data.len());
                                clipboard.note(ClipNote::LocalTooLarge { kind: clip.kind, size: len64(&clip) });
                            }
                            Offered::Sending => {
                                crate::note!(info, "剪贴板：发送 {:?} {} 字节（读取用时 {took} ms）", clip.kind, clip.data.len());
                            }
                            Offered::Skip => {}
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
    use super::*;

    /// 只实现必需方法的剪贴板：`note` 用默认实现。
    struct Plain;
    impl SystemClipboard for Plain {
        fn change_count(&self) -> u64 {
            0
        }
        fn read(&self) -> Option<Clip> {
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
}
