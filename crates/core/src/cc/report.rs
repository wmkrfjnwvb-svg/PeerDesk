//! 到达报告和发送历史：接收端只记每个包什么时候到，发送端对账出“每个包的发送时间、到达时间、大小”。
//!
//! 照 WebRTC 的 transport-wide congestion control（TWCC）：
//! - 接收端（`TransportSequenceNumberFeedbackGenenerator`）只记“序号 → 到达时间”，不做估计；
//! - 发送端（`TransportFeedbackAdapter`）保存“序号 → 发送时间、大小”，收到报告后逐包对上，
//!   交给网络控制器（[`super::NetworkController`]）。所有网络决定都在发送端做。
//!
//! 序号是数据报头里视频通道的序号：链路层每发一个视频通道的数据报加一（画面分片、校验分片、重传、探测都算），
//! 等于 WebRTC 的 transport-wide 序号。发送时间在包交给 QUIC 的那一刻记（QUIC 层不限速，
//! 交过去就发出，见 `peerdesk_net::safety`）。
//!
//! 报告格式（小端无关，按 [`crate::wire`] 的大端）：
//! 起始序号 u32、起始到达时间 u64（接收端时钟，微秒）、包数 u16、收到与否的位图、
//! 每个收到的包和上一个收到的包的到达时间差（250 µs 为单位，zigzag 变长整数；第一个相对起始到达时间）。
//! 时间差用 250 µs 为单位和 TWCC 一样：延迟估计按 5 ms 分组，足够细。

use std::collections::VecDeque;

use crate::time::Time;
use crate::wire::{Reader, Writer};
use crate::Error;

/// 到达时间差的单位（微秒）。
pub const DELTA_UNIT_US: u64 = 250;
/// 一份报告最多报多少个包（再多分到下一份）。
pub const MAX_REPORT_PACKETS: usize = 1024;
/// 发送历史保留多久（微秒）：报告丢了也能在下一份里对上。
const HISTORY_US: u64 = 10_000_000;

/// 一个包的种类（发送端记，报告里不带）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PacketKind {
    /// 画面分片或校验分片。
    Media,
    /// 重传的分片。
    Retransmit,
    /// 探测包（填充，接收端丢弃内容）：属于第几个探测组。
    Probe(u32),
}

/// 发出的一个包。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SentPacket {
    /// 序号。
    pub seq: u32,
    /// 交给 QUIC 的时刻（发送端时钟）。
    pub send_time: Time,
    /// 数据报字节数。
    pub size: u32,
    /// 种类。
    pub kind: PacketKind,
}

/// 对账后的一个包。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PacketResult {
    /// 发出的包。
    pub sent: SentPacket,
    /// 到达时刻（接收端时钟，和发送端时钟没有对齐，只能看差值）；`None` 表示报告说没收到。
    pub recv_time: Option<Time>,
}

/// 一份到达报告（解码后）。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ArrivalReport {
    /// 起始序号。
    pub base_seq: u32,
    /// 从起始序号起每个包的到达时刻（接收端时钟，精度 [`DELTA_UNIT_US`]）；`None` 表示没收到。
    pub arrivals: Vec<Option<Time>>,
}

impl ArrivalReport {
    /// 编码。
    pub fn encode(&self, w: &mut Writer) {
        let first = self
            .arrivals
            .iter()
            .flatten()
            .next()
            .copied()
            .unwrap_or(Time::ZERO);
        w.u32(self.base_seq)
            .u64(first.micros())
            .u16(u16::try_from(self.arrivals.len()).unwrap_or(u16::MAX));
        let mut bits = vec![0u8; self.arrivals.len().div_ceil(8)];
        for (i, a) in self.arrivals.iter().enumerate() {
            if a.is_some() {
                if let Some(b) = bits.get_mut(i / 8) {
                    *b |= 1 << (i % 8);
                }
            }
        }
        w.bytes(&bits);
        let mut prev = first.micros() / DELTA_UNIT_US;
        for at in self.arrivals.iter().flatten() {
            let units = at.micros() / DELTA_UNIT_US;
            let delta =
                i64::try_from(units).unwrap_or(i64::MAX) - i64::try_from(prev).unwrap_or(i64::MAX);
            put_varint(w, zigzag(delta));
            prev = units;
        }
    }

    /// 解码。
    pub fn decode(r: &mut Reader<'_>) -> Result<Self, Error> {
        let base_seq = r.u32()?;
        let first = r.u64()?;
        let count = usize::from(r.u16()?);
        if count > MAX_REPORT_PACKETS {
            return Err(Error::TooManyItems {
                len: count,
                max: MAX_REPORT_PACKETS,
            });
        }
        let bits = r.take(count.div_ceil(8))?.to_vec();
        let mut prev = i64::try_from(first / DELTA_UNIT_US).unwrap_or(i64::MAX);
        let mut arrivals = Vec::with_capacity(count);
        for i in 0..count {
            let received = bits.get(i / 8).is_some_and(|b| b >> (i % 8) & 1 == 1);
            if received {
                prev = prev.saturating_add(unzigzag(get_varint(r)?));
                let us = u64::try_from(prev.max(0))
                    .unwrap_or(0)
                    .saturating_mul(DELTA_UNIT_US);
                arrivals.push(Some(Time(us)));
            } else {
                arrivals.push(None);
            }
        }
        Ok(Self { base_seq, arrivals })
    }
}

