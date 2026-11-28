//! 文件传输协议（不做 IO）。
//!
//! 消息跑在一条可靠有序的流上，每条消息前面有 4 字节长度。流程：
//!
//! 1. 发送方发 [`FileMsg::Offer`]，列出要传的文件。
//! 2. 接收方回 [`FileMsg::Answer`]：每个文件已经有多少字节，以及这些字节的哈希。
//! 3. 发送方逐个文件发 `Start`、若干 `Data`、`End`（带整个文件的哈希）。
//!    已有部分的哈希与源文件一致时从断点续传，否则从头传。
//! 4. 接收方校验整个文件的哈希，回 [`FileMsg::Result`]。
//!
//! 磁盘读写通过 [`Store`] 抽象：测试用内存实现，真实程序用文件系统实现。
//! 接收方只认相对路径，任何可能逃出目标目录的路径都会被拒绝。

use std::collections::BTreeMap;

use crate::wire::{Reader, Writer};
use crate::Error;

/// 单个数据块的最大长度。
pub const MAX_CHUNK: usize = 64 * 1024;
/// 一次传输最多包含的文件数。
pub const MAX_FILES: usize = 4096;
/// 相对路径的最大长度（字节）。
pub const MAX_PATH_LEN: usize = 1024;
/// 一条消息的最大长度。
pub const MAX_FRAME: usize = 2 * 1024 * 1024;
/// 中止原因的最大长度。
pub const MAX_REASON_LEN: usize = 200;

/// 文件内容的哈希（BLAKE3）。
pub type Hash = [u8; 32];

/// 检查相对路径是否安全。
///
/// 拒绝：空路径、绝对路径、`.` 和 `..`、反斜杠、冒号、控制字符、
/// 以空格或点结尾的名字、Windows 保留设备名。发送方和接收方都要检查。
pub fn check_path(path: &str) -> Result<(), Error> {
    let bad = || Error::UnsafePath(path.to_owned());
    if path.is_empty() || path.len() > MAX_PATH_LEN {
        return Err(bad());
    }
    if path
        .chars()
        .any(|c| c == '\\' || c == ':' || c.is_control())
    {
        return Err(bad());
    }
    for part in path.split('/') {
        if part.is_empty() || part == "." || part == ".." {
            return Err(bad());
        }
        if part.ends_with(' ') || part.ends_with('.') {
            return Err(bad());
        }
        let stem = part.split('.').next().unwrap_or(part).to_ascii_uppercase();
        let reserved = matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
            || (stem.len() == 4
                && (stem.starts_with("COM") || stem.starts_with("LPT"))
                && stem.ends_with(|c: char| c.is_ascii_digit() && c != '0'));
        if reserved {
            return Err(bad());
        }
    }
    Ok(())
}

/// 一个待传文件的描述。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileMeta {
    /// 相对路径，用 `/` 分隔。
    pub path: String,
    /// 大小（字节）。
    pub size: u64,
    /// 修改时间（Unix 秒）。
    pub mtime: u64,
}

/// 接收方对一个文件的答复。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    /// 不需要这个文件。
    Skip,
    /// 接收。`offset` 是已有的字节数，`prefix_hash` 是这些字节的哈希。
    Accept {
        /// 已有的字节数。
        offset: u64,
        /// 已有字节的哈希。
        prefix_hash: Hash,
    },
}

/// 文件传输消息。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileMsg {
    /// 发送方：要传这些文件。
    Offer {
        /// 文件列表。
        files: Vec<FileMeta>,
    },
    /// 接收方：对每个文件的答复，顺序与 Offer 一致。
    Answer {
        /// 答复列表。
        decisions: Vec<Decision>,
    },
    /// 发送方：开始传第 `index` 个文件，从 `offset` 开始。
    Start {
        /// 文件序号。
        index: u32,
        /// 起始偏移。
        offset: u64,
    },
    /// 发送方：紧接上一块的数据。
    Data {
        /// 文件序号。
        index: u32,
        /// 数据。
        data: Vec<u8>,
    },
    /// 发送方：这个文件发完了。
    End {
        /// 文件序号。
        index: u32,
        /// 整个文件的哈希。
        hash: Hash,
    },
    /// 接收方：这个文件校验通过或失败。
    Result {
        /// 文件序号。
        index: u32,
        /// 是否成功。
        ok: bool,
    },
    /// 任一方：中止整个传输。
    Abort {
        /// 原因。
        reason: String,
    },
}

