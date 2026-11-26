//! 界面主题（设计方向“信号”）：设计令牌和几个统一外观的小部件。
//!
//! 原则：
//! 1. 颜色只表示连接状态：直连青绿、中继琥珀、出问题红；其余黑白灰。主按钮用墨色，不用品牌色。
//! 2. 分组靠留白和细线，不堆卡片；圆角两档（控件、面板），状态标签用胶囊。
//! 3. 界面代码里的颜色、间距、圆角、字号都从这里的令牌取，不写数字。
//!
//! 令牌用 Rust 常量，不用数据文件：编译期检查，没有解析失败这条路（业内也有放进 RON/JSON 再热加载的，
//! 例如 Rerun；我们的体量用不上热加载）。深色主题设计稿没有画，按同样的角色配了一套，
//! 文字和底色的对比度由测试保证不低于 4.5:1。图标用 Phosphor（MIT）。

use eframe::egui::{
    self, epaint::MarginF32, epaint::Shadow, Align, Color32, CornerRadius, FontFamily, FontId,
    Frame, Layout, Margin, Painter, Pos2, Response, RichText, Sense, Stroke, StrokeKind, Theme, Ui,
    Vec2, Visuals, WidgetInfo, WidgetType,
};

use super::fonts;

/// 声明界面用到的图标：只有这里点名的图标才打进程序（字体子集由 `scripts/build-fonts.py`
/// 按这份清单生成，测试检查子集里确实都有）。用到新图标时加在这里，再重新生成字体。
macro_rules! icons {
    ($($name:ident),* $(,)?) => {
        /// 图标（Phosphor Regular 的字形，当文字画）。
        pub mod icon {
            $(pub use egui_phosphor::regular::$name;)*
            /// 清单里的全部图标。
            pub const ALL: &[&str] = &[$($name),*];
        }
    };
}

icons! {
    APPLE_LOGO, ARROWS_CLOCKWISE, ARROW_COUNTER_CLOCKWISE, ARROW_LINE_UP, ARROW_RIGHT, ARROW_UP,
    CARET_DOWN, CARET_UP, CHART_BAR, CHECK_CIRCLE, CIRCLE, CLOCK_COUNTER_CLOCKWISE, COPY,
    CORNERS_IN, CORNERS_OUT, CURSOR_CLICK, DESKTOP, DESKTOP_TOWER, DOTS_SIX_VERTICAL,
    DOWNLOAD_SIMPLE, EYE, EYE_SLASH, FILE, FILE_PLUS, FOLDER, FOLDER_OPEN, FOLDER_PLUS, GEAR_SIX,
    HAND_EYE, INFO, KEYBOARD, LINK, LINUX_LOGO, LOCK_SIMPLE, MONITOR, NUMBER_SQUARE_ONE,
    PHONE_DISCONNECT, PLUG, SHIELD_WARNING, SLIDERS_HORIZONTAL, SPEAKER_HIGH, SPEAKER_SLASH,
    TRASH, UPLOAD_SIMPLE, WARNING, WIFI_HIGH, WINDOWS_LOGO, X, X_CIRCLE,
}

/// 间距（逻辑像素）。名字就是数值，只用这一档尺度里的数。
pub mod space {
    /// 2：紧挨着的两行字之间。
    pub const S2: f32 = 2.0;
    /// 4：导航项之间、图标和它的说明之间。
    pub const S4: f32 = 4.0;
    /// 6：标签和它下面的值、一排小按钮之间。
    pub const S6: f32 = 6.0;
    /// 8：默认的控件间距。
    pub const S8: f32 = 8.0;
    /// 10：图标和文字、表单里的控件之间。
    pub const S10: f32 = 10.0;
    /// 12：标题和内容之间。
    pub const S12: f32 = 12.0;
    /// 14：按钮左右的内边距、面板标题栏的内边距。
    pub const S14: f32 = 14.0;
    /// 16：表格单元格左右的内边距。
    pub const S16: f32 = 16.0;
    /// 20：大按钮左右的内边距、侧边栏上下的内边距。
    pub const S20: f32 = 20.0;
    /// 24：面板上下的内边距。
    pub const S24: f32 = 24.0;
    /// 28：面板左右的内边距。
    pub const S28: f32 = 28.0;
    /// 36：页面上下的内边距、页面里大段之间。
    pub const S36: f32 = 36.0;
    /// 44：页面左右的内边距。
    pub const S44: f32 = 44.0;
    /// 48：面板里并排的几组内容之间。
    pub const S48: f32 = 48.0;
}

/// 圆角。只有两档，外加胶囊。
pub mod radius {
    /// 控件：按钮、输入框、导航项、菜单项。
    pub const CONTROL: f32 = 6.0;
    /// 面板：承载内容的白底区域、浮层。
    pub const PANEL: f32 = 10.0;
    /// 胶囊：状态标签、进度条。
    pub const PILL: f32 = 255.0;
    /// 套在一排控件外面的槽（分段选择）：控件圆角加上槽的留边，看起来同心。不算第三档。
    pub const TRACK: f32 = CONTROL + super::size::TRACK_INSET;
}

/// 字号。
pub mod font {
    /// 12：角标、表头旁的补充说明。
    pub const CAPTION: f32 = 12.0;
    /// 13：次要说明、表头、小按钮。
    pub const SMALL: f32 = 13.0;
    /// 14：正文、按钮。
    pub const BODY: f32 = 14.0;
    /// 16：小标题、大输入框里的字。
    pub const SUBTITLE: f32 = 16.0;
    /// 20：页面标题。
    pub const TITLE: f32 = 20.0;
    /// 24：密码。
    pub const SECRET: f32 = 24.0;
    /// 30：窄窗口里的设备码。
    pub const CODE_COMPACT: f32 = 30.0;
    /// 40：设备码（界面的主角）。
    pub const CODE: f32 = 40.0;
    /// 18：导航和按钮里的图标。
    pub const ICON: f32 = 18.0;
    /// 22：品牌标记、提示条里的图标。
    pub const ICON_LARGE: f32 = 22.0;
    /// 36：空状态的大图标。
    pub const ICON_HERO: f32 = 36.0;
    /// 设备码的字距（占字号的比例）：三位一组更好认。
    pub const CODE_TRACKING: f32 = 0.04;
    /// 密码的字距（占字号的比例）。
    pub const SECRET_TRACKING: f32 = 0.08;
}

