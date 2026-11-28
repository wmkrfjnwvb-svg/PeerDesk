//! “操作到画面”怎么量：帧上带着它包含到第几个输入，控制端等到第一幅带上这次操作的画面才算。
//!
//! 为什么（2026-11-28）：原来的算法是“发出操作之后显示的下一幅画面”。画面静止时被控端其实一直在出帧
//! （光标闪、时钟跳，每次变化后面跟着十几帧补清晰的小帧，实测每秒 13–18 帧），下一幅画面多半是操作生效
//! **之前**采的，量出来的数偏小，解释不了“手机上打字比 ToDesk 晚几毫秒”。量尺不对，先修量尺。
//!
//! 做法：
//! - 被控端（[`HostStamper`]）记着已经注入到第几个输入。每次发现画面变了，就把“这幅内容包含到第几个输入”
//!   定下来；画面没变的帧（补清晰等）内容还是原来那幅，戳也不变。带上新输入的第一帧编完时，
//!   发一条 [`FrameStamp`]（控制通道的数据报，按帧号对应），后面两帧再各重复一次防丢。
//!   戳只增不减，所以“帧号 ≥ F 的帧都包含到第 S 个输入”，没有操作时一个字节都不多发。
//! - 控制端（[`ReactionTracker`]）：操作 N 的延迟 = 发出 N 到显示出第一幅帧号 ≥ F 的画面，
//!   其中 F 是第一条 `inputs > N` 的戳的帧号。戳比画面晚到也能对上（留着最近显示的帧）。
//!
//! 量不到的地方（写清楚，不装作量到了）：
//! - 被控端不知道哪次变化是这次输入引起的：注入之后的第一次画面变化就算。输入生效之前恰好有别的变化
//!   （光标闪）时会读小；每秒几次变化、应用几十毫秒画出来，大约一成的样本受影响，所以看中位数。
//! - “发现画面变了”是会话循环取画面的时刻，不是系统真正采到的时刻（差最多一个采集间隔加一个取帧间隔）。
//! - 输入没有改变画面时永远等不到，超过 [`REACTION_LIMIT_US`] 记为“没等到画面”。
//! - “交给界面”之后到真正亮在屏幕上（合成、刷新）不在这里面。

use std::collections::VecDeque;

use crate::error::Error;
use crate::input::InputEvent;
use crate::time::Time;
use crate::wire::{Reader, Writer};

/// 戳的标记字节（控制通道的数据报靠第一个字节区分：反馈、[`crate::stats::TAG`]、这个）。
pub const TAG: u8 = 0xA6;
/// 操作之后这么久还没有带上它的画面，就不算这一次（多半是操作没有改变画面）。
pub const REACTION_LIMIT_US: u64 = 2_000_000;
/// 一条戳除了随它那一帧发一次，后面再跟着重复几帧（数据报会丢）。
const REPEATS: u8 = 2;
/// 控制端最多记这么多个等画面的操作、最近的戳、最近显示的帧。
const MAX_INPUTS: usize = 64;
const MAX_STAMPS: usize = 16;
const MAX_SHOWN: usize = 64;

/// 被控端发现带输入的画面变化时，编码器在干什么。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Busy {
    /// 空着：这一轮就能编。
    #[default]
    Idle,
    /// 正在编一帧画面没变的帧（补清晰、补关键帧、控制端窗口变了）。
    Unchanged,
    /// 正在编一帧画面变了的帧。
    Changed,
}

impl Busy {
    /// 由会话循环的状态得出：`encoding` 是编码器里有没有帧，`changed` 是那一帧的画面变没变。
    #[must_use]
    pub const fn of(encoding: bool, changed: bool) -> Self {
        match (encoding, changed) {
            (false, _) => Busy::Idle,
            (true, false) => Busy::Unchanged,
            (true, true) => Busy::Changed,
        }
    }

    const fn id(self) -> u8 {
        match self {
            Busy::Idle => 0,
            Busy::Unchanged => 1,
            Busy::Changed => 2,
        }
    }

    const fn from_id(id: u8) -> Result<Self, Error> {
        match id {
            0 => Ok(Busy::Idle),
            1 => Ok(Busy::Unchanged),
            2 => Ok(Busy::Changed),
            _ => Err(Error::Inconsistent("编码器状态只能是 0、1、2")),
        }
    }
}

