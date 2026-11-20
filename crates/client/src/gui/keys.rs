//! egui 的按键到 USB HID 用途码（键盘页 0x07）的映射。
//!
//! 优先用物理键位（与键盘布局无关）；egui 只在拿不到物理键位时给逻辑键，
//! 这时把符号映射到美式键盘上产生它的那个键。egui 不单独报告修饰键，
//! 修饰键由 [`MODIFIERS`] 按修饰状态的变化合成。

use eframe::egui::{Key, Modifiers};

/// 左 Ctrl。
pub const LEFT_CTRL: u16 = 0xE0;
/// 左 Shift。
pub const LEFT_SHIFT: u16 = 0xE1;
/// 左 Alt（macOS 的 Option）。
pub const LEFT_ALT: u16 = 0xE2;
/// 左 GUI（Windows 键、macOS 的 Command）。
pub const LEFT_GUI: u16 = 0xE3;

/// 从修饰状态里取出某个修饰键是否按下。
pub type Pressed = fn(&Modifiers) -> bool;

/// 修饰键：从修饰状态里取出是否按下，以及对应的用途码。
pub const MODIFIERS: [(Pressed, u16); 4] = [
    (|m| m.ctrl, LEFT_CTRL),
    (|m| m.shift, LEFT_SHIFT),
    (|m| m.alt, LEFT_ALT),
    (|m| m.mac_cmd, LEFT_GUI),
];

