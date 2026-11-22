//! 剪贴板同步的协议和逻辑（设计见 `docs/clipboard.md`）。
//!
//! 复制时直接把内容推给对方（不等对方粘贴时再要），有大小上限，超过只通知对方、由界面提示走文件传输。
//! 新的复制开始时，没发完的旧内容作废：只同步最新的一份。写进本机剪贴板的远端内容记下哈希，
//! 下次读到相同内容不再推回去（防回声）。

use std::collections::VecDeque;

use crate::wire::{Reader, Writer};
use crate::Error;

/// 每块内容的大小。
pub const CHUNK: usize = 64 * 1024;
/// 一条消息的最大长度（块加上消息头）。
pub const MAX_FRAME: usize = CHUNK + 64;

/// 剪贴板内容的种类。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClipKind {
    /// UTF-8 文本。
    Text,
    /// PNG 图片。
    Png,
}

impl ClipKind {
    const fn id(self) -> u8 {
        match self {
            Self::Text => 1,
            Self::Png => 2,
        }
    }

    const fn from_id(id: u8) -> Result<Self, Error> {
        match id {
            1 => Ok(Self::Text),
            2 => Ok(Self::Png),
            other => Err(Error::UnknownMessage(other)),
        }
    }
}

/// 一份剪贴板内容。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Clip {
    /// 种类。
    pub kind: ClipKind,
    /// 内容：文本是 UTF-8，图片是 PNG。
    pub data: Vec<u8>,
}

impl Clip {
    /// 文本。
    #[must_use]
    pub fn text(s: &str) -> Self {
        Self {
            kind: ClipKind::Text,
            data: s.as_bytes().to_vec(),
        }
    }

    /// 内容的哈希（种类也算在内）。
    #[must_use]
    /// 文本不计换行符的差别（Windows 存 `\r\n`，macOS/Linux 存 `\n`，同一段文字来回同步不算变化）。
    pub fn hash(&self) -> [u8; 32] {
        let mut h = blake3::Hasher::new();
        h.update(&[self.kind.id()]);
        match self.kind {
            ClipKind::Text => {
                for part in self.data.split(|&b| b == b'\r') {
                    h.update(part);
                }
            }
            ClipKind::Png => {
                h.update(&self.data);
            }
        }
        *h.finalize().as_bytes()
    }
}

/// 大小上限（字节）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// 文本。
    pub text: u64,
    /// 图片（PNG 编码后）。
    pub image: u64,
}

impl Limits {
    /// 默认：文本 4 MB，图片 16 MB。
    pub const DEFAULT: Self = Self {
        text: 4 * 1024 * 1024,
        image: 16 * 1024 * 1024,
    };

    /// 这种内容的上限。
    #[must_use]
    pub const fn of(self, kind: ClipKind) -> u64 {
        match kind {
            ClipKind::Text => self.text,
            ClipKind::Png => self.image,
        }
    }
}

/// 剪贴板流上的消息。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClipMsg {
    /// 开始一份新内容。`seq` 比之前的大，之前没发完的作废。
    Offer {
        /// 序号。
        seq: u32,
        /// 种类。
        kind: ClipKind,
        /// 总字节数。
        size: u64,
    },
    /// 一块内容。
    Chunk {
        /// 属于哪一份。
        seq: u32,
        /// 内容。
        data: Vec<u8>,
    },
    /// 这一份发完了。
    Done {
        /// 序号。
        seq: u32,
        /// 内容的哈希，接收方核对。
        hash: [u8; 32],
    },
    /// 对方复制的内容超过上限，没有发过来。
    TooLarge {
        /// 序号。
        seq: u32,
        /// 种类。
        kind: ClipKind,
        /// 大小。
        size: u64,
    },
}

impl ClipMsg {
    /// 编码并加上 4 字节长度前缀，可直接写入流。
    #[must_use]
    pub fn frame(&self) -> Vec<u8> {
        let mut body = Writer::new();
        match self {
            Self::Offer { seq, kind, size } => {
                body.u8(1).u32(*seq).u8(kind.id()).u64(*size);
            }
            Self::Chunk { seq, data } => {
                body.u8(2).u32(*seq).bytes(data);
            }
            Self::Done { seq, hash } => {
                body.u8(3).u32(*seq).bytes(hash);
            }
            Self::TooLarge { seq, kind, size } => {
                body.u8(4).u32(*seq).u8(kind.id()).u64(*size);
            }
        }
        let body = body.into_vec();
        let mut w = Writer::new();
        // 消息长度不会超过 MAX_FRAME（块最大 CHUNK），装得进 u32。
        w.u32(u32::try_from(body.len()).unwrap_or(u32::MAX))
            .bytes(&body);
        w.into_vec()
    }