/// 哪些输入算“操作”：按下键、按下鼠标键、打的字。两端用同一个定义。
/// 鼠标移动、滚轮、抬起不算：太密，而且多半不改变画面。
#[must_use]
pub const fn counts(event: &InputEvent) -> bool {
    match event {
        InputEvent::Key { down, .. } | InputEvent::MouseButton { down, .. } => *down,
        InputEvent::Text(_) => true,
        InputEvent::MouseMove { .. }
        | InputEvent::MouseRelative { .. }
        | InputEvent::Wheel { .. }
        | InputEvent::ReleaseAll => false,
    }
}

/// `inputs`（已经注入的个数，也就是下一个期望的序号）包不包含序号为 `seq` 的输入。序号会回绕，按差值判断。
#[must_use]
pub const fn includes(inputs: u32, seq: u32) -> bool {
    let ahead = inputs.wrapping_sub(seq);
    ahead != 0 && ahead <= u32::MAX / 2
}

/// 帧号 `frame` 是不是不早于 `first`（帧号会回绕）。
const fn not_before(frame: u32, first: u32) -> bool {
    frame.wrapping_sub(first) <= u32::MAX / 2
}

fn clamp(us: u64) -> u32 {
    u32::try_from(us).unwrap_or(u32::MAX)
}

/// 被控端随帧发的戳：从 `frame_id` 这一帧起，画面包含到第 `inputs` 个输入；外加被控端这一段的分解。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct FrameStamp {
    /// 第一帧带上这些输入的帧号。
    pub frame_id: u32,
    /// 已经注入的输入个数（序号小于它的都注入了）。
    pub inputs: u32,
    /// 发现画面变了的时候编码器在干什么。
    pub busy: Busy,
    /// 等编码器的时候有没有被“网络受限”（在途上限、发送节奏）挡过。
    pub gated: bool,
    /// 注入（这一帧第一次带上的输入里最早的那个）到发现画面变了（微秒）。
    pub inject_to_change_us: u32,
    /// 发现画面变了到交给编码器（微秒）：编码器空着、网络不挡时是 0。
    pub wait_us: u32,
    /// 交给编码器到编完（微秒）。
    pub encode_us: u32,
}

impl FrameStamp {
    /// 编码成数据报载荷。
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        let mut w = Writer::new();
        w.u8(TAG).u8(1).u32(self.frame_id).u32(self.inputs);
        w.u8(self.busy.id() | (u8::from(self.gated) << 2));
        w.u32(self.inject_to_change_us)
            .u32(self.wait_us)
            .u32(self.encode_us);
        w.into_vec()
    }

    /// 解码。标记不对或数据不完整时出错；末尾多出的字段忽略（以后可以往后加）。
    ///
    /// # Errors
    /// 见上。
    pub fn decode(buf: &[u8]) -> Result<Self, Error> {
        let mut r = Reader::new(buf);
        let tag = r.u8()?;
        if tag != TAG {
            return Err(Error::UnknownMessage(tag));
        }
        let _version = r.u8()?;
        let (frame_id, inputs, flags) = (r.u32()?, r.u32()?, r.u8()?);
        Ok(Self {
            frame_id,
            inputs,
            busy: Busy::from_id(flags & 0b11)?,
            gated: flags & 0b100 != 0,
            inject_to_change_us: r.u32()?,
            wait_us: r.u32()?,
            encode_us: r.u32()?,
        })
    }
}

/// 一组数的中位数（没有时为 0）。
fn median(v: &[u64]) -> u64 {
    let mut v = v.to_vec();
    v.sort_unstable();
    v.get(v.len() / 2).copied().unwrap_or(0)
}

/// 微秒写成毫秒，一位小数。
fn ms(us: u64) -> String {
    let tenths = us.saturating_add(50) / 100;
    format!("{}.{}", tenths / 10, tenths % 10)
}

/// 发现了带输入的画面变化，还没交给编码器。
#[derive(Debug, Clone, Copy)]
struct Pending {
    inputs: u32,
    inject_to_change_us: u64,
    changed_at: Time,
    busy: Busy,
    gated: bool,
}