/// 控件尺寸。
pub mod size {
    /// 大控件的高度：连接表单的输入框和按钮、设备码旁的图标按钮（也是最小点击区域）。
    pub const CONTROL_LARGE: f32 = 44.0;
    /// 普通控件的高度：表格行里的按钮、工具栏按钮。
    pub const CONTROL: f32 = 36.0;
    /// 小控件的高度：行内的文字按钮。
    pub const CONTROL_SMALL: f32 = 32.0;
    /// 分段选择里每一段的高度。
    pub const SEGMENT: f32 = 30.0;
    /// 分段选择的槽比里面的段大出的一圈。
    pub const TRACK_INSET: f32 = 3.0;
    /// 导航项的高度。
    pub const NAV_ITEM: f32 = 38.0;
    /// 侧边栏的宽度。
    pub const SIDEBAR: f32 = 220.0;
    /// 状态圆点的直径。
    pub const DOT: f32 = 8.0;
    /// 细线、边框。
    pub const LINE: f32 = 1.0;
    /// 空心圆点（离线）的线宽。
    pub const DOT_RING: f32 = 1.5;
    /// 强调的边框（当前面板、键盘焦点）。
    pub const LINE_STRONG: f32 = 2.0;
    /// 开关的宽和高。
    pub const TOGGLE: [f32; 2] = [36.0, 20.0];
    /// 开关里的圆钮离边的距离。
    pub const TOGGLE_INSET: f32 = 3.0;
    /// 普通输入框的默认宽度。
    pub const INPUT_WIDTH: f32 = 240.0;
}

/// 动画时长（秒）。
pub mod motion {
    /// 悬停、开关这类状态变化。
    pub const FAST: f32 = 0.12;
}

/// 字重。egui 没有字重的概念，每个字重是一个字体族（见 `fonts.rs`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Weight {
    /// 400：正文。
    Regular,
    /// 500：导航、名称、状态标签、表头。
    Medium,
    /// 600：标题、主按钮、设备码。
    Semibold,
}

impl Weight {
    /// 这个字重对应的字体族。
    #[must_use]
    pub fn family(self) -> FontFamily {
        match self {
            Self::Regular => FontFamily::Proportional,
            Self::Medium => FontFamily::Name(fonts::MEDIUM.into()),
            Self::Semibold => FontFamily::Name(fonts::SEMIBOLD.into()),
        }
    }
}

/// 指定字号和字重的字体。
#[must_use]
pub fn font_id(size: f32, weight: Weight) -> FontId {
    FontId::new(size, weight.family())
}

/// 指定字号和字重的文字。
#[must_use]
pub fn text(text: impl Into<String>, size: f32, weight: Weight) -> RichText {
    RichText::new(text).font(font_id(size, weight))
}

/// 连接状态。界面上只有它有颜色。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Signal {
    /// 直连（青绿）：也用于“在线”“就绪”。
    Direct,
    /// 中继（琥珀）：能用，但绕路或者需要留意。
    Relay,
    /// 出问题（红）。
    Problem,
    /// 没有连接（灰）：关闭、离线、等待。
    Idle,
}

/// 一种连接状态的三个颜色。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SignalColors {
    /// 圆点、图标、细线。
    pub solid: Color32,
    /// 状态标签和提示条的浅底。
    pub soft: Color32,
    /// 浅底上的文字。
    pub text: Color32,
}

/// 一套配色，按角色命名。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Palette {
    /// 是否深色。
    pub dark: bool,
    /// 底：窗口背景。
    pub bg: Color32,
    /// 面：侧边栏、面板、输入框、菜单。
    pub surface: Color32,
    /// 墨：正文、标题、主按钮的底。
    pub ink: Color32,
    /// 墨色底上的字（主按钮的文字）。
    pub on_ink: Color32,
    /// 次要文字：说明、表头、没选中的导航项。
    pub muted: Color32,
    /// 不能点的控件上的字。
    pub disabled: Color32,
    /// 线：面板边框、分隔线。
    pub line: Color32,
    /// 重一点的线：输入框的边框。
    pub line_strong: Color32,
    /// 轻一点的线：表格行之间。
    pub line_soft: Color32,
    /// 选中的导航项、鼠标停留的按钮和菜单项的底。
    pub hover: Color32,
    /// 鼠标停留的表格行的底。
    pub row_hover: Color32,
    /// 槽：分段选择、进度条、开关关着时的底。
    pub track: Color32,
    /// 直连。
    pub direct: SignalColors,
    /// 中继。
    pub relay: SignalColors,
    /// 出问题。
    pub problem: SignalColors,
    /// 没有连接。
    pub idle: SignalColors,
    /// 危险操作按钮（结束、断开）的底，上面用白字。
    pub danger_fill: Color32,
    /// 浮层（菜单、提示）的阴影。
    pub shadow: Color32,
}

/// 危险操作按钮上的字（两套配色都是白色）。
pub const ON_DANGER: Color32 = Color32::WHITE;

