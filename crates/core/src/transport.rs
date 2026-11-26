//! 视频帧的分片、纠错与重组。
//!
//! 发送端把一帧切成不超过数据报上限的分片，并可以附加校验分片；接收端按帧号重组。
//! 规则是“新帧优先”：一帧完整后，比它旧的未完成帧立刻作废；超过截止时间的帧也作废。
//!
//! 纠错用分组异或校验：数据分片按序号轮流分到若干组，每组一个校验分片，
//! 组内丢一个分片可以恢复。它便宜、没有额外依赖，对随机丢包有效；
//! 对成串的丢包效果有限，那种情况靠重传（见 [`crate::adaptive`]）。

use std::collections::{BTreeMap, VecDeque};

use crate::proto::VideoHeader;
use crate::wire::{Reader, Writer};
use crate::{Error, Time};

/// 一个视频分片。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VideoPacket {
    /// 分片头。校验分片里，`frag_index` 是组号，`frag_count` 是组数。
    pub header: VideoHeader,
    /// 是否是校验分片。
    pub parity: bool,
    /// 分片数据。
    pub data: Vec<u8>,
}

/// 把一帧切成分片，每片数据不超过 `max_payload` 字节。空帧产生一个空分片。
pub fn fragment(
    frame_id: u32,
    ref_frame: u32,
    data: &[u8],
    max_payload: usize,
) -> Result<Vec<VideoPacket>, Error> {
    if max_payload == 0 {
        return Err(Error::InvalidArgument("max_payload 不能为 0"));
    }
    let chunks: Vec<&[u8]> = if data.is_empty() {
        vec![data]
    } else {
        data.chunks(max_payload).collect()
    };
    let frag_count = u16::try_from(chunks.len()).map_err(|_| Error::PayloadTooLarge {
        len: data.len(),
        max: max_payload.saturating_mul(usize::from(u16::MAX)),
    })?;
    Ok(chunks
        .into_iter()
        .zip(0u16..)
        .map(|(chunk, frag_index)| VideoPacket {
            header: VideoHeader {
                frame_id,
                frag_index,
                frag_count,
                ref_frame,
            },
            parity: false,
            data: chunk.to_vec(),
        })
        .collect())
}

/// 校验分片数据区开头的固定长度：数据分片总数(2) + 长度异或(2)。
pub const PARITY_PREFIX: usize = 4;

fn xor_into(acc: &mut Vec<u8>, data: &[u8]) {
    if acc.len() < data.len() {
        acc.resize(data.len(), 0);
    }
    for (a, b) in acc.iter_mut().zip(data) {
        *a ^= *b;
    }
}

fn len16(data: &[u8]) -> u16 {
    u16::try_from(data.len()).unwrap_or(u16::MAX)
}

/// 为一帧的数据分片生成校验分片。`permille` 是冗余比例（千分比）：
/// 校验分片数 = 数据分片数 × 比例，向上取整，至多等于数据分片数。比例为 0 时不生成。
#[must_use]
pub fn parity_for(packets: &[VideoPacket], permille: u16) -> Vec<VideoPacket> {
    let data: Vec<&VideoPacket> = packets.iter().filter(|p| !p.parity).collect();
    let Some(first) = data.first() else {
        return Vec::new();
    };
    let k = data.len();
    let groups = k
        .saturating_mul(usize::from(permille))
        .div_ceil(1000)
        .min(k);
    let (Ok(k16), Ok(groups16)) = (u16::try_from(k), u16::try_from(groups)) else {
        return Vec::new();
    };
    (0..groups16)
        .map(|group| {
            let mut xor = Vec::new();
            let mut len_xor = 0u16;
            for member in data.iter().skip(usize::from(group)).step_by(groups) {
                xor_into(&mut xor, &member.data);
                len_xor ^= len16(&member.data);
            }
            let mut w = Writer::new();
            w.u16(k16).u16(len_xor).bytes(&xor);
            VideoPacket {
                header: VideoHeader {
                    frame_id: first.header.frame_id,
                    frag_index: group,
                    frag_count: groups16,
                    ref_frame: first.header.ref_frame,
                },
                parity: true,
                data: w.into_vec(),
            }
        })
        .collect()
}

/// 用 RS 码为一帧的数据分片生成校验分片：`permille` 是冗余比例，校验分片数向上取整，
/// 至多等于数据分片数且 k + m ≤ 256。数据区开头：数据分片总数(2) + 分片长度(2) + 最后一片长度(2)。
#[must_use]
pub fn rs_parity_for(packets: &[VideoPacket], permille: u16) -> Vec<VideoPacket> {
    let data: Vec<&VideoPacket> = packets.iter().filter(|p| !p.parity).collect();
    let Some(first) = data.first() else {
        return Vec::new();
    };
    let k = data.len();
    let m = k
        .saturating_mul(usize::from(permille))
        .div_ceil(1000)
        .min(k)
        .min(256usize.saturating_sub(k));
    let refs: Vec<&[u8]> = data.iter().map(|p| p.data.as_slice()).collect();
    let (Ok(k16), Ok(m16), Ok(parity)) = (
        u16::try_from(k),
        u16::try_from(m),
        crate::rs::encode(&refs, m),
    ) else {
        return Vec::new();
    };
    let len = len16(&first.data);
    let last_len = data.last().map_or(0, |p| len16(&p.data));
    parity
        .into_iter()
        .zip(0u16..)
        .map(|(bytes, j)| {
            let mut w = Writer::new();
            w.u16(k16).u16(len).u16(last_len).bytes(&bytes);
            VideoPacket {
                header: VideoHeader {
                    frame_id: first.header.frame_id,
                    frag_index: j,
                    frag_count: m16,
                    ref_frame: first.header.ref_frame,
                },
                parity: true,
                data: w.into_vec(),
            }
        })
        .collect()
}

/// RS 校验分片数据区开头的固定长度：数据分片总数(2) + 分片长度(2) + 最后一片长度(2)。
pub const RS_PARITY_PREFIX: usize = 6;

/// 重组完成的一帧。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    /// 帧号。
    pub frame_id: u32,
    /// 参考帧号。
    pub ref_frame: u32,
    /// 帧数据。
    pub data: Vec<u8>,
    /// 收到第一个分片的时间。
    pub first_seen: Time,
    /// 收齐的时间。
    pub completed: Time,
}

/// 重组器的计数。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ReassemblyStats {
    /// 交付的帧数。
    pub delivered: u64,
    /// 作废的帧数（超时、被新帧取代、缓冲区满）。
    pub expired: u64,
    /// 重复的分片数。
    pub duplicates: u64,
    /// 属于已交付或已作废帧的迟到分片数。
    pub stale: u64,
    /// 靠校验分片恢复出来的数据分片数。
    pub recovered: u64,
    /// 收齐了、但参考的帧还在补，先放着等它的帧数（H.264，见 [`Reassembler::set_keyframe_grace`]）。
    pub held: u64,
}

/// 纠错方式（会话里双方协商：都支持 RS 时用 RS，否则用异或）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Fec {
    /// 分组异或：每组补一个丢包（老版本只认这个）。
    #[default]
    Xor,
    /// Reed-Solomon：收到任意 k 个分片就能还原（见 [`crate::rs`]）。
    Rs,
}