/// 带输入的那一帧正在编码器里。
#[derive(Debug, Clone, Copy)]
struct Flying {
    frame_id: u32,
    pending: Pending,
    wait_us: u64,
    dispatched_at: Time,
}

/// 被控端这一秒的各次操作响应。
#[derive(Debug, Default)]
struct HostWindow {
    inject_to_change: Vec<u64>,
    wait: Vec<u64>,
    busy_wait: Vec<u64>,
    encode: Vec<u64>,
    unchanged: u32,
    changed: u32,
    gated: u32,
}

/// 被控端：给帧盖“包含到第几个输入”的戳，并量自己这一段（注入 → 画面变了 → 交给编码器 → 编完）。
///
/// 会话循环按发生的顺序调用：[`Self::injected`]（注入了输入）、[`Self::changed`]（这一轮取到的画面变了）、
/// [`Self::gated`]（想编但被网络挡住）、[`Self::dispatched`]（一帧交给了编码器）、[`Self::encoded`]（一帧编完）。
#[derive(Debug, Default)]
pub struct HostStamper {
    inputs: u32,
    /// 还没有被哪次画面变化带上的输入里，最早那个的注入时间。
    uncovered_since: Option<Time>,
    pending: Option<Pending>,
    flying: Option<Flying>,
    /// 最近一条戳和还要重复的次数。
    announce: Option<(FrameStamp, u8)>,
    window: HostWindow,
}

impl HostStamper {
    /// 创建。
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// 注入了算“操作”的输入（见 [`counts`]）；`next_expected` 是接收端下一个期望的序号（之前的都注入了）。
    pub fn injected(&mut self, now: Time, next_expected: u32) {
        self.inputs = next_expected;
        if self.uncovered_since.is_none() {
            self.uncovered_since = Some(now);
        }
    }

    /// 这一轮取到的画面和上一幅不一样。`busy` 是这时候编码器在干什么。
    /// 之前没有注入过新输入时什么也不做（这次变化和操作无关）。
    pub fn changed(&mut self, now: Time, busy: Busy) {
        let Some(since) = self.uncovered_since.take() else {
            return;
        };
        match self.pending.as_mut() {
            // 上一次带输入的变化还在等编码器：用户等的是最早那一次，时间照旧，只把新输入算进去
            Some(p) => p.inputs = self.inputs,
            None => {
                self.pending = Some(Pending {
                    inputs: self.inputs,
                    inject_to_change_us: now.since(since),
                    changed_at: now,
                    busy,
                    gated: false,
                });
            }
        }
    }

    /// 这一轮想编但被网络挡住了（在途上限、发送节奏）。没有等着的带输入的变化时什么也不做。
    pub const fn gated(&mut self) {
        if let Some(p) = self.pending.as_mut() {
            p.gated = true;
        }
    }

    /// 第 `frame_id` 帧交给了编码器（编的是此刻最新的画面，所以等着的变化就在这一帧里）。
    pub fn dispatched(&mut self, now: Time, frame_id: u32) {
        if let Some(pending) = self.pending.take() {
            self.flying = Some(Flying {
                frame_id,
                pending,
                wait_us: now.since(pending.changed_at),
                dispatched_at: now,
            });
        }
    }

    /// 第 `frame_id` 帧编完了。要随这一帧发戳时返回它（第一帧带上新输入的帧，以及之后重复的几帧）。
    pub fn encoded(&mut self, now: Time, frame_id: u32) -> Option<FrameStamp> {
        let Some(f) = self.flying.take_if(|f| f.frame_id == frame_id) else {
            let (stamp, left) = self.announce.as_mut()?;
            *left = left.checked_sub(1)?;
            return Some(*stamp);
        };
        let encode_us = now.since(f.dispatched_at);
        let w = &mut self.window;
        w.inject_to_change.push(f.pending.inject_to_change_us);
        w.wait.push(f.wait_us);
        w.encode.push(encode_us);
        match f.pending.busy {
            Busy::Idle => {}
            Busy::Unchanged => w.unchanged = w.unchanged.saturating_add(1),
            Busy::Changed => w.changed = w.changed.saturating_add(1),
        }
        if f.pending.gated {
            w.gated = w.gated.saturating_add(1);
        }
        if f.pending.busy != Busy::Idle || f.pending.gated {
            w.busy_wait.push(f.wait_us);
        }
        let stamp = FrameStamp {
            frame_id,
            inputs: f.pending.inputs,
            busy: f.pending.busy,
            gated: f.pending.gated,
            inject_to_change_us: clamp(f.pending.inject_to_change_us),
            wait_us: clamp(f.wait_us),
            encode_us: clamp(encode_us),
        };
        self.announce = Some((stamp, REPEATS));
        Some(stamp)
    }

