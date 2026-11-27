//! 主窗口的尺寸和排版算法（紧凑的对话框式窗口）。只算不画，`app.rs` 拿结果去画。
//!
//! 为什么是紧凑窗口：产品负责人真机反馈“客户端感觉太大了”，UU 远程、ToDesk 都是对话框模式。
//! 远程桌面的主窗口只做三件事（看设备码和密码、连另一台、挑设备），一屏放得下，不需要铺满屏幕。
//!
//! 同行的主窗口大小（没有官方文档，下面都是凭印象，**未核实**，需要用真机装一遍量出来）：
//! ToDesk、UU 远程、AnyDesk、RustDesk 的主窗口默认都在 800–950 × 520–650 逻辑像素一档，
//! 并且都能调整大小。所以默认 840×580、最小 640×480：比原来的 1280×800 小一半多，
//! 在 1366×768 的小笔记本上也不会顶满屏幕。

use super::theme::{size, space};

/// 主窗口默认大小（逻辑像素，不含系统标题栏）。
pub const WINDOW_DEFAULT: [f32; 2] = [840.0, 600.0];
/// 主窗口最小大小。再小，设备码、密码和连接栏就挤不下了。
pub const WINDOW_MIN: [f32; 2] = [640.0, 480.0];

/// 内容区左右、上下的边距。
pub const PAGE_MARGIN: [f32; 2] = [space::S24, space::S12];
/// 内容区底下不留边距：下面紧接着状态栏，滚动区一直到状态栏上沿。
pub const PAGE_MARGIN_BOTTOM: f32 = 0.0;
/// 状态栏上下的边距和最小高度。
pub const STATUS_MARGIN_Y: f32 = space::S4;
/// 状态栏内容的高度（实测：一行图标加字）。
pub const STATUS_MIN_HEIGHT: f32 = 30.0;
/// 各段之间的间距。
pub const SECTION_GAP: f32 = space::S16;
/// 设备表的行高：设备（两行字）、分组（一行字）、表头。
pub const ROW_HEIGHT: f32 = 48.0;
pub const GROUP_ROW_HEIGHT: f32 = 36.0;
pub const HEADER_HEIGHT: f32 = 34.0;
/// 设备表：设备码、操作两列的宽度（名称用剩下的）。
pub const COL_CODE: f32 = 128.0;
pub const COL_ACTIONS: f32 = 190.0;
pub const COL_LAST: f32 = 110.0;
/// 名称列至少这么宽，否则不显示单独的“上次连接”列（并进名称下面那一行）。
pub const NAME_MIN: f32 = 200.0;
/// 名称列至少这么宽，否则连设备码列也不显示。
pub const NAME_COMPACT: f32 = 140.0;

/// 以下是实测值（Xvfb 截图量出来，字体或控件尺寸改了要重新量）。
/// “这台电脑”面板的高度：设备码、密码一行加状态一行，含内边距。
pub const THIS_PC_HEIGHT: f32 = 138.0;
/// 页面标题行（“这台电脑”和开关）的高度。
pub const TITLE_HEIGHT: f32 = 32.0;
/// “连接另一台电脑”：小标题加输入行。
pub const CONNECT_HEIGHT: f32 = 74.0;
/// 设备小标题行（含切换）。
pub const DEVICES_HEADER_HEIGHT: f32 = 38.0;
/// 设备码块和密码块（紧凑字号）的宽度，加上它们之间的间距：放在一行要这么宽。
pub const CODE_BLOCK_WIDTH: f32 = 270.0;
pub const PASSWORD_BLOCK_WIDTH: f32 = 250.0;
pub const BLOCK_GAP: f32 = space::S28;
/// 面板里面够这么宽时，设备码用大字号。
pub const CODE_WIDE: f32 = 640.0;
/// 面板里面够这么宽时，状态放在设备码和密码的右边；否则放到下一行。
pub const THIS_PC_WIDE: f32 = 900.0;
/// 页面内容最宽这么多（再宽，一行字太长不好读）。
pub const PAGE_MAX_WIDTH: f32 = 1060.0;

/// 窗口宽度减去侧边栏和左右边距，是页面内容的宽度。
#[must_use]
pub fn page_width(window_width: f32) -> f32 {
    (window_width - size::SIDEBAR - 2.0 * PAGE_MARGIN[0]).clamp(0.0, PAGE_MAX_WIDTH)
}

/// “这台电脑”面板里面的宽度（减去面板内边距）。
#[must_use]
pub fn this_pc_inner_width(window_width: f32) -> f32 {
    page_width(window_width) - 2.0 * space::S20
}

/// 设备码用小一号的字吗。
#[must_use]
pub fn code_is_compact(inner_width: f32) -> bool {
    inner_width < CODE_WIDE
}

/// 状态放在设备码和密码的右边吗。
#[must_use]
pub fn status_beside(inner_width: f32) -> bool {
    inner_width >= THIS_PC_WIDE
}

/// 设备表的列宽。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TablePlan {
    /// 设备码列宽；`None` 时不显示（窗口很窄，名称更要紧）。
    pub code: Option<f32>,
    /// 单独的“上次连接”列；`None` 时上次连接并进名称下面那一行。
    pub last: Option<f32>,
    /// 操作列宽。
    pub actions: f32,
}

