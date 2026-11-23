//! 可靠流上的消息收发。每条流的第一个字节说明用途。

use peerdesk_core::file::{Deframer, FileMsg};
use peerdesk_core::proto::Control;
use peerdesk_core::signal::SignalMsg;
use quinn::{RecvStream, SendStream};

use crate::NetError;

/// 流的用途。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamKind {
    /// 控制消息。
    Control,
    /// 文件传输。
    File,
    /// 信令（设备与服务器之间）。
    Signal,
    /// 剪贴板同步（双方协商出剪贴板能力才有）。
    Clipboard,
    /// 声音：控制端打开声音时开，开着被控端就发声音（双方协商出声音能力才有）。
    Audio,
    /// 客户端 → 服务器：崩溃报告（见 [`peerdesk_core::crash`]）。
    Crash,
}

impl StreamKind {
    const fn id(self) -> u8 {
        match self {
            StreamKind::Control => 1,
            StreamKind::File => 2,
            StreamKind::Signal => 3,
            StreamKind::Clipboard => 4,
            StreamKind::Audio => 5,
            StreamKind::Crash => 6,
        }
    }

    const fn from_id(id: u8) -> Option<Self> {
        match id {
            1 => Some(StreamKind::Control),
            2 => Some(StreamKind::File),
            3 => Some(StreamKind::Signal),
            4 => Some(StreamKind::Clipboard),
            5 => Some(StreamKind::Audio),
            6 => Some(StreamKind::Crash),
            _ => None,
        }
    }
}

/// 控制消息的最大长度。
const MAX_CONTROL_LEN: usize = 4096;

fn closed(e: impl std::fmt::Display) -> NetError {
    NetError::Closed(e.to_string())
}

/// 在新打开的流上写入用途标记。
pub async fn write_kind(send: &mut SendStream, kind: StreamKind) -> Result<(), NetError> {
    send.write_all(&[kind.id()]).await.map_err(closed)
}

/// 读取对端打开的流的用途标记。
pub async fn read_kind(recv: &mut RecvStream) -> Result<StreamKind, NetError> {
    let mut byte = [0u8; 1];
    recv.read_exact(&mut byte).await.map_err(closed)?;
    StreamKind::from_id(byte[0]).ok_or(NetError::Protocol("未知的流类型"))
}

/// 写一条控制消息（2 字节长度前缀）。
pub async fn write_control(send: &mut SendStream, msg: &Control) -> Result<(), NetError> {
    let body = msg.encode()?;
    let len = u16::try_from(body.len()).map_err(|_| NetError::Protocol("控制消息过长"))?;
    let mut framed = Vec::with_capacity(body.len().saturating_add(2));
    framed.extend_from_slice(&len.to_be_bytes());
    framed.extend_from_slice(&body);
    send.write_all(&framed).await.map_err(closed)
}

/// 读一条控制消息。
pub async fn read_control(recv: &mut RecvStream) -> Result<Control, NetError> {
    let mut len = [0u8; 2];
    recv.read_exact(&mut len).await.map_err(closed)?;
    let len = usize::from(u16::from_be_bytes(len));
    if len > MAX_CONTROL_LEN {
        return Err(NetError::Protocol("控制消息过长"));
    }
    let mut body = vec![0u8; len];
    recv.read_exact(&mut body).await.map_err(closed)?;
    Ok(Control::decode(&body)?)
}

/// 写一条信令消息（2 字节长度前缀）。
pub async fn write_signal(send: &mut SendStream, msg: &SignalMsg) -> Result<(), NetError> {
    let body = msg.encode();
    let len = u16::try_from(body.len()).map_err(|_| NetError::Protocol("信令消息过长"))?;
    let mut framed = Vec::with_capacity(body.len().saturating_add(2));
    framed.extend_from_slice(&len.to_be_bytes());
    framed.extend_from_slice(&body);
    send.write_all(&framed).await.map_err(closed)
}

/// 读一条信令消息。
pub async fn read_signal(recv: &mut RecvStream) -> Result<SignalMsg, NetError> {
    let mut len = [0u8; 2];
    recv.read_exact(&mut len).await.map_err(closed)?;
    let len = usize::from(u16::from_be_bytes(len));
    if len > MAX_CONTROL_LEN {
        return Err(NetError::Protocol("信令消息过长"));
    }
    let mut body = vec![0u8; len];
    recv.read_exact(&mut body).await.map_err(closed)?;
    Ok(SignalMsg::decode(&body)?)
}

/// 写一条文件传输消息。
pub async fn write_file_msg(send: &mut SendStream, msg: &FileMsg) -> Result<(), NetError> {
    send.write_all(&msg.frame()?).await.map_err(closed)
}

/// 从流里读文件传输消息，直到至少读到一条或流结束（返回空列表）。
pub async fn read_file_msgs(
    recv: &mut RecvStream,
    deframer: &mut Deframer,
) -> Result<Vec<FileMsg>, NetError> {
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        match recv.read(&mut buf).await.map_err(closed)? {
            None => return Ok(Vec::new()),
            Some(n) => {
                let msgs = deframer.push(buf.get(..n).unwrap_or_default())?;
                if !msgs.is_empty() {
                    return Ok(msgs);
                }
            }
        }
    }
}

/// 发一份崩溃报告，等服务器确认已保存（回一个字节）。
pub async fn write_crash(
    send: &mut SendStream,
    recv: &mut RecvStream,
    report: &peerdesk_core::crash::Report,
) -> Result<(), NetError> {
    let bytes = report.encode().map_err(NetError::Core)?;
    send.write_all(&bytes).await.map_err(closed)?;
    send.finish().map_err(closed)?;
    let mut ack = [0u8; 1];
    recv.read_exact(&mut ack).await.map_err(closed)?;
    Ok(())
}

/// 读一份崩溃报告（不可信输入，限长）。
pub async fn read_crash(recv: &mut RecvStream) -> Result<peerdesk_core::crash::Report, NetError> {
    let bytes = recv
        .read_to_end(peerdesk_core::crash::MAX_REPORT)
        .await
        .map_err(closed)?;
    peerdesk_core::crash::Report::decode(&bytes).map_err(NetError::Core)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stream_kind_ids() {
        for kind in [
            StreamKind::Control,
            StreamKind::File,
            StreamKind::Signal,
            StreamKind::Clipboard,
            StreamKind::Audio,
            StreamKind::Crash,
        ] {
            assert_eq!(StreamKind::from_id(kind.id()), Some(kind));
        }
        assert_eq!(StreamKind::from_id(0), None);
        assert_eq!(StreamKind::from_id(7), None);
    }
}
