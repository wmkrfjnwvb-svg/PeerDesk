//! 权限引导的示意图：用代码画一个 macOS“系统设置”窗口的线框图，箭头指向要打开的开关。
//!
//! 不用截图：截图会随系统版本、语言、浅色/深色变化，还要为每个版本各备一套；画出来的图跟着
//! 系统版本显示正确的叫法（见 [`peerdesk_platform::guide::mac_settings_names`]），浅色/深色都清楚。

use eframe::egui::{
    self, pos2, vec2, Align2, Color32, CornerRadius, FontId, Rect, Sense, Shape, Stroke,
    StrokeKind, Ui,
};
use peerdesk_platform::guide::MacSettingsNames;

use super::theme::Palette;

/// 窗口里的颜色（仿 macOS 的浅色/深色外观）。
struct Win {
    window: Color32,
    titlebar: Color32,
    sidebar: Color32,
    bar: Color32,
    text: Color32,
    toggle_off: Color32,
}

const fn win(dark: bool) -> Win {
    if dark {
        Win {
            window: Color32::from_rgb(0x26, 0x28, 0x2C),
            titlebar: Color32::from_rgb(0x2E, 0x30, 0x35),
            sidebar: Color32::from_rgb(0x1F, 0x21, 0x25),
            bar: Color32::from_rgb(0x3A, 0x3D, 0x43),
            text: Color32::from_rgb(0xE8, 0xE8, 0xEA),
            toggle_off: Color32::from_rgb(0x48, 0x4B, 0x52),
        }
    } else {
        Win {
            window: Color32::from_rgb(0xFF, 0xFF, 0xFF),
            titlebar: Color32::from_rgb(0xEC, 0xEC, 0xEF),
            sidebar: Color32::from_rgb(0xF3, 0xF3, 0xF6),
            bar: Color32::from_rgb(0xE2, 0xE2, 0xE7),
            text: Color32::from_rgb(0x1D, 0x1D, 0x1F),
            toggle_off: Color32::from_rgb(0xD5, 0xD5, 0xDA),
        }
    }
}

/// macOS 的开关。
fn mac_toggle(painter: &egui::Painter, center: egui::Pos2, on: bool, w: &Win) {
    let rect = Rect::from_center_size(center, vec2(30.0, 17.0));
    let fill = if on {
        Color32::from_rgb(0x34, 0xC7, 0x59)
    } else {
        w.toggle_off
    };
    painter.rect_filled(rect, 8.5, fill);
    let x = if on {
        rect.right() - 8.5
    } else {
        rect.left() + 8.5
    };
    painter.circle_filled(pos2(x, center.y), 7.0, Color32::WHITE);
}

