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

/// 把 `root` 下的 `files` 发给对端。返回每个文件的结果和实际发送的字节数。
pub async fn send_files(
    link: &Link,
    root: &Path,
    files: Vec<FileMeta>,
) -> Result<(Vec<FileResult>, u64), NetError> {
    let mut store = FsStore::new(root);
    let mut sender = FileSender::new(files.clone())?;
    let (mut send, mut recv) = link.open_stream().await?;
    let offer = sender.offer()?;
    // 对端拒绝（例如没有文件权限）时会先写 Abort 再停止接收：这时我们写清单可能失败，
    // 但仍要读它的答复，拿到拒绝原因，而不是把整个会话当成出错。
    let wrote = async {
        write_kind(&mut send, StreamKind::File).await?;
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
            Ok(Some(msg)) => write_file_msg(&mut send, &msg).await?,
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
    mut send: SendStream,
    mut recv: RecvStream,
    root: &Path,
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