#[derive(Debug, Clone)]
enum Parity {
    Xor {
        len_xor: u16,
        xor: Vec<u8>,
    },
    Rs {
        len: u16,
        last_len: u16,
        data: Vec<u8>,
    },
}

#[derive(Debug, Clone)]
struct Partial {
    first_seen: Time,
    /// 最近一次收到这一帧分片的时间。
    last_seen: Time,
    ref_frame: u32,
    parts: Vec<Option<Vec<u8>>>,
    have: usize,
    /// 收到的最大数据分片序号（还没收到数据分片时为 `None`）：比它小的缺片是“空洞”，比它大的是“尾部”。
    top: Option<u16>,
    /// 校验分片，按组号存放；长度就是组数。还没收到任何校验分片时为空。
    parity: Vec<Option<Parity>>,
}

impl Partial {
    /// 用已有的校验分片尽量恢复缺失的数据分片，返回恢复的个数。
    fn recover(&mut self) -> u64 {
        if self
            .parity
            .iter()
            .flatten()
            .any(|p| matches!(p, Parity::Rs { .. }))
        {
            return self.recover_rs();
        }
        self.recover_xor()
    }

    /// RS：缺的分片数不超过收到的校验分片数时一次全部还原。
    fn recover_rs(&mut self) -> u64 {
        let m = self.parity.len();
        let k = self.parts.len();
        let mut shape = None;
        let parity: Vec<Option<&[u8]>> = self
            .parity
            .iter()
            .map(|p| match p {
                Some(Parity::Rs {
                    len,
                    last_len,
                    data,
                }) => {
                    shape = Some((*len, *last_len));
                    Some(data.as_slice())
                }
                Some(Parity::Xor { .. }) | None => None,
            })
            .collect();
        let Some((len, last_len)) = shape else {
            return 0;
        };
        let data: Vec<Option<&[u8]>> = self.parts.iter().map(Option::as_deref).collect();
        let Some(rebuilt) = crate::rs::reconstruct(&data, &parity, m) else {
            return 0;
        };
        let mut recovered = 0u64;
        for (index, mut bytes) in rebuilt {
            let want = usize::from(if index + 1 == k { last_len } else { len });
            // 长度对不上说明校验分片与数据不匹配（损坏或伪造）：放弃这一片
            if want > bytes.len() {
                continue;
            }
            bytes.truncate(want);
            if let Some(slot) = self.parts.get_mut(index) {
                *slot = Some(bytes);
                self.have = self.have.saturating_add(1);
                recovered = recovered.saturating_add(1);
            }
        }
        recovered
    }

    fn recover_xor(&mut self) -> u64 {
        let groups = self.parity.len();
        let mut recovered = 0u64;
        for (group, parity) in self.parity.iter().enumerate() {
            let Some(Parity::Xor { len_xor, xor }) = parity else {
                continue;
            };
            let mut missing = None;
            let mut missing_count = 0usize;
            let mut xor = xor.clone();
            let mut len = *len_xor;
            for (index, part) in self.parts.iter().enumerate().skip(group).step_by(groups) {
                match part {
                    Some(data) => {
                        xor_into(&mut xor, data);
                        len ^= len16(data);
                    }
                    None => {
                        missing = Some(index);
                        missing_count = missing_count.saturating_add(1);
                    }
                }
            }
            if let (Some(index), 1) = (missing, missing_count) {
                // 长度对不上说明校验分片与数据不匹配（损坏或伪造）：放弃恢复。
                if usize::from(len) <= xor.len() {
                    xor.truncate(usize::from(len));
                    if let Some(slot) = self.parts.get_mut(index) {
                        *slot = Some(xor);
                        self.have = self.have.saturating_add(1);
                        recovered = recovered.saturating_add(1);
                    }
                }
            }
        }
        recovered
    }
}

/// 重传请求里的分片序号取这个值：请求整帧重传（接收端一个分片都没收到，只从后面的帧知道它存在）。
/// 老版本的发送端没有这个分片，直接忽略。
pub const WHOLE_FRAME: u16 = u16::MAX;

/// 一帧从第一个分片算起最多等这么多个截止时间（关键帧除外，见 [`Reassembler::expire`]）。
const MAX_FRAME_LIFETIME_FACTOR: u64 = 8;

/// 按截止时间重组视频帧。
#[derive(Debug, Clone)]
pub struct Reassembler {
    deadline_us: u64,
    max_pending: usize,
    pending: BTreeMap<u32, Partial>,
    /// 小于等于这个帧号的帧不再接受。
    floor: Option<u32>,
    stats: ReassemblyStats,
    /// 纠错方式（会话协商的结果）。
    fec: Fec,
    /// H.264 模式：参考自己的帧当作关键帧，给更长的时间（见 [`Reassembler::expire`]）；其余帧要等参考帧
    /// 交出之后才能解，所以按参考顺序交出（见 `held`）。只对 H.264 打开：块编码的每一帧都参考自己、都很小，
    /// 丢了靠重发块恢复，给长时间反而拖慢恢复。
    keyframe_grace: bool,
    /// 收齐了、但参考的帧还在补（在 `pending` 或也在这里等）的帧，按帧号。参考帧交出时按顺序跟着交出，
    /// 参考帧作废时跟着作废。WebRTC 的帧缓冲也是这样：帧收齐了但不连续就等，不跳过去
    /// （`FrameBuffer`）。以前是“新帧收齐就作废比它旧的未完成帧”，可 H.264 的新帧参考旧帧，旧帧一作废新帧也解不了，
    /// 只能重发关键帧：2026-11 微软轨迹 ms-09235（8 Mbps、平均丢包 12%）40 秒 46 个关键帧，56 帧是这样没的
    /// （旧帧缺片的重传要一个往返，约 50 ms，下一帧 33 ms 后就收齐了）。
    held: BTreeMap<u32, Frame>,
    /// 跟着参考帧一起可以交出的帧（按顺序），用 [`Reassembler::take_ready`] 取。
    ready: VecDeque<Frame>,
    /// 一个分片都没收到、只从后面的帧（它们的参考帧号）知道存在的帧：请求整帧重传（[`WHOLE_FRAME`]），
    /// 参考它的帧先放着等。值是发现的时刻，没进展超过截止时间就作废。WebRTC 按包序号请求重传，整帧丢光也能补；
    /// 我们按帧号和分片序号请求，整帧丢了以前只能等下一帧发现接不上、再出关键帧（2026-11 微软轨迹 8 Mbps、
    /// 12–40% 丢包时每次一两个小帧整个丢掉，各出一个关键帧）。
    wanted: BTreeMap<u32, Time>,
    /// 收到过分片的最新帧号：更新的帧已经在到，说明发送端发完了旧帧，旧帧的尾部没到就是丢了。
    newest: u32,
}

