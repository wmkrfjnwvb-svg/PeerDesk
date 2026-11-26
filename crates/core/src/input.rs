//! 输入事件及其在不可靠通道上的传输。
//!
//! 输入走数据报，可能丢失。每个包携带最近若干个尚未确认的事件，丢一两个包不影响；
//! 如果丢失超过了冗余能覆盖的范围，接收端先释放所有按键再继续，避免“按键卡住”。

use std::collections::{BTreeSet, VecDeque};

use crate::wire::{Reader, Writer};
use crate::Error;

/// 文本事件的最大长度（字节）。
pub const MAX_TEXT_LEN: usize = 128;
/// 一个输入包最多携带的事件数。
pub const MAX_EVENTS_PER_PACKET: usize = 32;

/// 一个输入事件。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InputEvent {
    /// 按键。`code` 是 USB HID 用途码，表示物理键位，与键盘布局无关。
    Key {
        /// 物理键位。
        code: u16,
        /// 按下为 true，抬起为 false。
        down: bool,
    },
    /// 鼠标移到绝对位置，坐标按被控端屏幕归一化到 0..=65535。
    MouseMove {
        /// 横坐标。
        x: u16,
        /// 纵坐标。
        y: u16,
    },
    /// 鼠标相对移动（被控端程序锁定光标时使用）。
    MouseRelative {
        /// 横向位移。
        dx: i16,
        /// 纵向位移。
        dy: i16,
    },
    /// 鼠标按键。
    MouseButton {
        /// 按键编号：0 左键，1 右键，2 中键，3 及以上为侧键。
        button: u8,
        /// 按下为 true，抬起为 false。
        down: bool,
    },
    /// 滚轮，高精度增量。
    Wheel {
        /// 横向。
        dx: i16,
        /// 纵向。
        dy: i16,
    },
    /// 直接输入一段文本（控制端输入法上屏的内容）。
    Text(String),
    /// 释放所有按下的键和鼠标按键。
    ReleaseAll,
    /// 推迟执行的粘贴：原子地按一次用户当时按的那个组合键（例如 Ctrl+V）。
    ///
    /// 控制端按粘贴时刚复制的内容还没到被控端，这一下按键被扣住，内容到了再用这个事件补上。
    /// 这时用户手上的修饰键可能已经松开、也可能按着别的键，所以由被控端一次做完：
    /// 先抬起现在按着的其他修饰键，按下 `modifiers`，敲一下 `code`，再恢复原样（见 [`KeyState::paste`]）。
    /// 只发给声明了 [`Caps::CLIPBOARD_LAZY`](crate::proto::Caps::CLIPBOARD_LAZY) 的对端。
    Paste {
        /// 要按着的修饰键：第 i 位对应 HID 用途码 `0xE0 + i`（和 HID 键盘报告的修饰键字节一样）。
        modifiers: u8,
        /// 要敲的键（V 或 Insert）。
        code: u16,
    },
}

impl InputEvent {
    fn encode(&self, w: &mut Writer) -> Result<(), Error> {
        match self {
            InputEvent::Key { code, down } => {
                w.u8(1).u16(*code).u8(u8::from(*down));
            }
            InputEvent::MouseMove { x, y } => {
                w.u8(2).u16(*x).u16(*y);
            }
            InputEvent::MouseRelative { dx, dy } => {
                w.u8(3).bytes(&dx.to_be_bytes()).bytes(&dy.to_be_bytes());
            }
            InputEvent::MouseButton { button, down } => {
                w.u8(4).u8(*button).u8(u8::from(*down));
            }
            InputEvent::Wheel { dx, dy } => {
                w.u8(5).bytes(&dx.to_be_bytes()).bytes(&dy.to_be_bytes());
            }
            InputEvent::Text(text) => {
                w.u8(6);
                w.string(text, MAX_TEXT_LEN)?;
            }
            InputEvent::ReleaseAll => {
                w.u8(7);
            }
            InputEvent::Paste { modifiers, code } => {
                w.u8(8).u8(*modifiers).u16(*code);
            }
        }
        Ok(())
    }

