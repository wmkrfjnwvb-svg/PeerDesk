//! 把观看窗口收到的 egui 事件翻译成发给被控端的输入事件。
//!
//! - 键盘按物理键位发送（HID 用途码），被控端按它自己的键盘布局出字；
//! - 修饰键一一对应，跨系统也不对调：Control ↔ Ctrl，Option ↔ Alt，Command ↔ Win 键（HID 的 GUI）。
//!   Mac 控制 Windows 时复制按 Control+C（和在 Windows 上一样），Command+C 发过去是 Win+C；
//! - 输入法上屏的文字作为 [`InputEvent::Text`] 发送，组字过程中的按键不发；
//! - 鼠标位置按显示出来的画面归一化，画面外的移动和按下不发；
//! - 窗口失去焦点时发 [`InputEvent::ReleaseAll`]，避免对端按键卡住。

use std::collections::BTreeSet;

use eframe::egui::{
    Event, ImeEvent, Key, Modifiers, MouseWheelUnit, PointerButton, Pos2, Rect, Vec2,
};
use num_traits::ToPrimitive;
use peerdesk_core::input::{InputEvent, MAX_TEXT_LEN};

use super::coords::normalize;
use super::keys::{hid_of, MODIFIERS};

/// 滚轮一格的高精度增量（与 Windows 的 WHEEL_DELTA 相同）。
pub const NOTCH: f32 = 120.0;
/// 触控板按像素滚动时，多少个点算一格。
const POINTS_PER_NOTCH: f32 = 40.0;
/// 按页滚动时，一页算几格。
const NOTCHES_PER_PAGE: f32 = 3.0;
/// 显示或隐藏统计信息的键。它不发给被控端。
pub const OVERLAY_KEY: Key = Key::F12;

/// 一批事件翻译的结果。
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Translated {
    /// 要发给被控端的事件，按顺序。
    pub events: Vec<InputEvent>,
    /// 用户按了切换统计信息的键（次数）。
    pub overlay_toggles: u32,
}

/// 翻译状态：哪些键和按钮已经按下、修饰键状态、输入法是否在组字。
#[derive(Debug, Default)]
pub struct InputState {
    modifiers: BTreeSet<u16>,
    keys: BTreeSet<u16>,
    buttons: BTreeSet<u8>,
    composing: bool,
    pointer: Option<Pos2>,
    wheel: Vec2,
}

fn button_index(button: PointerButton) -> u8 {
    match button {
        PointerButton::Primary => 0,
        PointerButton::Secondary => 1,
        PointerButton::Middle => 2,
        PointerButton::Extra1 => 3,
        PointerButton::Extra2 => 4,
    }
}

/// 把文本切成不超过协议上限的段，不切断字符。
#[must_use]
pub fn split_text(text: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut current = String::new();
    for c in text.chars() {
        if current.len() + c.len_utf8() > MAX_TEXT_LEN {
            parts.push(std::mem::take(&mut current));
        }
        current.push(c);
    }
    if !current.is_empty() {
        parts.push(current);
    }
    parts
}

impl InputState {
    /// 新状态：什么都没按。
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// 是否有键、修饰键或鼠标按钮处于按下状态。
    #[must_use]
    pub fn anything_down(&self) -> bool {
        !(self.keys.is_empty() && self.modifiers.is_empty() && self.buttons.is_empty())
    }

    /// 翻译一帧的事件。`modifiers` 是这一帧结束时的修饰键状态，`picture` 是画面显示的位置
    /// （还没有画面时为 `None`，鼠标事件全部忽略）。
    pub fn translate(
        &mut self,
        events: &[Event],
        modifiers: Modifiers,
        picture: Option<Rect>,
    ) -> Translated {
        let mut out = Translated::default();
        for event in events {
            self.one(event, modifiers, picture, &mut out);
        }
        self.sync_modifiers(modifiers, &mut out.events);
        out
    }

    fn sync_modifiers(&mut self, modifiers: Modifiers, out: &mut Vec<InputEvent>) {
        for (pressed, code) in MODIFIERS {
            let want = pressed(&modifiers);
            let has = self.modifiers.contains(&code);
            if want && !has {
                self.modifiers.insert(code);
                out.push(InputEvent::Key { code, down: true });
            } else if !want && has {
                self.modifiers.remove(&code);
                out.push(InputEvent::Key { code, down: false });
            }
        }
    }

