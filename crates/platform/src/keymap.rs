//! 键位表：USB HID 用途码（键盘页 0x07，表示物理键位）→ 各系统的键码。
//!
//! 三张表都按**物理位置**对应，不经过键盘布局：被控端用什么布局、输入法，按下的就是同一个位置的键，
//! 和本地按键盘完全一样。参考：USB HID Usage Tables 第 10 章；Linux `drivers/hid/hid-input.c` 的
//! `hid_keyboard[]`（evdev 键码）；微软 “Keyboard Scan Code Specification” 附录 C（Set 1 扫描码）；
//! macOS `HIToolbox/Events.h` 的 `kVK_*`。

/// 常用键的 HID 用途码。
pub mod hid {
    /// A（其余字母依次加一，到 Z 是 0x1D）。
    pub const A: u16 = 0x04;
    /// 1（其余数字依次加一，9 是 0x26）。
    pub const DIGIT1: u16 = 0x1E;
    /// 0。
    pub const DIGIT0: u16 = 0x27;
    /// 回车。
    pub const ENTER: u16 = 0x28;
    /// Esc。
    pub const ESCAPE: u16 = 0x29;
    /// 退格。
    pub const BACKSPACE: u16 = 0x2A;
    /// Tab。
    pub const TAB: u16 = 0x2B;
    /// 空格。
    pub const SPACE: u16 = 0x2C;
    /// Caps Lock。
    pub const CAPS_LOCK: u16 = 0x39;
    /// F1（到 F12 依次加一）。
    pub const F1: u16 = 0x3A;
    /// Pause。
    pub const PAUSE: u16 = 0x48;
    /// 方向键右。
    pub const RIGHT: u16 = 0x4F;
    /// 方向键左。
    pub const LEFT: u16 = 0x50;
    /// 方向键下。
    pub const DOWN: u16 = 0x51;
    /// 方向键上。
    pub const UP: u16 = 0x52;
    /// 左 Ctrl。
    pub const LEFT_CTRL: u16 = 0xE0;
    /// 左 Shift。
    pub const LEFT_SHIFT: u16 = 0xE1;
    /// 左 Alt（Option）。
    pub const LEFT_ALT: u16 = 0xE2;
    /// 左 Win（Command）。
    pub const LEFT_GUI: u16 = 0xE3;
    /// 右 Ctrl。
    pub const RIGHT_CTRL: u16 = 0xE4;
    /// 右 Shift。
    pub const RIGHT_SHIFT: u16 = 0xE5;
    /// 右 Alt（Option）。
    pub const RIGHT_ALT: u16 = 0xE6;
    /// 右 Win（Command）。
    pub const RIGHT_GUI: u16 = 0xE7;
}