impl Palette {
    /// 浅色（设计稿）。
    pub const LIGHT: Self = Self {
        dark: false,
        bg: Color32::from_rgb(0xF4, 0xF5, 0xF7),
        surface: Color32::from_rgb(0xFF, 0xFF, 0xFF),
        ink: Color32::from_rgb(0x15, 0x18, 0x1D),
        on_ink: Color32::from_rgb(0xFF, 0xFF, 0xFF),
        muted: Color32::from_rgb(0x5A, 0x62, 0x70),
        disabled: Color32::from_rgb(0x6B, 0x73, 0x80),
        line: Color32::from_rgb(0xDD, 0xE1, 0xE6),
        line_strong: Color32::from_rgb(0xB9, 0xC0, 0xC9),
        line_soft: Color32::from_rgb(0xEE, 0xF0, 0xF3),
        hover: Color32::from_rgb(0xEE, 0xF0, 0xF3),
        row_hover: Color32::from_rgb(0xF7, 0xF8, 0xFA),
        track: Color32::from_rgb(0xE7, 0xEA, 0xEE),
        direct: SignalColors {
            solid: Color32::from_rgb(0x0B, 0x7A, 0x75),
            soft: Color32::from_rgb(0xE3, 0xF2, 0xF1),
            text: Color32::from_rgb(0x07, 0x59, 0x55),
        },
        relay: SignalColors {
            solid: Color32::from_rgb(0xA3, 0x54, 0x07),
            soft: Color32::from_rgb(0xFF, 0xF6, 0xEA),
            text: Color32::from_rgb(0x8A, 0x4A, 0x06),
        },
        problem: SignalColors {
            solid: Color32::from_rgb(0xC2, 0x36, 0x2F),
            soft: Color32::from_rgb(0xFD, 0xEE, 0xEC),
            text: Color32::from_rgb(0xA3, 0x24, 0x1E),
        },
        idle: SignalColors {
            solid: Color32::from_rgb(0x8B, 0x93, 0x9F),
            soft: Color32::from_rgb(0xEE, 0xF0, 0xF3),
            text: Color32::from_rgb(0x5A, 0x62, 0x70),
        },
        danger_fill: Color32::from_rgb(0xC2, 0x36, 0x2F),
        // 设计稿：rgba(21, 24, 29, 0.14)，这里是预乘后的值
        shadow: Color32::from_rgba_premultiplied(3, 3, 4, 36),
    };

    /// 深色。设计稿只画了深色的工具条（底 #111318、字 #ECEEF1、直连 #3CC5B8、中继 #F0A44B），
    /// 其余按浅色的角色一一对应配出来。
    pub const DARK: Self = Self {
        dark: true,
        bg: Color32::from_rgb(0x11, 0x13, 0x18),
        surface: Color32::from_rgb(0x1A, 0x1D, 0x23),
        ink: Color32::from_rgb(0xEC, 0xEE, 0xF1),
        on_ink: Color32::from_rgb(0x15, 0x18, 0x1D),
        muted: Color32::from_rgb(0xA9, 0xB0, 0xBB),
        disabled: Color32::from_rgb(0x8D, 0x95, 0xA1),
        line: Color32::from_rgb(0x30, 0x35, 0x3E),
        line_strong: Color32::from_rgb(0x4B, 0x52, 0x5E),
        line_soft: Color32::from_rgb(0x25, 0x29, 0x31),
        hover: Color32::from_rgb(0x27, 0x2B, 0x33),
        row_hover: Color32::from_rgb(0x20, 0x24, 0x2B),
        track: Color32::from_rgb(0x2A, 0x2F, 0x38),
        direct: SignalColors {
            solid: Color32::from_rgb(0x3C, 0xC5, 0xB8),
            soft: Color32::from_rgb(0x12, 0x33, 0x31),
            text: Color32::from_rgb(0x6F, 0xD9, 0xCF),
        },
        relay: SignalColors {
            solid: Color32::from_rgb(0xF0, 0xA4, 0x4B),
            soft: Color32::from_rgb(0x3A, 0x2A, 0x14),
            text: Color32::from_rgb(0xF5, 0xBD, 0x7A),
        },
        problem: SignalColors {
            solid: Color32::from_rgb(0xF0, 0x78, 0x6F),
            soft: Color32::from_rgb(0x3D, 0x1B, 0x19),
            text: Color32::from_rgb(0xF7, 0xA0, 0x99),
        },
        idle: SignalColors {
            solid: Color32::from_rgb(0x7D, 0x85, 0x91),
            soft: Color32::from_rgb(0x27, 0x2B, 0x33),
            text: Color32::from_rgb(0xA9, 0xB0, 0xBB),
        },
        danger_fill: Color32::from_rgb(0xC2, 0x36, 0x2F),
        shadow: Color32::from_rgba_premultiplied(0, 0, 0, 110),
    };

    /// 当前（跟随系统的浅色/深色）。
    #[must_use]
    pub fn of(ctx: &egui::Context) -> Self {
        match ctx.theme() {
            Theme::Dark => Self::DARK,
            Theme::Light => Self::LIGHT,
        }
    }

    /// 某种连接状态的颜色。
    #[must_use]
    pub const fn signal(&self, signal: Signal) -> SignalColors {
        match signal {
            Signal::Direct => self.direct,
            Signal::Relay => self.relay,
            Signal::Problem => self.problem,
            Signal::Idle => self.idle,
        }
    }

    /// 文字链接和行内文字按钮（取消、重试）的颜色：和设计稿一样用直连的青绿。
    #[must_use]
    pub const fn link(&self) -> Color32 {
        if self.dark {
            self.direct.solid
        } else {
            self.direct.text
        }
    }
}