    fn key_down(&mut self, code: u16, out: &mut Vec<InputEvent>) {
        self.keys.insert(code);
        out.push(InputEvent::Key { code, down: true });
    }

    fn key_up(&mut self, code: u16, out: &mut Vec<InputEvent>) {
        // 只抬起自己按下过的键：组字时按下、或者获得焦点前就按着的键不发。
        if self.keys.remove(&code) {
            out.push(InputEvent::Key { code, down: false });
        }
    }

    /// 放开一切，回到初始状态。
    fn reset(&mut self) {
        *self = Self::default();
    }

    fn pointer_to(&mut self, pos: Pos2, picture: Option<Rect>, out: &mut Vec<InputEvent>) -> bool {
        self.pointer = Some(pos);
        match picture.and_then(|p| normalize(pos, p)) {
            Some((x, y)) => {
                out.push(InputEvent::MouseMove { x, y });
                true
            }
            None => false,
        }
    }

    fn wheel(
        &mut self,
        unit: MouseWheelUnit,
        delta: Vec2,
        picture: Option<Rect>,
        out: &mut Vec<InputEvent>,
    ) {
        let inside = self
            .pointer
            .zip(picture)
            .is_some_and(|(pos, pic)| normalize(pos, pic).is_some());
        if !inside {
            return;
        }
        let scale = match unit {
            MouseWheelUnit::Point => NOTCH / POINTS_PER_NOTCH,
            MouseWheelUnit::Line => NOTCH,
            MouseWheelUnit::Page => NOTCH * NOTCHES_PER_PAGE,
        };
        // 不足一个单位的部分留到下次，慢慢滑触控板也能滚动。
        self.wheel += delta * scale;
        let whole = |v: f32| v.trunc().clamp(-32767.0, 32767.0);
        let (dx, dy) = (whole(self.wheel.x), whole(self.wheel.y));
        self.wheel.x -= dx;
        self.wheel.y -= dy;
        let (dx, dy) = (dx.to_i16().unwrap_or(0), dy.to_i16().unwrap_or(0));
        if dx != 0 || dy != 0 {
            out.push(InputEvent::Wheel { dx, dy });
        }
    }

    /// 复制、剪切、粘贴的快捷键被 egui 截成了专门的事件，按下的那个键要补上；
    /// 抬起时仍是普通的按键事件。粘贴用的是被控端自己的剪贴板。
    fn shortcut(&mut self, key: Key, modifiers: Modifiers, out: &mut Vec<InputEvent>) {
        if !self.composing {
            self.sync_modifiers(modifiers, out);
            if let Some(code) = hid_of(key) {
                self.key_down(code, out);
            }
        }
    }