    fn decode(r: &mut Reader<'_>) -> Result<Self, Error> {
        let flag = |v: u8| match v {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(Error::Inconsistent("按下标志只能是 0 或 1")),
        };
        let i16_of = |r: &mut Reader<'_>| r.u16().map(|v| i16::from_be_bytes(v.to_be_bytes()));
        Ok(match r.u8()? {
            1 => InputEvent::Key {
                code: r.u16()?,
                down: flag(r.u8()?)?,
            },
            2 => InputEvent::MouseMove {
                x: r.u16()?,
                y: r.u16()?,
            },
            3 => InputEvent::MouseRelative {
                dx: i16_of(r)?,
                dy: i16_of(r)?,
            },
            4 => InputEvent::MouseButton {
                button: r.u8()?,
                down: flag(r.u8()?)?,
            },
            5 => InputEvent::Wheel {
                dx: i16_of(r)?,
                dy: i16_of(r)?,
            },
            6 => InputEvent::Text(r.string(MAX_TEXT_LEN)?),
            7 => InputEvent::ReleaseAll,
            8 => InputEvent::Paste {
                modifiers: r.u8()?,
                code: r.u16()?,
            },
            other => return Err(Error::UnknownMessage(other)),
        })
    }
}

/// 控制端：给事件编号，并把尚未确认的事件打进每个包里。
#[derive(Debug, Clone)]
pub struct InputSender {
    next_seq: u32,
    unacked: VecDeque<(u32, InputEvent)>,
    /// 队列里从这个下标开始的事件还没有发出过。
    unsent_from: usize,
}

impl Default for InputSender {
    fn default() -> Self {
        Self::new()
    }
}

impl InputSender {
    /// 创建。
    #[must_use]
    pub const fn new() -> Self {
        Self {
            next_seq: 0,
            unacked: VecDeque::new(),
            unsent_from: 0,
        }
    }

    /// 加入一个事件。连续的、还没发出的绝对鼠标移动会合并成最后一个位置。
    pub fn push(&mut self, event: InputEvent) {
        let is_move = |e: &InputEvent| match e {
            InputEvent::MouseMove { .. } => true,
            InputEvent::Key { .. }
            | InputEvent::MouseRelative { .. }
            | InputEvent::MouseButton { .. }
            | InputEvent::Wheel { .. }
            | InputEvent::Text(_)
            | InputEvent::ReleaseAll
            | InputEvent::Paste { .. } => false,
        };
        if is_move(&event) && self.unacked.len() > self.unsent_from {
            if let Some((_, last)) = self.unacked.back_mut() {
                if is_move(last) {
                    *last = event;
                    return;
                }
            }
        }
        // 队列有上限：对端长时间不确认时丢弃最旧的事件，接收端会因序号跳变而释放所有按键。
        if self.unacked.len() >= MAX_EVENTS_PER_PACKET {
            self.unacked.pop_front();
            self.unsent_from = self.unsent_from.saturating_sub(1);
        }
        self.unacked.push_back((self.next_seq, event));
        self.next_seq = self.next_seq.wrapping_add(1);
    }

    /// 是否有还没发出过的事件。
    #[must_use]
    pub fn has_unsent(&self) -> bool {
        self.unacked.len() > self.unsent_from
    }

    /// 尚未确认的事件数。
    #[must_use]
    pub fn pending(&self) -> usize {
        self.unacked.len()
    }

    /// 生成一个包，包含所有尚未确认的事件；没有时返回 `None`。
    pub fn packet(&mut self) -> Result<Option<Vec<u8>>, Error> {
        let Some((base, _)) = self.unacked.front() else {
            return Ok(None);
        };
        let mut w = Writer::new();
        w.u32(*base);
        w.u8(u8::try_from(self.unacked.len()).unwrap_or(u8::MAX));
        for (_, event) in &self.unacked {
            event.encode(&mut w)?;
        }
        self.unsent_from = self.unacked.len();
        Ok(Some(w.into_vec()))
    }

    /// 对端确认已处理到 `next_expected` 之前的所有事件。
    pub fn ack(&mut self, next_expected: u32) {
        while let Some((seq, _)) = self.unacked.front() {
            // 序号会回绕：用差值的符号位判断先后。
            if next_expected.wrapping_sub(*seq) == 0
                || next_expected.wrapping_sub(*seq) > u32::MAX / 2
            {
                break;
            }
            self.unacked.pop_front();
            self.unsent_from = self.unsent_from.saturating_sub(1);
        }
    }
}