/// HID → Linux evdev 键码（X11 的键码是它加 8）。
#[must_use]
pub const fn to_evdev(code: u16) -> Option<u16> {
    let k = match code {
        // 字母
        0x04 => 30,
        0x05 => 48,
        0x06 => 46,
        0x07 => 32,
        0x08 => 18,
        0x09 => 33,
        0x0A => 34,
        0x0B => 35,
        0x0C => 23,
        0x0D => 36,
        0x0E => 37,
        0x0F => 38,
        0x10 => 50,
        0x11 => 49,
        0x12 => 24,
        0x13 => 25,
        0x14 => 16,
        0x15 => 19,
        0x16 => 31,
        0x17 => 20,
        0x18 => 22,
        0x19 => 47,
        0x1A => 17,
        0x1B => 45,
        0x1C => 21,
        0x1D => 44,
        // 数字 1..9、0
        0x1E..=0x27 => code - 0x1E + 2,
        0x28 => 28,                      // Enter
        0x29 => 1,                       // Esc
        0x2A => 14,                      // Backspace
        0x2B => 15,                      // Tab
        0x2C => 57,                      // Space
        0x2D => 12,                      // -
        0x2E => 13,                      // =
        0x2F => 26,                      // [
        0x30 => 27,                      // ]
        0x31 => 43,                      // \
        0x32 => 43,                      // 欧洲键盘的 #（与 \ 同一个键码）
        0x33 => 39,                      // ;
        0x34 => 40,                      // '
        0x35 => 41,                      // `
        0x36 => 51,                      // ,
        0x37 => 52,                      // .
        0x38 => 53,                      // /
        0x39 => 58,                      // Caps Lock
        0x3A..=0x43 => code - 0x3A + 59, // F1..F10
        0x44 => 87,                      // F11
        0x45 => 88,                      // F12
        0x46 => 99,                      // PrintScreen（SysRq）
        0x47 => 70,                      // Scroll Lock
        0x48 => 119,                     // Pause
        0x49 => 110,                     // Insert
        0x4A => 102,                     // Home
        0x4B => 104,                     // PageUp
        0x4C => 111,                     // Delete
        0x4D => 107,                     // End
        0x4E => 109,                     // PageDown
        0x4F => 106,                     // →
        0x50 => 105,                     // ←
        0x51 => 108,                     // ↓
        0x52 => 103,                     // ↑
        0x53 => 69,                      // Num Lock
        0x54 => 98,                      // 小键盘 /
        0x55 => 55,                      // 小键盘 *
        0x56 => 74,                      // 小键盘 -
        0x57 => 78,                      // 小键盘 +
        0x58 => 96,                      // 小键盘 Enter
        0x59 => 79,                      // 小键盘 1
        0x5A => 80,
        0x5B => 81,
        0x5C => 75,
        0x5D => 76,
        0x5E => 77,
        0x5F => 71,
        0x60 => 72,
        0x61 => 73,                       // 小键盘 9
        0x62 => 82,                       // 小键盘 0
        0x63 => 83,                       // 小键盘 .
        0x64 => 86,                       // 欧洲键盘的 \（102 键）
        0x65 => 127,                      // 菜单键（Compose）
        0x66 => 116,                      // Power
        0x67 => 117,                      // 小键盘 =
        0x68..=0x73 => code - 0x68 + 183, // F13..F24
        0x74 => 134,                      // Execute（Open）
        0x75 => 138,                      // Help
        0x76 => 130,                      // Menu（Props）
        0x77 => 132,                      // Select（Front）
        0x78 => 128,                      // Stop
        0x79 => 129,                      // Again
        0x7A => 131,                      // Undo
        0x7B => 137,                      // Cut
        0x7C => 133,                      // Copy
        0x7D => 135,                      // Paste
        0x7E => 136,                      // Find
        0x7F => 113,                      // 静音
        0x80 => 115,                      // 音量 +
        0x81 => 114,                      // 音量 -
        0x85 => 121,                      // 小键盘 ,
        0x87 => 89,                       // 日文 Ro
        0x88 => 93,                       // 日文 片假名/平假名
        0x89 => 124,                      // 日文 ¥
        0x8A => 92,                       // 日文 变换
        0x8B => 94,                       // 日文 无变换
        0x8C => 95,                       // 小键盘 日文逗号
        0x90 => 122,                      // 韩文 한/영
        0x91 => 123,                      // 韩文 汉字
        0x92 => 90,                       // 日文 片假名
        0x93 => 91,                       // 日文 平假名
        0x94 => 85,                       // 日文 半角/全角
        0xE0 => 29,                       // 左 Ctrl
        0xE1 => 42,                       // 左 Shift
        0xE2 => 56,                       // 左 Alt
        0xE3 => 125,                      // 左 Win
        0xE4 => 97,                       // 右 Ctrl
        0xE5 => 54,                       // 右 Shift
        0xE6 => 100,                      // 右 Alt
        0xE7 => 126,                      // 右 Win
        _ => return None,
    };
    Some(k)
}