fn zigzag(v: i64) -> u64 {
    u64::from_ne_bytes(((v << 1) ^ (v >> 63)).to_ne_bytes())
}

fn unzigzag(v: u64) -> i64 {
    i64::from_ne_bytes((v >> 1).to_ne_bytes()) ^ -i64::from_ne_bytes((v & 1).to_ne_bytes())
}

fn put_varint(w: &mut Writer, mut v: u64) {
    loop {
        let byte = u8::try_from(v & 0x7f).unwrap_or(0);
        v >>= 7;
        if v == 0 {
            w.u8(byte);
            return;
        }
        w.u8(byte | 0x80);
    }
}

fn get_varint(r: &mut Reader<'_>) -> Result<u64, Error> {
    let mut v = 0u64;
    for shift in (0..64).step_by(7) {
        let byte = r.u8()?;
        v |= u64::from(byte & 0x7f) << shift;
        if byte & 0x80 == 0 {
            return Ok(v);
        }
    }
    Err(Error::Inconsistent("变长整数太长"))
}

/// 接收端：记每个视频通道数据报的到达时间，攒成报告。
#[derive(Debug, Clone, Default)]
pub struct ArrivalRecorder {
    /// 下一份报告的起始序号（`None`：还没收到过）。
    next_base: Option<u32>,
    /// 从 `next_base` 起收到的包：序号偏移 → 到达时刻。
    pending: Vec<(u32, Time)>,
}

impl ArrivalRecorder {
    /// 创建。
    #[must_use]
    pub const fn new() -> Self {
        Self {
            next_base: None,
            pending: Vec::new(),
        }
    }

    /// 收到序号 `seq` 的包。
    pub fn on_packet(&mut self, now: Time, seq: u32) {
        let base = *self.next_base.get_or_insert(seq);
        // 比已经报告过的还早（乱序、迟到）：不再报（数据本身照样交给重组）
        let offset = seq.wrapping_sub(base);
        if offset >= u32::MAX / 2 {
            return;
        }
        if self.pending.iter().all(|(s, _)| *s != seq) {
            self.pending.push((seq, now));
        }
    }

    /// 有没有没报告的包。
    #[must_use]
    pub fn has_pending(&self) -> bool {
        !self.pending.is_empty()
    }

    /// 取一份报告：从上次报告到最新收到的包；中间没收到的报“没收到”。没有新包时为 `None`。
    pub fn report(&mut self) -> Option<ArrivalReport> {
        let base = self.next_base?;
        let highest = self
            .pending
            .iter()
            .map(|(s, _)| s.wrapping_sub(base))
            .max()?;
        let count = usize::try_from(highest)
            .unwrap_or(usize::MAX)
            .saturating_add(1)
            .min(MAX_REPORT_PACKETS);
        let mut arrivals = vec![None; count];
        let mut rest = Vec::new();
        for (seq, at) in self.pending.drain(..) {
            let offset = usize::try_from(seq.wrapping_sub(base)).unwrap_or(usize::MAX);
            match arrivals.get_mut(offset) {
                Some(slot) => *slot = Some(at),
                None => rest.push((seq, at)),
            }
        }
        self.pending = rest;
        self.next_base = Some(base.wrapping_add(u32::try_from(count).unwrap_or(u32::MAX)));
        Some(ArrivalReport {
            base_seq: base,
            arrivals,
        })
    }
}

/// 发送端：发出的包（等报告对账）。
#[derive(Debug, Clone, Default)]
pub struct SendHistory {
    packets: VecDeque<SentPacket>,
}

impl SendHistory {
    /// 创建。
    #[must_use]
    pub const fn new() -> Self {
        Self {
            packets: VecDeque::new(),
        }
    }

    /// 记一个发出的包（序号递增）。太老的丢掉。
    pub fn on_sent(&mut self, packet: SentPacket) {
        while self
            .packets
            .front()
            .is_some_and(|p| packet.send_time.since(p.send_time) > HISTORY_US)
        {
            self.packets.pop_front();
        }
        self.packets.push_back(packet);
    }

    /// 对账一份报告：报告里每个能在历史里找到的包，按序号顺序交出。报告过的包从历史里去掉。
    pub fn resolve(&mut self, report: &ArrivalReport) -> Vec<PacketResult> {
        let mut out = Vec::with_capacity(report.arrivals.len());
        for (i, recv_time) in report.arrivals.iter().enumerate() {
            let seq = report
                .base_seq
                .wrapping_add(u32::try_from(i).unwrap_or(u32::MAX));
            if let Some(pos) = self.packets.iter().position(|p| p.seq == seq) {
                if let Some(sent) = self.packets.remove(pos) {
                    out.push(PacketResult {
                        sent,
                        recv_time: *recv_time,
                    });
                }
            }
        }
        out
    }