/// 两个颜色的对比度（WCAG 2.x 的相对亮度公式），1 到 21。
#[must_use]
pub fn contrast(a: Color32, b: Color32) -> f32 {
    fn luminance(c: Color32) -> f32 {
        let channel = |v: u8| {
            let v = f32::from(v) / 255.0;
            if v <= 0.040_45 {
                v / 12.92
            } else {
                ((v + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * channel(c.r()) + 0.7152 * channel(c.g()) + 0.0722 * channel(c.b())
    }
    let (la, lb) = (luminance(a), luminance(b));
    (la.max(lb) + 0.05) / (la.min(lb) + 0.05)
}

/// 内边距（左右、上下）。
#[must_use]
pub fn pad(x: f32, y: f32) -> Margin {
    MarginF32::symmetric(x, y).into()
}

/// 某个配色下的 egui 外观。
#[must_use]
pub fn visuals(p: &Palette) -> Visuals {
    let mut v = if p.dark {
        Visuals::dark()
    } else {
        Visuals::light()
    };
    let line = Stroke::new(size::LINE, p.line);
    v.panel_fill = p.bg;
    v.window_fill = p.surface;
    v.window_stroke = line;
    v.window_corner_radius = CornerRadius::from(radius::PANEL);
    v.menu_corner_radius = CornerRadius::from(radius::PANEL);
    let shadow = Shadow {
        offset: [0, 8],
        blur: 24,
        spread: 0,
        color: p.shadow,
    };
    v.window_shadow = shadow;
    v.popup_shadow = shadow;
    v.extreme_bg_color = p.surface;
    v.faint_bg_color = p.row_hover;
    v.hyperlink_color = p.link();
    v.warn_fg_color = p.relay.text;
    v.error_fg_color = p.problem.text;
    // 选中的文字和选中的选项：灰底墨字（不用颜色，颜色留给连接状态）
    v.selection.bg_fill = p.track;
    v.selection.stroke = Stroke::new(size::LINE, p.ink);
    // 不用 override_text_color：它会盖过各种状态下的文字颜色。文字颜色按控件状态给。
    v.widgets.noninteractive.bg_stroke = line;
    v.widgets.noninteractive.bg_fill = p.surface;
    v.widgets.noninteractive.weak_bg_fill = p.surface;
    // 普通按钮：白底细边框；停留时变灰；按下、键盘焦点时边框变成墨色
    v.widgets.inactive.weak_bg_fill = p.surface;
    v.widgets.inactive.bg_fill = p.track;
    v.widgets.inactive.bg_stroke = line;
    v.widgets.hovered.weak_bg_fill = p.hover;
    v.widgets.hovered.bg_fill = p.hover;
    v.widgets.hovered.bg_stroke = Stroke::new(size::LINE, p.line_strong);
    v.widgets.active.weak_bg_fill = p.track;
    v.widgets.active.bg_fill = p.track;
    v.widgets.active.bg_stroke = Stroke::new(size::LINE_STRONG, p.ink);
    v.widgets.open.weak_bg_fill = p.hover;
    v.widgets.open.bg_fill = p.hover;
    v.widgets.open.bg_stroke = Stroke::new(size::LINE, p.line_strong);
    for w in [
        &mut v.widgets.noninteractive,
        &mut v.widgets.inactive,
        &mut v.widgets.hovered,
        &mut v.widgets.active,
        &mut v.widgets.open,
    ] {
        w.corner_radius = CornerRadius::from(radius::CONTROL);
        w.fg_stroke = Stroke::new(size::LINE, p.ink);
        w.expansion = 0.0;
    }
    v.text_cursor.stroke = Stroke::new(size::LINE_STRONG, p.ink);
    // 能点的东西都显示手形指针
    v.interact_cursor = Some(egui::CursorIcon::PointingHand);
    v
}

/// 外观：跟随系统、浅色、深色。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Appearance {
    /// 跟随系统。
    System,
    /// 浅色。
    Light,
    /// 深色。
    Dark,
}

/// 外观设置存在数据目录的这个文件里。
pub const APPEARANCE_FILE: &str = "appearance.txt";

impl Appearance {
    /// 读取保存的外观（没有或读不懂时跟随系统）。
    #[must_use]
    pub fn load(dir: &std::path::Path) -> Self {
        match std::fs::read_to_string(dir.join(APPEARANCE_FILE))
            .unwrap_or_default()
            .trim()
        {
            "light" => Self::Light,
            "dark" => Self::Dark,
            _ => Self::System,
        }
    }

    /// 保存。
    pub fn save(self, dir: &std::path::Path) -> std::io::Result<()> {
        let text = match self {
            Self::System => "system",
            Self::Light => "light",
            Self::Dark => "dark",
        };
        std::fs::create_dir_all(dir)?;
        std::fs::write(dir.join(APPEARANCE_FILE), text)
    }

    /// 应用到界面。
    pub fn apply(self, ctx: &egui::Context) {
        ctx.set_theme(match self {
            Self::System => egui::ThemePreference::System,
            Self::Light => egui::ThemePreference::Light,
            Self::Dark => egui::ThemePreference::Dark,
        });
    }
}

/// 把字体、两套配色、间距和字号装进界面；之后跟随系统的浅色/深色自动切换。
/// 返回字体的安装结果（没找到系统中文字体时，调用方要告诉用户）。
pub fn install(ctx: &egui::Context) -> fonts::Installed {
    let (defs, installed) = fonts::definitions();
    ctx.set_fonts(defs);
    ctx.set_visuals_of(Theme::Light, visuals(&Palette::LIGHT));
    ctx.set_visuals_of(Theme::Dark, visuals(&Palette::DARK));
    ctx.all_styles_mut(|style| {
        style.spacing.item_spacing = Vec2::splat(space::S8);
        style.spacing.button_padding = Vec2::new(space::S14, space::S6);
        style.spacing.interact_size.y = size::CONTROL_SMALL;
        style.spacing.text_edit_width = size::INPUT_WIDTH;
        style.spacing.menu_margin = pad(space::S6, space::S6);
        // 滚动条：细的，浮在内容上面，不滚动时不占地方
        style.spacing.scroll = egui::style::ScrollStyle::thin();
        style.animation_time = motion::FAST;
        for (style_name, size, weight) in [
            (egui::TextStyle::Small, font::CAPTION, Weight::Regular),
            (egui::TextStyle::Body, font::BODY, Weight::Regular),
            (egui::TextStyle::Button, font::BODY, Weight::Regular),
            (egui::TextStyle::Monospace, font::BODY, Weight::Regular),
            (egui::TextStyle::Heading, font::TITLE, Weight::Semibold),
        ] {
            style.text_styles.insert(style_name, font_id(size, weight));
        }
    });
    installed
}

/// 面板：承载一组内容的白底区域（深色时深灰），细边框、圆角。
pub fn panel<R>(ui: &mut Ui, p: &Palette, add: impl FnOnce(&mut Ui) -> R) -> R {
    Frame::new()
        .fill(p.surface)
        .stroke(Stroke::new(size::LINE, p.line))
        .corner_radius(radius::PANEL)
        .inner_margin(pad(space::S28, space::S24))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            add(ui)
        })
        .inner
}

/// 提示条：某种连接状态的浅底加同色细边，里面说清楚发生了什么、怎么办。
pub fn notice<R>(
    ui: &mut Ui,
    p: &Palette,
    signal: Signal,
    add: impl FnOnce(&mut Ui, SignalColors) -> R,
) -> R {
    let colors = p.signal(signal);
    Frame::new()
        .fill(colors.soft)
        .stroke(Stroke::new(size::LINE, colors.solid))
        .corner_radius(radius::PANEL)
        .inner_margin(pad(space::S16, space::S12))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            add(ui, colors)
        })
        .inner
}