/// 画“系统设置 → 隐私与安全性 → `item`”的示意图：列表里 PeerDesk 一行高亮，箭头指向它的开关。
/// `granted` 时开关是打开的、不再画箭头。`callout` 是箭头旁的提示文字，`unlock_hint` 是解锁提示。
pub fn mac_settings(
    ui: &mut Ui,
    p: &Palette,
    names: &MacSettingsNames,
    item: &str,
    granted: bool,
    callout: &str,
    unlock_hint: &str,
) {
    let w = win(p.dark);
    // 固定大小：太窄时列表和开关会挤在一起
    let width = 580.0;
    let (outer, _) = ui.allocate_exact_size(vec2(width, 290.0), Sense::hover());
    let painter = ui.painter_at(outer.expand(4.0));
    // 窗口留出右侧给箭头的提示
    let window = Rect::from_min_size(outer.min, vec2(width - 120.0, 260.0));
    painter.add(Shape::rect_filled(
        window.translate(vec2(0.0, 6.0)).expand(2.0),
        CornerRadius::same(12),
        Color32::from_black_alpha(if p.dark { 90 } else { 30 }),
    ));
    painter.rect(
        window,
        CornerRadius::same(10),
        w.window,
        Stroke::new(1.0_f32, p.line),
        StrokeKind::Inside,
    );

    // 标题栏：红黄绿三个圆点 + 程序名
    let title = Rect::from_min_size(window.min, vec2(window.width(), 30.0));
    painter.rect_filled(
        title,
        CornerRadius {
            nw: 10,
            ne: 10,
            sw: 0,
            se: 0,
        },
        w.titlebar,
    );
    for (i, c) in [(0xFF, 0x5F, 0x57), (0xFE, 0xBC, 0x2E), (0x28, 0xC8, 0x40)]
        .into_iter()
        .enumerate()
    {
        let x = title.left() + 16.0 + 18.0 * f32::from(u8::try_from(i).unwrap_or(0));
        painter.circle_filled(
            pos2(x, title.center().y),
            5.5,
            Color32::from_rgb(c.0, c.1, c.2),
        );
    }
    let _ = painter.text(
        title.center(),
        Align2::CENTER_CENTER,
        names.app,
        FontId::proportional(12.5),
        w.text,
    );

    // 左侧栏：几行占位，中间一行是“隐私与安全性”（选中）
    let sidebar = Rect::from_min_max(
        pos2(window.left(), title.bottom()),
        pos2(window.left() + 150.0, window.bottom()),
    );
    painter.rect_filled(
        sidebar,
        CornerRadius {
            nw: 0,
            ne: 0,
            sw: 10,
            se: 0,
        },
        w.sidebar,
    );
    let row_h = 26.0;
    for i in 0..6u8 {
        let y = sidebar.top() + 14.0 + f32::from(i) * row_h;
        let row = Rect::from_min_size(pos2(sidebar.left() + 10.0, y), vec2(130.0, 20.0));
        if i == 3 {
            painter.rect_filled(row, 6.0, p.ink);
            let _ = painter.text(
                row.left_center() + vec2(10.0, 0.0),
                Align2::LEFT_CENTER,
                names.section,
                FontId::proportional(11.5),
                Color32::WHITE,
            );
        } else {
            painter.circle_filled(row.left_center() + vec2(10.0, 0.0), 5.0, w.bar);
            painter.rect_filled(
                Rect::from_min_size(
                    row.left_center() + vec2(22.0, -3.5),
                    vec2(70.0 - f32::from(i) * 6.0, 7.0),
                ),
                3.5,
                w.bar,
            );
        }
    }
    // 解锁提示（12 及更早）
    if names.unlock {
        let lock = pos2(sidebar.left() + 22.0, sidebar.bottom() - 20.0);
        let _ = painter.text(
            lock,
            Align2::CENTER_CENTER,
            egui_phosphor::regular::LOCK_SIMPLE,
            FontId::proportional(16.0),
            p.relay.solid,
        );
        let _ = painter.text(
            lock + vec2(14.0, 0.0),
            Align2::LEFT_CENTER,
            unlock_hint,
            FontId::proportional(10.5),
            p.relay.solid,
        );
    }

    // 右侧：标题“屏幕录制”，说明的占位条，应用列表
    let content = Rect::from_min_max(
        pos2(sidebar.right() + 18.0, title.bottom() + 14.0),
        pos2(window.right() - 16.0, window.bottom() - 14.0),
    );
    let _ = painter.text(
        content.left_top(),
        Align2::LEFT_TOP,
        item,
        FontId::proportional(15.0),
        w.text,
    );
    painter.rect_filled(
        Rect::from_min_size(
            content.left_top() + vec2(0.0, 26.0),
            vec2(content.width() * 0.8, 7.0),
        ),
        3.5,
        w.bar,
    );
    let list = Rect::from_min_size(
        content.left_top() + vec2(0.0, 46.0),
        vec2(content.width(), 3.0 * 34.0),
    );
    painter.rect(
        list,
        8.0,
        w.window,
        Stroke::new(1.0_f32, w.bar),
        StrokeKind::Inside,
    );
    let mut target = None;
    for i in 0..3u8 {
        let row = Rect::from_min_size(
            list.min + vec2(0.0, f32::from(i) * 34.0),
            vec2(list.width(), 34.0),
        );
        let icon = row.left_center() + vec2(18.0, 0.0);
        let toggle = row.right_center() - vec2(24.0, 0.0);
        if i == 1 {
            // PeerDesk 这一行：高亮
            painter.rect(
                row.shrink(2.0),
                6.0,
                p.hover,
                Stroke::new(1.5_f32, p.ink),
                StrokeKind::Inside,
            );
            painter.rect_filled(Rect::from_center_size(icon, vec2(18.0, 18.0)), 4.0, p.ink);
            let _ = painter.text(
                icon,
                Align2::CENTER_CENTER,
                egui_phosphor::regular::MONITOR,
                FontId::proportional(11.0),
                Color32::WHITE,
            );
            let _ = painter.text(
                icon + vec2(16.0, 0.0),
                Align2::LEFT_CENTER,
                "PeerDesk",
                FontId::proportional(12.5),
                w.text,
            );
            mac_toggle(&painter, toggle, granted, &w);
            target = Some(toggle);
        } else {
            painter.circle_filled(icon, 8.0, w.bar);
            painter.rect_filled(
                Rect::from_min_size(
                    icon + vec2(16.0, -3.5),
                    vec2(60.0 + f32::from(i) * 14.0, 7.0),
                ),
                3.5,
                w.bar,
            );
            mac_toggle(&painter, toggle, true, &w);
        }
        if i > 0 {
            painter.hline(row.x_range(), row.top(), Stroke::new(1.0_f32, w.bar));
        }
    }

    // 箭头：从右侧的提示指向 PeerDesk 的开关；开关外一圈脉动的光圈
    if let (Some(toggle), false) = (target, granted) {
        let t = ui.input(|i| i.time);
        let pulse = 0.5 + 0.5 * (t * 3.0).sin();
        #[allow(clippy::cast_possible_truncation)]
        let pulse = pulse as f32;
        painter.circle_stroke(
            toggle,
            16.0 + 4.0 * pulse,
            Stroke::new(2.0_f32, p.ink.gamma_multiply(0.4 + 0.5 * pulse)),
        );
        let tip = toggle + vec2(22.0, 10.0);
        let from = pos2(outer.right() - 60.0, toggle.y + 60.0);
        painter.arrow(from, tip - from, Stroke::new(2.5_f32, p.ink));
        let bubble = Rect::from_center_size(from + vec2(0.0, 18.0), vec2(112.0, 28.0));
        painter.rect_filled(bubble, 8.0, p.ink);
        let _ = painter.text(
            bubble.center(),
            Align2::CENTER_CENTER,
            callout,
            FontId::proportional(12.5),
            Color32::WHITE,
        );
        ui.ctx()
            .request_repaint_after(std::time::Duration::from_millis(33));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use peerdesk_platform::guide::mac_settings_names;

    #[test]
    fn draws_every_variant() {
        let ctx = egui::Context::default();
        super::super::theme::install(&ctx);
        let _ = ctx.run(egui::RawInput::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                for (dark, major, english, granted) in [
                    (false, 14, false, false),
                    (true, 15, true, false),
                    (false, 12, false, true),
                    (true, 11, true, true),
                ] {
                    let p = if dark { Palette::DARK } else { Palette::LIGHT };
                    let names = mac_settings_names(major, english);
                    mac_settings(
                        ui,
                        &p,
                        &names,
                        names.screen,
                        granted,
                        "打开这个开关",
                        "点锁解锁",
                    );
                }
            });
        });
    }
}
