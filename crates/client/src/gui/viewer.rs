//! 观看窗口（eframe 接线）：每个连接一个系统窗口。只画画面和工具栏、转发输入，
//! 逻辑在 `coords`、`input`、`state`、`stats`、`session` 里。

use std::time::{Duration, Instant};

use peerdesk_core::proto::PeerOs;

use super::assistant::{os_label, Shortcut};
use super::theme::{icon, icon_button, pill, Palette};

use eframe::egui::{
    self, pos2, vec2, Align2, Color32, FontId, Frame, Rect, RichText, TextureHandle,
    TextureOptions, ViewportBuilder, ViewportId,
};

use super::coords::fit;
use super::i18n::Strings;
use super::input::InputState;
use super::session::ViewHandle;
use super::state::{route_name, ViewPhase};
use super::stats::{average_bps, bitrate, millis, FpsMeter};

/// 一次 [`Viewer::show`] 的结果。
#[derive(Debug, Clone)]
pub struct Outcome {
    /// 窗口还开着。
    pub open: bool,
    /// 这一帧里连上了。
    pub connected: bool,
    /// 被控端的系统和电脑名。
    pub peer: (PeerOs, String),
}

/// 一个观看窗口。
pub struct Viewer {
    id: ViewportId,
    title: String,
    handle: ViewHandle,
    phase: ViewPhase,
    texture: Option<TextureHandle>,
    size: [u16; 2],
    input: InputState,
    overlay: bool,
    fps: FpsMeter,
    frames_seen: u64,
    started: Instant,
    /// 通知弹框里展开了升级说明。
    show_upgrade: bool,
    /// 剪贴板提示和它出现的时刻（显示几秒后消失）。
    clip_toast: Option<(String, Instant)>,
    /// 被控端的系统（握手后得知），助手据此选快捷键。
    peer_os: PeerOs,
    /// 被控端的电脑名（老版本为空）。
    peer_name: String,
    /// 浮动助手是否展开。
    assistant: bool,
    /// 是否全屏。
    fullscreen: bool,
}

impl Viewer {
    /// 为一个刚开始的会话建窗口。
    pub fn new(serial: u64, title: String, handle: ViewHandle) -> Self {
        Self {
            id: ViewportId::from_hash_of(("peerdesk-viewer", serial)),
            title,
            handle,
            phase: ViewPhase::Connecting,
            texture: None,
            size: [0, 0],
            input: InputState::new(),
            overlay: false,
            fps: FpsMeter::default(),
            frames_seen: 0,
            started: Instant::now(),
            show_upgrade: false,
            clip_toast: None,
            peer_os: PeerOs::Unknown,
            peer_name: String::new(),
            assistant: true,
            fullscreen: false,
        }
    }

    /// 结束会话（窗口随后关闭）。
    pub fn stop(&mut self) {
        if let Some(event) = self.input.release_all() {
            let _ = self.handle.send(event);
        }
        self.handle.stop();
    }

    /// 画这个窗口。
    pub fn show(&mut self, ctx: &egui::Context, s: &'static Strings) -> Outcome {
        let builder = ViewportBuilder::default()
            .with_title(&self.title)
            .with_inner_size([1296.0, 780.0])
            .with_min_inner_size([320.0, 240.0]);
        ctx.show_viewport_immediate(self.id, builder, |ctx, _class| self.ui(ctx, s))
    }

