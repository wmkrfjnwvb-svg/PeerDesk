//! 界面主题：配色（亮色、暗色两套，跟随系统）、全局样式，以及几个统一外观的小部件
//! （卡片、开关、状态标签、图标按钮、主按钮、导航项）。图标用 Phosphor（MIT）。

use eframe::egui::{
    self, Align, Color32, CornerRadius, FontId, Frame, Layout, Margin, Response, RichText, Sense,
    Stroke, Theme, Ui, Vec2, Visuals, Widget,
};

pub use egui_phosphor::regular as icon;

/// 一套配色。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Palette {
    /// 是否暗色。
    pub dark: bool,
    /// 窗口底色。
    pub bg: Color32,
    /// 侧边栏。
    pub sidebar: Color32,
    /// 卡片。
    pub card: Color32,
    /// 卡片和输入框的边框。
    pub border: Color32,
    /// 输入框底色。
    pub input: Color32,
    /// 正文。
    pub text: Color32,
    /// 次要文字。
    pub muted: Color32,
    /// 品牌色（主按钮、选中项、链接）。
    pub accent: Color32,
    /// 品牌色的浅底（选中的导航项、标签底色）。
    pub accent_soft: Color32,
    /// 成功、在线。
    pub success: Color32,
    /// 警告、等待。
    pub warning: Color32,
    /// 错误、断开。
    pub danger: Color32,
}

/// 品牌蓝。
const ACCENT: Color32 = Color32::from_rgb(0x3B, 0x6C, 0xF6);

impl Palette {
    /// 亮色。
    pub const LIGHT: Self = Self {
        dark: false,
        bg: Color32::from_rgb(0xF4, 0xF6, 0xFA),
        sidebar: Color32::from_rgb(0xFF, 0xFF, 0xFF),
        card: Color32::from_rgb(0xFF, 0xFF, 0xFF),
        border: Color32::from_rgb(0xE2, 0xE6, 0xEE),
        input: Color32::from_rgb(0xF6, 0xF8, 0xFB),
        text: Color32::from_rgb(0x1B, 0x23, 0x30),
        muted: Color32::from_rgb(0x6B, 0x76, 0x88),
        accent: ACCENT,
        accent_soft: Color32::from_rgb(0xE8, 0xEF, 0xFE),
        success: Color32::from_rgb(0x1F, 0x9D, 0x63),
        warning: Color32::from_rgb(0xD9, 0x8E, 0x04),
        danger: Color32::from_rgb(0xE0, 0x43, 0x4B),
    };

    /// 暗色。
    pub const DARK: Self = Self {
        dark: true,
        bg: Color32::from_rgb(0x11, 0x14, 0x1A),
        sidebar: Color32::from_rgb(0x17, 0x1B, 0x22),
        card: Color32::from_rgb(0x1B, 0x20, 0x29),
        border: Color32::from_rgb(0x2B, 0x31, 0x3D),
        input: Color32::from_rgb(0x14, 0x18, 0x1F),
        text: Color32::from_rgb(0xE6, 0xE9, 0xEF),
        muted: Color32::from_rgb(0x8B, 0x94, 0xA5),
        accent: Color32::from_rgb(0x5B, 0x86, 0xFF),
        accent_soft: Color32::from_rgb(0x20, 0x2B, 0x45),
        success: Color32::from_rgb(0x34, 0xC2, 0x80),
        warning: Color32::from_rgb(0xF0, 0xA8, 0x20),
        danger: Color32::from_rgb(0xF0, 0x5A, 0x62),
    };

    /// 当前（跟随系统的亮色/暗色）。
    #[must_use]
    pub fn of(ctx: &egui::Context) -> Self {
        match ctx.theme() {
            Theme::Dark => Self::DARK,
            Theme::Light => Self::LIGHT,
        }
    }
}