    /// 解码一条消息（不含长度前缀）。末尾多出的字节忽略（兼容以后追加的字段），`Chunk` 除外。
    pub fn decode(buf: &[u8]) -> Result<Self, Error> {
        let mut r = Reader::new(buf);
        Ok(match r.u8()? {
            1 => Self::Offer {
                seq: r.u32()?,
                kind: ClipKind::from_id(r.u8()?)?,
                size: r.u64()?,
            },
            2 => {
                let seq = r.u32()?;
                let data = buf.get(5..).unwrap_or_default().to_vec();
                Self::Chunk { seq, data }
            }
            3 => {
                let seq = r.u32()?;
                let mut hash = [0u8; 32];
                hash.copy_from_slice(r.take(32)?);
                Self::Done { seq, hash }
            }
            4 => Self::TooLarge {
                seq: r.u32()?,
                kind: ClipKind::from_id(r.u8()?)?,
                size: r.u64()?,
            },
            other => return Err(Error::UnknownMessage(other)),
        })
    }
}

/// 从流里读到的字节中切出一条条消息。
#[derive(Debug, Clone, Default)]
pub struct Deframer {
    buf: Vec<u8>,
}

impl Deframer {
    /// 放入新读到的字节，取出所有完整的消息。
    pub fn push(&mut self, bytes: &[u8]) -> Result<Vec<ClipMsg>, Error> {
        self.buf.extend_from_slice(bytes);
        let mut out = Vec::new();
        loop {
            let mut r = Reader::new(&self.buf);
            let Ok(len) = r.u32() else { break };
            let len = usize::try_from(len).unwrap_or(usize::MAX);
            if len > MAX_FRAME {
                return Err(Error::PayloadTooLarge {
                    len,
                    max: MAX_FRAME,
                });
            }
            let Ok(body) = r.take(len) else { break };
            out.push(ClipMsg::decode(body)?);
            self.buf.drain(..len.saturating_add(4));
        }
        Ok(out)
    }
}

/// 本机剪贴板变了以后该怎么办。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Offered {
    /// 不用发：和刚从对方收到的一样（回声），或者和上次发的一样。
    Skip,
    /// 超过上限，只通知对方。
    TooLarge,
    /// 开始发送。
    Sending,
}

/// 发送方：把本机剪贴板的新内容切成消息。
#[derive(Debug, Clone)]
pub struct Sender {
    limits: Limits,
    seq: u32,
    /// 最近一次从对方收到、写进本机剪贴板的内容。
    echo: Option<[u8; 32]>,
    /// 最近一次发出去的内容。
    last: Option<[u8; 32]>,
    queue: VecDeque<ClipMsg>,
}

impl Sender {
    /// 创建。
    #[must_use]
    pub const fn new(limits: Limits) -> Self {
        Self {
            limits,
            seq: 0,
            echo: None,
            last: None,
            queue: VecDeque::new(),
        }
    }

    /// 本机剪贴板有了新内容。没发完的旧内容作废。
    pub fn offer(&mut self, clip: &Clip) -> Offered {
        let hash = clip.hash();
        if self.echo == Some(hash) || self.last == Some(hash) {
            return Offered::Skip;
        }
        self.last = Some(hash);
        self.seq = self.seq.wrapping_add(1);
        self.queue.clear();
        let seq = self.seq;
        let size = u64::try_from(clip.data.len()).unwrap_or(u64::MAX);
        if size > self.limits.of(clip.kind) {
            self.queue.push_back(ClipMsg::TooLarge {
                seq,
                kind: clip.kind,
                size,
            });
            return Offered::TooLarge;
        }
        self.queue.push_back(ClipMsg::Offer {
            seq,
            kind: clip.kind,
            size,
        });
        for part in clip.data.chunks(CHUNK) {
            self.queue.push_back(ClipMsg::Chunk {
                seq,
                data: part.to_vec(),
            });
        }
        self.queue.push_back(ClipMsg::Done { seq, hash });
        Offered::Sending
    }