/// HID → X11 键码（evdev + 8，适用于所有使用 evdev 驱动的 X 服务器，包括 Xvfb）。
#[must_use]
pub fn to_x11(code: u16) -> Option<u8> {
    to_evdev(code).and_then(|k| u8::try_from(k + 8).ok())
}

/// Windows 上注入一个键的方式。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum WinKey {
    /// Set 1 扫描码；`extended` 表示带 E0 前缀（`KEYEVENTF_EXTENDEDKEY`）。
    Scan {
        /// 扫描码。
        code: u16,
        /// 是否是扩展键。
        extended: bool,
    },
    /// 没有单一扫描码的键（Pause 是 E1 1D 45 序列），用虚拟键码。
    Virtual(u16),
}

const fn scan(code: u16) -> WinKey {
    WinKey::Scan {
        code,
        extended: false,
    }
}

const fn ext(code: u16) -> WinKey {
    WinKey::Scan {
        code,
        extended: true,
    }
}

/// HID → Windows 的扫描码。
#[must_use]
pub fn to_windows(code: u16) -> Option<WinKey> {
    let k = match code {
        // 主键区的 Set 1 扫描码与 evdev 键码相同
        0x04..=0x45 | 0x47 | 0x55..=0x57 | 0x59..=0x64 => return to_evdev(code).map(scan),
        0x46 => ext(0x37),             // PrintScreen
        0x48 => WinKey::Virtual(0x13), // Pause（VK_PAUSE）
        0x49 => ext(0x52),             // Insert
        0x4A => ext(0x47),             // Home
        0x4B => ext(0x49),             // PageUp
        0x4C => ext(0x53),             // Delete
        0x4D => ext(0x4F),             // End
        0x4E => ext(0x51),             // PageDown
        0x4F => ext(0x4D),             // →
        0x50 => ext(0x4B),             // ←
        0x51 => ext(0x50),             // ↓
        0x52 => ext(0x48),             // ↑
        // Num Lock 用扫描码注入时必须带扩展标志，否则系统把 0x45 当成 Pause
        0x53 => ext(0x45),
        0x54 => ext(0x35),                       // 小键盘 /
        0x58 => ext(0x1C),                       // 小键盘 Enter
        0x65 => ext(0x5D),                       // 菜单键
        0x66 => ext(0x5E),                       // Power
        0x67 => scan(0x59),                      // 小键盘 =
        0x68..=0x72 => scan(code - 0x68 + 0x64), // F13..F23
        0x73 => scan(0x76),                      // F24
        0x7F => ext(0x20),                       // 静音
        0x80 => ext(0x30),                       // 音量 +
        0x81 => ext(0x2E),                       // 音量 -
        0x85 => scan(0x7E),                      // 小键盘 ,
        0x87 => scan(0x73),                      // 日文 Ro
        0x88 => scan(0x70),                      // 日文 片假名/平假名
        0x89 => scan(0x7D),                      // 日文 ¥
        0x8A => scan(0x79),                      // 日文 变换
        0x8B => scan(0x7B),                      // 日文 无变换
        0x90 => scan(0xF2),                      // 韩文 한/영
        0x91 => scan(0xF1),                      // 韩文 汉字
        0xE0 => scan(0x1D),                      // 左 Ctrl
        0xE1 => scan(0x2A),                      // 左 Shift
        0xE2 => scan(0x38),                      // 左 Alt
        0xE3 => ext(0x5B),                       // 左 Win
        0xE4 => ext(0x1D),                       // 右 Ctrl
        0xE5 => scan(0x36),                      // 右 Shift
        0xE6 => ext(0x38),                       // 右 Alt
        0xE7 => ext(0x5C),                       // 右 Win
        _ => return None,
    };
    Some(k)
}