    fn one(
        &mut self,
        event: &Event,
        frame_modifiers: Modifiers,
        picture: Option<Rect>,
        out: &mut Translated,
    ) {
        match event {
            Event::Key {
                key,
                physical_key,
                pressed,
                repeat,
                modifiers,
            } => {
                let key = physical_key.unwrap_or(*key);
                if key == OVERLAY_KEY {
                    if *pressed && !*repeat {
                        out.overlay_toggles += 1;
                    }
                    return;
                }
                let Some(code) = hid_of(key) else { return };
                if !*pressed {
                    // 本机剪贴板里没有文字（只有文件或图片）时，egui-winit 把粘贴快捷键的按下整个吞掉，
                    // 不产生任何事件，只有抬起会到这里。抬起时补一次按下，否则复制了文件或图片以后
                    // 在远程按粘贴没有反应。
                    let swallowed_paste = key == Key::V
                        && modifiers.command
                        && !self.composing
                        && !self.keys.contains(&code);
                    if swallowed_paste {
                        self.sync_modifiers(*modifiers, &mut out.events);
                        self.key_down(code, &mut out.events);
                    }
                    self.key_up(code, &mut out.events);
                } else if !self.composing {
                    self.sync_modifiers(*modifiers, &mut out.events);
                    self.key_down(code, &mut out.events);
                }
            }
            // 复制、剪切、粘贴的快捷键被 egui 截成了专门的事件，按下的那个键要补上；
            // 抬起时仍是普通的按键事件。粘贴用的是被控端自己的剪贴板。
            // 修饰键照当时按着的发：Mac 上截的是 Command+C，发出去是 GUI+C（对方的 Win+C），不换成 Ctrl。
            Event::Copy => self.shortcut(Key::C, frame_modifiers, &mut out.events),
            Event::Cut => self.shortcut(Key::X, frame_modifiers, &mut out.events),
            Event::Paste(_) => self.shortcut(Key::V, frame_modifiers, &mut out.events),
            // 字符已经通过物理键位发出去了。
            Event::Text(_) => {}
            Event::Ime(ime) => match ime {
                ImeEvent::Enabled => {}
                ImeEvent::Preedit(text) => self.composing = !text.is_empty(),
                ImeEvent::Commit(text) => {
                    // 没有经过组字的纯 ASCII 上屏（有的系统在不组字时也这样报告）已经作为按键发过了。
                    if self.composing || !text.is_ascii() {
                        out.events
                            .extend(split_text(text).into_iter().map(InputEvent::Text));
                    }
                    self.composing = false;
                }
                ImeEvent::Disabled => self.composing = false,
            },
            Event::PointerMoved(pos) => {
                self.pointer_to(*pos, picture, &mut out.events);
            }
            Event::PointerButton {
                pos,
                button,
                pressed,
                modifiers,
            } => {
                let index = button_index(*button);
                if *pressed {
                    if self.pointer_to(*pos, picture, &mut out.events) {
                        self.sync_modifiers(*modifiers, &mut out.events);
                        self.buttons.insert(index);
                        out.events.push(InputEvent::MouseButton {
                            button: index,
                            down: true,
                        });
                    }
                } else if self.buttons.remove(&index) {
                    // 拖到画面外再松开也要发，否则对端的按钮一直按着。
                    self.pointer_to(*pos, picture, &mut out.events);
                    out.events.push(InputEvent::MouseButton {
                        button: index,
                        down: false,
                    });
                }
            }
            Event::PointerGone => self.pointer = None,
            Event::MouseWheel {
                unit,
                delta,
                modifiers,
            } => {
                self.sync_modifiers(*modifiers, &mut out.events);
                self.wheel(*unit, *delta, picture, &mut out.events);
            }
            Event::WindowFocused(focused) => {
                if !*focused {
                    self.reset();
                    out.events.push(InputEvent::ReleaseAll);
                }
            }
            Event::MouseMoved(_)
            | Event::Zoom(_)
            | Event::Touch { .. }
            | Event::Screenshot { .. } => {}
        }
    }

