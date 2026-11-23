//! 崩溃报告：客户端崩溃后，下次启动时把日志的最后一段传给服务器（测试版默认打开）。
//!
//! 线上格式（一条专用的 QUIC 流）：`版本(1) | 说明(string16) | 日志长度(u32) | 日志`。
//! 服务器收下后回一个字节表示已保存。说明是“程序版本、提交号、系统、崩溃原因”。

use crate::error::Error;
use crate::wire::{Reader, Writer};

/// 格式版本。
pub const VERSION: u8 = 1;
/// 日志最多传这么多字节（取最后一段）。
pub const MAX_LOG: usize = 256 * 1024;
/// 说明最长字节数。
pub const MAX_SUMMARY: usize = 4096;
/// 整条报告的上限（服务器读流时用）。
pub const MAX_REPORT: usize = MAX_LOG + MAX_SUMMARY + 16;

/// 一份崩溃报告。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Report {
    /// 程序版本、提交号、系统、崩溃原因。
    pub summary: String,
    /// 日志的最后一段（UTF-8 文本，可能从一行中间开始）。
    pub log: Vec<u8>,
}

impl Report {
    /// 组装报告：说明截到上限（按字符边界），日志只留最后 [`MAX_LOG`] 字节。
    #[must_use]
    pub fn new(summary: &str, log: &[u8]) -> Self {
        let mut end = summary.len().min(MAX_SUMMARY);
        while !summary.is_char_boundary(end) {
            end = end.saturating_sub(1);
        }
        let start = log.len().saturating_sub(MAX_LOG);
        Self {
            summary: summary.get(..end).unwrap_or_default().to_owned(),
            log: log.get(start..).unwrap_or_default().to_vec(),
        }
    }

    /// 编码。
    ///
    /// # Errors
    /// 说明或日志超过上限（用 [`Report::new`] 组装的不会）。
    pub fn encode(&self) -> Result<Vec<u8>, Error> {
        if self.log.len() > MAX_LOG {
            return Err(Error::PayloadTooLarge {
                len: self.log.len(),
                max: MAX_LOG,
            });
        }
        let mut w = Writer::new();
        w.u8(VERSION).string16(&self.summary, MAX_SUMMARY)?;
        w.u32(u32::try_from(self.log.len()).unwrap_or(u32::MAX))
            .bytes(&self.log);
        Ok(w.into_vec())
    }

    /// 解码（来自网络，不可信）。
    ///
    /// # Errors
    /// 版本不认识、长度超限或数据不完整。
    pub fn decode(buf: &[u8]) -> Result<Self, Error> {
        let mut r = Reader::new(buf);
        let version = r.u8()?;
        if version != VERSION {
            return Err(Error::UnknownMessage(version));
        }
        let summary = r.string16(MAX_SUMMARY)?;
        let len = usize::try_from(r.u32()?).unwrap_or(usize::MAX);
        if len > MAX_LOG {
            return Err(Error::PayloadTooLarge { len, max: MAX_LOG });
        }
        let log = r.take(len)?.to_vec();
        Ok(Self { summary, log })
    }
}

/// 服务器上保存报告的文件名：`时间-设备指纹前 8 位.log`，只含安全字符。
#[must_use]
pub fn file_name(unix_secs: u64, device: &[u8; 32]) -> String {
    let fp: String = device.iter().take(4).map(|b| format!("{b:02x}")).collect();
    format!("{unix_secs}-{fp}.log")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_and_limits() {
        let r = Report::new(
            "PeerDesk 0.1.0 (abc1234) linux：panicked at x",
            b"line1\nline2\n",
        );
        assert_eq!(Report::decode(&r.encode().unwrap()).unwrap(), r);
        // 日志只留最后一段，说明按字符截断
        let big = vec![b'x'; MAX_LOG + 10];
        let long = "崩".repeat(MAX_SUMMARY);
        let r = Report::new(&long, &big);
        assert_eq!(r.log.len(), MAX_LOG);
        assert!(r.summary.len() <= MAX_SUMMARY && r.summary.len() > MAX_SUMMARY - 3);
        assert!(r.encode().unwrap().len() <= MAX_REPORT);
        // 手工造的超限报告编码失败
        let bad = Report {
            summary: String::new(),
            log: big,
        };
        assert!(bad.encode().is_err());
    }

    #[test]
    fn rejects_untrusted_input() {
        let ok = Report::new("s", b"log").encode().unwrap();
        let mut wrong_version = ok.clone();
        wrong_version[0] = 9;
        assert!(Report::decode(&wrong_version).is_err());
        assert!(Report::decode(&ok[..ok.len() - 1]).is_err());
        assert!(Report::decode(&[]).is_err());
        // 声称的日志长度超限
        let mut w = Writer::new();
        w.u8(VERSION).string16("s", MAX_SUMMARY).unwrap();
        w.u32(u32::try_from(MAX_LOG + 1).unwrap());
        assert!(Report::decode(&w.into_vec()).is_err());
    }

    #[test]
    fn file_names_are_safe() {
        assert_eq!(
            file_name(1_700_000_000, &[0xab; 32]),
            "1700000000-abababab.log"
        );
    }
}