/// HID → macOS 虚拟键码（`kVK_*`）。苹果键盘没有的键按苹果的习惯对应：
/// Insert → Help，Num Lock → 小键盘 Clear，PrintScreen / Scroll Lock / Pause → F13 / F14 / F15。
#[must_use]
pub const fn to_mac(code: u16) -> Option<u16> {
    let k = match code {
        0x04 => 0x00,        // A
        0x05 => 0x0B,        // B
        0x06 => 0x08,        // C
        0x07 => 0x02,        // D
        0x08 => 0x0E,        // E
        0x09 => 0x03,        // F
        0x0A => 0x05,        // G
        0x0B => 0x04,        // H
        0x0C => 0x22,        // I
        0x0D => 0x26,        // J
        0x0E => 0x28,        // K
        0x0F => 0x25,        // L
        0x10 => 0x2E,        // M
        0x11 => 0x2D,        // N
        0x12 => 0x1F,        // O
        0x13 => 0x23,        // P
        0x14 => 0x0C,        // Q
        0x15 => 0x0F,        // R
        0x16 => 0x01,        // S
        0x17 => 0x11,        // T
        0x18 => 0x20,        // U
        0x19 => 0x09,        // V
        0x1A => 0x0D,        // W
        0x1B => 0x07,        // X
        0x1C => 0x10,        // Y
        0x1D => 0x06,        // Z
        0x1E => 0x12,        // 1
        0x1F => 0x13,        // 2
        0x20 => 0x14,        // 3
        0x21 => 0x15,        // 4
        0x22 => 0x17,        // 5
        0x23 => 0x16,        // 6
        0x24 => 0x1A,        // 7
        0x25 => 0x1C,        // 8
        0x26 => 0x19,        // 9
        0x27 => 0x1D,        // 0
        0x28 => 0x24,        // Return
        0x29 => 0x35,        // Escape
        0x2A => 0x33,        // Delete（退格）
        0x2B => 0x30,        // Tab
        0x2C => 0x31,        // Space
        0x2D => 0x1B,        // -
        0x2E => 0x18,        // =
        0x2F => 0x21,        // [
        0x30 => 0x1E,        // ]
        0x31 | 0x32 => 0x2A, // \ 和欧洲键盘的 #
        0x33 => 0x29,        // ;
        0x34 => 0x27,        // '
        0x35 => 0x32,        // `
        0x36 => 0x2B,        // ,
        0x37 => 0x2F,        // .
        0x38 => 0x2C,        // /
        0x39 => 0x39,        // Caps Lock
        0x3A => 0x7A,        // F1
        0x3B => 0x78,        // F2
        0x3C => 0x63,        // F3
        0x3D => 0x76,        // F4
        0x3E => 0x60,        // F5
        0x3F => 0x61,        // F6
        0x40 => 0x62,        // F7
        0x41 => 0x64,        // F8
        0x42 => 0x65,        // F9
        0x43 => 0x6D,        // F10
        0x44 => 0x67,        // F11
        0x45 => 0x6F,        // F12
        0x46 => 0x69,        // PrintScreen → F13
        0x47 => 0x6B,        // Scroll Lock → F14
        0x48 => 0x71,        // Pause → F15
        0x49 => 0x72,        // Insert → Help
        0x4A => 0x73,        // Home
        0x4B => 0x74,        // PageUp
        0x4C => 0x75,        // Delete（向前删除）
        0x4D => 0x77,        // End
        0x4E => 0x79,        // PageDown
        0x4F => 0x7C,        // →
        0x50 => 0x7B,        // ←
        0x51 => 0x7D,        // ↓
        0x52 => 0x7E,        // ↑
        0x53 => 0x47,        // Num Lock → 小键盘 Clear
        0x54 => 0x4B,        // 小键盘 /
        0x55 => 0x43,        // 小键盘 *
        0x56 => 0x4E,        // 小键盘 -
        0x57 => 0x45,        // 小键盘 +
        0x58 => 0x4C,        // 小键盘 Enter
        0x59 => 0x53,        // 小键盘 1
        0x5A => 0x54,
        0x5B => 0x55,
        0x5C => 0x56,
        0x5D => 0x57,
        0x5E => 0x58,
        0x5F => 0x59,
        0x60 => 0x5B,
        0x61 => 0x5C, // 小键盘 9
        0x62 => 0x52, // 小键盘 0
        0x63 => 0x41, // 小键盘 .
        0x64 => 0x0A, // 欧洲键盘的 \（ISO Section）
        0x67 => 0x51, // 小键盘 =
        0x68 => 0x69, // F13
        0x69 => 0x6B, // F14
        0x6A => 0x71, // F15
        0x6B => 0x6A, // F16
        0x6C => 0x40, // F17
        0x6D => 0x4F, // F18
        0x6E => 0x50, // F19
        0x6F => 0x5A, // F20
        0x75 => 0x72, // Help
        0x7F => 0x4A, // 静音
        0x80 => 0x48, // 音量 +
        0x81 => 0x49, // 音量 -
        0x85 => 0x5F, // 小键盘 ,（JIS）
        0x87 => 0x5E, // 日文 Ro（JIS 下划线）
        0x89 => 0x5D, // 日文 ¥
        0x90 => 0x68, // かな
        0x91 => 0x66, // 英数
        0xE0 => 0x3B, // 左 Control
        0xE1 => 0x38, // 左 Shift
        0xE2 => 0x3A, // 左 Option
        0xE3 => 0x37, // 左 Command
        0xE4 => 0x3E, // 右 Control
        0xE5 => 0x3C, // 右 Shift
        0xE6 => 0x3D, // 右 Option
        0xE7 => 0x36, // 右 Command
        _ => return None,
    };
    Some(k)
}