/// 按键对应的用途码。没有对应键位的返回 `None`。
#[must_use]
pub const fn hid_of(key: Key) -> Option<u16> {
    Some(match key {
        Key::A => 0x04,
        Key::B => 0x05,
        Key::C => 0x06,
        Key::D => 0x07,
        Key::E => 0x08,
        Key::F => 0x09,
        Key::G => 0x0A,
        Key::H => 0x0B,
        Key::I => 0x0C,
        Key::J => 0x0D,
        Key::K => 0x0E,
        Key::L => 0x0F,
        Key::M => 0x10,
        Key::N => 0x11,
        Key::O => 0x12,
        Key::P => 0x13,
        Key::Q => 0x14,
        Key::R => 0x15,
        Key::S => 0x16,
        Key::T => 0x17,
        Key::U => 0x18,
        Key::V => 0x19,
        Key::W => 0x1A,
        Key::X => 0x1B,
        Key::Y => 0x1C,
        Key::Z => 0x1D,
        // 美式键盘上 Shift+1 是感叹号。
        Key::Num1 | Key::Exclamationmark => 0x1E,
        Key::Num2 => 0x1F,
        Key::Num3 => 0x20,
        Key::Num4 => 0x21,
        Key::Num5 => 0x22,
        Key::Num6 => 0x23,
        Key::Num7 => 0x24,
        Key::Num8 => 0x25,
        Key::Num9 => 0x26,
        Key::Num0 => 0x27,
        Key::Enter => 0x28,
        Key::Escape => 0x29,
        Key::Backspace => 0x2A,
        Key::Tab => 0x2B,
        Key::Space => 0x2C,
        Key::Minus => 0x2D,
        Key::Equals => 0x2E,
        Key::OpenBracket | Key::OpenCurlyBracket => 0x2F,
        Key::CloseBracket | Key::CloseCurlyBracket => 0x30,
        Key::Backslash | Key::Pipe => 0x31,
        Key::Semicolon | Key::Colon => 0x33,
        Key::Quote => 0x34,
        Key::Backtick => 0x35,
        Key::Comma => 0x36,
        Key::Period => 0x37,
        Key::Slash | Key::Questionmark => 0x38,
        Key::F1 => 0x3A,
        Key::F2 => 0x3B,
        Key::F3 => 0x3C,
        Key::F4 => 0x3D,
        Key::F5 => 0x3E,
        Key::F6 => 0x3F,
        Key::F7 => 0x40,
        Key::F8 => 0x41,
        Key::F9 => 0x42,
        Key::F10 => 0x43,
        Key::F11 => 0x44,
        Key::F12 => 0x45,
        Key::Insert => 0x49,
        Key::Home => 0x4A,
        Key::PageUp => 0x4B,
        Key::Delete => 0x4C,
        Key::End => 0x4D,
        Key::PageDown => 0x4E,
        Key::ArrowRight => 0x4F,
        Key::ArrowLeft => 0x50,
        Key::ArrowDown => 0x51,
        Key::ArrowUp => 0x52,
        // egui 的物理键位里，Plus 只来自小键盘的加号。
        Key::Plus => 0x57,
        Key::F13 => 0x68,
        Key::F14 => 0x69,
        Key::F15 => 0x6A,
        Key::F16 => 0x6B,
        Key::F17 => 0x6C,
        Key::F18 => 0x6D,
        Key::F19 => 0x6E,
        Key::F20 => 0x6F,
        Key::F21 => 0x70,
        Key::F22 => 0x71,
        Key::F23 => 0x72,
        Key::F24 => 0x73,
        Key::Cut => 0x7B,
        Key::Copy => 0x7C,
        Key::Paste => 0x7D,
        // HID 键盘页没有 F25 以上；浏览器后退在消费者页，不走键盘事件。
        Key::F25
        | Key::F26
        | Key::F27
        | Key::F28
        | Key::F29
        | Key::F30
        | Key::F31
        | Key::F32
        | Key::F33
        | Key::F34
        | Key::F35
        | Key::BrowserBack => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn letters_digits_and_function_keys_are_contiguous() {
        let letters = [
            Key::A,
            Key::B,
            Key::C,
            Key::D,
            Key::E,
            Key::F,
            Key::G,
            Key::H,
            Key::I,
            Key::J,
            Key::K,
            Key::L,
            Key::M,
            Key::N,
            Key::O,
            Key::P,
            Key::Q,
            Key::R,
            Key::S,
            Key::T,
            Key::U,
            Key::V,
            Key::W,
            Key::X,
            Key::Y,
            Key::Z,
        ];
        for (code, key) in (0x04u16..).zip(letters) {
            assert_eq!(hid_of(key), Some(code), "{key:?}");
        }
        let digits = [
            Key::Num1,
            Key::Num2,
            Key::Num3,
            Key::Num4,
            Key::Num5,
            Key::Num6,
            Key::Num7,
            Key::Num8,
            Key::Num9,
            Key::Num0,
        ];
        for (code, key) in (0x1Eu16..).zip(digits) {
            assert_eq!(hid_of(key), Some(code), "{key:?}");
        }
        let f = [
            Key::F1,
            Key::F2,
            Key::F3,
            Key::F4,
            Key::F5,
            Key::F6,
            Key::F7,
            Key::F8,
            Key::F9,
            Key::F10,
            Key::F11,
            Key::F12,
        ];
        for (code, key) in (0x3Au16..).zip(f) {
            assert_eq!(hid_of(key), Some(code), "{key:?}");
        }
        let f13 = [
            Key::F13,
            Key::F14,
            Key::F15,
            Key::F16,
            Key::F17,
            Key::F18,
            Key::F19,
            Key::F20,
            Key::F21,
            Key::F22,
            Key::F23,
            Key::F24,
        ];
        for (code, key) in (0x68u16..).zip(f13) {
            assert_eq!(hid_of(key), Some(code), "{key:?}");
        }
    }

    #[test]
    fn named_keys_and_symbols() {
        for (key, code) in [
            (Key::Enter, 0x28),
            (Key::Escape, 0x29),
            (Key::Backspace, 0x2A),
            (Key::Tab, 0x2B),
            (Key::Space, 0x2C),
            (Key::Minus, 0x2D),
            (Key::Equals, 0x2E),
            (Key::OpenBracket, 0x2F),
            (Key::OpenCurlyBracket, 0x2F),
            (Key::CloseBracket, 0x30),
            (Key::CloseCurlyBracket, 0x30),
            (Key::Backslash, 0x31),
            (Key::Pipe, 0x31),
            (Key::Semicolon, 0x33),
            (Key::Colon, 0x33),
            (Key::Quote, 0x34),
            (Key::Backtick, 0x35),
            (Key::Comma, 0x36),
            (Key::Period, 0x37),
            (Key::Slash, 0x38),
            (Key::Questionmark, 0x38),
            (Key::Exclamationmark, 0x1E),
            (Key::Insert, 0x49),
            (Key::Home, 0x4A),
            (Key::PageUp, 0x4B),
            (Key::Delete, 0x4C),
            (Key::End, 0x4D),
            (Key::PageDown, 0x4E),
            (Key::ArrowRight, 0x4F),
            (Key::ArrowLeft, 0x50),
            (Key::ArrowDown, 0x51),
            (Key::ArrowUp, 0x52),
            (Key::Plus, 0x57),
            (Key::Cut, 0x7B),
            (Key::Copy, 0x7C),
            (Key::Paste, 0x7D),
        ] {
            assert_eq!(hid_of(key), Some(code), "{key:?}");
        }
    }

    #[test]
    fn every_key_is_mapped_except_the_ones_hid_lacks() {
        let unmapped: Vec<Key> = Key::ALL
            .iter()
            .copied()
            .filter(|k| hid_of(*k).is_none())
            .collect();
        assert_eq!(unmapped.len(), 12, "{unmapped:?}");
        assert!(unmapped.contains(&Key::BrowserBack));
        assert!(unmapped.contains(&Key::F35));
    }

    #[test]
    fn modifier_table() {
        let all = Modifiers {
            alt: true,
            ctrl: true,
            shift: true,
            mac_cmd: true,
            command: true,
        };
        let codes: Vec<u16> = MODIFIERS
            .iter()
            .filter(|(on, _)| on(&all))
            .map(|m| m.1)
            .collect();
        assert_eq!(codes, vec![LEFT_CTRL, LEFT_SHIFT, LEFT_ALT, LEFT_GUI]);
        assert!(MODIFIERS.iter().all(|(on, _)| !on(&Modifiers::NONE)));
    }
}