/// 被控端：去重、保序，并在出现无法弥补的丢失时要求释放所有按键。
#[derive(Debug, Clone, Default)]
pub struct InputReceiver {
    next_expected: u32,
}

impl InputReceiver {
    /// 创建。
    #[must_use]
    pub const fn new() -> Self {
        Self { next_expected: 0 }
    }

    /// 下一个期望的序号，也就是要回给控制端的确认值。
    #[must_use]
    pub const fn next_expected(&self) -> u32 {
        self.next_expected
    }

    /// 处理一个包，返回其中新的事件，按顺序排列。
    pub fn on_packet(&mut self, bytes: &[u8]) -> Result<Vec<InputEvent>, Error> {
        let mut r = Reader::new(bytes);
        let base = r.u32()?;
        let count = usize::from(r.u8()?);
        if count > MAX_EVENTS_PER_PACKET {
            return Err(Error::TooManyItems {
                len: count,
                max: MAX_EVENTS_PER_PACKET,
            });
        }
        let mut events = Vec::with_capacity(count);
        for _ in 0..count {
            events.push(InputEvent::decode(&mut r)?);
        }
        r.finish()?;

        let mut out = Vec::new();
        let ahead = base.wrapping_sub(self.next_expected);
        if ahead != 0 && ahead <= u32::MAX / 2 {
            // 中间有事件永久丢失（可能包含按键抬起）：先全部释放，保证不会卡键。
            out.push(InputEvent::ReleaseAll);
            self.next_expected = base;
        }
        for (event, offset) in events.into_iter().zip(0u32..) {
            let seq = base.wrapping_add(offset);
            if seq == self.next_expected {
                out.push(event);
                self.next_expected = self.next_expected.wrapping_add(1);
            }
        }
        Ok(out)
    }
}

/// 修饰键的 HID 用途码范围：左 Ctrl、左 Shift、左 Alt、左 GUI、右 Ctrl、右 Shift、右 Alt、右 GUI。
pub const MODIFIER_FIRST: u16 = 0xE0;
/// 见 [`MODIFIER_FIRST`]。
pub const MODIFIER_LAST: u16 = 0xE7;

/// 被控端记录哪些键和鼠标按键处于按下状态，用于断线或失焦时全部释放。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct KeyState {
    keys: BTreeSet<u16>,
    buttons: BTreeSet<u8>,
}

impl KeyState {
    /// 创建。
    #[must_use]
    pub const fn new() -> Self {
        Self {
            keys: BTreeSet::new(),
            buttons: BTreeSet::new(),
        }
    }

    /// 根据一个即将注入的事件更新状态。
    pub fn apply(&mut self, event: &InputEvent) {
        match event {
            InputEvent::Key { code, down: true } => {
                self.keys.insert(*code);
            }
            InputEvent::Key { code, down: false } => {
                self.keys.remove(code);
            }
            InputEvent::MouseButton { button, down: true } => {
                self.buttons.insert(*button);
            }
            InputEvent::MouseButton {
                button,
                down: false,
            } => {
                self.buttons.remove(button);
            }
            InputEvent::ReleaseAll => {
                self.keys.clear();
                self.buttons.clear();
            }
            // 粘贴由 [`KeyState::paste`] 展开成按键再记，本身不改状态
            InputEvent::MouseMove { .. }
            | InputEvent::MouseRelative { .. }
            | InputEvent::Wheel { .. }
            | InputEvent::Text(_)
            | InputEvent::Paste { .. } => {}
        }
    }

    /// 当前是否没有任何键或按键处于按下状态。
    #[must_use]
    pub fn is_idle(&self) -> bool {
        self.keys.is_empty() && self.buttons.is_empty()
    }