/// macOS 的修饰键标志（`kCGEventFlagMask*` 加上区分左右的 `NX_DEVICE*KEYMASK`）。
pub mod mac_flags {
    /// Caps Lock 锁定。
    pub const CAPS_LOCK: u64 = 0x0001_0000;
    /// Shift。
    pub const SHIFT: u64 = 0x0002_0000;
    /// Control。
    pub const CONTROL: u64 = 0x0004_0000;
    /// Option。
    pub const OPTION: u64 = 0x0008_0000;
    /// Command。
    pub const COMMAND: u64 = 0x0010_0000;
    /// 小键盘上的键。
    pub const NUMERIC_PAD: u64 = 0x0020_0000;
    /// Fn（方向键、Home/End 等功能键也带这个标志）。
    pub const FUNCTION: u64 = 0x0080_0000;
}

/// 一个 macOS 键码作为修饰键时对应的标志；不是修饰键时为 `None`。
#[must_use]
pub const fn mac_modifier_flags(kvk: u16) -> Option<u64> {
    use mac_flags::{COMMAND, CONTROL, OPTION, SHIFT};
    let f = match kvk {
        0x38 => SHIFT | 0x02,
        0x3C => SHIFT | 0x04,
        0x3B => CONTROL | 0x01,
        0x3E => CONTROL | 0x2000,
        0x3A => OPTION | 0x20,
        0x3D => OPTION | 0x40,
        0x37 => COMMAND | 0x08,
        0x36 => COMMAND | 0x10,
        0x3F => mac_flags::FUNCTION,
        _ => return None,
    };
    Some(f)
}

/// 按住的这些键（macOS 键码）加上 Caps Lock 的状态，事件应当带的修饰键标志。
#[must_use]
pub fn mac_event_flags(held: impl IntoIterator<Item = u16>, caps_lock: bool) -> u64 {
    let flags = held
        .into_iter()
        .filter_map(mac_modifier_flags)
        .fold(0, |acc, f| acc | f);
    if caps_lock {
        flags | mac_flags::CAPS_LOCK
    } else {
        flags
    }
}