    /// 已发出、还没有报告的字节数（在途量）。
    #[must_use]
    pub fn in_flight(&self) -> u64 {
        self.packets.iter().map(|p| u64::from(p.size)).sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sent(seq: u32, us: u64) -> SentPacket {
        SentPacket {
            seq,
            send_time: Time(us),
            size: 1200,
            kind: PacketKind::Media,
        }
    }

    #[test]
    fn reports_round_trip() {
        let report = ArrivalReport {
            base_seq: 7,
            arrivals: vec![
                Some(Time(1_000_000)),
                None,
                Some(Time(1_003_000)),
                Some(Time(1_002_000)), // 乱序：时间差为负
                None,
                Some(Time(1_900_000)), // 大跳
            ],
        };
        let mut w = Writer::new();
        report.encode(&mut w);
        let bytes = w.into_vec();
        let back = ArrivalReport::decode(&mut Reader::new(&bytes)).unwrap();
        assert_eq!(back, report);
        // 全没收到
        let empty = ArrivalReport {
            base_seq: 1,
            arrivals: vec![None, None],
        };
        let mut w = Writer::new();
        empty.encode(&mut w);
        let bytes = w.into_vec();
        assert_eq!(
            ArrivalReport::decode(&mut Reader::new(&bytes)).unwrap(),
            empty
        );
        // 太长、截断
        let mut w = Writer::new();
        w.u32(0)
            .u64(0)
            .u16(u16::try_from(MAX_REPORT_PACKETS + 1).unwrap());
        let bytes = w.into_vec();
        assert!(ArrivalReport::decode(&mut Reader::new(&bytes)).is_err());
        let mut w = Writer::new();
        w.u32(0).u64(0).u16(1).u8(1).u8(0x80);
        let bytes = w.into_vec();
        assert!(ArrivalReport::decode(&mut Reader::new(&bytes)).is_err());
        let mut w = Writer::new();
        w.u32(0).u64(0).u16(1).u8(1);
        for _ in 0..10 {
            w.u8(0xff);
        }
        let bytes = w.into_vec();
        assert!(
            ArrivalReport::decode(&mut Reader::new(&bytes)).is_err(),
            "变长整数太长"
        );
        for v in [0i64, 1, -1, 63, -64, i64::MAX / 4, i64::MIN / 4] {
            assert_eq!(unzigzag(zigzag(v)), v);
        }
    }

    #[test]
    fn recorder_reports_gaps_and_skips_late_packets() {
        let mut r = ArrivalRecorder::new();
        assert!(r.report().is_none(), "还没收到过");
        r.on_packet(Time(1000), 10);
        r.on_packet(Time(2000), 12);
        r.on_packet(Time(2000), 12); // 重复的只记一次
        assert!(r.has_pending());
        let rep = r.report().unwrap();
        assert_eq!(rep.base_seq, 10);
        assert_eq!(rep.arrivals, vec![Some(Time(1000)), None, Some(Time(2000))]);
        assert!(r.report().is_none(), "没有新包");
        // 11 迟到：已经报过“没收到”，不再报
        r.on_packet(Time(3000), 11);
        r.on_packet(Time(3000), 13);
        let rep = r.report().unwrap();
        assert_eq!(rep.base_seq, 13);
        assert_eq!(rep.arrivals, vec![Some(Time(3000))]);
        // 太多的分到下一份
        let mut big = ArrivalRecorder::new();
        let n = u32::try_from(MAX_REPORT_PACKETS).unwrap();
        for s in 0..n + 5 {
            big.on_packet(Time(u64::from(s)), s);
        }
        assert_eq!(big.report().unwrap().arrivals.len(), MAX_REPORT_PACKETS);
        let rest = big.report().unwrap();
        assert_eq!((rest.base_seq, rest.arrivals.len()), (n, 5));
        // 序号回绕
        let mut wrap = ArrivalRecorder::new();
        wrap.on_packet(Time(1), u32::MAX);
        wrap.on_packet(Time(2), 0);
        assert_eq!(wrap.report().unwrap().arrivals.len(), 2);
    }

    #[test]
    fn history_resolves_reports_and_tracks_in_flight() {
        let mut h = SendHistory::new();
        for seq in 0..4 {
            h.on_sent(sent(seq, u64::from(seq) * 1000));
        }
        assert_eq!(h.in_flight(), 4800);
        let res = h.resolve(&ArrivalReport {
            base_seq: 1,
            arrivals: vec![Some(Time(50_000)), None, Some(Time(52_000)), Some(Time(9))],
        });
        assert_eq!(res.len(), 3, "序号 4 不在历史里");
        assert_eq!(res[0].sent.seq, 1);
        assert_eq!(res[1].recv_time, None);
        assert_eq!(h.in_flight(), 1200, "报告过的不再算在途");
        // 太老的丢掉
        h.on_sent(sent(9, HISTORY_US + 10_000));
        assert_eq!(h.in_flight(), 1200, "序号 0 太老，换成 9");
        assert!(h
            .resolve(&ArrivalReport {
                base_seq: 0,
                arrivals: vec![Some(Time(0))]
            })
            .is_empty());
    }
}