    /// 把 [`InputEvent::Paste`] 展开成一串按键：抬起现在按着、组合键里没有的修饰键，按下组合键里
    /// 还没按着的修饰键，敲一下 `code`，再把修饰键恢复成原来的样子。状态不变（前后按着的键一样）。
    ///
    /// 例子：用户按 Ctrl+V 后已经松手，接着按住 Shift 选东西，这时粘贴到了：
    /// 抬起 Shift → 按下 Ctrl → V 按下、抬起 → 抬起 Ctrl → 按下 Shift。
    #[must_use]
    pub fn paste(&self, modifiers: u8, code: u16) -> Vec<InputEvent> {
        let key = |code, down| InputEvent::Key { code, down };
        let wanted = |m: u16| modifiers & (1u8 << (m - MODIFIER_FIRST)) != 0;
        let all = MODIFIER_FIRST..=MODIFIER_LAST;
        let lift: Vec<u16> = all
            .clone()
            .filter(|m| self.keys.contains(m) && !wanted(*m))
            .collect();
        let press: Vec<u16> = all
            .filter(|m| !self.keys.contains(m) && wanted(*m))
            .collect();
        let mut out: Vec<InputEvent> = lift.iter().map(|m| key(*m, false)).collect();
        out.extend(press.iter().map(|m| key(*m, true)));
        // 这个键本来就按着（不应该发生：控制端扣住了它的按下）：先抬起，不然系统只当它是重复
        if self.keys.contains(&code) {
            out.push(key(code, false));
        }
        out.push(key(code, true));
        out.push(key(code, false));
        out.extend(press.iter().rev().map(|m| key(*m, false)));
        out.extend(lift.iter().rev().map(|m| key(*m, true)));
        if self.keys.contains(&code) {
            out.push(key(code, true));
        }
        out
    }

    /// 生成把所有按下的键和按键抬起的事件，并清空状态。
    pub fn release_all(&mut self) -> Vec<InputEvent> {
        let keys = std::mem::take(&mut self.keys);
        let buttons = std::mem::take(&mut self.buttons);
        keys.into_iter()
            .map(|code| InputEvent::Key { code, down: false })
            .chain(buttons.into_iter().map(|button| InputEvent::MouseButton {
                button,
                down: false,
            }))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: u16, down: bool) -> InputEvent {
        InputEvent::Key { code, down }
    }

    fn all_kinds() -> Vec<InputEvent> {
        vec![
            key(4, true),
            key(4, false),
            InputEvent::MouseMove { x: 0, y: u16::MAX },
            InputEvent::MouseRelative {
                dx: -5,
                dy: i16::MIN,
            },
            InputEvent::MouseButton {
                button: 2,
                down: true,
            },
            InputEvent::Wheel {
                dx: i16::MAX,
                dy: -1,
            },
            InputEvent::Text("你好 hello".into()),
            InputEvent::ReleaseAll,
            InputEvent::Paste {
                modifiers: 0b0001_0001,
                code: 0x19,
            },
        ]
    }

    #[test]
    fn every_event_kind_roundtrips_in_order() {
        let mut tx = InputSender::new();
        let mut rx = InputReceiver::new();
        assert_eq!(tx.packet().unwrap(), None);
        for e in all_kinds() {
            tx.push(e);
        }
        assert!(tx.has_unsent());
        let packet = tx.packet().unwrap().unwrap();
        assert!(!tx.has_unsent());
        assert_eq!(rx.on_packet(&packet).unwrap(), all_kinds());
        assert_eq!(rx.next_expected(), 9);
        // 同一个包再来一次：全部是重复的
        assert!(rx.on_packet(&packet).unwrap().is_empty());
        tx.ack(rx.next_expected());
        assert_eq!(tx.pending(), 0);
        assert_eq!(tx.packet().unwrap(), None);
    }

    #[test]
    fn redundancy_covers_a_lost_packet() {
        let mut tx = InputSender::default();
        let mut rx = InputReceiver::default();
        tx.push(key(1, true));
        let _lost = tx.packet().unwrap().unwrap();
        tx.push(key(1, false));
        let second = tx.packet().unwrap().unwrap();
        // 第一个包丢了，第二个包里带着两个事件
        assert_eq!(
            rx.on_packet(&second).unwrap(),
            vec![key(1, true), key(1, false)]
        );
    }