/// 按表的宽度定列：先保住名称的宽度，宽了依次多出“设备码”列、“上次连接”列；
/// 没有单独“上次连接”列时它并进名称下面那一行。
#[must_use]
pub fn table_plan(width: f32) -> TablePlan {
    let base = width - space::S16 * 2.0 - COL_ACTIONS;
    let with_code = base - COL_CODE - space::S16;
    let code = (with_code >= NAME_COMPACT).then_some(COL_CODE);
    let last = (code.is_some() && with_code - COL_LAST >= NAME_MIN).then_some(COL_LAST);
    TablePlan {
        code,
        last,
        actions: COL_ACTIONS,
    }
}

/// 主页在窗口里从上到下至少要这么高，才能同时看到：这台电脑、连接栏、设备表头和 `rows` 行设备、状态栏。
#[must_use]
pub fn home_required_height(rows: usize) -> f32 {
    // 行数很小，转成 f32 不会丢精度
    #[allow(clippy::cast_precision_loss)]
    let rows = rows as f32;
    PAGE_MARGIN[1]
        + PAGE_MARGIN_BOTTOM
        + TITLE_HEIGHT
        + THIS_PC_HEIGHT
        + SECTION_GAP
        + CONNECT_HEIGHT
        + SECTION_GAP
        + DEVICES_HEADER_HEIGHT
        + HEADER_HEIGHT
        + rows * ROW_HEIGHT
        + status_bar_height()
}

/// 底部状态栏的高度。
#[must_use]
pub const fn status_bar_height() -> f32 {
    2.0 * STATUS_MARGIN_Y + STATUS_MIN_HEIGHT
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_window_is_a_compact_dialog() {
        // 比原来的 1280×800 小得多，又大到放得下主页
        const {
            assert!(WINDOW_DEFAULT[0] <= 900.0 && WINDOW_DEFAULT[1] <= 640.0);
        }
        const {
            assert!(WINDOW_DEFAULT[0] >= WINDOW_MIN[0] && WINDOW_DEFAULT[1] >= WINDOW_MIN[1]);
        }
        const {
            assert!(WINDOW_MIN[0] <= 700.0 && WINDOW_MIN[1] <= 500.0);
        }
        // 1366×768 的小笔记本上，加上系统标题栏和任务栏也放得下
        const {
            assert!(WINDOW_DEFAULT[1] + 120.0 <= 768.0);
        }
    }

    #[test]
    fn home_shows_everything_at_the_default_size() {
        // 默认尺寸：这台电脑、连接栏、设备表头加至少 3 行设备，不用滚动
        assert!(
            home_required_height(3) <= WINDOW_DEFAULT[1],
            "需要 {}",
            home_required_height(3)
        );
        // 最小尺寸：至少设备表头可见（其余靠滚动）
        assert!(home_required_height(0) <= WINDOW_MIN[1] + 80.0);
    }

    #[test]
    fn code_and_password_fit_in_one_row_at_the_default_width() {
        let inner = this_pc_inner_width(WINDOW_DEFAULT[0]);
        assert!(code_is_compact(inner), "默认宽度用小一号的设备码");
        assert!(!status_beside(inner), "状态放到下一行");
        assert!(
            CODE_BLOCK_WIDTH + BLOCK_GAP + PASSWORD_BLOCK_WIDTH <= inner,
            "{inner}"
        );
        // 最宽的页面：大号设备码，状态在旁边
        let wide = this_pc_inner_width(2560.0);
        assert!(!code_is_compact(wide) && status_beside(wide));
    }

    #[test]
    fn table_keeps_the_name_readable() {
        let default = table_plan(page_width(WINDOW_DEFAULT[0]));
        assert_eq!(default.last, None, "默认宽度：上次连接并进名称下面");
        assert!(default.code.is_some(), "默认宽度：设备码列在");
        let name_at = |width: f32| {
            let plan = table_plan(width);
            width
                - space::S16 * 2.0
                - plan.actions
                - plan.code.map_or(0.0, |c| c + space::S16)
                - plan.last.unwrap_or(0.0)
        };
        for window in [WINDOW_MIN[0], WINDOW_DEFAULT[0], 1000.0, 1280.0, 2560.0] {
            let width = page_width(window);
            assert!(name_at(width) >= 110.0, "{window}: {}", name_at(width));
        }
        // 最小窗口：设备码列让位给名称
        let narrow = table_plan(page_width(WINDOW_MIN[0]));
        assert_eq!(narrow.code, None);
        assert_eq!(narrow.last, None);
        // 最宽：三列都在，名称仍不少于 NAME_MIN
        let wide = table_plan(1000.0);
        assert!(wide.code.is_some() && wide.last.is_some());
        assert!(name_at(1000.0) >= NAME_MIN);
    }

    #[test]
    fn page_width_is_bounded() {
        assert_eq!(page_width(0.0), 0.0);
        assert_eq!(page_width(10_000.0), PAGE_MAX_WIDTH);
        assert_eq!(
            page_width(WINDOW_DEFAULT[0]),
            WINDOW_DEFAULT[0] - size::SIDEBAR - 2.0 * PAGE_MARGIN[0]
        );
    }
}