    /// 刚把对方的内容写进本机剪贴板：之后本机读到同样的内容不再发回去。
    pub fn applied_remote(&mut self, clip: &Clip) {
        self.echo = Some(clip.hash());
    }

    /// 本机剪贴板里已经是这份内容（刚发出去的，或刚从对方收到的）：不用再写一遍。
    /// 两套同步工具同时开着（例如另一个远程软件也在同步剪贴板）时，避免来回覆盖。
    #[must_use]
    pub fn is_local(&self, clip: &Clip) -> bool {
        let hash = clip.hash();
        self.last == Some(hash) || self.echo == Some(hash)
    }

    /// 下一条要发的消息（不取出）。
    #[must_use]
    pub fn peek(&self) -> Option<ClipMsg> {
        self.queue.front().cloned()
    }

    /// 下一条要发的消息。
    pub fn next_msg(&mut self) -> Option<ClipMsg> {
        self.queue.pop_front()
    }

    /// 还有没发的消息。
    #[must_use]
    pub fn pending(&self) -> usize {
        self.queue.len()
    }
}

/// 接收方得到的结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Received {
    /// 一份完整的内容，可以写进本机剪贴板。
    Clip(Clip),
    /// 对方复制的内容太大，没发过来（界面提示用文件传输）。
    TooLarge {
        /// 种类。
        kind: ClipKind,
        /// 大小。
        size: u64,
    },
}

#[derive(Debug, Clone)]
struct Incoming {
    seq: u32,
    kind: ClipKind,
    size: u64,
    data: Vec<u8>,
}

/// 接收方：拼出完整内容并核对。
#[derive(Debug, Clone)]
pub struct Receiver {
    limits: Limits,
    current: Option<Incoming>,
    /// 见过的最大序号：更老的消息丢掉。
    newest: Option<u32>,
}

impl Receiver {
    /// 创建。
    #[must_use]
    pub const fn new(limits: Limits) -> Self {
        Self {
            limits,
            current: None,
            newest: None,
        }
    }

    fn fresh(&mut self, seq: u32) -> bool {
        // 序号回绕时 wrapping 比较：新的比旧的“大”不到一半范围。
        let newer = self
            .newest
            .is_none_or(|n| seq.wrapping_sub(n) < u32::MAX / 2);
        if newer {
            self.newest = Some(seq);
        }
        newer
    }