/// 空洞后面又到了这么多个序号的分片，空洞就算丢了，不用再等（RFC 9002 第 6.1.1 节的包数门限 kPacketThreshold）。
const REORDER_PACKETS: u16 = 3;
/// 尾部缺片：这一帧这么多个平均分片间隔没再来新分片，才算停了（窄带上分片本来就隔得开）。
const TAIL_GAPS: u64 = 3;
/// 尾部等待的上限。
const TAIL_MAX_US: u64 = 150_000;

impl Reassembler {
    /// 创建。`deadline_us` 是一帧从首个分片到收齐允许的最长时间；
    /// `max_pending` 是同时重组的帧数上限（至少 1）。
    #[must_use]
    pub fn new(deadline_us: u64, max_pending: usize) -> Self {
        Self {
            deadline_us,
            max_pending: max_pending.max(1),
            pending: BTreeMap::new(),
            floor: None,
            stats: ReassemblyStats::default(),
            keyframe_grace: false,
            fec: Fec::Xor,
            held: BTreeMap::new(),
            ready: VecDeque::new(),
            wanted: BTreeMap::new(),
            newest: 0,
        }
    }

    /// 设置纠错方式（握手协商之后）。
    pub fn set_fec(&mut self, fec: Fec) {
        self.fec = fec;
    }

    /// 打开或关闭 H.264 模式：关键帧宽限，按参考顺序交出（见 `held`）。
    pub fn set_keyframe_grace(&mut self, on: bool) {
        self.keyframe_grace = on;
    }

    /// 计数。
    #[must_use]
    pub const fn stats(&self) -> ReassemblyStats {
        self.stats
    }

    /// 正在重组的帧数。
    #[must_use]
    pub fn pending(&self) -> usize {
        self.pending.len()
    }

    /// 最近一个不再接受的帧号（已交付或已作废）。
    #[must_use]
    pub const fn floor(&self) -> Option<u32> {
        self.floor
    }

    fn raise_floor(&mut self, frame_id: u32) {
        self.floor = Some(self.floor.map_or(frame_id, |f| f.max(frame_id)));
    }

    /// 判定丢了的数据分片，最多返回 `limit` 个 `(帧号, 分片序号)`，接收端据此请求重传。
    ///
    /// 按空洞判断（RFC 9002 第 6.1 节、WebRTC 按包序号的空洞请求重传），不按“这一帧第一片到了多久”：
    /// - 空洞（比已收到的最大序号小）：后面又到了 [`REORDER_PACKETS`] 个序号，或者这一帧 `min_age_us` 没来新分片，算丢；
    /// - 尾部（比最大序号大）：这一帧停了（[`TAIL_GAPS`] 个平均分片间隔、至少 `2 × min_age_us` 没来新分片），
    ///   或者更新的帧已经在到、这一帧也 `min_age_us` 没来新分片，算丢。
    ///
    /// 以前一帧的第一片到了 10 ms 就把所有没到的分片都要一遍：大帧（30 Mbps 下 70 KB 以上）后半段还在路上就被要重传。
    /// 2026-11-26 用户日志：两分钟里请求重传 5293 片、实际丢包 97 个，大帧把排队撑到 100–320 ms。
    #[must_use]
    pub fn missing(&self, now: Time, min_age_us: u64, limit: usize) -> Vec<(u32, u16)> {
        let whole = self
            .wanted
            .iter()
            .filter(|(_, at)| now.since(**at) >= min_age_us)
            .map(|(frame_id, _)| (*frame_id, WHOLE_FRAME));
        let parts = self.pending.iter().flat_map(|(frame_id, p)| {
            let quiet = now.since(p.last_seen);
            let received = u64::try_from(p.have).unwrap_or(u64::MAX);
            let gap = p.last_seen.since(p.first_seen) / received.saturating_sub(1).max(1);
            let tail_after = gap
                .saturating_mul(TAIL_GAPS)
                .max(min_age_us.saturating_mul(2))
                .min(TAIL_MAX_US);
            let tail_lost = quiet >= tail_after || (self.newest > *frame_id && quiet >= min_age_us);
            let top = p.top;
            p.parts
                .iter()
                .zip(0u16..)
                .filter(move |(part, index)| {
                    part.is_none()
                        && match top {
                            Some(top) if *index < top => {
                                top - *index >= REORDER_PACKETS || quiet >= min_age_us
                            }
                            Some(_) | None => tail_lost,
                        }
                })
                .map(|(_, index)| (*frame_id, index))
        });
        whole.chain(parts).take(limit).collect()
    }