/// 某个配色下的 egui 外观。
#[must_use]
pub fn visuals(p: &Palette) -> Visuals {
    let mut v = if p.dark {
        Visuals::dark()
    } else {
        Visuals::light()
    };
    let radius = CornerRadius::same(8);
    v.panel_fill = p.bg;
    v.window_fill = p.card;
    v.window_stroke = Stroke::new(1.0_f32, p.border);
    v.window_corner_radius = CornerRadius::same(12);
    v.extreme_bg_color = p.input;
    v.faint_bg_color = p.input;
    v.hyperlink_color = p.accent;
    v.selection.bg_fill = p.accent;
    v.selection.stroke = Stroke::new(1.0_f32, Color32::WHITE);
    // 不用 override_text_color：它会盖过选中项的白字（蓝底黑字）。文字颜色按控件状态给。
    v.widgets.noninteractive.bg_stroke = Stroke::new(1.0_f32, p.border);
    v.widgets.noninteractive.fg_stroke = Stroke::new(1.0_f32, p.text);
    v.widgets.inactive.fg_stroke = Stroke::new(1.0_f32, p.text);
    v.widgets.hovered.fg_stroke = Stroke::new(1.5_f32, p.text);
    v.widgets.active.fg_stroke = Stroke::new(2.0_f32, p.text);
    v.widgets.open.fg_stroke = Stroke::new(1.0_f32, p.text);
    for w in [
        &mut v.widgets.inactive,
        &mut v.widgets.hovered,
        &mut v.widgets.active,
        &mut v.widgets.open,
    ] {
        w.corner_radius = radius;
    }
    v.widgets.inactive.weak_bg_fill = p.input;
    v.widgets.inactive.bg_fill = p.input;
    v.widgets.inactive.bg_stroke = Stroke::new(1.0_f32, p.border);
    v.widgets.hovered.weak_bg_fill = p.accent_soft;
    v.widgets.hovered.bg_fill = p.accent_soft;
    v.widgets.hovered.bg_stroke = Stroke::new(1.0_f32, p.accent);
    v.widgets.active.weak_bg_fill = p.accent_soft;
    v.widgets.active.bg_stroke = Stroke::new(1.0_f32, p.accent);
    v.text_cursor.stroke = Stroke::new(2.0_f32, p.accent);
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

/// 把两套配色和间距装进界面；之后跟随系统的亮色/暗色自动切换。
pub fn install(ctx: &egui::Context) {
    ctx.set_visuals_of(Theme::Light, visuals(&Palette::LIGHT));
    ctx.set_visuals_of(Theme::Dark, visuals(&Palette::DARK));
    ctx.all_styles_mut(|style| {
        style.spacing.item_spacing = Vec2::new(8.0, 8.0);
        style.spacing.button_padding = Vec2::new(12.0, 6.0);
        style.spacing.interact_size.y = 30.0;
        style.spacing.text_edit_width = 240.0;
        style
            .text_styles
            .insert(egui::TextStyle::Body, FontId::proportional(14.0));
        style
            .text_styles
            .insert(egui::TextStyle::Button, FontId::proportional(14.0));
        style
            .text_styles
            .insert(egui::TextStyle::Heading, FontId::proportional(20.0));
        style
            .text_styles
            .insert(egui::TextStyle::Small, FontId::proportional(12.0));
    });
}

/// 卡片：白底（暗色时深灰）、细边框、圆角。
pub fn card<R>(ui: &mut Ui, p: &Palette, add: impl FnOnce(&mut Ui) -> R) -> R {
    Frame::new()
        .fill(p.card)
        .stroke(Stroke::new(1.0_f32, p.border))
        .corner_radius(CornerRadius::same(12))
        .inner_margin(Margin::same(20))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            add(ui)
        })
        .inner
}

/// 卡片标题：图标 + 标题，右侧可以放东西。
/// 先放右侧的东西，标题用剩下的宽度、放不下时截断加省略号（电脑名很长时不压住右侧的状态）。
pub fn card_title(ui: &mut Ui, p: &Palette, glyph: &str, title: &str, right: impl FnOnce(&mut Ui)) {
    ui.horizontal(|ui| {
        ui.label(RichText::new(glyph).size(18.0).color(p.accent));
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            right(ui);
            ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                ui.add(egui::Label::new(RichText::new(title).size(16.0).strong()).truncate())
                    .on_hover_text(title);
            });
        });
    });
    ui.add_space(6.0);
}

/// 次要文字。
pub fn muted(ui: &mut Ui, p: &Palette, text: &str) -> Response {
    ui.label(RichText::new(text).size(12.5).color(p.muted))
}

