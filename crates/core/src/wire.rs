//! 线上字节的读写工具。读取端面对的是不可信输入：任何越界都返回错误，绝不 panic。

use crate::Error;

/// 从字节切片顺序读取，大端序。
#[derive(Debug, Clone)]
pub struct Reader<'a> {
    buf: &'a [u8],
}

impl<'a> Reader<'a> {
    /// 包装一段字节。
    #[must_use]
    pub const fn new(buf: &'a [u8]) -> Self {
        Self { buf }
    }

    /// 剩余字节数。
    #[must_use]
    pub const fn remaining(&self) -> usize {
        self.buf.len()
    }

    /// 取走 `n` 个字节。
    pub fn take(&mut self, n: usize) -> Result<&'a [u8], Error> {
        if self.buf.len() < n {
            return Err(Error::Truncated {
                need: n,
                have: self.buf.len(),
            });
        }
        let (head, tail) = self.buf.split_at(n);
        self.buf = tail;
        Ok(head)
    }

    fn array<const N: usize>(&mut self) -> Result<[u8; N], Error> {
        let bytes = self.take(N)?;
        let mut out = [0u8; N];
        out.copy_from_slice(bytes);
        Ok(out)
    }

    /// 读一个 u8。
    pub fn u8(&mut self) -> Result<u8, Error> {
        Ok(u8::from_be_bytes(self.array()?))
    }

    /// 读一个 u16。
    pub fn u16(&mut self) -> Result<u16, Error> {
        Ok(u16::from_be_bytes(self.array()?))
    }

    /// 读一个 u32。
    pub fn u32(&mut self) -> Result<u32, Error> {
        Ok(u32::from_be_bytes(self.array()?))
    }

    /// 读一个 u64。
    pub fn u64(&mut self) -> Result<u64, Error> {
        Ok(u64::from_be_bytes(self.array()?))
    }

    /// 读一个带 1 字节长度前缀的 UTF-8 字符串，长度不得超过 `max`。
    pub fn string(&mut self, max: usize) -> Result<String, Error> {
        let len = usize::from(self.u8()?);
        if len > max {
            return Err(Error::StringTooLong { len, max });
        }
        let bytes = self.take(len)?;
        std::str::from_utf8(bytes)
            .map(str::to_owned)
            .map_err(|_| Error::BadUtf8)
    }

    /// 读一个带 2 字节长度前缀的 UTF-8 字符串，长度不得超过 `max`。
    pub fn string16(&mut self, max: usize) -> Result<String, Error> {
        let len = usize::from(self.u16()?);
        if len > max {
            return Err(Error::StringTooLong { len, max });
        }
        let bytes = self.take(len)?;
        std::str::from_utf8(bytes)
            .map(str::to_owned)
            .map_err(|_| Error::BadUtf8)
    }

    /// 取走剩余的全部字节。
    #[must_use]
    pub const fn rest(self) -> &'a [u8] {
        self.buf
    }

    /// 要求已经读完，否则报多余字节。
    pub const fn finish(self) -> Result<(), Error> {
        if self.buf.is_empty() {
            Ok(())
        } else {
            Err(Error::TrailingBytes(self.buf.len()))
        }
    }
}

/// 顺序写入，大端序。
#[derive(Debug, Clone, Default)]
pub struct Writer {
    buf: Vec<u8>,
}

impl Writer {
    /// 空的写入器。
    #[must_use]
    pub const fn new() -> Self {
        Self { buf: Vec::new() }
    }

    /// 写一个 u8。
    pub fn u8(&mut self, v: u8) -> &mut Self {
        self.buf.push(v);
        self
    }

    /// 写一个 u16。
    pub fn u16(&mut self, v: u16) -> &mut Self {
        self.buf.extend_from_slice(&v.to_be_bytes());
        self
    }

    /// 写一个 u32。
    pub fn u32(&mut self, v: u32) -> &mut Self {
        self.buf.extend_from_slice(&v.to_be_bytes());
        self
    }

    /// 写一个 u64。
    pub fn u64(&mut self, v: u64) -> &mut Self {
        self.buf.extend_from_slice(&v.to_be_bytes());
        self
    }

    /// 原样写入字节。
    pub fn bytes(&mut self, v: &[u8]) -> &mut Self {
        self.buf.extend_from_slice(v);
        self
    }

    /// 写一个带 1 字节长度前缀的字符串，长度不得超过 `max`（且不超过 255）。
    pub fn string(&mut self, v: &str, max: usize) -> Result<&mut Self, Error> {
        let limit = max.min(usize::from(u8::MAX));
        let len = u8::try_from(v.len())
            .ok()
            .filter(|l| usize::from(*l) <= limit)
            .ok_or(Error::StringTooLong {
                len: v.len(),
                max: limit,
            })?;
        self.buf.push(len);
        self.buf.extend_from_slice(v.as_bytes());
        Ok(self)
    }