impl FileMsg {
    /// 消息名。
    #[must_use]
    pub const fn name(&self) -> &'static str {
        match self {
            FileMsg::Offer { .. } => "Offer",
            FileMsg::Answer { .. } => "Answer",
            FileMsg::Start { .. } => "Start",
            FileMsg::Data { .. } => "Data",
            FileMsg::End { .. } => "End",
            FileMsg::Result { .. } => "Result",
            FileMsg::Abort { .. } => "Abort",
        }
    }

    /// 编码（不含长度前缀）。
    pub fn encode(&self) -> Result<Vec<u8>, Error> {
        let count = |len: usize| {
            u16::try_from(len)
                .ok()
                .filter(|c| usize::from(*c) <= MAX_FILES)
                .ok_or(Error::TooManyItems {
                    len,
                    max: MAX_FILES,
                })
        };
        let mut w = Writer::new();
        match self {
            FileMsg::Offer { files } => {
                w.u8(1).u16(count(files.len())?);
                for file in files {
                    w.string16(&file.path, MAX_PATH_LEN)?;
                    w.u64(file.size).u64(file.mtime);
                }
            }
            FileMsg::Answer { decisions } => {
                w.u8(2).u16(count(decisions.len())?);
                for decision in decisions {
                    match decision {
                        Decision::Skip => {
                            w.u8(0);
                        }
                        Decision::Accept {
                            offset,
                            prefix_hash,
                        } => {
                            w.u8(1).u64(*offset).bytes(prefix_hash);
                        }
                    }
                }
            }
            FileMsg::Start { index, offset } => {
                w.u8(3).u32(*index).u64(*offset);
            }
            FileMsg::Data { index, data } => {
                if data.len() > MAX_CHUNK {
                    return Err(Error::PayloadTooLarge {
                        len: data.len(),
                        max: MAX_CHUNK,
                    });
                }
                w.u8(4).u32(*index).bytes(data);
            }
            FileMsg::End { index, hash } => {
                w.u8(5).u32(*index).bytes(hash);
            }
            FileMsg::Result { index, ok } => {
                w.u8(6).u32(*index).u8(u8::from(*ok));
            }
            FileMsg::Abort { reason } => {
                w.u8(7);
                w.string(reason, MAX_REASON_LEN)?;
            }
        }
        Ok(w.into_vec())
    }

    /// 解码（不含长度前缀）。
    pub fn decode(buf: &[u8]) -> Result<Self, Error> {
        fn hash(r: &mut Reader<'_>) -> Result<Hash, Error> {
            let mut out = [0u8; 32];
            out.copy_from_slice(r.take(32)?);
            Ok(out)
        }
        fn count(r: &mut Reader<'_>) -> Result<usize, Error> {
            let len = usize::from(r.u16()?);
            if len > MAX_FILES {
                return Err(Error::TooManyItems {
                    len,
                    max: MAX_FILES,
                });
            }
            Ok(len)
        }
        let mut r = Reader::new(buf);
        let msg = match r.u8()? {
            1 => {
                let n = count(&mut r)?;
                let mut files = Vec::with_capacity(n.min(256));
                for _ in 0..n {
                    files.push(FileMeta {
                        path: r.string16(MAX_PATH_LEN)?,
                        size: r.u64()?,
                        mtime: r.u64()?,
                    });
                }
                FileMsg::Offer { files }
            }
            2 => {
                let n = count(&mut r)?;
                let mut decisions = Vec::with_capacity(n.min(256));
                for _ in 0..n {
                    decisions.push(match r.u8()? {
                        0 => Decision::Skip,
                        1 => Decision::Accept {
                            offset: r.u64()?,
                            prefix_hash: hash(&mut r)?,
                        },
                        other => return Err(Error::UnknownMessage(other)),
                    });
                }
                FileMsg::Answer { decisions }
            }
            3 => FileMsg::Start {
                index: r.u32()?,
                offset: r.u64()?,
            },
            4 => {
                let index = r.u32()?;
                let data = r.rest();
                if data.len() > MAX_CHUNK {
                    return Err(Error::PayloadTooLarge {
                        len: data.len(),
                        max: MAX_CHUNK,
                    });
                }
                return Ok(FileMsg::Data {
                    index,
                    data: data.to_vec(),
                });
            }
            5 => FileMsg::End {
                index: r.u32()?,
                hash: hash(&mut r)?,
            },
            6 => FileMsg::Result {
                index: r.u32()?,
                ok: match r.u8()? {
                    0 => false,
                    1 => true,
                    _ => return Err(Error::Inconsistent("结果标志只能是 0 或 1")),
                },
            },
            7 => FileMsg::Abort {
                reason: r.string(MAX_REASON_LEN)?,
            },
            other => return Err(Error::UnknownMessage(other)),
        };
        r.finish()?;
        Ok(msg)
    }

    /// 编码并加上 4 字节长度前缀，可直接写入流。
    pub fn frame(&self) -> Result<Vec<u8>, Error> {
        let body = self.encode()?;
        let len = u32::try_from(body.len())
            .ok()
            .filter(|l| usize::try_from(*l).is_ok_and(|l| l <= MAX_FRAME))
            .ok_or(Error::PayloadTooLarge {
                len: body.len(),
                max: MAX_FRAME,
            })?;
        let mut w = Writer::new();
        w.u32(len).bytes(&body);
        Ok(w.into_vec())
    }
}