/// 一段内容的标题，右侧可以放东西。
/// 先放右侧的东西，标题用剩下的宽度、放不下时截断加省略号（电脑名很长时不压住右侧的状态）。
pub fn section_title(ui: &mut Ui, title: &str, right: impl FnOnce(&mut Ui)) {
    ui.horizontal(|ui| {
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            right(ui);
            ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                ui.add(egui::Label::new(text(title, font::SUBTITLE, Weight::Semibold)).truncate())
                    .on_hover_text(title);
            });
        });
    });
    ui.add_space(space::S6);
}

/// 页面标题。
pub fn page_title(ui: &mut Ui, title: &str) -> Response {
    ui.label(text(title, font::TITLE, Weight::Semibold))
}

/// 次要文字。
pub fn muted(ui: &mut Ui, p: &Palette, content: &str) -> Response {
    ui.label(text(content, font::SMALL, Weight::Regular).color(p.muted))
}

/// 开关。`label` 给读屏软件念（开关旁边显示的文字由调用方自己放）。返回值在切换时 `changed()`。
pub fn toggle(ui: &mut Ui, p: &Palette, on: &mut bool, label: &str, enabled: bool) -> Response {
    let sense = if enabled {
        Sense::click()
    } else {
        Sense::hover()
    };
    let (rect, mut response) = ui.allocate_exact_size(Vec2::from(size::TOGGLE), sense);
    if response.clicked() {
        *on = !*on;
        response.mark_changed();
    }
    let state = *on;
    response.widget_info(|| WidgetInfo::selected(WidgetType::Checkbox, enabled, state, label));
    let t = ui.ctx().animate_bool(response.id, *on);
    // 开：墨色的槽；关：灰色的槽。不能点时整体变淡。
    let mut track = lerp(p.line_strong, p.ink, t);
    let mut knob = lerp(p.surface, p.on_ink, t);
    if !enabled {
        track = lerp(p.surface, track, 0.5);
        knob = lerp(track, knob, 0.5);
    }
    let radius = rect.height() / 2.0;
    ui.painter().rect_filled(rect, radius, track);
    let x = egui::lerp((rect.left() + radius)..=(rect.right() - radius), t);
    ui.painter().circle_filled(
        egui::pos2(x, rect.center().y),
        radius - size::TOGGLE_INSET,
        knob,
    );
    if response.has_focus() {
        ui.painter().rect_stroke(
            rect.expand(size::LINE_STRONG),
            radius + size::LINE_STRONG,
            Stroke::new(size::LINE_STRONG, p.ink),
            StrokeKind::Outside,
        );
    }
    response
}

fn lerp(a: Color32, b: Color32, t: f32) -> Color32 {
    let mix = |x: u8, y: u8| {
        let v = f32::from(x) + (f32::from(y) - f32::from(x)) * t.clamp(0.0, 1.0);
        // 夹在 0..=255 之内
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let v = v.round() as u8;
        v
    };
    Color32::from_rgb(mix(a.r(), b.r()), mix(a.g(), b.g()), mix(a.b(), b.b()))
}

/// 状态圆点。没有连接（离线、关闭）时画空心的：不只靠颜色区分。
pub fn paint_dot(painter: &Painter, center: Pos2, p: &Palette, signal: Signal) {
    let color = p.signal(signal).solid;
    let r = size::DOT / 2.0;
    match signal {
        Signal::Direct | Signal::Relay | Signal::Problem => {
            painter.circle_filled(center, r, color);
        }
        Signal::Idle => {
            painter.circle_stroke(
                center,
                r - size::DOT_RING / 2.0,
                Stroke::new(size::DOT_RING, color),
            );
        }
    }
}

/// 状态标签（胶囊）：圆点加文字，浅色底。整个标签自己画，宽度贴合内容，
/// 在右对齐的布局里也不会反过来或被拉宽。
pub fn pill(ui: &mut Ui, p: &Palette, label: &str, signal: Signal) -> Response {
    let colors = p.signal(signal);
    let galley = ui.painter().layout_no_wrap(
        label.to_owned(),
        font_id(font::BODY, Weight::Medium),
        colors.text,
    );
    let size = Vec2::new(
        space::S12 + size::DOT + space::S8 + galley.size().x + space::S12,
        galley.size().y + 2.0 * space::S4,
    );
    let (rect, response) = ui.allocate_exact_size(size, Sense::hover());
    response.widget_info(|| WidgetInfo::labeled(WidgetType::Label, true, label));
    let painter = ui.painter();
    painter.rect_filled(rect, radius::PILL, colors.soft);
    let dot = egui::pos2(rect.left() + space::S12 + size::DOT / 2.0, rect.center().y);
    paint_dot(painter, dot, p, signal);
    let at = egui::pos2(
        dot.x + size::DOT / 2.0 + space::S8,
        rect.center().y - galley.size().y / 2.0,
    );
    painter.galley(at, galley, colors.text);
    response
}

/// 控件的三档大小。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControlSize {
    /// 连接表单、设备码旁边。
    Large,
    /// 表格行、工具栏。
    Medium,
    /// 行内。
    Small,
}

impl ControlSize {
    /// 高度。
    #[must_use]
    pub const fn height(self) -> f32 {
        match self {
            Self::Large => size::CONTROL_LARGE,
            Self::Medium => size::CONTROL,
            Self::Small => size::CONTROL_SMALL,
        }
    }

    /// 文字按钮左右的内边距。
    #[must_use]
    pub const fn padding(self) -> f32 {
        match self {
            Self::Large => space::S20,
            Self::Medium => space::S14,
            Self::Small => space::S12,
        }
    }

    /// 字号。
    #[must_use]
    pub const fn font(self) -> f32 {
        match self {
            Self::Large | Self::Medium => font::BODY,
            Self::Small => font::SMALL,
        }
    }
}