    /// 这一秒的一行；这一秒没有带输入的帧编完时为 `None`。取完清零。
    pub fn take_line(&mut self) -> Option<String> {
        if self.window.encode.is_empty() {
            return None;
        }
        let w = std::mem::take(&mut self.window);
        Some(format!(
            "操作响应（被控端）{} 次，各段中位：注入→画面变了 {} ms，画面变了→交给编码器 {} ms，编码 {} ms；\
             画面变了的时候编码器正忙 {} 次（在编画面没变的帧 {} 次、在编变化的帧 {} 次），被网络受限挡住 {} 次，\
             这些次等了 {} ms",
            w.encode.len(),
            ms(median(&w.inject_to_change)),
            ms(median(&w.wait)),
            ms(median(&w.encode)),
            w.unchanged.saturating_add(w.changed),
            w.unchanged,
            w.changed,
            w.gated,
            ms(median(&w.busy_wait)),
        ))
    }
}

/// 控制端显示出来的一帧。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Shown {
    /// 帧号。
    pub frame_id: u32,
    /// 收到这一帧第一个分片的时间。
    pub first_packet: Time,
    /// 解码完（会话循环拿到画面）的时间。
    pub decoded: Time,
    /// 交给界面之后的时间。
    pub shown: Time,
}

/// 一次操作的响应：总时间和各段。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Reaction {
    /// 输入的序号。
    pub seq: u32,
    /// 发出操作到画面交给界面（微秒）。
    pub total_us: u64,
    /// 发出操作到收到那一帧的第一个分片（微秒）：上行、被控端（见 `host`）、下行。
    pub to_first_packet_us: u64,
    /// 第一个分片到解码完（微秒）：收齐、等解码线程、解码。
    pub to_decoded_us: u64,
    /// 解码完到交给界面（微秒）。
    pub to_shown_us: u64,
    /// 显示的是第几帧。
    pub frame_id: u32,
    /// 被控端这一段。
    pub host: FrameStamp,
}

/// 控制端：把发出的操作和带上它的第一幅画面对起来。
#[derive(Debug, Default)]
pub struct ReactionTracker {
    inputs: VecDeque<(u32, Time)>,
    stamps: VecDeque<FrameStamp>,
    shown: VecDeque<Shown>,
    /// 挤掉的操作（等画面的太多）；下次 [`Self::expire`] 一起报告。
    dropped: u32,
}

impl ReactionTracker {
    /// 创建。
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// 发出了序号为 `seq` 的操作。
    pub fn sent(&mut self, now: Time, seq: u32) {
        if self.inputs.len() >= MAX_INPUTS {
            self.inputs.pop_front();
            self.dropped = self.dropped.saturating_add(1);
        }
        self.inputs.push_back((seq, now));
    }

    /// 收到一条戳。返回因此对上的操作（戳比画面晚到时）。
    pub fn stamp(&mut self, stamp: FrameStamp) -> Vec<Reaction> {
        // 重复发来的同一条：已经有了
        if self
            .stamps
            .iter()
            .any(|s| (s.frame_id, s.inputs) == (stamp.frame_id, stamp.inputs))
        {
            return Vec::new();
        }
        if self.stamps.len() >= MAX_STAMPS {
            self.stamps.pop_front();
        }
        self.stamps.push_back(stamp);
        self.resolve()
    }

    /// 显示了一帧。返回因此对上的操作。
    pub fn shown(&mut self, frame: Shown) -> Vec<Reaction> {
        if self.shown.len() >= MAX_SHOWN {
            self.shown.pop_front();
        }
        self.shown.push_back(frame);
        self.resolve()
    }