    /// 处理一条消息。对方违反协议（超过上限、内容和声明的不符）时返回错误。
    pub fn on_msg(&mut self, msg: ClipMsg) -> Result<Option<Received>, Error> {
        match msg {
            ClipMsg::Offer { seq, kind, size } => {
                if !self.fresh(seq) {
                    return Ok(None);
                }
                if size > self.limits.of(kind) {
                    return Err(Error::PayloadTooLarge {
                        len: usize::try_from(size).unwrap_or(usize::MAX),
                        max: usize::try_from(self.limits.of(kind)).unwrap_or(usize::MAX),
                    });
                }
                self.current = Some(Incoming {
                    seq,
                    kind,
                    size,
                    data: Vec::new(),
                });
                Ok(None)
            }
            ClipMsg::Chunk { seq, data } => {
                let Some(cur) = self.current.as_mut().filter(|c| c.seq == seq) else {
                    return Ok(None);
                };
                let total = cur.data.len().saturating_add(data.len());
                if u64::try_from(total).unwrap_or(u64::MAX) > cur.size {
                    self.current = None;
                    return Err(Error::Inconsistent("剪贴板内容比声明的长"));
                }
                cur.data.extend_from_slice(&data);
                Ok(None)
            }
            ClipMsg::Done { seq, hash } => {
                let Some(cur) = self.current.take_if(|c| c.seq == seq) else {
                    return Ok(None);
                };
                let clip = Clip {
                    kind: cur.kind,
                    data: cur.data,
                };
                if u64::try_from(clip.data.len()).unwrap_or(u64::MAX) != cur.size
                    || clip.hash() != hash
                {
                    return Err(Error::Inconsistent("剪贴板内容校验不对"));
                }
                if clip.kind == ClipKind::Text && std::str::from_utf8(&clip.data).is_err() {
                    return Err(Error::Inconsistent("剪贴板文本不是 UTF-8"));
                }
                Ok(Some(Received::Clip(clip)))
            }
            ClipMsg::TooLarge { seq, kind, size } => {
                if !self.fresh(seq) {
                    return Ok(None);
                }
                self.current = None;
                Ok(Some(Received::TooLarge { kind, size }))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::indexing_slicing)]
    use super::*;

    fn drain(s: &mut Sender) -> Vec<ClipMsg> {
        std::iter::from_fn(|| s.next_msg()).collect()
    }

    /// 经过编码、任意切开的字节流，再交给接收方。
    fn deliver(msgs: &[ClipMsg], r: &mut Receiver, cut: usize) -> Vec<Received> {
        let bytes: Vec<u8> = msgs.iter().flat_map(ClipMsg::frame).collect();
        let mut d = Deframer::default();
        let mut out = Vec::new();
        for part in bytes.chunks(cut) {
            for m in d.push(part).unwrap() {
                out.extend(r.on_msg(m).unwrap());
            }
        }
        out
    }

    #[test]
    fn text_and_large_image_round_trip() {
        let mut s = Sender::new(Limits::DEFAULT);
        let mut r = Receiver::new(Limits::DEFAULT);
        assert_eq!(s.offer(&Clip::text("你好 hello")), Offered::Sending);
        assert_eq!(s.pending(), 3);
        assert!(matches!(s.peek(), Some(ClipMsg::Offer { .. })));
        let got = deliver(&drain(&mut s), &mut r, 7);
        assert_eq!(got, vec![Received::Clip(Clip::text("你好 hello"))]);

        // 多块的图片
        let png = Clip {
            kind: ClipKind::Png,
            data: (0..CHUNK * 3 + 5)
                .map(|i| u8::try_from(i % 251).unwrap())
                .collect(),
        };
        assert_eq!(s.offer(&png), Offered::Sending);
        assert_eq!(s.pending(), 1 + 4 + 1);
        let got = deliver(&drain(&mut s), &mut r, 10_000);
        assert_eq!(got, vec![Received::Clip(png)]);
    }

    #[test]
    fn line_endings_do_not_count_as_a_change() {
        let mut s = Sender::new(Limits::DEFAULT);
        assert_eq!(s.offer(&Clip::text("a\nb")), Offered::Sending);
        // 对方（Windows）把同一段文字存成 \r\n 又同步回来：已经是本机的内容，不写也不再发
        let back = Clip::text("a\r\nb");
        assert!(s.is_local(&back));
        assert_eq!(s.offer(&back), Offered::Skip);
        assert!(!s.is_local(&Clip::text("other")));
        // 图片逐字节比较
        let png = Clip {
            kind: ClipKind::Png,
            data: vec![b'\r', 1],
        };
        let png2 = Clip {
            kind: ClipKind::Png,
            data: vec![1],
        };
        assert_ne!(png.hash(), png2.hash());
    }

    #[test]
    fn no_echo_and_no_repeats() {
        let mut s = Sender::new(Limits::DEFAULT);
        let clip = Clip::text("x");
        // 刚从对方收到并写进本机的，不发回去
        s.applied_remote(&clip);
        assert_eq!(s.offer(&clip), Offered::Skip);
        assert_eq!(s.pending(), 0);
        // 同样的内容不重复发
        assert_eq!(s.offer(&Clip::text("y")), Offered::Sending);
        drain(&mut s);
        assert_eq!(s.offer(&Clip::text("y")), Offered::Skip);
    }

    #[test]
    fn newer_copy_cancels_the_unsent_rest() {
        let mut s = Sender::new(Limits::DEFAULT);
        let mut r = Receiver::new(Limits::DEFAULT);
        let big = Clip {
            kind: ClipKind::Png,
            data: vec![1; CHUNK * 4],
        };
        s.offer(&big);
        // 只发出了开头两条，用户又复制了新文本
        let first: Vec<ClipMsg> = (0..2).filter_map(|_| s.next_msg()).collect();
        s.offer(&Clip::text("new"));
        let mut all = first;
        all.extend(drain(&mut s));
        let got = deliver(&all, &mut r, 1000);
        assert_eq!(got, vec![Received::Clip(Clip::text("new"))]);
        // 迟到的旧消息被忽略
        assert_eq!(
            r.on_msg(ClipMsg::Offer {
                seq: 1,
                kind: ClipKind::Text,
                size: 1
            })
            .unwrap(),
            None
        );
        assert_eq!(
            r.on_msg(ClipMsg::Chunk {
                seq: 1,
                data: vec![0]
            })
            .unwrap(),
            None
        );
        assert_eq!(
            r.on_msg(ClipMsg::Done {
                seq: 1,
                hash: [0; 32]
            })
            .unwrap(),
            None
        );
    }

    #[test]
    fn too_large_is_only_announced() {
        let limits = Limits { text: 4, image: 8 };
        let mut s = Sender::new(limits);
        let mut r = Receiver::new(limits);
        assert_eq!(s.offer(&Clip::text("12345")), Offered::TooLarge);
        let got = deliver(&drain(&mut s), &mut r, 3);
        assert_eq!(
            got,
            vec![Received::TooLarge {
                kind: ClipKind::Text,
                size: 5
            }]
        );
        assert_eq!(limits.of(ClipKind::Png), 8);
        // 对方无视上限硬发：报错
        assert!(r
            .on_msg(ClipMsg::Offer {
                seq: 9,
                kind: ClipKind::Png,
                size: 9
            })
            .is_err());
        // 旧序号的超限通知被忽略
        assert_eq!(
            r.on_msg(ClipMsg::TooLarge {
                seq: 2,
                kind: ClipKind::Png,
                size: 9
            })
            .unwrap(),
            None
        );
    }

    #[test]
    fn protocol_violations_are_errors() {
        let mut r = Receiver::new(Limits::DEFAULT);
        let offer = |seq, size| ClipMsg::Offer {
            seq,
            kind: ClipKind::Text,
            size,
        };
        // 比声明的长
        r.on_msg(offer(1, 2)).unwrap();
        assert!(r
            .on_msg(ClipMsg::Chunk {
                seq: 1,
                data: vec![b'a'; 3]
            })
            .is_err());
        // 哈希不对
        r.on_msg(offer(2, 1)).unwrap();
        r.on_msg(ClipMsg::Chunk {
            seq: 2,
            data: vec![b'a'],
        })
        .unwrap();
        assert!(r
            .on_msg(ClipMsg::Done {
                seq: 2,
                hash: [0; 32]
            })
            .is_err());
        // 长度不足
        r.on_msg(offer(3, 2)).unwrap();
        assert!(r
            .on_msg(ClipMsg::Done {
                seq: 3,
                hash: Clip::text("").hash()
            })
            .is_err());
        // 文本不是 UTF-8
        let bad = Clip {
            kind: ClipKind::Text,
            data: vec![0xff],
        };
        r.on_msg(offer(4, 1)).unwrap();
        r.on_msg(ClipMsg::Chunk {
            seq: 4,
            data: vec![0xff],
        })
        .unwrap();
        assert!(r
            .on_msg(ClipMsg::Done {
                seq: 4,
                hash: bad.hash()
            })
            .is_err());
        // 序号回绕后仍然算新的
        let mut w = Receiver::new(Limits::DEFAULT);
        w.on_msg(offer(u32::MAX, 1)).unwrap();
        w.on_msg(offer(0, 1)).unwrap();
        assert_eq!(w.current.as_ref().map(|c| c.seq), Some(0));
    }

    #[test]
    fn decoding_edge_cases() {
        assert_eq!(ClipMsg::decode(&[9]), Err(Error::UnknownMessage(9)));
        assert_eq!(
            ClipMsg::decode(&[1, 0, 0, 0, 1, 7, 0, 0, 0, 0, 0, 0, 0, 0]),
            Err(Error::UnknownMessage(7))
        );
        // 以后追加的字段被忽略
        let mut b = ClipMsg::TooLarge {
            seq: 1,
            kind: ClipKind::Png,
            size: 2,
        }
        .frame();
        b.push(0xee);
        assert!(ClipMsg::decode(&b[4..]).is_ok());
        // 超长的消息
        let mut d = Deframer::default();
        assert!(d.push(&u32::MAX.to_be_bytes()).is_err());
    }
}