/// 按指定的大小放一个按钮。不能点时保持不透明（egui 默认把整个控件变淡，白字会看不清），
/// 由调用方给出不能点时的颜色。
fn add_button(
    ui: &mut Ui,
    button: egui::Button<'_>,
    min: Vec2,
    padding: f32,
    enabled: bool,
) -> Response {
    let old_padding = ui.spacing().button_padding;
    let old_alpha = ui.visuals().disabled_alpha;
    ui.spacing_mut().button_padding = Vec2::new(padding, 0.0);
    ui.visuals_mut().disabled_alpha = 1.0;
    let response = ui.add_enabled(enabled, button.min_size(min));
    ui.spacing_mut().button_padding = old_padding;
    ui.visuals_mut().disabled_alpha = old_alpha;
    response
}

/// 鼠标停在实心按钮上时盖一层半透明的字色，让按钮看得出“能点”。
fn hover_wash(ui: &Ui, response: &Response, on: Color32) {
    if response.hovered() && response.enabled() {
        ui.painter()
            .rect_filled(response.rect, radius::CONTROL, on.gamma_multiply(0.12));
    }
}

/// 主按钮：墨色底、反色字。一个区域里只放一个。
pub fn primary_button(
    ui: &mut Ui,
    p: &Palette,
    label: &str,
    size: ControlSize,
    enabled: bool,
) -> Response {
    let (fill, color) = if enabled {
        (p.ink, p.on_ink)
    } else {
        (p.track, p.disabled)
    };
    let button = egui::Button::new(text(label, size.font(), Weight::Semibold).color(color))
        .fill(fill)
        .stroke(Stroke::NONE)
        .corner_radius(radius::CONTROL);
    let response = add_button(
        ui,
        button,
        Vec2::new(0.0, size.height()),
        size.padding(),
        enabled,
    );
    hover_wash(ui, &response, p.on_ink);
    response
}

/// 次要按钮：面色底、细边框、墨色字。
pub fn secondary_button(
    ui: &mut Ui,
    p: &Palette,
    label: &str,
    size: ControlSize,
    enabled: bool,
) -> Response {
    let mut button =
        egui::Button::new(text(label, size.font(), Weight::Regular).color(if enabled {
            p.ink
        } else {
            p.disabled
        }))
        .corner_radius(radius::CONTROL);
    if !enabled {
        button = button.fill(p.bg).stroke(Stroke::new(size::LINE, p.line));
    }
    add_button(
        ui,
        button,
        Vec2::new(0.0, size.height()),
        size.padding(),
        enabled,
    )
}

/// 危险操作按钮（结束会话、断开）：红底白字。
pub fn danger_button(ui: &mut Ui, p: &Palette, label: &str, size: ControlSize) -> Response {
    let button = egui::Button::new(text(label, size.font(), Weight::Semibold).color(ON_DANGER))
        .fill(p.danger_fill)
        .stroke(Stroke::NONE)
        .corner_radius(radius::CONTROL);
    let response = add_button(
        ui,
        button,
        Vec2::new(0.0, size.height()),
        size.padding(),
        true,
    );
    hover_wash(ui, &response, ON_DANGER);
    response
}

/// 文字按钮（取消、重试、去设置）：没有底和边框，链接色。
pub fn link_button(ui: &mut Ui, p: &Palette, label: &str) -> Response {
    let button =
        egui::Button::new(text(label, font::BODY, Weight::Medium).color(p.link())).frame(false);
    add_button(
        ui,
        button,
        Vec2::new(0.0, size::CONTROL_SMALL),
        space::S10,
        true,
    )
}

/// 只有图标的方形按钮，细边框。`tip` 是鼠标停留时的说明，也是读屏软件念的名字。
pub fn icon_button(
    ui: &mut Ui,
    p: &Palette,
    glyph: &str,
    tip: &str,
    size: ControlSize,
) -> Response {
    let button = egui::Button::new(text(glyph, font::ICON, Weight::Regular).color(p.ink))
        .corner_radius(radius::CONTROL);
    let response = add_button(ui, button, Vec2::splat(size.height()), 0.0, true);
    response.widget_info(|| WidgetInfo::labeled(WidgetType::Button, true, tip));
    response.on_hover_text(tip)
}

/// 侧边栏的导航项：图标加文字。选中的是灰底墨字，没选中的是次要文字色。
pub fn nav_item(ui: &mut Ui, p: &Palette, glyph: &str, label: &str, selected: bool) -> Response {
    let (fill, color, weight) = if selected {
        (p.hover, p.ink, Weight::Medium)
    } else {
        (Color32::TRANSPARENT, p.muted, Weight::Regular)
    };
    let mut job = egui::text::LayoutJob::default();
    let part = |size: f32, weight: Weight| egui::TextFormat {
        font_id: font_id(size, weight),
        color,
        valign: Align::Center,
        ..Default::default()
    };
    job.append(glyph, 0.0, part(font::ICON, Weight::Regular));
    job.append(label, space::S10, part(font::BODY, weight));
    let width = ui.available_width();
    let button = egui::Button::new(job)
        .fill(fill)
        .stroke(Stroke::NONE)
        .corner_radius(radius::CONTROL)
        .selected(false);
    let response = ui
        .with_layout(Layout::top_down(Align::LEFT), |ui| {
            add_button(
                ui,
                button,
                Vec2::new(width, size::NAV_ITEM),
                space::S10,
                true,
            )
        })
        .inner;
    response.widget_info(|| WidgetInfo::selected(WidgetType::Button, true, selected, label));
    response
}

/// 大号输入框（连接表单、设置里的地址）。
pub fn big_input(
    ui: &mut Ui,
    p: &Palette,
    value: &mut String,
    hint: &str,
    secret: bool,
) -> Response {
    // 输入框的边框比面板的线重一档
    let old = ui.visuals().widgets.inactive.bg_stroke;
    ui.visuals_mut().widgets.inactive.bg_stroke = Stroke::new(size::LINE, p.line_strong);
    let response = ui.add(
        egui::TextEdit::singleline(value)
            .hint_text(RichText::new(hint).color(p.muted))
            .password(secret)
            .font(font_id(font::SUBTITLE, Weight::Regular))
            .margin(pad(space::S14, space::S10))
            .min_size(Vec2::new(0.0, size::CONTROL_LARGE))
            .desired_width(f32::INFINITY),
    );
    ui.visuals_mut().widgets.inactive.bg_stroke = old;
    response
}