/// 这个 macOS 键本身要带的标志：小键盘上的键带 `NUMERIC_PAD`，方向键带 `NUMERIC_PAD | FUNCTION`，
/// Home/End/PageUp/PageDown/向前删除/Help/F 键带 `FUNCTION`（与真键盘一致，有些程序靠它区分）。
#[must_use]
pub const fn mac_key_flags(kvk: u16) -> u64 {
    use mac_flags::{FUNCTION, NUMERIC_PAD};
    match kvk {
        0x7B..=0x7E => NUMERIC_PAD | FUNCTION,
        0x41 | 0x43 | 0x45 | 0x47 | 0x4B | 0x4C | 0x4E | 0x51..=0x59 | 0x5B | 0x5C => NUMERIC_PAD,
        0x72..=0x77
        | 0x79
        | 0x7A
        | 0x60..=0x65
        | 0x67
        | 0x69
        | 0x6A
        | 0x6B
        | 0x6D
        | 0x6F
        | 0x71
        | 0x40
        | 0x4F
        | 0x50
        | 0x5A => FUNCTION,
        _ => 0,
    }
}

/// 一个字符对应的 X11 keysym：Latin-1 直接对应，其余用 Unicode keysym（`0x0100_0000 + 码点`）。
#[must_use]
pub fn x11_keysym(c: char) -> Option<u32> {
    let cp = u32::from(c);
    Some(match c {
        '\r' | '\n' => 0xff0d, // Return
        '\t' => 0xff09,        // Tab
        '\u{8}' => 0xff08,     // BackSpace
        c if c.is_control() => return None,
        _ if matches!(cp, 0x20..=0x7e | 0xa0..=0xff) => cp,
        _ => 0x0100_0000 | cp,
    })
}