/// 开关（iOS 风格）。返回值在切换时 `changed()`。
pub fn toggle(ui: &mut Ui, p: &Palette, on: &mut bool, enabled: bool) -> Response {
    let size = Vec2::new(40.0, 22.0);
    let sense = if enabled {
        Sense::click()
    } else {
        Sense::hover()
    };
    let (rect, mut response) = ui.allocate_exact_size(size, sense);
    if response.clicked() {
        *on = !*on;
        response.mark_changed();
    }
    let t = ui.ctx().animate_bool(response.id, *on);
    let off = if p.dark {
        Color32::from_rgb(0x3A, 0x41, 0x4E)
    } else {
        Color32::from_rgb(0xCB, 0xD2, 0xDD)
    };
    let mut track = lerp(off, p.accent, t);
    if !enabled {
        track = track.gamma_multiply(0.5);
    }
    let radius = rect.height() / 2.0;
    ui.painter().rect_filled(rect, radius, track);
    let x = egui::lerp((rect.left() + radius)..=(rect.right() - radius), t);
    ui.painter()
        .circle_filled(egui::pos2(x, rect.center().y), radius - 3.0, Color32::WHITE);
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

/// 状态标签：带颜色的圆点 + 文字，浅色底。圆点和文字排成一段文字，
/// 宽度贴合内容，在右对齐的布局里也不会反过来或被拉宽。
pub fn pill(ui: &mut Ui, text: &str, color: Color32) -> Response {
    let mut job = egui::text::LayoutJob::default();
    job.append(
        "● ",
        0.0,
        egui::TextFormat {
            font_id: FontId::proportional(9.0),
            color,
            valign: Align::Center,
            ..Default::default()
        },
    );
    job.append(
        text,
        0.0,
        egui::TextFormat {
            font_id: FontId::proportional(12.5),
            color,
            valign: Align::Center,
            ..Default::default()
        },
    );
    Frame::new()
        .fill(color.gamma_multiply(0.14))
        .corner_radius(CornerRadius::same(10))
        .inner_margin(Margin::symmetric(10, 3))
        .show(ui, |ui| ui.label(job))
        .response
}

/// 只有图标的小按钮（无边框），鼠标停留显示说明。
pub fn icon_button(ui: &mut Ui, glyph: &str, tip: &str) -> Response {
    egui::Button::new(RichText::new(glyph).size(16.0))
        .frame(false)
        .min_size(Vec2::new(28.0, 28.0))
        .ui(ui)
        .on_hover_text(tip)
}

/// 主按钮：品牌色填充、白字。
pub fn primary_button(ui: &mut Ui, p: &Palette, text: &str, width: f32, enabled: bool) -> Response {
    let button = egui::Button::new(
        RichText::new(text)
            .size(15.0)
            .strong()
            .color(Color32::WHITE),
    )
    .fill(if enabled {
        p.accent
    } else {
        p.accent.gamma_multiply(0.55)
    })
    .stroke(Stroke::NONE)
    .corner_radius(CornerRadius::same(10))
    // 不能点时不响应点击，但不用 add_enabled_ui：它会把白字也淡化到几乎看不见
    .sense(if enabled {
        Sense::click()
    } else {
        Sense::hover()
    });
    // add_sized 让文字居中
    ui.add_sized(Vec2::new(width, 40.0), button)
}

/// 侧边栏的导航项。
pub fn nav_item(ui: &mut Ui, p: &Palette, glyph: &str, text: &str, selected: bool) -> Response {
    let (fill, color) = if selected {
        (p.accent_soft, p.accent)
    } else {
        (Color32::TRANSPARENT, p.text)
    };
    let width = ui.available_width();
    let button = egui::Button::new(
        RichText::new(format!("{glyph}   {text}"))
            .size(14.5)
            .color(color),
    )
    .fill(fill)
    .stroke(Stroke::NONE)
    .corner_radius(CornerRadius::same(8))
    .min_size(Vec2::new(width, 38.0));
    ui.with_layout(Layout::top_down(Align::LEFT), |ui| ui.add(button))
        .inner
}

/// 大号输入框（连接表单用）。
pub fn big_input(
    ui: &mut Ui,
    p: &Palette,
    value: &mut String,
    hint: &str,
    secret: bool,
) -> Response {
    ui.add(
        egui::TextEdit::singleline(value)
            .hint_text(RichText::new(hint).color(p.muted))
            .password(secret)
            .font(FontId::proportional(16.0))
            .margin(Margin::symmetric(12, 10))
            .desired_width(f32::INFINITY),
    )
}

/// 程序图标（窗口、任务栏）：品牌蓝圆角方块里一个白色显示器。返回 `size × size` 的 RGBA。
#[must_use]
pub fn app_icon(size: u32) -> Vec<u8> {
    let n = f32::from(u16::try_from(size).unwrap_or(256));
    let mut out = Vec::with_capacity(usize::try_from(size * size * 4).unwrap_or(0));
    let radius = n * 0.22;
    // 显示器：屏幕外框、底座
    let screen = (n * 0.22, n * 0.26, n * 0.78, n * 0.64);
    let border = n * 0.06;
    let stand = (n * 0.44, n * 0.64, n * 0.56, n * 0.74);
    let base = (n * 0.34, n * 0.73, n * 0.66, n * 0.78);
    let inside =
        |r: (f32, f32, f32, f32), x: f32, y: f32| x >= r.0 && x < r.2 && y >= r.1 && y < r.3;
    for py in 0..size {
        for px in 0..size {
            let (x, y) = (
                f32::from(u16::try_from(px).unwrap_or(0)) + 0.5,
                f32::from(u16::try_from(py).unwrap_or(0)) + 0.5,
            );
            // 圆角方块：到最近角圆心的距离
            let cx = x.clamp(radius, n - radius);
            let cy = y.clamp(radius, n - radius);
            let d = ((x - cx).powi(2) + (y - cy).powi(2)).sqrt();
            let alpha = (radius + 0.5 - d).clamp(0.0, 1.0);
            let frame = inside(screen, x, y)
                && !inside(
                    (
                        screen.0 + border,
                        screen.1 + border,
                        screen.2 - border,
                        screen.3 - border,
                    ),
                    x,
                    y,
                );
            let white = frame || inside(stand, x, y) || inside(base, x, y);
            let (r, g, b) = if white {
                (255, 255, 255)
            } else {
                (0x3B, 0x6C, 0xF6)
            };
            // 0..=1 乘 255，在 u8 范围内
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            let a = (alpha * 255.0).round() as u8;
            out.extend_from_slice(&[r, g, b, a]);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    #![allow(clippy::indexing_slicing)]
    use super::*;

    /// 在无界面的 egui 里跑一帧。
    fn frame(dark: bool, f: impl FnMut(&mut Ui)) {
        let ctx = egui::Context::default();
        install(&ctx);
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
                card(ui, &p, |ui| {
                    card_title(ui, &p, icon::DESKTOP, "本设备", |ui| {
                        let _ = pill(ui, "等待连接", p.success);
                    });
                    let _ = muted(ui, &p, "说明");
                    let mut on = false;
                    let _ = toggle(ui, &p, &mut on, true);
                    let _ = toggle(ui, &p, &mut on, false);
                    let _ = icon_button(ui, icon::COPY, "复制");
                    let _ = primary_button(ui, &p, "连接", 200.0, true);
                    let _ = primary_button(ui, &p, "连接", 200.0, false);
                    let _ = nav_item(ui, &p, icon::GEAR_SIX, "设置", true);
                    let _ = nav_item(ui, &p, icon::GEAR_SIX, "设置", false);
                    let mut text = String::new();
                    let _ = big_input(ui, &p, &mut text, "设备码", false);
                });
            });
        }
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
    fn icon_has_a_transparent_corner_and_white_monitor() {
        let size = 64;
        let px = app_icon(size);
        assert_eq!(px.len(), 64 * 64 * 4);
        let at = |x: usize, y: usize| &px[(y * 64 + x) * 4..(y * 64 + x) * 4 + 4];
        assert_eq!(at(0, 0)[3], 0, "圆角外透明");
        assert_eq!(at(32, 5), [0x3B, 0x6C, 0xF6, 255], "品牌蓝");
        assert_eq!(at(16, 30), [255, 255, 255, 255], "显示器外框");
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