    /// 放入一个分片。收齐一帧时返回它。
    ///
    /// 分片头自相矛盾（同一帧的分片总数或参考帧前后不一致）时返回错误。
    pub fn push(&mut self, now: Time, packet: VideoPacket) -> Result<Option<Frame>, Error> {
        let header = packet.header;
        header.validate()?;
        self.newest = self.newest.max(header.frame_id);
        // 校验分片的数据区开头带着数据分片总数。
        let (data_count, parity) = if packet.parity {
            let mut r = Reader::new(&packet.data);
            let count = r.u16()?;
            match self.fec {
                Fec::Xor => {
                    let len_xor = r.u16()?;
                    if count == 0 || header.frag_count > count {
                        return Err(Error::Inconsistent("校验分片的分组数不合法"));
                    }
                    let xor = r.rest().to_vec();
                    (count, Some(Parity::Xor { len_xor, xor }))
                }
                Fec::Rs => {
                    let len = r.u16()?;
                    let last_len = r.u16()?;
                    let total = usize::from(count).saturating_add(usize::from(header.frag_count));
                    if count == 0 || total > 256 || last_len > len {
                        return Err(Error::Inconsistent("RS 校验分片的参数不合法"));
                    }
                    let data = r.rest().to_vec();
                    (
                        count,
                        Some(Parity::Rs {
                            len,
                            last_len,
                            data,
                        }),
                    )
                }
            }
        } else {
            (header.frag_count, None)
        };

        if self.floor.is_some_and(|f| header.frame_id <= f) {
            self.stats.stale = self.stats.stale.saturating_add(1);
            return Ok(None);
        }
        // 已经收齐、在等参考帧的帧：迟到的重传或校验分片
        if self.held.contains_key(&header.frame_id) {
            self.stats.duplicates = self.stats.duplicates.saturating_add(1);
            return Ok(None);
        }
        if !self.pending.contains_key(&header.frame_id) {
            // 缓冲区满：作废最旧的帧，给新帧让位（等参考帧的也算在内）。
            let excess = self
                .pending
                .len()
                .saturating_add(self.held.len())
                .saturating_add(1)
                .saturating_sub(self.max_pending);
            for _ in 0..excess {
                let oldest = match (self.pending.keys().next(), self.held.keys().next()) {
                    (Some(a), Some(b)) => Some(*a.min(b)),
                    (a, b) => a.or(b).copied(),
                };
                if let Some(oldest) = oldest {
                    self.pending.remove(&oldest);
                    self.held.remove(&oldest);
                    self.stats.expired = self.stats.expired.saturating_add(1);
                    self.raise_floor(oldest);
                }
            }
            self.drop_stale();
            self.drop_orphans();
            if self.floor.is_some_and(|f| header.frame_id <= f) {
                self.stats.stale = self.stats.stale.saturating_add(1);
                return Ok(None);
            }
        }

        // 第一次见到这一帧：它要是参考一个从没见过、又还没交出的帧，那一帧整个丢了，请求整帧重传
        if !self.pending.contains_key(&header.frame_id) {
            self.wanted.remove(&header.frame_id);
            let r = header.ref_frame;
            // 帧号从 1 开始，0 表示“没有帧”
            let unknown = self.keyframe_grace
                && r != 0
                && r != header.frame_id
                && r < header.frame_id
                && self.floor.is_none_or(|f| r > f)
                && !self.pending.contains_key(&r)
                && !self.held.contains_key(&r);
            if unknown {
                self.wanted.entry(r).or_insert(now);
            }
        }
        let partial = self
            .pending
            .entry(header.frame_id)
            .or_insert_with(|| Partial {
                first_seen: now,
                last_seen: now,
                ref_frame: header.ref_frame,
                parts: vec![None; usize::from(data_count)],
                have: 0,
                top: None,
                parity: Vec::new(),
            });
        partial.last_seen = now;
        if partial.parts.len() != usize::from(data_count) {
            return Err(Error::Inconsistent("同一帧的分片总数不一致"));
        }
        if partial.ref_frame != header.ref_frame {
            return Err(Error::Inconsistent("同一帧的参考帧不一致"));
        }

        if let Some(parity) = parity {
            let groups = usize::from(header.frag_count);
            if partial.parity.is_empty() {
                partial.parity = vec![None; groups];
            }
            if partial.parity.len() != groups {
                return Err(Error::Inconsistent("同一帧的校验分组数不一致"));
            }
            let slot = partial
                .parity
                .get_mut(usize::from(header.frag_index))
                .ok_or(Error::Inconsistent("校验分组序号越界"))?;
            if slot.is_some() {
                self.stats.duplicates = self.stats.duplicates.saturating_add(1);
                return Ok(None);
            }
            *slot = Some(parity);
        } else {
            // validate() 已保证序号小于总数；这里仍然不索引，越界时按错误返回而不是 panic。
            let slot = partial
                .parts
                .get_mut(usize::from(header.frag_index))
                .ok_or(Error::Inconsistent("分片序号越界"))?;
            if slot.is_some() {
                self.stats.duplicates = self.stats.duplicates.saturating_add(1);
                return Ok(None);
            }
            *slot = Some(packet.data);
            partial.have = partial.have.saturating_add(1);
            partial.top = partial.top.max(Some(header.frag_index));
        }

        if partial.have < partial.parts.len() && !partial.parity.is_empty() {
            let recovered = partial.recover();
            self.stats.recovered = self.stats.recovered.saturating_add(recovered);
        }
        if partial.have < partial.parts.len() {
            return Ok(None);
        }

        let Some(done) = self.pending.remove(&header.frame_id) else {
            return Ok(None);
        };
        let frame = Frame {
            frame_id: header.frame_id,
            ref_frame: done.ref_frame,
            data: done.parts.into_iter().flatten().flatten().collect(),
            first_seen: done.first_seen,
            completed: now,
        };
        // 参考的帧还在补：先放着，它交出时跟着交出
        if self.waits(&frame) {
            self.stats.held = self.stats.held.saturating_add(1);
            self.held.insert(frame.frame_id, frame);
            return Ok(None);
        }
        let mut last = frame.frame_id;
        self.deliver(last);
        // 等着这一帧的帧按顺序跟着交出
        while let Some(next) = self
            .held
            .iter()
            .find(|(_, f)| f.ref_frame == last)
            .map(|(id, _)| *id)
        {
            if let Some(f) = self.held.remove(&next) {
                self.deliver(next);
                self.ready.push_back(f);
            }
            last = next;
        }
        self.drop_orphans();
        Ok(Some(frame))
    }

    /// 上一次 [`Self::push`] 交出一帧时，跟着它一起能交出的帧（按顺序），每次取一个。
    pub fn take_ready(&mut self) -> Option<Frame> {
        self.ready.pop_front()
    }

    /// 这一帧要不要等参考帧（H.264 模式下，参考的帧还在补或也在等）。
    fn waits(&self, frame: &Frame) -> bool {
        self.keyframe_grace && frame.ref_frame != frame.frame_id && self.awaited(frame.ref_frame)
    }

    /// 这一帧还可能交出：还在补、收齐了在等，或者整个丢了、在请求整帧重传。
    fn awaited(&self, frame_id: u32) -> bool {
        self.pending.contains_key(&frame_id)
            || self.held.contains_key(&frame_id)
            || self.wanted.contains_key(&frame_id)
    }

    /// 交出 `frame_id`：比它旧的未完成帧和等着的帧都作废（新帧优先，它不需要它们）。
    fn deliver(&mut self, frame_id: u32) {
        self.raise_floor(frame_id);
        self.drop_stale();
        self.stats.delivered = self.stats.delivered.saturating_add(1);
    }

    /// 作废帧号不大于 `floor` 的未完成帧和等着的帧。
    fn drop_stale(&mut self) {
        let Some(floor) = self.floor else {
            return;
        };
        let after = floor.saturating_add(1);
        self.wanted = self.wanted.split_off(&after);
        let keep = self.pending.split_off(&after);
        let held = self.held.split_off(&after);
        let dropped = self.pending.len().saturating_add(self.held.len());
        self.stats.expired = self
            .stats
            .expired
            .saturating_add(u64::try_from(dropped).unwrap_or(u64::MAX));
        self.pending = keep;
        self.held = held;
    }

    /// 参考帧已经没了（作废）的等待帧永远解不了：作废，连同等它们的帧。
    fn drop_orphans(&mut self) {
        loop {
            let orphans: Vec<u32> = self
                .held
                .iter()
                .filter(|(_, f)| !self.awaited(f.ref_frame))
                .map(|(id, _)| *id)
                .collect();
            let Some(last) = orphans.last().copied() else {
                return;
            };
            for id in &orphans {
                self.held.remove(id);
                self.stats.expired = self.stats.expired.saturating_add(1);
            }
            self.raise_floor(last);
            self.drop_stale();
        }
    }