/// 美式布局下一个 ASCII 字符要按的键：`(HID 用途码, 是否要按住 Shift)`。测试和演示用。
#[must_use]
pub fn ascii_to_hid(c: char) -> Option<(u16, bool)> {
    const SHIFTED_DIGITS: &str = ")!@#$%^&*(";
    let cp = u16::try_from(u32::from(c)).ok()?;
    Some(match c {
        'a'..='z' => (hid::A + cp - u16::from(b'a'), false),
        'A'..='Z' => (hid::A + cp - u16::from(b'A'), true),
        '1'..='9' => (hid::DIGIT1 + cp - u16::from(b'1'), false),
        '0' => (hid::DIGIT0, false),
        '\n' => (hid::ENTER, false),
        '\t' => (hid::TAB, false),
        ' ' => (hid::SPACE, false),
        '-' => (0x2D, false),
        '_' => (0x2D, true),
        '=' => (0x2E, false),
        '+' => (0x2E, true),
        '[' => (0x2F, false),
        '{' => (0x2F, true),
        ']' => (0x30, false),
        '}' => (0x30, true),
        '\\' => (0x31, false),
        '|' => (0x31, true),
        ';' => (0x33, false),
        ':' => (0x33, true),
        '\'' => (0x34, false),
        '"' => (0x34, true),
        '`' => (0x35, false),
        '~' => (0x35, true),
        ',' => (0x36, false),
        '<' => (0x36, true),
        '.' => (0x37, false),
        '>' => (0x37, true),
        '/' => (0x38, false),
        '?' => (0x38, true),
        _ => {
            let digit = SHIFTED_DIGITS.find(c)?;
            let digit = u16::try_from(digit).ok()?;
            let code = if digit == 0 {
                hid::DIGIT0
            } else {
                hid::DIGIT1 + digit - 1
            };
            (code, true)
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    /// 所有已知的 HID 键码。
    fn known() -> impl Iterator<Item = u16> {
        (0u16..=0xFF).filter(|c| to_evdev(*c).is_some())
    }

    #[test]
    fn evdev_spot_checks() {
        // 来自 Linux input-event-codes.h
        assert_eq!(to_evdev(0x04), Some(30)); // KEY_A
        assert_eq!(to_evdev(0x1D), Some(44)); // KEY_Z
        assert_eq!(to_evdev(0x1E), Some(2)); // KEY_1
        assert_eq!(to_evdev(0x27), Some(11)); // KEY_0
        assert_eq!(to_evdev(0x3A), Some(59)); // KEY_F1
        assert_eq!(to_evdev(0x43), Some(68)); // KEY_F10
        assert_eq!(to_evdev(0x68), Some(183)); // KEY_F13
        assert_eq!(to_evdev(0x73), Some(194)); // KEY_F24
        assert_eq!(to_evdev(0xE7), Some(126)); // KEY_RIGHTMETA
        assert_eq!(to_x11(0x04), Some(38), "X11 的 a 是 38");
        assert_eq!(to_evdev(0x00), None);
        assert_eq!(to_evdev(0x03), None);
        assert_eq!(to_evdev(0xE8), None);
        assert_eq!(to_x11(0x200), None);
    }

    #[test]
    fn evdev_codes_are_unique_except_hash_key() {
        let mut seen = HashSet::new();
        for code in known() {
            let k = to_evdev(code).unwrap();
            assert!(
                seen.insert(k) || code == 0x32,
                "重复的 evdev 键码 {k}（HID {code:#x}）"
            );
        }
        assert!(seen.len() > 140);
    }

    #[test]
    fn windows_scancodes() {
        let s = |code| WinKey::Scan {
            code,
            extended: false,
        };
        let e = |code| WinKey::Scan {
            code,
            extended: true,
        };
        assert_eq!(to_windows(0x04), Some(s(0x1E))); // A
        assert_eq!(to_windows(0x28), Some(s(0x1C))); // Enter
        assert_eq!(to_windows(0x44), Some(s(0x57))); // F11
        assert_eq!(to_windows(0x47), Some(s(0x46))); // Scroll Lock
        assert_eq!(to_windows(0x58), Some(e(0x1C))); // 小键盘 Enter
        assert_eq!(to_windows(0x4F), Some(e(0x4D))); // →
        assert_eq!(to_windows(0x53), Some(e(0x45))); // Num Lock
        assert_eq!(to_windows(0x55), Some(s(0x37))); // 小键盘 *
        assert_eq!(to_windows(0x59), Some(s(0x4F))); // 小键盘 1
        assert_eq!(to_windows(0x64), Some(s(0x56))); // 102 键
        assert_eq!(to_windows(0x68), Some(s(0x64))); // F13
        assert_eq!(to_windows(0x72), Some(s(0x6E))); // F23
        assert_eq!(to_windows(0x73), Some(s(0x76))); // F24
        assert_eq!(to_windows(0x48), Some(WinKey::Virtual(0x13))); // Pause
        assert_eq!(to_windows(0xE6), Some(e(0x38))); // 右 Alt
        assert_eq!(to_windows(0x00), None);
        assert_eq!(to_windows(0x74), None);
        // 同一个 (扫描码, 扩展) 不能对应两个键（欧洲键盘的 # 与 \ 本来就是同一个扫描码）
        let mut seen = HashSet::new();
        for code in 0u16..=0xFF {
            if let Some(k) = to_windows(code) {
                assert!(
                    seen.insert(k) || code == 0x32,
                    "重复的 Windows 键 {k:?}（HID {code:#x}）"
                );
            }
        }
        assert!(seen.len() > 120);
    }

    #[test]
    fn mac_keycodes() {
        assert_eq!(to_mac(0x04), Some(0x00)); // A
        assert_eq!(to_mac(0x16), Some(0x01)); // S
        assert_eq!(to_mac(0x28), Some(0x24)); // Return
        assert_eq!(to_mac(0x2A), Some(0x33)); // 退格
        assert_eq!(to_mac(0x4C), Some(0x75)); // 向前删除
        assert_eq!(to_mac(0xE3), Some(0x37)); // Command
        assert_eq!(to_mac(0x65), None, "苹果键盘没有菜单键");
        let mut seen = HashSet::new();
        for code in 0u16..=0xFF {
            if let Some(k) = to_mac(code) {
                // \ 与欧洲 #、PrintScreen 与 F13 等按苹果习惯共用一个键
                if !seen.insert(k) {
                    assert!(matches!(code, 0x32 | 0x68..=0x6A | 0x75), "HID {code:#x}");
                }
            }
        }
        assert!(seen.len() > 110);
    }

    #[test]
    fn mac_flags_follow_held_modifiers() {
        use mac_flags::*;
        assert_eq!(mac_event_flags([], false), 0);
        assert_eq!(mac_event_flags([0x00], true), CAPS_LOCK);
        assert_eq!(
            mac_event_flags([0x38, 0x36, 0x00], false),
            SHIFT | 0x02 | COMMAND | 0x10
        );
        for (kvk, flag) in [
            (0x3C, SHIFT),
            (0x3B, CONTROL),
            (0x3E, CONTROL),
            (0x3A, OPTION),
            (0x3D, OPTION),
            (0x37, COMMAND),
            (0x3F, FUNCTION),
        ] {
            assert_eq!(mac_modifier_flags(kvk).unwrap() & flag, flag);
        }
        assert_eq!(mac_key_flags(0x7B), NUMERIC_PAD | FUNCTION);
        assert_eq!(mac_key_flags(0x53), NUMERIC_PAD);
        for kvk in [
            0x73, 0x79, 0x7A, 0x60, 0x65, 0x67, 0x69, 0x6A, 0x6B, 0x6D, 0x6F, 0x71, 0x40,
        ] {
            assert_eq!(mac_key_flags(kvk), FUNCTION, "{kvk:#x}");
        }
        for kvk in [0x41, 0x43, 0x45, 0x47, 0x4B, 0x4C, 0x4E, 0x5B, 0x5C] {
            assert_eq!(mac_key_flags(kvk), NUMERIC_PAD, "{kvk:#x}");
        }
        assert_eq!(mac_key_flags(0x00), 0);
    }

    #[test]
    fn keysyms() {
        assert_eq!(x11_keysym('a'), Some(0x61));
        assert_eq!(x11_keysym('é'), Some(0xe9));
        assert_eq!(x11_keysym('世'), Some(0x0100_4e16));
        assert_eq!(x11_keysym('\r'), Some(0xff0d));
        assert_eq!(x11_keysym('\n'), Some(0xff0d));
        assert_eq!(x11_keysym('\t'), Some(0xff09));
        assert_eq!(x11_keysym('\u{8}'), Some(0xff08));
        assert_eq!(x11_keysym('\u{1}'), None);
    }

    #[test]
    fn ascii_keys() {
        assert_eq!(ascii_to_hid('a'), Some((0x04, false)));
        assert_eq!(ascii_to_hid('Z'), Some((0x1D, true)));
        assert_eq!(ascii_to_hid('1'), Some((0x1E, false)));
        assert_eq!(ascii_to_hid('0'), Some((0x27, false)));
        assert_eq!(ascii_to_hid(')'), Some((0x27, true)));
        assert_eq!(ascii_to_hid('!'), Some((0x1E, true)));
        assert_eq!(ascii_to_hid('('), Some((0x26, true)));
        assert_eq!(ascii_to_hid('\n'), Some((hid::ENTER, false)));
        assert_eq!(ascii_to_hid('\t'), Some((hid::TAB, false)));
        assert_eq!(ascii_to_hid('世'), None);
        assert_eq!(ascii_to_hid('\u{1F600}'), None);
        assert_eq!(ascii_to_hid('\u{7}'), None);
        // 所有可打印 ASCII 都有对应的键，而且都在三张表里
        for c in ' '..='~' {
            let (code, _) = ascii_to_hid(c).unwrap_or_else(|| panic!("{c:?}"));
            assert!(to_evdev(code).is_some() && to_windows(code).is_some());
            assert!(to_mac(code).is_some());
        }
    }
}