    /// 丢掉等了太久的操作（见 [`REACTION_LIMIT_US`]），返回这次丢掉几个（含挤掉的）。
    pub fn expire(&mut self, now: Time) -> u32 {
        let mut gone = std::mem::take(&mut self.dropped);
        while self
            .inputs
            .front()
            .is_some_and(|(_, at)| now.since(*at) > REACTION_LIMIT_US)
        {
            self.inputs.pop_front();
            gone = gone.saturating_add(1);
        }
        gone
    }

    /// 还在等画面的操作数。
    #[must_use]
    pub fn waiting(&self) -> usize {
        self.inputs.len()
    }

    fn resolve(&mut self) -> Vec<Reaction> {
        let mut done = Vec::new();
        let (stamps, shown) = (&self.stamps, &self.shown);
        self.inputs.retain(|(seq, at)| {
            // 第一条包含这个操作的戳，和帧号不早于它的第一幅显示出来的画面
            let hit = stamps
                .iter()
                .find(|s| includes(s.inputs, *seq))
                .and_then(|s| {
                    shown
                        .iter()
                        .find(|f| not_before(f.frame_id, s.frame_id))
                        .map(|f| (s, f))
                });
            let Some((stamp, frame)) = hit else {
                return true;
            };
            done.push(Reaction {
                seq: *seq,
                total_us: frame.shown.since(*at),
                to_first_packet_us: frame.first_packet.since(*at),
                to_decoded_us: frame.decoded.since(frame.first_packet),
                to_shown_us: frame.shown.since(frame.decoded),
                frame_id: frame.frame_id,
                host: *stamp,
            });
            false
        });
        done
    }
}

/// 控制端这一秒的操作响应：新算法的各次，和旧算法（操作后的下一幅画面）的各次，放在一起对照。
#[derive(Debug, Default)]
pub struct ReactionWindow {
    reactions: Vec<Reaction>,
    totals: Vec<u64>,
    next_frame: Vec<u64>,
    unanswered: u32,
}

impl ReactionWindow {
    /// 记一次对上的操作。
    pub fn add(&mut self, r: Reaction) {
        self.totals.push(r.total_us);
        self.reactions.push(r);
    }

    /// 记一次旧算法的结果（微秒）。
    pub fn add_next_frame(&mut self, us: u64) {
        self.next_frame.push(us);
    }

    /// 记没等到画面的操作数。
    pub const fn add_unanswered(&mut self, n: u32) {
        self.unanswered = self.unanswered.saturating_add(n);
    }

    /// 新算法这一秒各次的总时间（微秒）。
    #[must_use]
    pub fn totals(&self) -> &[u64] {
        &self.totals
    }

    /// 旧算法这一秒各次的时间（微秒）。
    #[must_use]
    pub fn next_frame(&self) -> &[u64] {
        &self.next_frame
    }