/// 从流里读到的字节中切出一条条消息。
#[derive(Debug, Clone, Default)]
pub struct Deframer {
    buf: Vec<u8>,
}

impl Deframer {
    /// 创建。
    #[must_use]
    pub const fn new() -> Self {
        Self { buf: Vec::new() }
    }

    /// 放入新读到的字节，取出所有完整的消息。
    pub fn push(&mut self, bytes: &[u8]) -> Result<Vec<FileMsg>, Error> {
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
            out.push(FileMsg::decode(body)?);
            self.buf.drain(..len.saturating_add(4));
        }
        Ok(out)
    }
}

/// 存储抽象。路径都是已经通过 [`check_path`] 的相对路径。
pub trait Store {
    /// 读源文件的一段（发送方用）。返回的长度可以小于 `len`，表示文件到头了。
    fn read(&mut self, path: &str, offset: u64, len: usize) -> Result<Vec<u8>, Error>;
    /// 未完成的接收文件现在有多长；不存在时返回 0（接收方用）。
    fn partial_len(&mut self, path: &str) -> Result<u64, Error>;
    /// 读未完成的接收文件的一段（接收方用）。
    fn read_partial(&mut self, path: &str, offset: u64, len: usize) -> Result<Vec<u8>, Error>;
    /// 在未完成的接收文件的 `offset` 处写入。
    fn write(&mut self, path: &str, offset: u64, data: &[u8]) -> Result<(), Error>;
    /// 把未完成的接收文件截断到 `len`。
    fn truncate(&mut self, path: &str, len: u64) -> Result<(), Error>;
    /// 接收完成：把未完成的文件变成正式文件，并设置修改时间。
    fn finish(&mut self, path: &str, mtime: u64) -> Result<(), Error>;
}

/// 内存里的存储，用于测试和仿真。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MemStore {
    /// 源文件。
    pub sources: BTreeMap<String, Vec<u8>>,
    /// 未完成的接收文件。
    pub partials: BTreeMap<String, Vec<u8>>,
    /// 已完成的接收文件及其修改时间。
    pub finished: BTreeMap<String, (Vec<u8>, u64)>,
}

