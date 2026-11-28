//! Android 的按键码（`KeyEvent.KEYCODE_*`）换成 USB HID 用途码（被控端认的物理键位，见
//! [`peerdesk_core::input::InputEvent::Key`]）。外接键盘和快捷键栏用；软键盘打的字走文本（`InputEvent::Text`）。
//!
//! 码表来源：Android `KeyEvent` 的常量（developer.android.com/reference/android/view/KeyEvent）和
//! USB HID Usage Tables 第 10 章（键盘页 0x07）。不认识的键返回 `None`，不乱发。

/// Android 按键码 → HID 用途码。
#[must_use]
pub fn hid(keycode: i32) -> Option<u16> {
    let code = match keycode {
        // 字母 A–Z：KEYCODE_A = 29 … KEYCODE_Z = 54 → 0x04 … 0x1D
        29..=54 => 0x04 + (keycode - 29),
        // 数字：KEYCODE_0 = 7 → 0x27；KEYCODE_1 = 8 … KEYCODE_9 = 16 → 0x1E … 0x26
        7 => 0x27,
        8..=16 => 0x1E + (keycode - 8),
        // F1–F12：KEYCODE_F1 = 131 … KEYCODE_F12 = 142 → 0x3A … 0x45
        131..=142 => 0x3A + (keycode - 131),
        66 => 0x28,  // ENTER
        111 => 0x29, // ESCAPE
        67 => 0x2A,  // DEL（退格）
        61 => 0x2B,  // TAB
        62 => 0x2C,  // SPACE
        69 => 0x2D,  // MINUS
        70 => 0x2E,  // EQUALS
        71 => 0x2F,  // LEFT_BRACKET
        72 => 0x30,  // RIGHT_BRACKET
        73 => 0x31,  // BACKSLASH
        74 => 0x33,  // SEMICOLON
        75 => 0x34,  // APOSTROPHE
        68 => 0x35,  // GRAVE
        55 => 0x36,  // COMMA
        56 => 0x37,  // PERIOD
        76 => 0x38,  // SLASH
        115 => 0x39, // CAPS_LOCK
        124 => 0x49, // INSERT
        122 => 0x4A, // MOVE_HOME
        92 => 0x4B,  // PAGE_UP
        112 => 0x4C, // FORWARD_DEL
        123 => 0x4D, // MOVE_END
        93 => 0x4E,  // PAGE_DOWN
        22 => 0x4F,  // DPAD_RIGHT
        21 => 0x50,  // DPAD_LEFT
        20 => 0x51,  // DPAD_DOWN
        19 => 0x52,  // DPAD_UP
        113 => 0xE0, // CTRL_LEFT
        59 => 0xE1,  // SHIFT_LEFT
        57 => 0xE2,  // ALT_LEFT
        117 => 0xE3, // META_LEFT（Windows 键、Command 键）
        114 => 0xE4, // CTRL_RIGHT
        60 => 0xE5,  // SHIFT_RIGHT
        58 => 0xE6,  // ALT_RIGHT
        118 => 0xE7, // META_RIGHT
        _ => return None,
    };
    u16::try_from(code).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_map_to_hid_usages() {
        assert_eq!(hid(29), Some(0x04), "A");
        assert_eq!(hid(54), Some(0x1D), "Z");
        assert_eq!(hid(7), Some(0x27), "0");
        assert_eq!(hid(8), Some(0x1E), "1");
        assert_eq!(hid(16), Some(0x26), "9");
        assert_eq!(hid(131), Some(0x3A), "F1");
        assert_eq!(hid(142), Some(0x45), "F12");
        assert_eq!(hid(66), Some(0x28), "回车");
        assert_eq!(hid(111), Some(0x29), "Esc");
        assert_eq!(hid(19), Some(0x52), "上");
        assert_eq!(hid(113), Some(0xE0), "左 Ctrl");
        assert_eq!(hid(118), Some(0xE7), "右 Command");
        // 不认识的键不发
        for unknown in [0, 1, 4, 24, 25, 82, 999, -1] {
            assert_eq!(hid(unknown), None, "{unknown}");
        }
        // 没有两个按键码对到同一个 HID 键位
        let mut seen = std::collections::BTreeMap::new();
        for k in 0..300 {
            if let Some(h) = hid(k) {
                assert!(
                    seen.insert(h, k).is_none(),
                    "{k} 和 {:?} 都是 {h:#x}",
                    seen.get(&h)
                );
            }
        }
        assert_eq!(seen.len(), 26 + 10 + 12 + 35);
    }
}
