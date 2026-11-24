//! 视频帧的分片、纠错与重组。
//!
//! 发送端把一帧切成不超过数据报上限的分片，并可以附加校验分片；接收端按帧号重组。
//! 规则是“新帧优先”：一帧完整后，比它旧的未完成帧立刻作废；超过截止时间的帧也作废。
//!
//! 纠错用分组异或校验：数据分片按序号轮流分到若干组，每组一个校验分片，
//! 组内丢一个分片可以恢复。它便宜、没有额外依赖，对随机丢包有效；
//! 对成串的丢包效果有限，那种情况靠重传（见 [`crate::adaptive`]）。

use std::collections::BTreeMap;

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
    /// 参考自己的帧当作关键帧，给更长的时间（见 [`Reassembler::expire`]）。只对 H.264 打开：
    /// 块编码的每一帧都参考自己、都很小，丢了靠重发块恢复，给长时间反而拖慢恢复。
    keyframe_grace: bool,
}

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
        }
    }

    /// 设置纠错方式（握手协商之后）。
    pub fn set_fec(&mut self, fec: Fec) {
        self.fec = fec;
    }

    /// 打开或关闭关键帧宽限（H.264 打开）。
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

    /// 等了至少 `min_age_us` 仍然缺失的数据分片，最多返回 `limit` 个 `(帧号, 分片序号)`。
    /// 接收端据此请求重传。
    #[must_use]
    pub fn missing(&self, now: Time, min_age_us: u64, limit: usize) -> Vec<(u32, u16)> {
        self.pending
            .iter()
            .filter(|(_, p)| now.since(p.first_seen) >= min_age_us)
            .flat_map(|(frame_id, p)| {
                p.parts
                    .iter()
                    .zip(0u16..)
                    .filter(|(part, _)| part.is_none())
                    .map(|(_, index)| (*frame_id, index))
            })
            .take(limit)
            .collect()
    }

    /// 放入一个分片。收齐一帧时返回它。
    ///
    /// 分片头自相矛盾（同一帧的分片总数或参考帧前后不一致）时返回错误。
    pub fn push(&mut self, now: Time, packet: VideoPacket) -> Result<Option<Frame>, Error> {
        let header = packet.header;
        header.validate()?;
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
        if !self.pending.contains_key(&header.frame_id) {
            // 缓冲区满：作废最旧的帧，给新帧让位。
            let excess = self
                .pending
                .len()
                .saturating_add(1)
                .saturating_sub(self.max_pending);
            for _ in 0..excess {
                if let Some((oldest, _)) = self.pending.pop_first() {
                    self.stats.expired = self.stats.expired.saturating_add(1);
                    self.raise_floor(oldest);
                }
            }
            if self.floor.is_some_and(|f| header.frame_id <= f) {
                self.stats.stale = self.stats.stale.saturating_add(1);
                return Ok(None);
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
        }

        if partial.have < partial.parts.len() && !partial.parity.is_empty() {
            let recovered = partial.recover();
            self.stats.recovered = self.stats.recovered.saturating_add(recovered);
        }
        if partial.have < partial.parts.len() {
            return Ok(None);
        }

        let done = self.pending.remove(&header.frame_id);
        // 新帧优先：比它旧的未完成帧全部作废。
        let newer = self.pending.split_off(&header.frame_id);
        let superseded = u64::try_from(self.pending.len()).unwrap_or(u64::MAX);
        self.stats.expired = self.stats.expired.saturating_add(superseded);
        self.pending = newer;
        self.raise_floor(header.frame_id);
        self.stats.delivered = self.stats.delivered.saturating_add(1);

        Ok(done.map(|done| Frame {
            frame_id: header.frame_id,
            ref_frame: done.ref_frame,
            data: done.parts.into_iter().flatten().flatten().collect(),
            first_seen: done.first_seen,
            completed: now,
        }))
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
        // 帧号低于最新作废帧的其余未完成帧也不可能再交付。
        if let Some(floor) = self.floor {
            let keep = self.pending.split_off(&floor);
            let dropped = u64::try_from(self.pending.len()).unwrap_or(u64::MAX);
            self.stats.expired = self.stats.expired.saturating_add(dropped);
            self.pending = keep;
        }
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

    #[test]
    fn missing_respects_age_and_limit() {
        let mut r = Reassembler::new(50_000, 4);
        let a = packets(1, 400, 100);
        let b = packets(2, 400, 100);
        r.push(Time(0), a[0].clone()).unwrap();
        r.push(Time(900), b[3].clone()).unwrap();
        assert_eq!(r.missing(Time(1000), 500, 10), vec![(1, 1), (1, 2), (1, 3)]);
        assert_eq!(
            r.missing(Time(1000), 0, 4),
            vec![(1, 1), (1, 2), (1, 3), (2, 0)]
        );
        assert!(r.missing(Time(1000), 5000, 10).is_empty());
        assert_eq!(r.floor(), None);
        r.expire(Time(60_000));
        assert_eq!(r.floor(), Some(2));
    }
}