fn slice(data: &[u8], offset: u64, len: usize) -> Vec<u8> {
    let start = usize::try_from(offset)
        .unwrap_or(usize::MAX)
        .min(data.len());
    let end = start.saturating_add(len).min(data.len());
    data.get(start..end).map(<[u8]>::to_vec).unwrap_or_default()
}

impl Store for MemStore {
    fn read(&mut self, path: &str, offset: u64, len: usize) -> Result<Vec<u8>, Error> {
        self.sources
            .get(path)
            .map(|data| slice(data, offset, len))
            .ok_or_else(|| Error::Storage(format!("源文件不存在：{path}")))
    }

    fn partial_len(&mut self, path: &str) -> Result<u64, Error> {
        Ok(self
            .partials
            .get(path)
            .map_or(0, |d| u64::try_from(d.len()).unwrap_or(u64::MAX)))
    }

    fn read_partial(&mut self, path: &str, offset: u64, len: usize) -> Result<Vec<u8>, Error> {
        Ok(self
            .partials
            .get(path)
            .map(|data| slice(data, offset, len))
            .unwrap_or_default())
    }

    fn write(&mut self, path: &str, offset: u64, data: &[u8]) -> Result<(), Error> {
        let file = self.partials.entry(path.to_owned()).or_default();
        if u64::try_from(file.len()).unwrap_or(u64::MAX) != offset {
            return Err(Error::Storage(format!("写入位置不连续：{path}")));
        }
        file.extend_from_slice(data);
        Ok(())
    }

    fn truncate(&mut self, path: &str, len: u64) -> Result<(), Error> {
        let file = self.partials.entry(path.to_owned()).or_default();
        file.truncate(usize::try_from(len).unwrap_or(usize::MAX));
        Ok(())
    }

    fn finish(&mut self, path: &str, mtime: u64) -> Result<(), Error> {
        let data = self.partials.remove(path).unwrap_or_default();
        self.finished.insert(path.to_owned(), (data, mtime));
        Ok(())
    }
}

/// 一个文件的传输结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// 已送达并通过校验。
    Done,
    /// 接收方不需要。
    Skipped,
    /// 校验失败。
    Failed,
}

const fn unexpected(state: &'static str, msg: &FileMsg) -> Error {
    Error::UnexpectedMessage {
        state,
        msg: msg.name(),
    }
}

fn hash_prefix(
    mut read: impl FnMut(u64, usize) -> Result<Vec<u8>, Error>,
    len: u64,
) -> Result<Option<blake3::Hasher>, Error> {
    let mut hasher = blake3::Hasher::new();
    let mut offset = 0u64;
    while offset < len {
        let want = usize::try_from(len.saturating_sub(offset))
            .unwrap_or(usize::MAX)
            .min(MAX_CHUNK);
        let chunk = read(offset, want)?;
        if chunk.is_empty() {
            // 文件比声称的短。
            return Ok(None);
        }
        hasher.update(&chunk);
        offset = offset.saturating_add(u64::try_from(chunk.len()).unwrap_or(u64::MAX));
    }
    Ok(Some(hasher))
}

#[derive(Debug)]
struct Sending {
    index: usize,
    offset: u64,
    started: bool,
    hasher: blake3::Hasher,
}

#[derive(Debug)]
enum SendState {
    Idle,
    AwaitAnswer,
    Sending,
    Aborted,
}

/// 发送方状态机。
#[derive(Debug)]
pub struct FileSender {
    files: Vec<FileMeta>,
    state: SendState,
    /// 每个文件的起始偏移和已算好的前缀哈希状态；`None` 表示跳过。
    plans: Vec<Option<(u64, blake3::Hasher)>>,
    current: Option<Sending>,
    next_index: usize,
    outcomes: Vec<Option<Outcome>>,
    bytes_sent: u64,
    /// 对方中止传输时给的原因。
    peer_abort: Option<String>,
}