/// 程序图标（窗口、任务栏）：墨色圆角方块里的白色品牌标记（两个圆环连一条线：两台电脑直连）。
/// 返回 `size × size` 的 RGBA。
#[must_use]
pub fn app_icon(size: u32) -> Vec<u8> {
    let n = f32::from(u16::try_from(size).unwrap_or(256));
    let mut out = Vec::with_capacity(usize::try_from(size * size * 4).unwrap_or(0));
    let ink = Palette::LIGHT.ink;
    let corner = n * 0.22;
    // 标记照设计稿的 24 格画法放大：圆环半径 3、圆心在 6 和 18、线宽 2，整体放大到占图标的六成
    let unit = n * 0.6 / 24.0;
    let (ring, stroke) = (3.0 * unit, 2.0 * unit);
    let (left, right, mid) = (n / 2.0 - 6.0 * unit, n / 2.0 + 6.0 * unit, n / 2.0);
    for py in 0..size {
        for px in 0..size {
            let (x, y) = (
                f32::from(u16::try_from(px).unwrap_or(0)) + 0.5,
                f32::from(u16::try_from(py).unwrap_or(0)) + 0.5,
            );
            // 圆角方块：到最近角圆心的距离
            let cx = x.clamp(corner, n - corner);
            let cy = y.clamp(corner, n - corner);
            let d = ((x - cx).powi(2) + (y - cy).powi(2)).sqrt();
            let alpha = (corner + 0.5 - d).clamp(0.0, 1.0);
            // 标记的覆盖率：两个圆环和中间的连线，取最大的（边缘半个像素内渐变）
            let ring_at = |c: f32| {
                let d = ((x - c).powi(2) + (y - mid).powi(2)).sqrt();
                (stroke / 2.0 + 0.5 - (d - ring).abs()).clamp(0.0, 1.0)
            };
            let bar_x = (0.5 + (x - (left + ring)).min((right - ring) - x)).clamp(0.0, 1.0);
            let bar = bar_x * (stroke / 2.0 + 0.5 - (y - mid).abs()).clamp(0.0, 1.0);
            let white = ring_at(left).max(ring_at(right)).max(bar);
            out.extend_from_slice(&[
                lerp(ink, Color32::WHITE, white).r(),
                lerp(ink, Color32::WHITE, white).g(),
                lerp(ink, Color32::WHITE, white).b(),
                unit_to_u8(alpha),
            ]);
        }
    }
    out
}