    /// 写一个带 2 字节长度前缀的字符串，长度不得超过 `max`（且不超过 65535）。
    pub fn string16(&mut self, v: &str, max: usize) -> Result<&mut Self, Error> {
        let limit = max.min(usize::from(u16::MAX));
        let len = u16::try_from(v.len())
            .ok()
            .filter(|l| usize::from(*l) <= limit)
            .ok_or(Error::StringTooLong {
                len: v.len(),
                max: limit,
            })?;
        self.u16(len);
        self.buf.extend_from_slice(v.as_bytes());
        Ok(self)
    }

    /// 已写入的长度。
    #[must_use]
    pub fn len(&self) -> usize {
        self.buf.len()
    }

    /// 是否还没写入任何内容。
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.buf.is_empty()
    }

    /// 取出结果。
    #[must_use]
    pub fn into_vec(self) -> Vec<u8> {
        self.buf
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_all_types() {
        let mut w = Writer::new();
        assert!(w.is_empty());
        w.u8(1).u16(2).u32(3).u64(4).bytes(&[9, 9]);
        w.string("区域", 32).unwrap();
        assert_eq!(w.len(), 1 + 2 + 4 + 8 + 2 + 1 + 6);
        let buf = w.into_vec();

        let mut r = Reader::new(&buf);
        assert_eq!(r.u8().unwrap(), 1);
        assert_eq!(r.u16().unwrap(), 2);
        assert_eq!(r.u32().unwrap(), 3);
        assert_eq!(r.u64().unwrap(), 4);
        assert_eq!(r.take(2).unwrap(), &[9, 9]);
        assert_eq!(r.string(32).unwrap(), "区域");
        assert_eq!(r.remaining(), 0);
        r.finish().unwrap();
    }

    #[test]
    fn truncated_reads_fail_without_consuming() {
        let mut r = Reader::new(&[1, 2, 3]);
        assert_eq!(r.u32(), Err(Error::Truncated { need: 4, have: 3 }));
        assert_eq!(r.u64(), Err(Error::Truncated { need: 8, have: 3 }));
        assert_eq!(r.remaining(), 3);
        assert_eq!(r.u16().unwrap(), 0x0102);
        assert_eq!(r.clone().rest(), &[3]);
        assert_eq!(r.finish(), Err(Error::TrailingBytes(1)));
        assert_eq!(
            Reader::new(&[]).u8(),
            Err(Error::Truncated { need: 1, have: 0 })
        );
    }

    #[test]
    fn string_limits_and_utf8() {
        // 长度前缀超过上限
        let mut r = Reader::new(&[5, b'a', b'b', b'c', b'd', b'e']);
        assert_eq!(r.string(4), Err(Error::StringTooLong { len: 5, max: 4 }));
        // 长度前缀声称的字节不存在
        let mut r = Reader::new(&[3, b'a']);
        assert_eq!(r.string(8), Err(Error::Truncated { need: 3, have: 1 }));
        // 非法 UTF-8
        let mut r = Reader::new(&[2, 0xff, 0xfe]);
        assert_eq!(r.string(8), Err(Error::BadUtf8));
        // 空字符串合法
        let mut r = Reader::new(&[0]);
        assert_eq!(r.string(0).unwrap(), "");

        // 2 字节长度前缀的版本
        let mut w = Writer::new();
        w.string16(&"p".repeat(300), 1024).unwrap();
        let buf = w.into_vec();
        assert_eq!(Reader::new(&buf).string16(1024).unwrap().len(), 300);
        assert_eq!(
            Reader::new(&buf).string16(299),
            Err(Error::StringTooLong { len: 300, max: 299 })
        );
        assert_eq!(
            Reader::new(&buf[..10]).string16(1024),
            Err(Error::Truncated { need: 300, have: 8 })
        );
        assert_eq!(Reader::new(&[0, 1, 0xff]).string16(8), Err(Error::BadUtf8));
        let mut w = Writer::new();
        assert_eq!(
            w.string16("abcde", 4).err(),
            Some(Error::StringTooLong { len: 5, max: 4 })
        );
        assert_eq!(
            w.string16(&"x".repeat(70_000), 100_000).err(),
            Some(Error::StringTooLong {
                len: 70_000,
                max: 65_535
            })
        );

        let mut w = Writer::new();
        assert_eq!(
            w.string("abcde", 4).err(),
            Some(Error::StringTooLong { len: 5, max: 4 })
        );
        let long = "x".repeat(300);
        assert_eq!(
            w.string(&long, 1000).err(),
            Some(Error::StringTooLong { len: 300, max: 255 })
        );
        assert!(w.is_empty());
    }
}