    /// 失去焦点或会话中断时调用：如果有东西按着，返回一个 [`InputEvent::ReleaseAll`]。
    pub fn release_all(&mut self) -> Option<InputEvent> {
        let any = self.anything_down();
        self.reset();
        any.then_some(InputEvent::ReleaseAll)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use eframe::egui::{pos2, vec2};

    fn key(key: Key, pressed: bool) -> Event {
        Event::Key {
            key,
            physical_key: Some(key),
            pressed,
            repeat: false,
            modifiers: Modifiers::NONE,
        }
    }

    fn down(code: u16) -> InputEvent {
        InputEvent::Key { code, down: true }
    }

    fn up(code: u16) -> InputEvent {
        InputEvent::Key { code, down: false }
    }

    fn pic() -> Option<Rect> {
        Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(100.0, 100.0)))
    }

    #[test]
    fn keys_use_physical_positions() {
        let mut s = InputState::new();
        // AZERTY 上按物理 Q 键：逻辑键是 A，发的是 Q 的位置
        let ev = Event::Key {
            key: Key::A,
            physical_key: Some(Key::Q),
            pressed: true,
            repeat: false,
            modifiers: Modifiers::NONE,
        };
        assert_eq!(
            s.translate(&[ev], Modifiers::NONE, None).events,
            vec![down(0x14)]
        );
        // 没有物理键位时用逻辑键
        let ev = Event::Key {
            key: Key::B,
            physical_key: None,
            pressed: true,
            repeat: true,
            modifiers: Modifiers::NONE,
        };
        assert_eq!(
            s.translate(&[ev], Modifiers::NONE, None).events,
            vec![down(0x05)]
        );
        // 抬起；没按过的键抬起不发；HID 没有的键不发
        let t = s.translate(
            &[key(Key::Q, false), key(Key::Z, false), key(Key::F30, true)],
            Modifiers::NONE,
            None,
        );
        assert_eq!(t.events, vec![up(0x14)]);
        // 字符事件忽略（已经按键位发过了）
        assert!(s
            .translate(&[Event::Text("q".into())], Modifiers::NONE, None)
            .events
            .is_empty());
    }

    #[test]
    fn modifiers_are_synthesized_from_state_changes() {
        let mut s = InputState::new();
        let ctrl = Modifiers::CTRL;
        let ev = Event::Key {
            key: Key::S,
            physical_key: Some(Key::S),
            pressed: true,
            repeat: false,
            modifiers: ctrl,
        };
        let t = s.translate(&[ev], ctrl, None);
        assert_eq!(t.events, vec![down(0xE0), down(0x16)]);
        assert!(s.anything_down());
        // 只松开 Ctrl：帧末的修饰状态变化也要发
        let t = s.translate(&[key(Key::S, false)], Modifiers::NONE, None);
        assert_eq!(t.events, vec![up(0x16), up(0xE0)]);
        assert!(!s.anything_down());
        // macOS 的 Command 是 GUI 键
        let t = s.translate(
            &[],
            Modifiers::MAC_CMD | Modifiers::SHIFT | Modifiers::ALT,
            None,
        );
        assert_eq!(t.events, vec![down(0xE1), down(0xE2), down(0xE3)]);
    }

    #[test]
    fn modifiers_map_one_to_one_without_swapping() {
        // Mac 上按 Command+C：egui 截成 Copy，发出去是 GUI（对方的 Win 键）+C，不是 Ctrl+C
        let mut s = InputState::new();
        let cmd = Modifiers::MAC_CMD | Modifiers::COMMAND;
        let t = s.translate(&[Event::Copy], cmd, None);
        assert_eq!(t.events, vec![down(0xE3), down(0x06)]);
        let t = s.translate(&[key(Key::C, false)], Modifiers::NONE, None);
        assert_eq!(t.events, vec![up(0x06), up(0xE3)]);
        // Mac 上按 Control+C：是普通按键，发出去是 Ctrl+C（Windows 上的复制）
        let ev = Event::Key {
            key: Key::C,
            physical_key: Some(Key::C),
            pressed: true,
            repeat: false,
            modifiers: Modifiers::CTRL,
        };
        let t = s.translate(&[ev], Modifiers::CTRL, None);
        assert_eq!(t.events, vec![down(0xE0), down(0x06)]);
        // Option 是 Alt
        let mut s = InputState::new();
        let t = s.translate(&[], Modifiers::ALT, None);
        assert_eq!(t.events, vec![down(0xE2)]);
    }

    #[test]
    fn a_paste_swallowed_by_egui_is_sent_on_release() {
        let release = |modifiers| Event::Key {
            key: Key::V,
            physical_key: Some(Key::V),
            pressed: false,
            repeat: false,
            modifiers,
        };
        // 本机剪贴板里只有文件：按下 Ctrl+V 没有任何事件，只有抬起。补上按下，再抬起
        let mut s = InputState::new();
        let t = s.translate(&[], Modifiers::COMMAND | Modifiers::CTRL, None);
        assert_eq!(t.events, vec![down(0xE0)]);
        let ctrl = Modifiers::COMMAND | Modifiers::CTRL;
        let t = s.translate(&[release(ctrl)], ctrl, None);
        assert_eq!(t.events, vec![down(0x19), up(0x19)]);
        // Mac 上是 Command+V：发出去是 GUI+V
        let mut s = InputState::new();
        let cmd = Modifiers::MAC_CMD | Modifiers::COMMAND;
        let t = s.translate(&[release(cmd)], cmd, None);
        assert_eq!(t.events, vec![down(0xE3), down(0x19), up(0x19)]);
        // 正常的粘贴（剪贴板里有文字，egui 给了 Paste）：抬起时不重复
        let mut s = InputState::new();
        let t = s.translate(&[Event::Paste("x".into()), release(ctrl)], ctrl, None);
        assert_eq!(t.events, vec![down(0xE0), down(0x19), up(0x19)]);
        // 没按着命令键的 V 抬起（获得焦点前就按着）、组字时：不补
        let mut s = InputState::new();
        let t = s.translate(&[release(Modifiers::NONE)], Modifiers::NONE, None);
        assert!(t.events.is_empty());
        let t = s.translate(
            &[Event::Ime(ImeEvent::Preedit("n".into())), release(ctrl)],
            Modifiers::NONE,
            None,
        );
        assert!(t.events.is_empty());
    }

    #[test]
    fn clipboard_shortcuts_become_key_presses() {
        let mut s = InputState::new();
        let t = s.translate(&[Event::Copy], Modifiers::CTRL, None);
        assert_eq!(t.events, vec![down(0xE0), down(0x06)]);
        let t = s.translate(
            &[key(Key::C, false), Event::Cut, Event::Paste("x".into())],
            Modifiers::CTRL,
            None,
        );
        assert_eq!(t.events, vec![up(0x06), down(0x1B), down(0x19)]);
    }

    #[test]
    fn f12_toggles_the_overlay_and_is_not_sent() {
        let mut s = InputState::new();
        let repeat = Event::Key {
            key: Key::F12,
            physical_key: Some(Key::F12),
            pressed: true,
            repeat: true,
            modifiers: Modifiers::NONE,
        };
        let t = s.translate(
            &[key(Key::F12, true), repeat, key(Key::F12, false)],
            Modifiers::NONE,
            None,
        );
        assert_eq!(t.overlay_toggles, 1);
        assert!(t.events.is_empty());
    }

    #[test]
    fn ime_commits_text_and_swallows_composition_keys() {
        let mut s = InputState::new();
        let events = [
            Event::Ime(ImeEvent::Enabled),
            Event::Ime(ImeEvent::Preedit("n".into())),
            key(Key::N, true),
            key(Key::N, false),
            Event::Copy,
            Event::Ime(ImeEvent::Preedit("ni".into())),
            Event::Ime(ImeEvent::Commit("你好".into())),
        ];
        let t = s.translate(&events, Modifiers::NONE, None);
        assert_eq!(t.events, vec![InputEvent::Text("你好".into())]);
        // 没有组字的 ASCII 上屏已经按键发过，不重复；非 ASCII 的直接上屏照发
        let t = s.translate(
            &[
                Event::Ime(ImeEvent::Commit("a".into())),
                Event::Ime(ImeEvent::Commit("é".into())),
                Event::Ime(ImeEvent::Preedit("x".into())),
                Event::Ime(ImeEvent::Disabled),
                key(Key::A, true),
                Event::Ime(ImeEvent::Preedit(String::new())),
            ],
            Modifiers::NONE,
            None,
        );
        assert_eq!(t.events, vec![InputEvent::Text("é".into()), down(0x04)]);
    }

    #[test]
    fn long_text_is_split_on_character_boundaries() {
        let text = "汉".repeat(100);
        let parts = split_text(&text);
        assert_eq!(parts.len(), 3);
        assert!(parts.iter().all(|p| p.len() <= MAX_TEXT_LEN));
        assert_eq!(parts.concat(), text);
        assert!(split_text("").is_empty());
    }

    #[test]
    fn mouse_inside_the_picture_only() {
        let mut s = InputState::new();
        let press = |pos, pressed| Event::PointerButton {
            pos,
            button: PointerButton::Primary,
            pressed,
            modifiers: Modifiers::NONE,
        };
        // 还没有画面：全部忽略
        assert!(s
            .translate(
                &[
                    Event::PointerMoved(pos2(5.0, 5.0)),
                    press(pos2(5.0, 5.0), true)
                ],
                Modifiers::NONE,
                None
            )
            .events
            .is_empty());
        // 画面外：忽略
        let t = s.translate(
            &[
                Event::PointerMoved(pos2(150.0, 5.0)),
                press(pos2(150.0, 5.0), true),
            ],
            Modifiers::NONE,
            pic(),
        );
        assert!(t.events.is_empty());
        // 画面内按下，拖到外面松开：松开也要发
        let t = s.translate(
            &[
                press(pos2(50.0, 50.0), true),
                press(pos2(150.0, 50.0), false),
                press(pos2(150.0, 50.0), false),
            ],
            Modifiers::NONE,
            pic(),
        );
        assert_eq!(
            t.events,
            vec![
                InputEvent::MouseMove { x: 32768, y: 32768 },
                InputEvent::MouseButton {
                    button: 0,
                    down: true
                },
                InputEvent::MouseButton {
                    button: 0,
                    down: false
                },
            ]
        );
        // 其他按钮和带修饰键的点击
        let mut events = Vec::new();
        for button in [
            PointerButton::Secondary,
            PointerButton::Middle,
            PointerButton::Extra1,
            PointerButton::Extra2,
        ] {
            events.push(Event::PointerButton {
                pos: pos2(0.0, 0.0),
                button,
                pressed: true,
                modifiers: Modifiers::SHIFT,
            });
        }
        let t = s.translate(&events, Modifiers::SHIFT, pic());
        let buttons: Vec<u8> = t
            .events
            .iter()
            .filter_map(|e| {
                if let InputEvent::MouseButton { button, down: true } = e {
                    Some(*button)
                } else {
                    None
                }
            })
            .collect();
        assert_eq!(buttons, vec![1, 2, 3, 4]);
        assert_eq!(t.events[1], down(0xE1));
        // 其他与输入无关的事件
        let t = s.translate(
            &[
                Event::MouseMoved(vec2(1.0, 1.0)),
                Event::Zoom(2.0),
                Event::PointerGone,
            ],
            Modifiers::SHIFT,
            pic(),
        );
        assert!(t.events.is_empty());
    }

    #[test]
    fn wheel_accumulates_fractions_and_needs_the_pointer_inside() {
        let mut s = InputState::new();
        let wheel = |unit, delta| Event::MouseWheel {
            unit,
            delta,
            modifiers: Modifiers::NONE,
        };
        // 指针不在画面上
        let t = s.translate(
            &[wheel(MouseWheelUnit::Line, vec2(0.0, 1.0))],
            Modifiers::NONE,
            pic(),
        );
        assert!(t.events.is_empty());
        let t = s.translate(
            &[
                Event::PointerMoved(pos2(10.0, 10.0)),
                wheel(MouseWheelUnit::Line, vec2(0.0, 1.0)),
                wheel(MouseWheelUnit::Page, vec2(-1.0, 0.0)),
                wheel(MouseWheelUnit::Point, vec2(0.0, -0.2)),
                wheel(MouseWheelUnit::Point, vec2(0.0, -0.2)),
            ],
            Modifiers::NONE,
            pic(),
        );
        assert_eq!(
            t.events[1..],
            [
                InputEvent::Wheel { dx: 0, dy: 120 },
                InputEvent::Wheel { dx: -360, dy: 0 },
                InputEvent::Wheel { dx: 0, dy: -1 },
            ]
        );
        // 巨大的增量被限制在 i16 范围内
        let t = s.translate(
            &[wheel(MouseWheelUnit::Page, vec2(0.0, 1000.0))],
            Modifiers::NONE,
            pic(),
        );
        assert_eq!(t.events, vec![InputEvent::Wheel { dx: 0, dy: 32767 }]);
    }

    #[test]
    fn focus_loss_releases_everything() {
        let mut s = InputState::new();
        let _ = s.translate(&[key(Key::A, true)], Modifiers::CTRL, None);
        let t = s.translate(
            &[Event::WindowFocused(true), Event::WindowFocused(false)],
            Modifiers::NONE,
            None,
        );
        assert_eq!(t.events, vec![InputEvent::ReleaseAll]);
        assert!(!s.anything_down());
        assert_eq!(s.release_all(), None);
        let _ = s.translate(&[key(Key::A, true)], Modifiers::NONE, None);
        assert_eq!(s.release_all(), Some(InputEvent::ReleaseAll));
    }
}

