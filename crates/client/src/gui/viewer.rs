//! 观看窗口（eframe 接线）：每个连接一个系统窗口。只画画面和工具栏、转发输入，
//! 逻辑在 `coords`、`input`、`state`、`stats`、`session` 里。

use std::time::Instant;

use eframe::egui::{
    self, pos2, vec2, Align2, Color32, FontId, Frame, Rect, TextureHandle, TextureOptions,
    ViewportBuilder, ViewportId,
};

use super::coords::fit;
use super::i18n::Strings;
use super::input::InputState;
use super::session::ViewHandle;
use super::state::{route_name, ViewPhase};
use super::stats::{average_bps, bitrate, millis, FpsMeter};

/// 一次 [`Viewer::show`] 的结果。
#[derive(Debug, Clone, Copy, Default)]
pub struct Outcome {
    /// 窗口还开着。
    pub open: bool,
    /// 这一帧里连上了。
    pub connected: bool,
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
            overlay: true,
            fps: FpsMeter::default(),
            frames_seen: 0,
            started: Instant::now(),
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
        };
        for event in self.handle.poll() {
            self.phase = self.phase.apply(&event, s);
            outcome.connected |= matches!(self.phase, ViewPhase::Connected { .. });
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

        egui::TopBottomPanel::top("toolbar").show(ctx, |ui| {
            ui.horizontal(|ui| {
                let live = !self.phase.is_ended();
                let label = if live { s.disconnect } else { s.close };
                if ui.button(label).clicked() {
                    self.stop();
                    outcome.open = false;
                }
                let _ = ui.toggle_value(&mut self.overlay, s.stats);
                match &self.phase {
                    ViewPhase::Connecting => ui.label(s.connecting),
                    ViewPhase::Connected { remote, .. } => {
                        ui.label(format!("{} {remote}", s.connected))
                    }
                    ViewPhase::Ended { status, .. } => {
                        let color = if status.error {
                            Color32::LIGHT_RED
                        } else {
                            ui.visuals().text_color()
                        };
                        ui.colored_label(color, &status.text)
                    }
                };
            });
        });

        let mut picture = None;
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
                    let text = match &self.phase {
                        ViewPhase::Ended { status, .. } => status.text.as_str(),
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
            });

        self.forward_input(ctx, picture);
        if ctx.input(|i| i.viewport().close_requested()) {
            self.stop();
            outcome.open = false;
        }
        outcome
    }

    fn paint_stats(
        &mut self,
        painter: &egui::Painter,
        avail: Rect,
        now: std::time::Duration,
        s: &Strings,
    ) {
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
            ViewPhase::Connecting | ViewPhase::Connected { .. } | ViewPhase::Ended { .. } => (
                s.live_stats_pending.to_owned(),
                s.live_stats_pending.to_owned(),
            ),
        };
        let lines = [
            format!("{}: {}", s.fps, self.fps.fps(now)),
            format!("{}: {rate}", s.bitrate),
            format!("{}: {rtt}", s.rtt),
            format!("{}: {}", s.latency, s.not_available),
            format!("{}: {}", s.path, route_name(self.phase.route(), s)),
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