    /// 作废没有进展的帧，返回它们的帧号。
    ///
    /// 按“多久没收到新分片”判断，而不是按“第一个分片到了多久”：窄带宽上一个关键帧可能要
    /// 好几百毫秒才送完，只要分片还在陆续到，就不该作废它（作废了发送端只能再发一个同样大的
    /// 关键帧，永远收不齐）。另有一个硬上限：截止时间的 8 倍。
    pub fn expire(&mut self, now: Time) -> Vec<u32> {
        let deadline = self.deadline_us;
        let hard = deadline.saturating_mul(MAX_FRAME_LIFETIME_FACTOR);
        let late: Vec<u32> = self
            .pending
            .iter()
            // 关键帧（参考自己）不受总寿命限制，只要还在陆续收到分片就继续等：它是恢复画面的唯一途径，
            // 窄带宽上一个关键帧本来就要传一两秒（400 kbps 传 96 KB 约 2 秒）。按总寿命作废的话
            // 永远收不齐，发送端又重发新的关键帧，画面始终出不来。没有进展时照常作废。
            // 关键帧“没有进展”的判定也放宽到总寿命上限：排队严重时 QUIC 的拥塞窗口要等确认，
            // 分片会成批到达、中间停顿几百毫秒（400 kbps、排队 1 秒时实测如此）。
            .filter(|(id, p)| {
                let key = self.keyframe_grace && p.ref_frame == **id;
                let stalled = now.since(p.last_seen) >= if key { hard } else { deadline };
                stalled || (!key && now.since(p.first_seen) >= hard)
            })
            .map(|(id, _)| *id)
            .collect();
        for id in &late {
            self.pending.remove(id);
            self.stats.expired = self.stats.expired.saturating_add(1);
            self.raise_floor(*id);
        }
        // 请求了整帧重传、截止时间内一个分片都没等到：作废（等它的帧跟着作废，发送端据此补关键帧）
        let gone: Vec<u32> = self
            .wanted
            .iter()
            .filter(|(_, at)| now.since(**at) >= deadline)
            .map(|(id, _)| *id)
            .collect();
        for id in &gone {
            self.wanted.remove(id);
            self.stats.expired = self.stats.expired.saturating_add(1);
            self.raise_floor(*id);
        }
        // 帧号低于最新作废帧的其余未完成帧也不可能再交付；等着作废帧的帧也解不了。
        self.drop_stale();
        self.drop_orphans();
        late
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn packets(frame_id: u32, len: usize, mtu: usize) -> Vec<VideoPacket> {
        let data: Vec<u8> = (0..len).map(|i| u8::try_from(i % 251).unwrap()).collect();
        fragment(frame_id, frame_id.saturating_sub(1), &data, mtu).unwrap()
    }

    #[test]
    fn fragment_sizes() {
        assert_eq!(packets(1, 0, 100).len(), 1);
        assert_eq!(packets(1, 1, 100).len(), 1);
        assert_eq!(packets(1, 100, 100).len(), 1);
        assert_eq!(packets(1, 101, 100).len(), 2);
        let p = packets(1, 250, 100);
        assert_eq!(p.len(), 3);
        assert_eq!(p[2].data.len(), 50);
        assert!(p.iter().all(|x| x.header.frag_count == 3));
        assert_eq!(
            fragment(1, 0, b"x", 0),
            Err(Error::InvalidArgument("max_payload 不能为 0"))
        );
        assert!(matches!(
            fragment(1, 0, &vec![0u8; 70_000], 1),
            Err(Error::PayloadTooLarge { .. })
        ));
        // 恰好 65535 片是允许的上限
        assert_eq!(fragment(1, 0, &vec![0u8; 65_535], 1).unwrap().len(), 65_535);
    }

    #[test]
    fn reassembles_out_of_order_and_ignores_duplicates() {
        let mut r = Reassembler::new(50_000, 4);
        let mut p = packets(5, 250, 100);
        p.reverse();
        assert_eq!(r.push(Time(10), p[0].clone()).unwrap(), None);
        assert_eq!(r.push(Time(11), p[0].clone()).unwrap(), None);
        assert_eq!(r.stats().duplicates, 1);
        assert_eq!(r.push(Time(12), p[1].clone()).unwrap(), None);
        assert_eq!(r.pending(), 1);
        let frame = r.push(Time(20), p[2].clone()).unwrap().unwrap();
        assert_eq!(frame.frame_id, 5);
        assert_eq!(frame.ref_frame, 4);
        assert_eq!(frame.data.len(), 250);
        assert_eq!(frame.data[249], 249);
        assert_eq!((frame.first_seen, frame.completed), (Time(10), Time(20)));
        assert_eq!(r.pending(), 0);
        // 已交付帧的迟到分片
        assert_eq!(r.push(Time(21), p[0].clone()).unwrap(), None);
        assert_eq!(r.stats().stale, 1);
        assert_eq!(r.stats().delivered, 1);
    }

    #[test]
    fn newer_complete_frame_supersedes_older_partial() {
        let mut r = Reassembler::new(50_000, 4);
        let old = packets(1, 200, 100);
        r.push(Time(0), old[0].clone()).unwrap();
        let newer = packets(3, 50, 100);
        let future = packets(9, 200, 100);
        r.push(Time(1), future[0].clone()).unwrap();
        assert!(r.push(Time(2), newer[0].clone()).unwrap().is_some());
        assert_eq!(r.stats().expired, 1); // 帧 1 作废
        assert_eq!(r.pending(), 1); // 帧 9 保留
        assert_eq!(r.push(Time(3), old[1].clone()).unwrap(), None);
        assert_eq!(r.stats().stale, 1);
        assert!(r.push(Time(4), future[1].clone()).unwrap().is_some());
    }

    #[test]
    fn h264_frames_wait_for_their_reference_and_come_out_in_order() {
        let mut r = Reassembler::new(50_000, 8);
        r.set_keyframe_grace(true);
        // 帧 1 缺一片（在等重传）；参考它的帧 2、3 先收齐：放着等，不把帧 1 作废
        let one = packets(1, 200, 100);
        r.push(Time(0), one[0].clone()).unwrap();
        assert_eq!(
            r.push(Time(1), packets(2, 50, 100)[0].clone()).unwrap(),
            None
        );
        assert_eq!(
            r.push(Time(2), packets(3, 50, 100)[0].clone()).unwrap(),
            None
        );
        assert_eq!(r.stats().held, 2);
        assert_eq!(r.stats().expired, 0);
        // 等着的帧又收到分片（重传、校验）：算重复
        assert_eq!(
            r.push(Time(3), packets(2, 50, 100)[0].clone()).unwrap(),
            None
        );
        assert_eq!(r.stats().duplicates, 1);
        // 帧 1 的重传到了：1、2、3 按顺序交出
        assert_eq!(
            r.push(Time(4), one[1].clone()).unwrap().map(|f| f.frame_id),
            Some(1)
        );
        assert_eq!(r.take_ready().map(|f| f.frame_id), Some(2));
        assert_eq!(r.take_ready().map(|f| f.frame_id), Some(3));
        assert_eq!(r.take_ready(), None);
        assert_eq!(r.floor(), Some(3));
        assert_eq!(r.stats().delivered, 3);

        // 参考帧补不回来（超时作废）：等它的帧跟着作废，发送端据此补关键帧
        r.push(
            Time(10),
            fragment(5, 3, &[1u8; 200], 100).unwrap()[0].clone(),
        )
        .unwrap();
        assert_eq!(
            r.push(Time(11), packets(6, 50, 100)[0].clone()).unwrap(),
            None
        );
        assert_eq!(
            r.push(Time(12), packets(7, 50, 100)[0].clone()).unwrap(),
            None
        );
        assert_eq!(r.expire(Time(50_010)), vec![5]);
        assert_eq!(r.floor(), Some(7));
        assert_eq!(r.stats().expired, 3);
        assert_eq!(r.take_ready(), None);

        // 关键帧收齐：不需要更旧的帧，在补的和等着的都作废
        r.push(
            Time(20),
            fragment(10, 7, &[1u8; 200], 100).unwrap()[0].clone(),
        )
        .unwrap();
        assert_eq!(
            r.push(Time(21), packets(11, 50, 100)[0].clone()).unwrap(),
            None
        );
        let key = fragment(12, 12, &[1u8; 50], 100).unwrap();
        assert_eq!(
            r.push(Time(22), key[0].clone())
                .unwrap()
                .map(|f| f.frame_id),
            Some(12)
        );
        assert_eq!(r.take_ready(), None);
        assert_eq!(r.stats().expired, 5);
        assert_eq!(r.pending(), 0);

        // 缓冲区满：等着的帧也算；作废最旧的未完成帧时，等它的帧跟着作废
        let mut r = Reassembler::new(1_000_000, 2);
        r.set_keyframe_grace(true);
        r.push(
            Time(0),
            fragment(20, 0, &[1u8; 200], 100).unwrap()[0].clone(),
        )
        .unwrap();
        assert_eq!(
            r.push(Time(1), packets(21, 50, 100)[0].clone()).unwrap(),
            None
        );
        r.push(Time(2), packets(22, 200, 100)[0].clone()).unwrap();
        assert_eq!(r.floor(), Some(21));
        assert_eq!(r.stats().expired, 2);
        assert_eq!(r.pending(), 1);
        // 不打开（块编码）：新帧收齐照旧作废更旧的未完成帧
        let mut r = Reassembler::new(50_000, 8);
        r.push(Time(0), packets(1, 200, 100)[0].clone()).unwrap();
        assert!(r
            .push(Time(1), packets(2, 50, 100)[0].clone())
            .unwrap()
            .is_some());
        assert_eq!(r.stats().held, 0);
        assert_eq!(r.stats().expired, 1);
    }

    #[test]
    fn a_frame_lost_entirely_is_requested_whole_and_its_successors_wait_for_it() {
        let mut r = Reassembler::new(50_000, 8);
        r.set_keyframe_grace(true);
        r.push(Time(0), packets(1, 50, 100)[0].clone()).unwrap();
        // 帧 2 一个分片都没到；帧 3（参考帧 2）收齐了：先放着，请求帧 2 整帧重传
        assert_eq!(
            r.push(Time(10), packets(3, 50, 100)[0].clone()).unwrap(),
            None
        );
        assert_eq!(r.missing(Time(10), 0, 8), vec![(2, WHOLE_FRAME)]);
        assert!(r.missing(Time(10), 5, 8).is_empty(), "刚发现：可能只是乱序");
        // 帧 2 重传到了：2、3 按顺序交出
        assert_eq!(
            r.push(Time(60), packets(2, 50, 100)[0].clone())
                .unwrap()
                .map(|f| f.frame_id),
            Some(2)
        );
        assert_eq!(r.take_ready().map(|f| f.frame_id), Some(3));
        assert!(r.missing(Time(60), 0, 8).is_empty());
        // 整帧重传等不来（截止时间内一个分片都没到）：作废，等它的帧跟着作废
        r.push(Time(100), packets(5, 50, 100)[0].clone()).unwrap();
        assert_eq!(r.missing(Time(100), 0, 8), vec![(4, WHOLE_FRAME)]);
        let _ = r.expire(Time(50_099));
        assert_eq!(r.floor(), Some(3));
        let _ = r.expire(Time(50_100));
        assert_eq!(r.floor(), Some(5));
        assert!(r.missing(Time(50_100), 0, 8).is_empty());
        // 帧 7 参考没见过的帧 6：请求；接着来了参考已交出的帧 5 的帧 8（编码器换了参考）：它交出后，
        // 帧 6、7 都用不着了，不再请求
        r.push(Time(50_200), packets(7, 50, 100)[0].clone())
            .unwrap();
        assert_eq!(r.missing(Time(50_200), 0, 8), vec![(6, WHOLE_FRAME)]);
        let eight = fragment(8, 5, &[1u8; 50], 100).unwrap();
        assert!(r.push(Time(50_201), eight[0].clone()).unwrap().is_some());
        assert!(r.missing(Time(50_300), 0, 8).is_empty());
        // 不打开（块编码）：不请求整帧
        let mut r = Reassembler::new(50_000, 8);
        r.push(Time(0), packets(1, 50, 100)[0].clone()).unwrap();
        r.push(Time(1), packets(3, 200, 100)[0].clone()).unwrap();
        assert!(r
            .missing(Time(1), 0, 8)
            .iter()
            .all(|(_, frag)| *frag != WHOLE_FRAME));
    }

    #[test]
    fn deadline_expiry() {
        let mut r = Reassembler::new(1000, 4);
        r.push(Time(0), packets(1, 200, 100)[0].clone()).unwrap();
        r.push(Time(600), packets(2, 200, 100)[0].clone()).unwrap();
        assert!(r.expire(Time(999)).is_empty());
        assert_eq!(r.expire(Time(1000)), vec![1]);
        assert_eq!(r.pending(), 1);
        assert_eq!(r.expire(Time(1600)), vec![2]);
        assert_eq!(r.stats().expired, 2);
        assert_eq!(r.pending(), 0);
    }

    #[test]
    fn slow_keyframes_survive_while_they_keep_arriving() {
        // 截止时间 1 ms，总寿命上限 8 ms；分片每 0.5 ms 到一个，一共 20 个，要 10 ms
        let key = fragment(5, 5, &[7u8; 2000], 100).unwrap();
        // 更旧的增量帧（发送端在关键帧送达前不发更新的帧）
        let delta = fragment(4, 3, &[7u8; 2000], 100).unwrap();
        let mut r = Reassembler::new(1000, 4);
        r.set_keyframe_grace(true);
        let mut done = None;
        for (i, (k, d)) in key.into_iter().zip(delta).enumerate() {
            let t = Time(u64::try_from(i).unwrap() * 500);
            let _ = r.expire(t);
            if let Some(f) = r.push(t, k).unwrap() {
                done = Some(f);
            }
            let _ = r.push(t, d);
        }
        // 关键帧收齐了；同样慢的增量帧超过总寿命被作废
        assert!(done.is_some_and(|f| f.frame_id == 5));
        assert!(r.stats().expired >= 1);
        // 关键帧长时间（总寿命上限）没有进展时照常作废
        let mut r = Reassembler::new(1000, 4);
        r.set_keyframe_grace(true);
        r.push(
            Time(0),
            fragment(3, 3, &[1u8; 300], 100).unwrap()[0].clone(),
        )
        .unwrap();
        assert!(r.expire(Time(7999)).is_empty());
        assert_eq!(r.expire(Time(8000)), vec![3]);
        // 不打开宽限（块编码）：参考自己的帧也按普通帧作废
        let mut r = Reassembler::new(1000, 4);
        r.push(
            Time(0),
            fragment(3, 3, &[1u8; 300], 100).unwrap()[0].clone(),
        )
        .unwrap();
        assert_eq!(r.expire(Time(1000)), vec![3]);
    }

    #[test]
    fn expiry_drops_older_frames_below_floor() {
        // 帧 2 先到并超时，帧 1 后到但更旧：帧 2 作废后帧 1 也不可能再交付。
        let mut r = Reassembler::new(1000, 4);
        r.push(Time(0), packets(2, 200, 100)[0].clone()).unwrap();
        r.push(Time(900), packets(1, 200, 100)[0].clone()).unwrap();
        assert_eq!(r.expire(Time(1000)), vec![2]);
        assert_eq!(r.pending(), 0);
        assert_eq!(r.stats().expired, 2);
    }

    #[test]
    fn buffer_full_evicts_oldest() {
        let mut r = Reassembler::new(1_000_000, 0); // 至少 1
        r.push(Time(0), packets(1, 200, 100)[0].clone()).unwrap();
        r.push(Time(1), packets(2, 200, 100)[0].clone()).unwrap();
        assert_eq!(r.pending(), 1);
        assert_eq!(r.stats().expired, 1);
        // 帧号低于已作废帧的新帧直接算迟到
        let mut r = Reassembler::new(1_000_000, 1);
        r.push(Time(0), packets(5, 200, 100)[0].clone()).unwrap();
        assert_eq!(
            r.push(Time(1), packets(3, 200, 100)[0].clone()).unwrap(),
            None
        );
        assert_eq!(r.stats().stale, 1);
        assert_eq!(r.pending(), 0);
    }

    #[test]
    fn inconsistent_headers_are_rejected() {
        let mut r = Reassembler::new(1000, 4);
        let p = packets(1, 250, 100);
        r.push(Time(0), p[0].clone()).unwrap();

        let mut wrong_count = p[1].clone();
        wrong_count.header.frag_count = 5;
        assert_eq!(
            r.push(Time(1), wrong_count),
            Err(Error::Inconsistent("同一帧的分片总数不一致"))
        );
        let mut wrong_ref = p[1].clone();
        wrong_ref.header.ref_frame = 77;
        assert_eq!(
            r.push(Time(1), wrong_ref),
            Err(Error::Inconsistent("同一帧的参考帧不一致"))
        );
        let mut bad_index = p[1].clone();
        bad_index.header.frag_index = 3;
        assert_eq!(
            r.push(Time(1), bad_index),
            Err(Error::InvalidFragment { index: 3, count: 3 })
        );
        let mut zero = p[1].clone();
        zero.header.frag_count = 0;
        assert!(matches!(
            r.push(Time(1), zero),
            Err(Error::InvalidFragment { .. })
        ));
    }

    #[test]
    fn parity_recovers_one_loss_per_group() {
        // 6 个数据分片，50% 冗余 → 3 组，每组 2 个成员（0,3）（1,4）（2,5）
        let data = packets(7, 550, 100);
        assert_eq!(data.len(), 6);
        let parity = parity_for(&data, 500);
        assert_eq!(parity.len(), 3);
        assert!(parity.iter().all(|p| p.parity && p.header.frag_count == 3));

        // 每组各丢一个（包括长度不同的最后一片）：全部恢复
        let mut r = Reassembler::new(50_000, 4);
        for i in [0usize, 1, 2] {
            assert_eq!(r.push(Time(0), data[i].clone()).unwrap(), None);
        }
        assert_eq!(r.push(Time(1), parity[0].clone()).unwrap(), None);
        assert_eq!(r.push(Time(1), parity[1].clone()).unwrap(), None);
        let frame = r.push(Time(2), parity[2].clone()).unwrap().unwrap();
        assert_eq!(frame.data.len(), 550);
        let expected: Vec<u8> = (0..550).map(|i| u8::try_from(i % 251).unwrap()).collect();
        assert_eq!(frame.data, expected);
        assert_eq!(r.stats().recovered, 3);

        // 校验分片先到、数据分片后到也可以
        let mut r = Reassembler::new(50_000, 4);
        for p in &parity {
            r.push(Time(0), p.clone()).unwrap();
        }
        assert_eq!(r.pending(), 1);
        for i in [3usize, 4] {
            r.push(Time(1), data[i].clone()).unwrap();
        }
        assert!(r.push(Time(2), data[5].clone()).unwrap().is_some());

        // 同一组丢两个：恢复不了，等重传
        let mut r = Reassembler::new(50_000, 4);
        for i in [1usize, 2, 4, 5] {
            r.push(Time(0), data[i].clone()).unwrap();
        }
        for p in &parity {
            assert_eq!(r.push(Time(1), p.clone()).unwrap(), None);
        }
        assert_eq!(r.stats().recovered, 0);
        assert_eq!(r.missing(Time(1), 0, 10), vec![(7, 0), (7, 3)]);
        // 重传来了一个，另一个靠校验恢复
        assert!(r.push(Time(2), data[0].clone()).unwrap().is_some());
        assert_eq!(r.stats().recovered, 1);
        // 重复的校验分片
        let mut r = Reassembler::new(50_000, 4);
        r.push(Time(0), parity[0].clone()).unwrap();
        r.push(Time(0), parity[0].clone()).unwrap();
        assert_eq!(r.stats().duplicates, 1);
    }

    #[test]
    fn rs_parity_rebuilds_several_losses_in_one_frame() {
        // 10 个数据分片（最后一片短），30% 冗余 → 3 个校验分片；丢 3 个数据分片照样收齐
        let data = packets(7, 950, 100);
        assert_eq!(data.len(), 10);
        let parity = rs_parity_for(&data, 300);
        assert_eq!(parity.len(), 3);
        let mut r = Reassembler::new(1_000_000, 4);
        r.set_fec(Fec::Rs);
        let mut done = None;
        for (i, p) in data.iter().enumerate() {
            if [1, 4, 9].contains(&i) {
                continue;
            }
            assert!(r.push(Time(1), p.clone()).unwrap().is_none());
        }
        for p in &parity {
            if let Some(f) = r.push(Time(2), p.clone()).unwrap() {
                done = Some(f);
            }
        }
        let frame = done.unwrap();
        let expected: Vec<u8> = data.iter().flat_map(|p| p.data.clone()).collect();
        assert_eq!(frame.data, expected);
        assert_eq!(r.stats().recovered, 3);
        // 丢 4 个、只有 3 个校验分片：收不齐（等重传）
        let mut r = Reassembler::new(1_000_000, 4);
        r.set_fec(Fec::Rs);
        for (i, p) in data.iter().enumerate() {
            if ![0, 1, 2, 3].contains(&i) {
                r.push(Time(1), p.clone()).unwrap();
            }
        }
        for p in &parity {
            assert!(r.push(Time(2), p.clone()).unwrap().is_none());
        }
        // 被篡改的校验分片（长度字段说的比实际数据长）：不“还原”出错误的内容
        let mut r = Reassembler::new(1_000_000, 4);
        r.set_fec(Fec::Rs);
        for (i, p) in data.iter().enumerate() {
            if i != 1 {
                r.push(Time(1), p.clone()).unwrap();
            }
        }
        let mut lying = parity[0].clone();
        lying.data[2] = 0x7f; // 分片长度 0x7f..
        lying.data[3] = 0xff;
        assert!(r.push(Time(2), lying).unwrap().is_none());
        assert_eq!(r.stats().recovered, 0);
        // 不合法的 RS 校验分片：报错
        let mut bad = parity[0].clone();
        bad.data[4] = 0xff; // 最后一片长度 > 分片长度
        bad.data[5] = 0xff;
        let mut r = Reassembler::new(1_000_000, 4);
        r.set_fec(Fec::Rs);
        assert!(r.push(Time(1), bad).is_err());
        // 没有数据分片 / 比例为 0 / 一帧超过 256 个分片：不生成
        assert!(rs_parity_for(&[], 300).is_empty());
        assert!(rs_parity_for(&packets(8, 3000, 10), 300).is_empty());
        assert!(rs_parity_for(&data, 0).is_empty());
    }

    #[test]
    fn parity_ratios() {
        let data = packets(1, 1000, 100); // 10 片
        assert!(parity_for(&data, 0).is_empty());
        assert_eq!(parity_for(&data, 1).len(), 1);
        assert_eq!(parity_for(&data, 100).len(), 1);
        assert_eq!(parity_for(&data, 101).len(), 2);
        assert_eq!(parity_for(&data, 1000).len(), 10);
        assert_eq!(parity_for(&data, 5000).len(), 10, "至多每片一个");
        assert!(parity_for(&[], 500).is_empty());
        // 单分片帧：校验分片就是它的副本
        let one = packets(2, 40, 100);
        let p = parity_for(&one, 200);
        assert_eq!(p.len(), 1);
        let mut r = Reassembler::new(1000, 4);
        let frame = r.push(Time(0), p[0].clone()).unwrap().unwrap();
        assert_eq!(frame.data, one[0].data);
        // 输入里混着校验分片时只看数据分片
        let mut mixed = data.clone();
        mixed.extend(parity_for(&data, 200));
        assert_eq!(parity_for(&mixed, 200).len(), 2);
    }

    #[test]
    fn malformed_parity_is_rejected_or_ignored() {
        let data = packets(3, 300, 100);
        let parity = parity_for(&data, 1000);
        let mut r = Reassembler::new(1000, 4);
        r.push(Time(0), data[0].clone()).unwrap();

        let mut truncated = parity[0].clone();
        truncated.data.truncate(3);
        assert!(matches!(
            r.push(Time(0), truncated),
            Err(Error::Truncated { .. })
        ));
        let mut zero = parity[0].clone();
        zero.data[..2].copy_from_slice(&[0, 0]);
        assert!(matches!(r.push(Time(0), zero), Err(Error::Inconsistent(_))));
        let mut wrong_count = parity[0].clone();
        wrong_count.data[..2].copy_from_slice(&[0, 9]);
        assert_eq!(
            r.push(Time(0), wrong_count),
            Err(Error::Inconsistent("同一帧的分片总数不一致"))
        );
        // 组数比数据分片还多
        let mut too_many = parity[0].clone();
        too_many.header.frag_count = 4;
        assert!(matches!(
            r.push(Time(0), too_many),
            Err(Error::Inconsistent(_))
        ));
        // 组数前后不一致
        r.push(Time(0), parity[0].clone()).unwrap();
        let mut other = parity[1].clone();
        other.header.frag_count = 2;
        assert_eq!(
            r.push(Time(0), other),
            Err(Error::Inconsistent("同一帧的校验分组数不一致"))
        );
        // 长度异或与数据对不上：不恢复，也不崩溃
        let mut r = Reassembler::new(1000, 4);
        let mut lying = parity[1].clone();
        lying.data[2..4].copy_from_slice(&[0xff, 0xff]);
        assert_eq!(r.push(Time(0), lying).unwrap(), None);
        assert_eq!(r.stats().recovered, 0);
    }

    /// 大帧还在陆续到的时候不要它的后半段（2026-11-26 用户日志：大帧后半段还在路上就被要重传，重传是实际丢包的几十倍）。
    #[test]
    fn the_tail_of_a_frame_still_arriving_is_not_requested() {
        let mut r = Reassembler::new(500_000, 4);
        let big = packets(1, 100 * 1000, 1000);
        assert_eq!(big.len(), 100);
        // 前 50 片每 400 µs 到一片（30 Mbps 左右的节奏）
        for (i, p) in big.iter().take(50).enumerate() {
            let at = u64::try_from(i).unwrap() * 400;
            r.push(Time(at), p.clone()).unwrap();
        }
        let last = 49 * 400;
        assert!(r.missing(Time(last + 1_000), 10_000, 200).is_empty(), "还在到");
        // 停了：3 个平均间隔、至少 2 倍最短等待（20 ms）没来新分片，尾部算丢
        assert!(r.missing(Time(last + 19_000), 10_000, 200).is_empty());
        assert_eq!(r.missing(Time(last + 20_000), 10_000, 200).len(), 50);
        // 更新的帧已经在到：发送端发完了这一帧，再等 10 ms 没来就算丢
        let mut r = Reassembler::new(500_000, 4);
        for p in big.iter().take(50) {
            r.push(Time(0), p.clone()).unwrap();
        }
        r.push(Time(1_000), packets(2, 400, 100)[0].clone()).unwrap();
        assert!(r.missing(Time(9_000), 10_000, 200).is_empty());
        assert_eq!(r.missing(Time(10_000), 10_000, 200).len(), 50, "第 2 帧自己的尾部还没到时候");
        // 窄带：分片本来就隔 30 ms，尾部按 3 个间隔（90 ms）才算停
        let mut r = Reassembler::new(500_000, 4);
        for (i, p) in big.iter().take(3).enumerate() {
            r.push(Time(u64::try_from(i).unwrap() * 30_000), p.clone()).unwrap();
        }
        assert!(r.missing(Time(60_000 + 80_000), 10_000, 200).is_empty());
        assert_eq!(r.missing(Time(60_000 + 90_000), 10_000, 200).len(), 97);
    }

    #[test]
    fn missing_respects_age_and_limit() {
        let mut r = Reassembler::new(50_000, 4);
        let a = packets(1, 400, 100);
        let b = packets(2, 400, 100);
        r.push(Time(0), a[0].clone()).unwrap();
        r.push(Time(900), b[3].clone()).unwrap();
        // 第 1 帧只到了第 0 片、之后 1000 µs 没动静，更新的第 2 帧也在到了：尾部算丢。
        // 第 2 帧到了第 3 片：第 0 片后面已经隔了 3 个序号，算丢；第 1、2 片刚 100 µs，可能只是乱序
        assert_eq!(
            r.missing(Time(1000), 500, 10),
            vec![(1, 1), (1, 2), (1, 3), (2, 0)]
        );
        assert_eq!(
            r.missing(Time(1000), 0, 4),
            vec![(1, 1), (1, 2), (1, 3), (2, 0)]
        );
        // 等得再久（5 ms）也不用等的：后面已经隔了 3 个序号的空洞
        assert_eq!(r.missing(Time(1000), 5000, 10), vec![(2, 0)]);
        assert_eq!(r.floor(), None);
        r.expire(Time(60_000));
        assert_eq!(r.floor(), Some(2));
    }
}