    #[test]
    fn partial_overlap_delivers_only_new_events() {
        let mut tx = InputSender::new();
        let mut rx = InputReceiver::new();
        tx.push(key(1, true));
        rx.on_packet(&tx.packet().unwrap().unwrap()).unwrap();
        tx.push(key(2, true));
        assert_eq!(
            rx.on_packet(&tx.packet().unwrap().unwrap()).unwrap(),
            vec![key(2, true)]
        );
        // 迟到的确认不会误删还没确认的事件
        tx.ack(1);
        assert_eq!(tx.pending(), 1);
        tx.ack(0);
        assert_eq!(tx.pending(), 1);
        tx.ack(2);
        assert_eq!(tx.pending(), 0);
    }

    #[test]
    fn unrecoverable_gap_releases_everything_first() {
        let mut rx = InputReceiver::new();
        let mut tx = InputSender::new();
        tx.push(key(1, true));
        rx.on_packet(&tx.packet().unwrap().unwrap()).unwrap();
        tx.ack(1);
        // 序号 1（按键抬起）永久丢失：发送端已经把它从队列里丢掉了
        tx.push(key(1, false));
        tx.ack(2);
        tx.push(key(9, true));
        let packet = tx.packet().unwrap().unwrap();
        assert_eq!(
            rx.on_packet(&packet).unwrap(),
            vec![InputEvent::ReleaseAll, key(9, true)]
        );
        assert_eq!(rx.next_expected(), 3);
    }

    #[test]
    fn unsent_mouse_moves_coalesce_but_sent_ones_do_not() {
        let mut tx = InputSender::new();
        tx.push(InputEvent::MouseMove { x: 1, y: 1 });
        tx.push(InputEvent::MouseMove { x: 2, y: 2 });
        tx.push(InputEvent::MouseMove { x: 3, y: 3 });
        assert_eq!(tx.pending(), 1);
        let mut rx = InputReceiver::new();
        assert_eq!(
            rx.on_packet(&tx.packet().unwrap().unwrap()).unwrap(),
            vec![InputEvent::MouseMove { x: 3, y: 3 }]
        );
        // 已经发出的移动不能再被改写，否则接收端会因序号相同而忽略新位置
        tx.push(InputEvent::MouseMove { x: 4, y: 4 });
        assert_eq!(tx.pending(), 2);
        // 中间隔了别的事件也不合并
        tx.push(key(1, true));
        tx.push(InputEvent::MouseMove { x: 5, y: 5 });
        assert_eq!(tx.pending(), 4);
    }

    #[test]
    fn queue_is_bounded_when_peer_never_acks() {
        let mut tx = InputSender::new();
        let mut rx = InputReceiver::new();
        for i in 0..100 {
            tx.push(key(i, true));
        }
        assert_eq!(tx.pending(), MAX_EVENTS_PER_PACKET);
        let events = rx.on_packet(&tx.packet().unwrap().unwrap()).unwrap();
        // 最旧的 68 个被丢弃：接收端先释放所有按键
        assert_eq!(events.first(), Some(&InputEvent::ReleaseAll));
        assert_eq!(events.len(), MAX_EVENTS_PER_PACKET + 1);
        assert_eq!(rx.next_expected(), 100);
    }

    #[test]
    fn sequence_numbers_wrap_around() {
        let mut tx = InputSender {
            next_seq: u32::MAX,
            unacked: VecDeque::new(),
            unsent_from: 0,
        };
        let mut rx = InputReceiver {
            next_expected: u32::MAX,
        };
        tx.push(key(1, true));
        tx.push(key(1, false));
        assert_eq!(
            rx.on_packet(&tx.packet().unwrap().unwrap()).unwrap().len(),
            2
        );
        assert_eq!(rx.next_expected(), 1);
        tx.ack(1);
        assert_eq!(tx.pending(), 0);
    }