impl FileSender {
    /// 创建。检查文件数量和每个路径。
    pub fn new(files: Vec<FileMeta>) -> Result<Self, Error> {
        if files.len() > MAX_FILES {
            return Err(Error::TooManyItems {
                len: files.len(),
                max: MAX_FILES,
            });
        }
        for file in &files {
            check_path(&file.path)?;
        }
        let n = files.len();
        Ok(Self {
            files,
            state: SendState::Idle,
            plans: Vec::new(),
            current: None,
            next_index: 0,
            outcomes: vec![None; n],
            bytes_sent: 0,
            peer_abort: None,
        })
    }

    /// 生成 Offer。只能调用一次。
    pub fn offer(&mut self) -> Result<FileMsg, Error> {
        match self.state {
            SendState::Idle => {
                self.state = SendState::AwaitAnswer;
                Ok(FileMsg::Offer {
                    files: self.files.clone(),
                })
            }
            SendState::AwaitAnswer | SendState::Sending | SendState::Aborted => {
                Err(Error::Inconsistent("Offer 只能发送一次"))
            }
        }
    }

    /// 处理接收方发来的消息。
    pub fn on_msg(&mut self, msg: &FileMsg, store: &mut dyn Store) -> Result<(), Error> {
        match (msg, &self.state) {
            (FileMsg::Answer { decisions }, SendState::AwaitAnswer) => {
                if decisions.len() != self.files.len() {
                    return Err(Error::Inconsistent("答复数量与文件数量不一致"));
                }
                let mut plans = Vec::with_capacity(decisions.len());
                for ((decision, file), outcome) in decisions
                    .iter()
                    .zip(&self.files)
                    .zip(self.outcomes.iter_mut())
                {
                    plans.push(match decision {
                        Decision::Skip => {
                            *outcome = Some(Outcome::Skipped);
                            None
                        }
                        Decision::Accept {
                            offset,
                            prefix_hash,
                        } => {
                            // 只有对方已有的部分与源文件完全一致才续传，否则从头开始。
                            let resumed = if *offset > 0 && *offset <= file.size {
                                hash_prefix(|o, l| store.read(&file.path, o, l), *offset)?
                                    .filter(|h| h.finalize().as_bytes() == prefix_hash)
                            } else {
                                None
                            };
                            Some(resumed.map_or_else(
                                || (0, blake3::Hasher::new()),
                                |hasher| (*offset, hasher),
                            ))
                        }
                    });
                }
                self.plans = plans;
                self.state = SendState::Sending;
                Ok(())
            }
            (FileMsg::Result { index, ok }, SendState::Sending) => {
                let slot = usize::try_from(*index)
                    .ok()
                    .and_then(|i| self.outcomes.get_mut(i))
                    .ok_or(Error::Inconsistent("结果里的文件序号越界"))?;
                *slot = Some(if *ok { Outcome::Done } else { Outcome::Failed });
                Ok(())
            }
            (FileMsg::Abort { reason }, _) => {
                self.state = SendState::Aborted;
                self.peer_abort = Some(reason.clone());
                Ok(())
            }
            (
                FileMsg::Offer { .. }
                | FileMsg::Start { .. }
                | FileMsg::Data { .. }
                | FileMsg::End { .. }
                | FileMsg::Answer { .. }
                | FileMsg::Result { .. },
                SendState::Idle | SendState::AwaitAnswer | SendState::Sending | SendState::Aborted,
            ) => Err(unexpected("发送方", msg)),
        }
    }