/// 观看窗口靠 `layer_id_at` 判断指针在不在助手、菜单上（在就不把移动和按下发给远程）。
/// 这里确认它的行为：画面在 Background 层，助手在 Foreground 层，第二帧起认得出。
#[cfg(test)]
mod layer_probe {
    #![allow(clippy::unwrap_used, clippy::indexing_slicing)]
    use eframe::egui;

    #[test]
    fn toolbar_is_found_above_the_picture() {
        let ctx = egui::Context::default();
        let mut found = Vec::new();
        for _ in 0..3 {
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(800.0, 600.0),
                )),
                ..Default::default()
            };
            let _ = ctx.run(input, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    ui.label("picture");
                });
                egui::Area::new(egui::Id::new("toolbar"))
                    .fixed_pos(egui::pos2(300.0, 8.0))
                    .order(egui::Order::Foreground)
                    .show(ctx, |ui| {
                        let _ = ui.button("button in toolbar");
                    });
                found.push((
                    ctx.layer_id_at(egui::pos2(310.0, 15.0)).map(|l| l.order),
                    ctx.layer_id_at(egui::pos2(100.0, 300.0)).map(|l| l.order),
                ));
            });
        }
        assert_eq!(found[2].0, Some(egui::Order::Foreground));
        assert_eq!(found[2].1, Some(egui::Order::Background));
    }
}
