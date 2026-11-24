//! 在 QUIC 流上跑文件传输协议。

use std::path::Path;

use peerdesk_core::file::{Deframer, FileMeta, FileMsg, FileReceiver, FileSender, Outcome};
use quinn::{RecvStream, SendStream};

use crate::fs_store::FsStore;
use crate::stream::{read_file_msgs, write_file_msg, write_kind, StreamKind};
use crate::{Link, NetError};

/// 一个文件的最终结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileResult {
    /// 相对路径。
    pub path: String,
    /// 结果；连接中断等原因导致没有结果时为 `None`。
    pub outcome: Option<Outcome>,
}

fn results(files: &[FileMeta], outcomes: &[Option<Outcome>]) -> Vec<FileResult> {
    files
        .iter()
        .zip(outcomes)
        .map(|(file, outcome)| FileResult {
            path: file.path.clone(),
            outcome: *outcome,
        })
        .collect()
}

/// 传输进度（界面按它算进度、速度和剩余时间）。
#[derive(Debug, Default)]
pub struct Progress {
    /// 已经完成的字节（断点续传时已有的部分也算）。
    pub done: std::sync::atomic::AtomicU64,
    /// 总字节（知道文件清单之后才有）。
    pub total: std::sync::atomic::AtomicU64,
}

impl Progress {
    /// （已完成, 总数）。
    #[must_use]
    pub fn get(&self) -> (u64, u64) {
        use std::sync::atomic::Ordering::Relaxed;
        (self.done.load(Relaxed), self.total.load(Relaxed))
    }
}

fn advance(progress: &Progress, msg: &FileMsg) {
    use std::sync::atomic::Ordering::Relaxed;
    match msg {
        FileMsg::Offer { files } => {
            let total = files.iter().map(|f| f.size).fold(0u64, u64::saturating_add);
            progress.total.store(total, Relaxed);
        }
        FileMsg::Start { offset, .. } => {
            progress.done.fetch_add(*offset, Relaxed);
        }
        FileMsg::Data { data, .. } => {
            let n = u64::try_from(data.len()).unwrap_or(0);
            progress.done.fetch_add(n, Relaxed);
        }
        FileMsg::Answer { .. }
        | FileMsg::End { .. }
        | FileMsg::Result { .. }
        | FileMsg::Abort { .. } => {}
    }
}

/// 把 `root` 下的 `files` 发给对端。返回每个文件的结果和实际发送的字节数。
pub async fn send_files(
    link: &Link,
    root: &Path,
    files: Vec<FileMeta>,
) -> Result<(Vec<FileResult>, u64), NetError> {
    let (mut send, recv) = link.open_stream().await?;
    // 对端拒绝（例如没有文件权限）时会先写 Abort 再停止接收：这时我们写清单可能失败，
    // 但仍要读它的答复，拿到拒绝原因，而不是把整个会话当成出错。
    let kind = write_kind(&mut send, StreamKind::File).await;
    send_on(send, recv, root, files, &Progress::default(), kind).await
}

/// 在一条已经打开的流上把 `root` 下的 `files` 发给对端（文件管理的上传、被控端的下载也用它）。
/// `wrote` 是之前写流头的结果：写失败时仍读对端的答复，拿到拒绝原因。
pub async fn send_on(
    mut send: SendStream,
    mut recv: RecvStream,
    root: &Path,
    files: Vec<FileMeta>,
    progress: &Progress,
    wrote: Result<(), NetError>,
) -> Result<(Vec<FileResult>, u64), NetError> {
    let mut store = FsStore::new(root);
    let mut sender = FileSender::new(files.clone())?;
    let offer = sender.offer()?;
    advance(progress, &offer);
    let wrote = async {
        wrote?;
        write_file_msg(&mut send, &offer).await
    }
    .await;

    let mut deframer = Deframer::new();
    let mut answered = false;
    while !answered {
        let msgs = match read_file_msgs(&mut recv, &mut deframer).await {
            Ok(msgs) => msgs,
            Err(e) => return Err(wrote.err().unwrap_or(e)),
        };
        if msgs.is_empty() {
            return Err(wrote
                .err()
                .unwrap_or_else(|| NetError::Closed("对端在答复前关闭了文件流".into())));
        }
        for msg in &msgs {
            sender.on_msg(msg, &mut store)?;
        }
        answered = true;
    }

    loop {
        match sender.poll(&mut store) {
            // write_all 在对端来不及收时会等待，所以读盘速度自然跟随网络速度。
            Ok(Some(msg)) => {
                write_file_msg(&mut send, &msg).await?;
                advance(progress, &msg);
            }
            Ok(None) => break,
            Err(e) => {
                let reason: String = e.to_string().chars().take(60).collect();
                let _ = write_file_msg(&mut send, &FileMsg::Abort { reason }).await;
                let _ = send.finish();
                return Err(e.into());
            }
        }
    }
    let _ = send.finish();

    while !sender.is_finished() && !sender.is_aborted() {
        let msgs = read_file_msgs(&mut recv, &mut deframer).await?;
        if msgs.is_empty() {
            break;
        }
        for msg in &msgs {
            sender.on_msg(msg, &mut store)?;
        }
    }
    Ok((results(&files, sender.outcomes()), sender.bytes_sent()))
}

/// 处理对端发起的一次文件传输，文件写到 `root` 下。
pub async fn receive_files(
    send: SendStream,
    recv: RecvStream,
    root: &Path,
) -> Result<Vec<FileResult>, NetError> {
    receive_on(send, recv, root, &Progress::default()).await
}

/// 同 [`receive_files`]，并记下收到的字节数（文件管理的下载、被控端的上传用）。
pub async fn receive_on(
    mut send: SendStream,
    mut recv: RecvStream,
    root: &Path,
    progress: &Progress,
) -> Result<Vec<FileResult>, NetError> {
    let mut store = FsStore::new(root);
    let mut receiver = FileReceiver::new();
    let mut deframer = Deframer::new();
    let mut files: Vec<FileMeta> = Vec::new();
    while !receiver.is_finished() && receiver.aborted().is_none() {
        let msgs = read_file_msgs(&mut recv, &mut deframer).await?;
        if msgs.is_empty() {
            break;
        }
        for msg in msgs {
            if let FileMsg::Offer { files: offered } = &msg {
                files.clone_from(offered);
            }
            advance(progress, &msg);
            match receiver.on_msg(msg, &mut store) {
                Ok(replies) => {
                    for reply in &replies {
                        write_file_msg(&mut send, reply).await?;
                    }
                }
                Err(e) => {
                    let reason: String = e.to_string().chars().take(60).collect();
                    let _ = write_file_msg(&mut send, &FileMsg::Abort { reason }).await;
                    let _ = send.finish();
                    return Err(e.into());
                }
            }
        }
    }
    let _ = send.finish();
    Ok(results(&files, receiver.outcomes()))
}