    /// 取下一条要发送的消息；暂时没有可发的返回 `None`。
    ///
    /// 由传输层在流可写时调用，所以读盘速度自然跟随网络速度。
    pub fn poll(&mut self, store: &mut dyn Store) -> Result<Option<FileMsg>, Error> {
        match self.state {
            SendState::Sending => {}
            SendState::Idle | SendState::AwaitAnswer | SendState::Aborted => return Ok(None),
        }
        if self.current.is_none() {
            // 找下一个要传的文件。
            while let Some(plan) = self.plans.get_mut(self.next_index) {
                let index = self.next_index;
                self.next_index = self.next_index.saturating_add(1);
                if let Some((offset, hasher)) = plan.take() {
                    self.current = Some(Sending {
                        index,
                        offset,
                        started: false,
                        hasher,
                    });
                    break;
                }
            }
        }
        let files = &self.files;
        let Some((sending, file)) = self
            .current
            .as_mut()
            .and_then(|s| files.get(s.index).map(|f| (s, f)))
        else {
            return Ok(None);
        };
        let index = u32::try_from(sending.index).unwrap_or(u32::MAX);
        if !sending.started {
            sending.started = true;
            return Ok(Some(FileMsg::Start {
                index,
                offset: sending.offset,
            }));
        }
        if sending.offset >= file.size {
            let hash = *sending.hasher.finalize().as_bytes();
            self.current = None;
            return Ok(Some(FileMsg::End { index, hash }));
        }
        let want = usize::try_from(file.size.saturating_sub(sending.offset))
            .unwrap_or(usize::MAX)
            .min(MAX_CHUNK);
        let data = store.read(&file.path, sending.offset, want)?;
        if data.len() != want {
            self.state = SendState::Aborted;
            return Err(Error::Inconsistent("源文件在传输过程中被修改"));
        }
        sending.hasher.update(&data);
        let len = u64::try_from(data.len()).unwrap_or(u64::MAX);
        sending.offset = sending.offset.saturating_add(len);
        self.bytes_sent = self.bytes_sent.saturating_add(len);
        Ok(Some(FileMsg::Data { index, data }))
    }

    /// 每个文件的结果；还没有结果的是 `None`。
    #[must_use]
    pub fn outcomes(&self) -> &[Option<Outcome>] {
        &self.outcomes
    }

    /// 是否所有文件都有了结果。
    #[must_use]
    pub fn is_finished(&self) -> bool {
        self.outcomes.iter().all(Option::is_some)
    }

    /// 是否已中止。
    #[must_use]
    pub const fn is_aborted(&self) -> bool {
        matches!(self.state, SendState::Aborted)
    }

    /// 对方中止传输时给的原因（例如对方磁盘满了、不允许传文件）；对方没有中止时为 `None`。
    #[must_use]
    pub fn peer_aborted(&self) -> Option<&str> {
        self.peer_abort.as_deref()
    }

    /// 本次实际发送的字节数（续传时不含对方已有的部分）。
    #[must_use]
    pub const fn bytes_sent(&self) -> u64 {
        self.bytes_sent
    }
}

#[derive(Debug)]
struct Receiving {
    index: usize,
    offset: u64,
    hasher: blake3::Hasher,
}

/// 接收方状态机。
#[derive(Debug, Default)]
pub struct FileReceiver {
    files: Vec<FileMeta>,
    /// Answer 里给出的偏移和对应的哈希状态；`None` 表示还没收到 Offer、已用过或已跳过。
    offers: Vec<Option<(u64, blake3::Hasher)>>,
    current: Option<Receiving>,
    outcomes: Vec<Option<Outcome>>,
    aborted: Option<String>,
    bytes_received: u64,
}