    #[test]
    fn malformed_packets_are_errors() {
        let mut rx = InputReceiver::new();
        assert!(matches!(rx.on_packet(&[]), Err(Error::Truncated { .. })));
        assert!(matches!(
            rx.on_packet(&[0, 0, 0, 0, 1]),
            Err(Error::Truncated { .. })
        ));
        assert_eq!(
            rx.on_packet(&[0, 0, 0, 0, 33]),
            Err(Error::TooManyItems { len: 33, max: 32 })
        );
        assert_eq!(
            rx.on_packet(&[0, 0, 0, 0, 1, 99]),
            Err(Error::UnknownMessage(99))
        );
        assert_eq!(
            rx.on_packet(&[0, 0, 0, 0, 1, 1, 0, 4, 2]),
            Err(Error::Inconsistent("按下标志只能是 0 或 1"))
        );
        assert_eq!(
            rx.on_packet(&[0, 0, 0, 0, 0, 7]),
            Err(Error::TrailingBytes(1))
        );
        // 出错的包不改变接收状态
        assert_eq!(rx.next_expected(), 0);
        let mut tx = InputSender::new();
        tx.push(InputEvent::Text("x".repeat(MAX_TEXT_LEN + 1)));
        assert!(matches!(tx.packet(), Err(Error::StringTooLong { .. })));
    }

    #[test]
    fn key_state_tracks_and_releases() {
        let mut state = KeyState::new();
        assert!(state.is_idle());
        for e in [
            key(4, true),
            key(5, true),
            key(4, false),
            InputEvent::MouseButton {
                button: 0,
                down: true,
            },
            InputEvent::MouseButton {
                button: 1,
                down: true,
            },
            InputEvent::MouseButton {
                button: 1,
                down: false,
            },
            InputEvent::MouseMove { x: 1, y: 1 },
            InputEvent::MouseRelative { dx: 1, dy: 1 },
            InputEvent::Wheel { dx: 0, dy: 1 },
            InputEvent::Text("a".into()),
        ] {
            state.apply(&e);
        }
        assert!(!state.is_idle());
        assert_eq!(
            state.release_all(),
            vec![
                key(5, false),
                InputEvent::MouseButton {
                    button: 0,
                    down: false
                }
            ]
        );
        assert!(state.is_idle());
        assert!(state.release_all().is_empty());
        state.apply(&key(1, true));
        state.apply(&InputEvent::ReleaseAll);
        assert_eq!(state, KeyState::default());
    }

    #[test]
    fn a_paste_expands_to_keys_and_restores_the_modifiers() {
        let apply = |state: &mut KeyState, events: &[InputEvent]| {
            for e in events {
                state.apply(e);
            }
        };
        // 什么都没按着：Ctrl+V
        let mut state = KeyState::new();
        let keys = state.paste(0b0000_0001, 0x19);
        assert_eq!(
            keys,
            vec![
                key(0xE0, true),
                key(0x19, true),
                key(0x19, false),
                key(0xE0, false)
            ]
        );
        apply(&mut state, &keys);
        assert_eq!(state, KeyState::new());
        // Ctrl 还按着：不重复按，也不抬起
        state.apply(&key(0xE0, true));
        assert_eq!(
            state.paste(0b0000_0001, 0x19),
            vec![key(0x19, true), key(0x19, false)]
        );
        // 现在按着 Shift 和右 Alt，要的是 Command+V：先抬起它们，粘贴完按原样恢复
        let mut state = KeyState::new();
        apply(&mut state, &[key(0xE1, true), key(0xE6, true)]);
        let before = state.clone();
        let keys = state.paste(0b0000_1000, 0x19);
        assert_eq!(
            keys,
            vec![
                key(0xE1, false),
                key(0xE6, false),
                key(0xE3, true),
                key(0x19, true),
                key(0x19, false),
                key(0xE3, false),
                key(0xE6, true),
                key(0xE1, true),
            ]
        );
        apply(&mut state, &keys);
        assert_eq!(state, before);
        // 粘贴事件本身不改状态；要敲的键本来按着时先抬起、最后按回去
        state.apply(&InputEvent::Paste {
            modifiers: 1,
            code: 0x19,
        });
        assert_eq!(state, before);
        let mut state = KeyState::new();
        state.apply(&key(0x49, true));
        let before = state.clone();
        let keys = state.paste(0b0000_0010, 0x49);
        assert_eq!(keys.first(), Some(&key(0xE1, true)));
        assert_eq!(keys.last(), Some(&key(0x49, true)));
        apply(&mut state, &keys);
        assert_eq!(state, before);
    }
}