/// 0..=1 换成 0..=255。
fn unit_to_u8(v: f32) -> u8 {
    // 0..=1 乘 255，在 u8 范围内
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let v = (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    v
}

#[cfg(test)]
mod tests {
    #![allow(clippy::indexing_slicing)]
    use super::*;

    /// 在无界面的 egui 里跑一帧。
    fn frame(dark: bool, f: impl FnMut(&mut Ui)) {
        let ctx = egui::Context::default();
        let _ = install(&ctx);
        ctx.set_theme(if dark { Theme::Dark } else { Theme::Light });
        let mut f = f;
        let _ = ctx.run(egui::RawInput::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| f(ui));
        });
    }

    #[test]
    fn palettes_follow_the_theme_and_widgets_render() {
        for dark in [false, true] {
            frame(dark, |ui| {
                let p = Palette::of(ui.ctx());
                assert_eq!(p.dark, dark);
                assert_eq!(ui.visuals().panel_fill, p.bg);
                let _ = page_title(ui, "设备");
                panel(ui, &p, |ui| {
                    section_title(ui, "这台电脑", |ui| {
                        for signal in [Signal::Direct, Signal::Relay, Signal::Problem, Signal::Idle]
                        {
                            let _ = pill(ui, &p, "等别人连接", signal);
                        }
                    });
                    let _ = muted(ui, &p, "说明");
                    let mut on = false;
                    let _ = toggle(ui, &p, &mut on, "允许别人连接", true);
                    let _ = toggle(ui, &p, &mut on, "允许别人连接", false);
                    for size in [ControlSize::Large, ControlSize::Medium, ControlSize::Small] {
                        let _ = icon_button(ui, &p, icon::COPY, "复制", size);
                        let _ = primary_button(ui, &p, "连接", size, true);
                        let _ = primary_button(ui, &p, "连接", size, false);
                        let _ = secondary_button(ui, &p, "只传文件", size, true);
                        let _ = secondary_button(ui, &p, "只传文件", size, false);
                        let _ = danger_button(ui, &p, "结束", size);
                    }
                    let _ = link_button(ui, &p, "取消");
                    let _ = nav_item(ui, &p, icon::GEAR_SIX, "设置", true);
                    let _ = nav_item(ui, &p, icon::GEAR_SIX, "设置", false);
                    let mut value = String::new();
                    let _ = big_input(ui, &p, &mut value, "设备码", false);
                    notice(ui, &p, Signal::Relay, |ui, colors| {
                        ui.colored_label(colors.text, "正在走中继");
                    });
                });
            });
        }
    }

    /// 点一下开关就切换；不能点的开关不动。键盘焦点和鼠标停留的画法也走一遍。
    #[test]
    fn toggle_flips_on_click_and_ignores_clicks_when_disabled() {
        for (enabled, expected) in [(true, true), (false, false)] {
            let ctx = egui::Context::default();
            let _ = install(&ctx);
            let mut on = false;
            let rect = std::cell::Cell::new(egui::Rect::NOTHING);
            let run = |events: Vec<egui::Event>, on: &mut bool| {
                let input = egui::RawInput {
                    events,
                    ..Default::default()
                };
                let _ = ctx.run(input, |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| {
                        let p = Palette::of(ui.ctx());
                        let r = toggle(ui, &p, on, "允许别人连接", enabled);
                        rect.set(r.rect);
                        r.request_focus();
                        let _ = primary_button(ui, &p, "连接", ControlSize::Large, true);
                    });
                });
            };
            run(Vec::new(), &mut on);
            let pos = rect.get().center();
            let button = |pressed| egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: egui::Modifiers::NONE,
            };
            run(vec![egui::Event::PointerMoved(pos), button(true)], &mut on);
            run(vec![button(false)], &mut on);
            assert_eq!(on, expected, "enabled={enabled}");
        }
    }

    /// 鼠标停在主按钮上时盖一层浅色。
    #[test]
    fn solid_buttons_show_hover() {
        let ctx = egui::Context::default();
        let _ = install(&ctx);
        let mut rect = egui::Rect::NOTHING;
        let mut hovered = false;
        for _ in 0..3 {
            let input = egui::RawInput {
                events: vec![egui::Event::PointerMoved(rect.center())],
                ..Default::default()
            };
            let _ = ctx.run(input, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    let p = Palette::of(ui.ctx());
                    let r = primary_button(ui, &p, "连接", ControlSize::Large, true);
                    rect = r.rect;
                    hovered = r.hovered();
                });
            });
        }
        assert!(hovered);
    }

    /// 文字和它的底色对比度不低于 4.5:1（WCAG AA），两套配色都要过。
    #[test]
    fn text_contrast_is_at_least_aa_in_both_palettes() {
        assert!((contrast(Color32::BLACK, Color32::WHITE) - 21.0).abs() < 0.01);
        assert!((contrast(Color32::WHITE, Color32::WHITE) - 1.0).abs() < 0.01);
        for p in [Palette::LIGHT, Palette::DARK] {
            let mut pairs = vec![
                ("墨/底", p.ink, p.bg),
                ("墨/面", p.ink, p.surface),
                ("墨/停留", p.ink, p.hover),
                ("墨/槽", p.ink, p.track),
                ("主按钮", p.on_ink, p.ink),
                ("次要/底", p.muted, p.bg),
                ("次要/面", p.muted, p.surface),
                ("次要/停留", p.muted, p.hover),
                ("次要/槽", p.muted, p.track),
                ("链接/底", p.link(), p.bg),
                ("链接/面", p.link(), p.surface),
                ("危险按钮", ON_DANGER, p.danger_fill),
            ];
            for signal in [Signal::Direct, Signal::Relay, Signal::Problem, Signal::Idle] {
                let c = p.signal(signal);
                pairs.push(("状态标签", c.text, c.soft));
                pairs.push(("状态文字/面", c.text, p.surface));
                pairs.push(("状态文字/底", c.text, p.bg));
            }
            for (what, fg, bg) in pairs {
                let ratio = contrast(fg, bg);
                assert!(ratio >= 4.5, "dark={} {what}: {ratio:.2}", p.dark);
            }
            // 不能点的控件不在 4.5:1 的要求之内（WCAG 1.4.3 把停用的控件排除在外），
            // 设计稿的取值在底色上是 4.4:1；这里只要求还认得出字（3:1）
            for bg in [p.bg, p.track] {
                assert!(contrast(p.disabled, bg) >= 3.0, "dark={} 不能点", p.dark);
            }
            // 圆点、图标这类图形要和底色分得开（WCAG 1.4.11：3:1）
            for signal in [Signal::Direct, Signal::Relay, Signal::Problem, Signal::Idle] {
                let ratio = contrast(p.signal(signal).solid, p.surface);
                assert!(ratio >= 3.0, "dark={} {signal:?}: {ratio:.2}", p.dark);
            }
        }
    }

    #[test]
    fn tokens_are_consistent() {
        assert_eq!(Weight::Regular.family(), FontFamily::Proportional);
        assert_ne!(Weight::Medium.family(), Weight::Semibold.family());
        assert_eq!(font_id(font::BODY, Weight::Regular).size, font::BODY);
        assert!(ControlSize::Large.height() > ControlSize::Medium.height());
        assert!(ControlSize::Medium.height() > ControlSize::Small.height());
        assert!(ControlSize::Large.padding() > ControlSize::Small.padding());
        assert!(ControlSize::Medium.font() > ControlSize::Small.font());
        assert_eq!(pad(space::S28, space::S24), Margin::symmetric(28, 24));
        assert_eq!(Palette::LIGHT.signal(Signal::Relay), Palette::LIGHT.relay);
        assert_ne!(Palette::LIGHT.link(), Palette::DARK.link());
        assert!(icon::ALL.contains(&icon::COPY));
    }

    #[test]
    fn appearance_is_saved_and_applied() {
        let dir = std::env::temp_dir().join(format!("peerdesk-appearance-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(Appearance::load(&dir), Appearance::System);
        for a in [Appearance::Light, Appearance::Dark, Appearance::System] {
            a.save(&dir).unwrap();
            assert_eq!(Appearance::load(&dir), a);
        }
        let ctx = egui::Context::default();
        Appearance::Dark.apply(&ctx);
        assert_eq!(ctx.theme(), Theme::Dark);
        Appearance::Light.apply(&ctx);
        assert_eq!(ctx.theme(), Theme::Light);
        Appearance::System.apply(&ctx);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn icon_is_an_ink_tile_with_the_white_peer_mark() {
        let size = 64;
        let px = app_icon(size);
        assert_eq!(px.len(), 64 * 64 * 4);
        let at = |x: usize, y: usize| &px[(y * 64 + x) * 4..(y * 64 + x) * 4 + 4];
        assert_eq!(at(0, 0)[3], 0, "圆角外透明");
        assert_eq!(at(32, 5), [0x15, 0x18, 0x1D, 255], "墨色");
        assert_eq!(at(32, 32), [255, 255, 255, 255], "连线");
        assert_eq!(at(22, 32), [0x15, 0x18, 0x1D, 255], "左圆环的圆心是空的");
        assert_eq!(at(17, 32), [255, 255, 255, 255], "左圆环");
        assert_eq!(unit_to_u8(2.0), 255);
    }

    #[test]
    fn colors_blend_and_palettes_differ() {
        assert_eq!(lerp(Color32::BLACK, Color32::WHITE, 0.0), Color32::BLACK);
        assert_eq!(lerp(Color32::BLACK, Color32::WHITE, 1.0), Color32::WHITE);
        assert_eq!(lerp(Color32::BLACK, Color32::WHITE, 5.0), Color32::WHITE);
        assert_ne!(Palette::LIGHT.bg, Palette::DARK.bg);
        assert!(!visuals(&Palette::LIGHT).dark_mode);
        assert!(visuals(&Palette::DARK).dark_mode);
    }
}