    /// 这一秒的一行；什么都没有时为 `None`。
    #[must_use]
    pub fn line(&self) -> Option<String> {
        if self.reactions.is_empty() && self.unanswered == 0 {
            return None;
        }
        let col = |f: fn(&Reaction) -> u64| -> String {
            ms(median(&self.reactions.iter().map(f).collect::<Vec<_>>()))
        };
        let busy = |b: Busy| self.reactions.iter().filter(|r| r.host.busy == b).count();
        Some(format!(
            "操作响应 {} 次（发出操作到显示第一幅带上它的画面）：中位 {} ms，最大 {} ms；各段中位：\
             发出→收到首包 {} ms（其中被控端 注入→画面变了 {} ms、等编码器 {} ms、编码 {} ms），\
             收到→解码完 {} ms，解码完→交给界面 {} ms；被控端画面变了的时候编码器正忙 {} 次\
             （其中在编画面没变的帧 {} 次）；旧算法（操作后的下一幅画面）{} 次 中位 {} ms；没等到画面 {} 次",
            self.reactions.len(),
            ms(median(&self.totals)),
            ms(self.totals.iter().copied().max().unwrap_or(0)),
            col(|r| r.to_first_packet_us),
            col(|r| u64::from(r.host.inject_to_change_us)),
            col(|r| u64::from(r.host.wait_us)),
            col(|r| u64::from(r.host.encode_us)),
            col(|r| r.to_decoded_us),
            col(|r| r.to_shown_us),
            busy(Busy::Unchanged) + busy(Busy::Changed),
            busy(Busy::Unchanged),
            self.next_frame.len(),
            ms(median(&self.next_frame)),
            self.unanswered,
        ))
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::indexing_slicing)]
    use super::*;

    const fn t(ms: u64) -> Time {
        Time::from_millis(ms)
    }

    const fn frame(frame_id: u32, first: u64, decoded: u64, shown: u64) -> Shown {
        Shown {
            frame_id,
            first_packet: t(first),
            decoded: t(decoded),
            shown: t(shown),
        }
    }

    #[test]
    fn stamp_roundtrip_and_tolerance() {
        let s = FrameStamp {
            frame_id: 77,
            inputs: 9,
            busy: Busy::Unchanged,
            gated: true,
            inject_to_change_us: 12_000,
            wait_us: 9_000,
            encode_us: 11_000,
        };
        let mut bytes = s.encode();
        assert_eq!(FrameStamp::decode(&bytes).unwrap(), s);
        // 以后加的字段：老版本忽略
        bytes.extend_from_slice(&[1, 2, 3]);
        assert_eq!(FrameStamp::decode(&bytes).unwrap(), s);
        for busy in [Busy::Idle, Busy::Changed] {
            let s = FrameStamp { busy, ..s };
            assert_eq!(FrameStamp::decode(&s.encode()).unwrap(), s);
        }
        // 不是戳、不完整、状态不认识
        assert!(FrameStamp::decode(&[crate::stats::TAG, 1]).is_err());
        assert!(FrameStamp::decode(&bytes[..12]).is_err());
        bytes[10] = 0b11;
        assert!(FrameStamp::decode(&bytes).is_err());
        assert_eq!(clamp(u64::MAX), u32::MAX);
        assert_eq!(
            [
                Busy::of(false, true),
                Busy::of(true, false),
                Busy::of(true, true)
            ],
            [Busy::Idle, Busy::Unchanged, Busy::Changed]
        );
    }

    #[test]
    fn presses_and_text_count_as_operations() {
        let yes = [
            InputEvent::Key {
                code: 4,
                down: true,
            },
            InputEvent::MouseButton {
                button: 0,
                down: true,
            },
            InputEvent::Text("好".into()),
        ];
        let no = [
            InputEvent::Key {
                code: 4,
                down: false,
            },
            InputEvent::MouseButton {
                button: 0,
                down: false,
            },
            InputEvent::MouseMove { x: 1, y: 2 },
            InputEvent::MouseRelative { dx: 1, dy: 2 },
            InputEvent::Wheel { dx: 0, dy: 1 },
            InputEvent::ReleaseAll,
        ];
        assert!(yes.iter().all(counts) && !no.iter().any(counts));
    }

    #[test]
    fn sequence_and_frame_order_survive_wraparound() {
        assert!(includes(1, 0) && includes(5, 4) && includes(5, 0));
        assert!(!includes(0, 0) && !includes(4, 4) && !includes(4, 5));
        assert!(includes(2, u32::MAX), "序号回绕");
        assert!(!includes(u32::MAX, 2));
        assert!(not_before(7, 7) && not_before(8, 7) && !not_before(6, 7));
        assert!(not_before(1, u32::MAX) && !not_before(u32::MAX, 1));
    }

    /// 被控端：画面没变的帧不带新输入；第一帧带上输入的帧编完时出戳，之后重复两帧。
    #[test]
    fn host_stamps_the_first_frame_whose_content_changed_after_injection() {
        let mut h = HostStamper::new();
        // 和操作无关的变化：不出戳
        h.changed(t(0), Busy::Idle);
        h.dispatched(t(0), 1);
        assert_eq!(h.encoded(t(8), 1), None);
        assert_eq!(h.take_line(), None);
        // 注入两个输入；之后几帧画面没变（补清晰）：还是不出戳
        h.dispatched(t(10), 2);
        h.injected(t(12), 1);
        h.injected(t(13), 2);
        assert_eq!(h.encoded(t(20), 2), None);
        h.dispatched(t(20), 3);
        // 画面变了，这时编码器正在编第 3 帧（画面没变的帧）：等它
        h.changed(t(30), Busy::Unchanged);
        assert_eq!(h.encoded(t(34), 3), None);
        h.dispatched(t(36), 4);
        let stamp = h.encoded(t(47), 4).unwrap();
        assert_eq!(
            stamp,
            FrameStamp {
                frame_id: 4,
                inputs: 2,
                busy: Busy::Unchanged,
                gated: false,
                inject_to_change_us: 18_000,
                wait_us: 6_000,
                encode_us: 11_000,
            }
        );
        // 后面两帧重复同一条，第三帧起不再发
        h.dispatched(t(50), 5);
        assert_eq!(h.encoded(t(55), 5), Some(stamp));
        assert_eq!(h.encoded(t(60), 6), Some(stamp));
        assert_eq!(h.encoded(t(65), 7), None);
        let line = h.take_line().unwrap();
        assert!(
            line.contains("操作响应（被控端）1 次")
                && line.contains("注入→画面变了 18.0 ms")
                && line.contains("画面变了→交给编码器 6.0 ms")
                && line.contains("编码 11.0 ms")
                && line.contains("正忙 1 次（在编画面没变的帧 1 次、在编变化的帧 0 次）")
                && line.contains("挡住 0 次，这些次等了 6.0 ms"),
            "{line}"
        );
        assert_eq!(h.take_line(), None, "取完清零");
    }

    /// 被控端：等编码器的时候又来了输入和变化，时间按最早那次算，输入算到最新；网络挡住也记下。
    #[test]
    fn host_merges_changes_that_wait_for_the_encoder() {
        let mut h = HostStamper::new();
        h.gated();
        h.dispatched(t(0), 1);
        h.injected(t(1), 1);
        h.changed(t(5), Busy::Changed);
        h.gated();
        h.injected(t(6), 2);
        h.changed(t(9), Busy::Idle);
        assert_eq!(h.encoded(t(10), 1), None);
        h.dispatched(t(12), 2);
        let stamp = h.encoded(t(20), 2).unwrap();
        assert_eq!(
            (stamp.inputs, stamp.busy, stamp.gated),
            (2, Busy::Changed, true)
        );
        assert_eq!(
            (stamp.inject_to_change_us, stamp.wait_us, stamp.encode_us),
            (4_000, 7_000, 8_000)
        );
        // 编码器空着：同一轮就交出去，不用等
        h.injected(t(30), 3);
        h.changed(t(31), Busy::Idle);
        h.dispatched(t(31), 3);
        let stamp = h.encoded(t(40), 3).unwrap();
        assert_eq!(
            (stamp.inputs, stamp.busy, stamp.wait_us),
            (3, Busy::Idle, 0)
        );
        let line = h.take_line().unwrap();
        assert!(
            line.contains("2 次")
                && line.contains("在编变化的帧 1 次")
                && line.contains("挡住 1 次"),
            "{line}"
        );
    }

    /// 控制端：画面没变的帧在流的时候，操作只在第一幅带上它的画面那里算。
    #[test]
    fn controller_waits_for_the_first_frame_that_includes_the_input() {
        let mut c = ReactionTracker::new();
        c.sent(t(100), 0);
        // 操作之后马上显示了两幅之前采的画面（补清晰的帧）：旧算法在这里就算了，新算法不算
        assert!(c.shown(frame(10, 101, 103, 104)).is_empty());
        assert!(c.shown(frame(11, 117, 119, 120)).is_empty());
        // 第 12 帧才带上这次操作；戳先到，画面后到
        let stamp = FrameStamp {
            frame_id: 12,
            inputs: 1,
            busy: Busy::Unchanged,
            wait_us: 9_000,
            ..FrameStamp::default()
        };
        assert!(c.stamp(stamp).is_empty());
        assert!(c.stamp(stamp).is_empty(), "重复的戳");
        assert_eq!(c.waiting(), 1);
        let done = c.shown(frame(12, 140, 146, 147));
        assert_eq!(
            done,
            vec![Reaction {
                seq: 0,
                total_us: 47_000,
                to_first_packet_us: 40_000,
                to_decoded_us: 6_000,
                to_shown_us: 1_000,
                frame_id: 12,
                host: stamp,
            }]
        );
        assert_eq!(c.waiting(), 0);
        // 同一幅画面不会再算一次
        assert!(c.shown(frame(13, 150, 151, 152)).is_empty());
    }

    /// 控制端：戳比画面晚到、带戳的那一帧丢了、一幅画面带上几个操作。
    #[test]
    fn controller_matches_late_stamps_lost_frames_and_batches() {
        let mut c = ReactionTracker::new();
        c.sent(t(0), 5);
        c.sent(t(4), 6);
        c.sent(t(8), 7);
        // 第 20 帧带上 5、6（inputs = 7），但它丢了；第 21 帧先显示，戳后到
        assert!(c.shown(frame(19, 1, 2, 3)).is_empty());
        assert!(c.shown(frame(21, 30, 34, 35)).is_empty());
        let done = c.stamp(FrameStamp {
            frame_id: 20,
            inputs: 7,
            ..FrameStamp::default()
        });
        assert_eq!(
            done.iter()
                .map(|r| (r.seq, r.total_us, r.frame_id))
                .collect::<Vec<_>>(),
            vec![(5, 35_000, 21), (6, 31_000, 21)]
        );
        // 7 还在等：下一条戳对应的帧显示时才算
        assert_eq!(c.waiting(), 1);
        c.stamp(FrameStamp {
            frame_id: 25,
            inputs: 8,
            ..FrameStamp::default()
        });
        assert!(c.shown(frame(24, 50, 51, 52)).is_empty());
        let done = c.shown(frame(25, 60, 61, 62));
        assert_eq!((done[0].seq, done[0].total_us), (7, 54_000));
    }

    /// 控制端：没改变画面的操作超时丢掉；等的太多时挤掉最早的；戳和帧的记录有上限。
    #[test]
    fn controller_gives_up_on_inputs_that_never_show() {
        let mut c = ReactionTracker::new();
        c.sent(t(0), 0);
        c.sent(t(1500), 1);
        assert_eq!(c.expire(t(2000)), 0);
        assert_eq!(c.expire(t(2001)), 1);
        assert_eq!(c.waiting(), 1);
        for seq in 2..=u32::try_from(MAX_INPUTS).unwrap() + 2 {
            c.sent(t(1600), seq);
        }
        assert_eq!(c.waiting(), MAX_INPUTS);
        assert_eq!(c.expire(t(1700)), 2, "挤掉的也算没等到");
        for i in 0..100 {
            c.stamp(FrameStamp {
                frame_id: 1000 + i,
                inputs: 0,
                ..FrameStamp::default()
            });
            c.shown(frame(i, 0, 0, 0));
        }
        assert_eq!((c.stamps.len(), c.shown.len()), (MAX_STAMPS, MAX_SHOWN));
    }

    #[test]
    fn window_line_puts_the_new_and_old_numbers_side_by_side() {
        let mut w = ReactionWindow::default();
        assert_eq!(w.line(), None);
        w.add_unanswered(2);
        assert!(
            w.line().unwrap().contains("操作响应 0 次")
                && w.line().unwrap().contains("没等到画面 2 次")
        );
        let reaction = |total_us: u64, busy: Busy| Reaction {
            seq: 0,
            total_us,
            to_first_packet_us: total_us - 7_000,
            to_decoded_us: 6_000,
            to_shown_us: 1_000,
            frame_id: 1,
            host: FrameStamp {
                busy,
                inject_to_change_us: 20_000,
                wait_us: 9_000,
                encode_us: 11_000,
                ..FrameStamp::default()
            },
        };
        w.add(reaction(60_000, Busy::Unchanged));
        w.add(reaction(90_000, Busy::Changed));
        w.add(reaction(70_000, Busy::Idle));
        w.add_next_frame(12_000);
        assert_eq!(w.totals(), &[60_000, 90_000, 70_000]);
        assert_eq!(w.next_frame(), &[12_000]);
        let line = w.line().unwrap();
        assert!(
            line.contains("操作响应 3 次")
                && line.contains("中位 70.0 ms，最大 90.0 ms")
                && line.contains("发出→收到首包 63.0 ms")
                && line.contains("注入→画面变了 20.0 ms、等编码器 9.0 ms、编码 11.0 ms")
                && line.contains("收到→解码完 6.0 ms，解码完→交给界面 1.0 ms")
                && line.contains("正忙 2 次（其中在编画面没变的帧 1 次）")
                && line.contains("旧算法（操作后的下一幅画面）1 次 中位 12.0 ms"),
            "{line}"
        );
    }
}