impl FileReceiver {
    /// 创建。
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// 处理发送方发来的消息，返回要回复的消息。
    pub fn on_msg(&mut self, msg: FileMsg, store: &mut dyn Store) -> Result<Vec<FileMsg>, Error> {
        if self.aborted.is_some() {
            return Err(unexpected("已中止", &msg));
        }
        match msg {
            FileMsg::Offer { files } => {
                if !self.files.is_empty() || files.is_empty() {
                    return Err(Error::Inconsistent("Offer 重复或为空"));
                }
                let mut decisions = Vec::with_capacity(files.len());
                let mut offers = Vec::with_capacity(files.len());
                for file in &files {
                    check_path(&file.path)?;
                    let have = store.partial_len(&file.path)?;
                    let resumed = if have > 0 && have <= file.size {
                        hash_prefix(|o, l| store.read_partial(&file.path, o, l), have)?
                    } else {
                        None
                    };
                    let (offset, hasher) =
                        resumed.map_or_else(|| (0, blake3::Hasher::new()), |h| (have, h));
                    decisions.push(Decision::Accept {
                        offset,
                        prefix_hash: *hasher.finalize().as_bytes(),
                    });
                    offers.push(Some((offset, hasher)));
                }
                self.outcomes = vec![None; files.len()];
                self.files = files;
                self.offers = offers;
                Ok(vec![FileMsg::Answer { decisions }])
            }
            FileMsg::Start { index, offset } => {
                if self.current.is_some() {
                    return Err(Error::Inconsistent("上一个文件还没结束"));
                }
                let i = usize::try_from(index).unwrap_or(usize::MAX);
                let (file, slot) = self
                    .files
                    .get(i)
                    .zip(self.offers.get_mut(i))
                    .ok_or(Error::Inconsistent("文件序号越界"))?;
                let (offered, hasher) = slot
                    .take()
                    .ok_or(Error::Inconsistent("这个文件已经开始过"))?;
                let hasher = if offset == offered {
                    hasher
                } else if offset == 0 {
                    // 发送方发现已有部分与源文件不一致：丢掉重来。
                    blake3::Hasher::new()
                } else {
                    return Err(Error::Inconsistent("起始偏移与答复不一致"));
                };
                store.truncate(&file.path, offset)?;
                self.current = Some(Receiving {
                    index: i,
                    offset,
                    hasher,
                });
                Ok(Vec::new())
            }
            FileMsg::Data { index, data } => {
                let (current, file) = self.current_for(index)?;
                let len = u64::try_from(data.len()).unwrap_or(u64::MAX);
                if current.offset.saturating_add(len) > file.size {
                    return Err(Error::Inconsistent("收到的数据超过了声明的文件大小"));
                }
                store.write(&file.path, current.offset, &data)?;
                current.hasher.update(&data);
                current.offset = current.offset.saturating_add(len);
                self.bytes_received = self.bytes_received.saturating_add(len);
                Ok(Vec::new())
            }
            FileMsg::End { index, hash } => {
                let (current, file) = self.current_for(index)?;
                let ok =
                    current.offset == file.size && current.hasher.finalize().as_bytes() == &hash;
                let i = current.index;
                if ok {
                    store.finish(&file.path, file.mtime)?;
                } else {
                    // 内容不对：不留下看起来可以续传的坏文件。
                    store.truncate(&file.path, 0)?;
                }
                self.current = None;
                if let Some(slot) = self.outcomes.get_mut(i) {
                    *slot = Some(if ok { Outcome::Done } else { Outcome::Failed });
                }
                Ok(vec![FileMsg::Result { index, ok }])
            }
            FileMsg::Abort { reason } => {
                self.aborted = Some(reason);
                Ok(Vec::new())
            }
            FileMsg::Answer { .. } | FileMsg::Result { .. } => Err(unexpected("接收方", &msg)),
        }
    }

    fn current_for(&mut self, index: u32) -> Result<(&mut Receiving, &FileMeta), Error> {
        let current = self
            .current
            .as_mut()
            .filter(|c| u32::try_from(c.index).is_ok_and(|i| i == index))
            .ok_or(Error::Inconsistent("数据不属于正在接收的文件"))?;
        let file = self
            .files
            .get(current.index)
            .ok_or(Error::Inconsistent("文件序号越界"))?;
        Ok((current, file))
    }

    /// 每个文件的结果。
    #[must_use]
    pub fn outcomes(&self) -> &[Option<Outcome>] {
        &self.outcomes
    }

    /// 是否已收到 Offer 且所有文件都有了结果。
    #[must_use]
    pub fn is_finished(&self) -> bool {
        !self.files.is_empty() && self.outcomes.iter().all(Option::is_some)
    }

    /// 对方中止时给出的原因。
    #[must_use]
    pub fn aborted(&self) -> Option<&str> {
        self.aborted.as_deref()
    }

    /// 本次实际收到的字节数。
    #[must_use]
    pub const fn bytes_received(&self) -> u64 {
        self.bytes_received
    }
}