    fn ui(&mut self, ctx: &egui::Context, s: &'static Strings) -> Outcome {
        let mut outcome = Outcome {
            open: true,
            connected: false,
            peer: (PeerOs::Unknown, String::new()),
        };
        for event in self.handle.poll() {
            if let super::session::ViewEvent::Clipboard(note) = &event {
                if let Some(text) = super::state::clip_message(note, s) {
                    self.clip_toast = Some((text, Instant::now()));
                }
            }
            if let super::session::ViewEvent::Peer(os, name) = &event {
                self.peer_os = *os;
                self.peer_name.clone_from(name);
            }
            self.phase = self.phase.apply(&event, s);
            outcome.connected |= matches!(self.phase, ViewPhase::Connected { .. });
            outcome.peer = (self.peer_os, self.peer_name.clone());
        }
        // 只有新画面到了才上传纹理。
        if let Some(image) = self.handle.frames().take() {
            self.size = [
                u16::try_from(image.size[0]).unwrap_or(u16::MAX),
                u16::try_from(image.size[1]).unwrap_or(u16::MAX),
            ];
            match &mut self.texture {
                Some(texture) => texture.set(image, TextureOptions::LINEAR),
                None => {
                    self.texture =
                        Some(ctx.load_texture("remote-screen", image, TextureOptions::LINEAR))
                }
            }
        }
        let now = self.started.elapsed();
        let frames = self.handle.frames().frames();
        self.fps
            .record(now, frames.saturating_sub(self.frames_seen));
        self.frames_seen = frames;

        let p = Palette::of(ctx);
        let mut picture = None;
        let live = matches!(self.phase, ViewPhase::Connected { .. });
        // 远程画面顶部居中的悬浮工具条（助手）：快捷键、声音、全屏、统计、断开。可以收起成一个小把手。
        egui::Area::new(egui::Id::new(("assistant", self.id)))
            .anchor(Align2::CENTER_TOP, vec2(0.0, 8.0))
            .order(egui::Order::Foreground)
            .show(ctx, |ui| {
                let frame = Frame::new()
                    .fill(p.card.gamma_multiply(0.96))
                    .stroke(egui::Stroke::new(1.0_f32, p.border))
                    .corner_radius(18)
                    .inner_margin(egui::Margin::symmetric(10, 4))
                    .shadow(egui::epaint::Shadow {
                        offset: [0, 4],
                        blur: 16,
                        spread: 0,
                        color: Color32::from_black_alpha(60),
                    });
                frame.show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 4.0;
                        if !self.assistant {
                            if icon_button(ui, icon::CARET_DOWN, s.expand).clicked() {
                                self.assistant = true;
                            }
                            return;
                        }
                        // 连接状态
                        let (state, color) = match &self.phase {
                            ViewPhase::Connecting => (s.connecting.to_owned(), p.warning),
                            ViewPhase::Reconnecting { .. } => (self.phase.headline(s), p.warning),
                            ViewPhase::Connected { route, .. } => (
                                format!("{} · {}", s.connected, route_name(Some(*route), s)),
                                p.success,
                            ),
                            ViewPhase::Ended { status, .. } => (
                                status.text.clone(),
                                if status.error { p.danger } else { p.muted },
                            ),
                        };
                        let _ = pill(ui, &state, color);
                        ui.separator();
                        // 快捷键菜单（按被控端系统选键）
                        ui.add_enabled_ui(live, |ui| {
                            ui.menu_button(
                                RichText::new(format!("{}  {}", icon::KEYBOARD, s.shortcuts)),
                                |ui| {
                                    ui.label(
                                        RichText::new(format!(
                                            "{}：{}",
                                            s.remote_os,
                                            os_label(self.peer_os, s)
                                        ))
                                        .size(12.0)
                                        .color(p.muted),
                                    );
                                    ui.separator();
                                    for sc in Shortcut::ALL {
                                        let events = sc.events(self.peer_os);
                                        let item = ui
                                            .add_enabled(
                                                !events.is_empty(),
                                                egui::Button::new(sc.label(s)).frame(false),
                                            )
                                            .on_disabled_hover_text(s.not_on_this_os);
                                        if item.clicked() {
                                            for event in events {
                                                let _ = self.handle.send(event);
                                            }
                                            ui.close();
                                        }
                                    }
                                },
                            );
                        });
                        // 声音：默认关
                        let sound = self.handle.sound();
                        let (glyph, tip) = if sound {
                            (icon::SPEAKER_HIGH, s.sound)
                        } else {
                            (icon::SPEAKER_SLASH, s.sound)
                        };
                        let b = toolbar_toggle(ui, &p, glyph, tip, sound, live);
                        if b.clicked() {
                            self.handle.set_sound(!sound);
                        }
                        let glyph = if self.fullscreen {
                            icon::CORNERS_IN
                        } else {
                            icon::CORNERS_OUT
                        };
                        if toolbar_toggle(ui, &p, glyph, s.fullscreen, self.fullscreen, true)
                            .clicked()
                        {
                            self.fullscreen = !self.fullscreen;
                            ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(
                                self.fullscreen,
                            ));
                        }
                        if toolbar_toggle(ui, &p, icon::CHART_BAR, s.stats, self.overlay, true)
                            .clicked()
                        {
                            self.overlay = !self.overlay;
                        }
                        ui.separator();
                        let (label, glyph) = if self.phase.is_ended() {
                            (s.close, icon::X_CIRCLE)
                        } else {
                            (s.disconnect, icon::PHONE_DISCONNECT)
                        };
                        let stop = egui::Button::new(
                            RichText::new(format!("{glyph}  {label}")).color(Color32::WHITE),
                        )
                        .fill(p.danger)
                        .corner_radius(12);
                        if ui.add(stop).clicked() {
                            self.stop();
                            outcome.open = false;
                        }
                        if icon_button(ui, icon::CARET_UP, s.collapse).clicked() {
                            self.assistant = false;
                        }
                    });
                });
                // 剪贴板提示：工具条下方显示 6 秒
                if let Some((text, at)) = &self.clip_toast {
                    if at.elapsed() < Duration::from_secs(6) {
                        ui.add_space(6.0);
                        Frame::new()
                            .fill(p.warning.gamma_multiply(0.9))
                            .corner_radius(10)
                            .inner_margin(egui::Margin::symmetric(12, 6))
                            .show(ui, |ui| {
                                ui.label(RichText::new(text).color(Color32::BLACK));
                            });
                        ctx.request_repaint_after(Duration::from_millis(500));
                    } else {
                        self.clip_toast = None;
                    }
                }
            });

        egui::CentralPanel::default()
            .frame(Frame::NONE.fill(Color32::BLACK))
            .show(ctx, |ui| {
                let avail = ui.max_rect();
                let painter = ui.painter();
                if let Some(texture) = &self.texture {
                    let rect = fit(avail, self.size, ctx.pixels_per_point());
                    let uv = Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0));
                    painter.image(texture.id(), rect, uv, Color32::WHITE);
                    picture = Some(rect);
                } else {
                    let headline = self.phase.headline(s);
                    let text = match &self.phase {
                        ViewPhase::Ended { .. } | ViewPhase::Reconnecting { .. } => {
                            headline.as_str()
                        }
                        ViewPhase::Connecting | ViewPhase::Connected { .. } => s.waiting_frame,
                    };
                    let _ = painter.text(
                        avail.center(),
                        Align2::CENTER_CENTER,
                        text,
                        FontId::proportional(18.0),
                        Color32::GRAY,
                    );
                }
                if self.overlay {
                    self.paint_stats(painter, avail, now, s);
                }
                // 重连中：盖一层提示，画面停在最后一帧。
                if matches!(self.phase, ViewPhase::Reconnecting { .. }) && self.texture.is_some() {
                    let _ = painter.text(
                        avail.center(),
                        Align2::CENTER_CENTER,
                        self.phase.headline(s),
                        FontId::proportional(20.0),
                        Color32::from_rgb(230, 160, 40),
                    );
                }
            });

        // 服务器通知（免费额度用完）：弹框，附升级会员的入口。
        if let ViewPhase::Ended {
            notice: Some(text), ..
        } = &self.phase
        {
            let text = text.clone();
            egui::Window::new(s.busy_title)
                .collapsible(false)
                .resizable(false)
                .anchor(Align2::CENTER_CENTER, vec2(0.0, 0.0))
                .show(ctx, |ui| {
                    ui.label(text);
                    ui.add_space(8.0);
                    if ui.button(RichText::new(s.upgrade).strong()).clicked() {
                        self.show_upgrade = true;
                    }
                    if self.show_upgrade {
                        ui.label(s.upgrade_hint);
                    }
                });
        }

        self.forward_input(ctx, picture);
        if ctx.input(|i| i.viewport().close_requested()) {
            self.stop();
            outcome.open = false;
        }
        outcome
    }

    fn paint_stats(&mut self, painter: &egui::Painter, avail: Rect, now: Duration, s: &Strings) {
        let (rtt, rate) = match &self.phase {
            ViewPhase::Ended {
                summary: Some(summary),
                ..
            } => (
                summary
                    .rtt
                    .map_or_else(|| s.not_available.to_owned(), millis),
                bitrate(average_bps(summary.video_bytes, summary.elapsed)),
            ),
            ViewPhase::Connecting
            | ViewPhase::Connected { .. }
            | ViewPhase::Reconnecting { .. }
            | ViewPhase::Ended { .. } => (
                s.live_stats_pending.to_owned(),
                s.live_stats_pending.to_owned(),
            ),
        };
        let lines = [
            format!("{}: {}", s.fps, self.fps.fps(now)),
            format!("{}: {rate}", s.bitrate),
            format!("{}: {rtt}", s.rtt),
            format!("{}: {}", s.latency, s.not_available),
            format!(
                "{}: {}",
                s.path,
                match self.phase.detail() {
                    "" => route_name(self.phase.route(), s),
                    detail => detail,
                }
            ),
            format!("{}: {}×{}", s.resolution, self.size[0], self.size[1]),
        ];
        let text = lines.join("\n");
        let at = avail.min + vec2(8.0, 8.0);
        let galley = painter.layout_no_wrap(text, FontId::monospace(13.0), Color32::WHITE);
        let bg = Rect::from_min_size(at, galley.size()).expand(6.0);
        painter.rect_filled(bg, 4.0, Color32::from_black_alpha(170));
        painter.galley(at, galley, Color32::WHITE);
    }

    fn forward_input(&mut self, ctx: &egui::Context, picture: Option<Rect>) {
        let (events, modifiers, focused, pointer) = ctx.input(|i| {
            (
                i.events.clone(),
                i.modifiers,
                i.focused,
                i.pointer.latest_pos(),
            )
        });
        // 工具栏的按钮不留键盘焦点，否则空格、回车会同时按下按钮。
        ctx.memory_mut(|m| {
            if let Some(id) = m.focused() {
                m.surrender_focus(id);
            }
        });
        let live = matches!(self.phase, ViewPhase::Connected { .. });
        let translated = self
            .input
            .translate(&events, modifiers, picture.filter(|_| live));
        for _ in 0..translated.overlay_toggles {
            self.overlay = !self.overlay;
        }
        if live {
            for event in translated.events {
                let _ = self.handle.send(event);
            }
            // 允许输入法：组字窗口跟着指针。
            if focused {
                if let Some(rect) = picture {
                    let at = pointer.unwrap_or(rect.min);
                    ctx.output_mut(|o| {
                        o.ime = Some(egui::output::IMEOutput {
                            rect,
                            cursor_rect: Rect::from_min_size(at, vec2(1.0, 16.0)),
                        });
                    });
                }
            }
        }
    }
}

/// 工具条上的开关按钮：图标，打开时用品牌色底。
fn toolbar_toggle(
    ui: &mut egui::Ui,
    p: &Palette,
    glyph: &str,
    tip: &str,
    on: bool,
    enabled: bool,
) -> egui::Response {
    let fill = if on {
        p.accent_soft
    } else {
        Color32::TRANSPARENT
    };
    let color = if on { p.accent } else { p.text };
    let button = egui::Button::new(RichText::new(glyph).size(17.0).color(color))
        .fill(fill)
        .stroke(egui::Stroke::NONE)
        .corner_radius(8)
        .min_size(vec2(32.0, 28.0));
    ui.add_enabled(enabled, button).on_hover_text(tip)
}
