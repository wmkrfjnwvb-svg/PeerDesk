//! 观看窗口（eframe 接线）：每个连接一个系统窗口。只画画面和工具栏、转发输入，
//! 逻辑在 `coords`、`input`、`state`、`stats`、`session` 里。

use std::time::{Duration, Instant};

use peerdesk_core::frame::Picture;
use peerdesk_core::proto::PeerOs;

use super::assistant::{os_label, Shortcut};
use super::theme::{font, icon, icon_button, pill, space, ControlSize, Palette, Signal};

use eframe::egui::{
    self, pos2, vec2, Align2, Color32, FontId, Frame, Pos2, Rect, RichText, TextureHandle,
    TextureOptions, ViewportBuilder, ViewportId,
};

use super::coords::fit;
use super::i18n::Strings;
use super::input::InputState;
use super::session::ViewHandle;
use super::state::{detail_rows, net_texts, route_name, NetLevel, NetMeter, ViewPhase};
use super::stats::FpsMeter;

/// 一次 [`Viewer::show`] 的结果。
#[derive(Debug, Clone)]
pub struct Outcome {
    /// 窗口还开着。
    pub open: bool,
    /// 这一帧里连上了。
    pub connected: bool,
    /// 这一帧里认证通过、握手完成（对方的系统和电脑名到了）。
    pub established: bool,
    /// 被控端的系统和电脑名。
    pub peer: (PeerOs, String),
    /// 点了助手上的“文件”：打开文件管理并选中这个会话。
    pub open_files: bool,
}

/// 一个观看窗口。
pub struct Viewer {
    /// 显示区域大小的防抖（见 [`super::viewsize`]）。
    sizer: super::viewsize::ViewSizer,
    id: ViewportId,
    title: String,
    handle: ViewHandle,
    phase: ViewPhase,
    texture: Option<TextureHandle>,
    /// NV12 画面用显卡转颜色显示（见 [`super::yuv`]）。
    yuv: super::yuv::YuvView,
    /// 现在显示的是 `yuv` 里的画面（不是 `texture`）。
    showing_yuv: bool,
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
    /// 网络状况（工具条上的提示）。
    net: NetMeter,
    /// 最近一秒的链路质量和对方状态（详情面板）。
    quality: Option<peerdesk_net::controller::LinkQuality>,
    /// 浮动助手是否展开。
    assistant: bool,
    /// 助手的位置（顶边中点）；没拖过时在画面顶部居中。
    assistant_at: Option<Pos2>,
    /// 是否全屏。
    fullscreen: bool,
    /// 按原始大小显示（最清楚）；默认放大铺满窗口。
    original: bool,
}

impl Viewer {
    /// 为一个刚开始的会话建窗口。
    pub fn new(serial: u64, title: String, handle: ViewHandle) -> Self {
        Self {
            sizer: super::viewsize::ViewSizer::default(),
            id: ViewportId::from_hash_of(("peerdesk-viewer", serial)),
            title,
            handle,
            phase: ViewPhase::Connecting,
            texture: None,
            yuv: super::yuv::YuvView::default(),
            showing_yuv: false,
            size: [0, 0],
            input: InputState::new(),
            // 截图用：PEERDESK_GUI_DETAILS=1 一打开就显示详情面板
            overlay: std::env::var_os("PEERDESK_GUI_DETAILS").is_some(),
            fps: FpsMeter::default(),
            frames_seen: 0,
            started: Instant::now(),
            show_upgrade: false,
            clip_toast: None,
            peer_os: PeerOs::Unknown,
            peer_name: String::new(),
            net: NetMeter::default(),
            quality: None,
            assistant: true,
            assistant_at: None,
            fullscreen: false,
            original: false,
        }
    }

    /// 结束会话（窗口随后关闭）。
    pub fn stop(&mut self) {
        if let Some(event) = self.input.release_all() {
            let _ = self.handle.send(event);
        }
        self.handle.stop();
    }

    /// 窗口标题（文件管理里选会话用）。
    #[must_use]
    pub fn title(&self) -> &str {
        &self.title
    }

    /// 会话处在哪个阶段（主窗口据此在连接表单下面显示进度和失败原因）。
    #[must_use]
    pub const fn phase(&self) -> &ViewPhase {
        &self.phase
    }

    /// 文件管理用的连接（见 [`ViewHandle::files_link`]）。
    #[must_use]
    pub fn files_link(&self) -> Option<std::sync::Arc<peerdesk_net::Link>> {
        self.handle.files_link()
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
            established: false,
            peer: (PeerOs::Unknown, String::new()),
            open_files: false,
        };
        for event in self.handle.poll() {
            if let super::session::ViewEvent::Clipboard(note) = &event {
                if let Some(text) = super::state::clip_message(note, s) {
                    self.clip_toast = Some((text, Instant::now()));
                }
            }
            if let super::session::ViewEvent::Quality(q) = &event {
                self.net.push(q);
                self.quality = Some(q.clone());
            }
            if let super::session::ViewEvent::Peer(os, name) = &event {
                self.peer_os = *os;
                self.peer_name.clone_from(name);
                outcome.established = true;
            }
            self.phase = self.phase.apply(&event, s);
            outcome.connected |= matches!(self.phase, ViewPhase::Connected { .. });
            outcome.peer = (self.peer_os, self.peer_name.clone());
        }
        // 只有新画面到了才上传纹理。
        if let Some(picture) = self.handle.frames().take() {
            let (w, h) = picture.size();
            self.size = [w, h];
            // NV12 交给显卡转颜色；显卡用不了（OpenGL 太老）时才在 CPU 上转（日志写明原因）
            let cpu = match picture {
                Picture::Nv12(frame) if self.yuv.failed().is_none() => {
                    self.yuv.set(frame);
                    self.showing_yuv = true;
                    None
                }
                Picture::Nv12(_)
                | Picture::Rgb(_)
                | Picture::Described(_)
                | Picture::Presented { .. } => picture.to_screen().ok(),
            };
            if let Some(screen) = cpu {
                self.showing_yuv = false;
                let image = super::session::to_color_image(&screen);
                match &mut self.texture {
                    Some(texture) => texture.set(image, TextureOptions::LINEAR),
                    None => {
                        self.texture =
                            Some(ctx.load_texture("remote-screen", image, TextureOptions::LINEAR))
                    }
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
        // 远程画面上的悬浮工具条（助手）：快捷键、声音、全屏、统计、断开。默认在顶部居中，
        // 按住空白处或左边的把手可以拖到任何位置；收起后是一个小图标，也能拖。
        // 位置自己记：按住左边的把手拖（egui 的可拖动区域会被按钮、文字盖住，拖不动）
        let screen = ctx.screen_rect();
        let at = self
            .assistant_at
            .unwrap_or_else(|| screen.center_top() + vec2(0.0, 8.0));
        egui::Area::new(egui::Id::new(("assistant", self.id)))
            .pivot(Align2::CENTER_TOP)
            .current_pos(at)
            .constrain(true)
            .order(egui::Order::Foreground)
            .show(ctx, |ui| {
                // 工具条上的字不能被选中（选字会吃掉拖动）
                ui.style_mut().interaction.selectable_labels = false;
                let frame = Frame::new()
                    .fill(p.surface.gamma_multiply(0.96))
                    .stroke(egui::Stroke::new(1.0_f32, p.line))
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
                        // 拖动把手：按住拖整个助手（收起时也能拖）
                        let grip = ui
                            .add(
                                egui::Label::new(
                                    RichText::new(icon::DOTS_SIX_VERTICAL)
                                        .size(font::ICON)
                                        .color(p.muted),
                                )
                                .selectable(false)
                                .sense(egui::Sense::drag()),
                            )
                            .on_hover_cursor(egui::CursorIcon::Grab);
                        if grip.dragged() {
                            ctx.set_cursor_icon(egui::CursorIcon::Grabbing);
                            let moved = at + grip.drag_delta();
                            // 不拖出窗口：顶边中点留在画面里
                            self.assistant_at = Some(pos2(
                                moved.x.clamp(screen.left() + 40.0, screen.right() - 40.0),
                                moved.y.clamp(screen.top(), screen.bottom() - 40.0),
                            ));
                        }
                        if !self.assistant {
                            let open = egui::Button::new(
                                RichText::new(format!("{}  {}", icon::MONITOR, icon::CARET_DOWN))
                                    .size(font::ICON)
                                    .color(p.ink),
                            )
                            .frame(false);
                            if ui.add(open).on_hover_text(s.expand).clicked() {
                                self.assistant = true;
                            }
                            return;
                        }
                        // 连接状态
                        let mut net_hint: Option<String> = None;
                        let (state, color) = match &self.phase {
                            ViewPhase::Connecting => (s.connecting.to_owned(), Signal::Idle),
                            ViewPhase::Reconnecting { .. } => {
                                (self.phase.headline(s), Signal::Relay)
                            }
                            ViewPhase::Connected { route, .. } => {
                                let base =
                                    format!("{} · {}", s.connected, route_name(Some(*route), s));
                                // 网络差时标签变色，并写上主要原因（例如“丢包 6%”）
                                match self.net.shown() {
                                    (NetLevel::Good, _) | (_, None) => (base, Signal::Direct),
                                    (level, Some(cause)) => {
                                        let (short, hint) = net_texts(cause, s);
                                        net_hint = Some(hint);
                                        let color = if level == NetLevel::Poor {
                                            Signal::Problem
                                        } else {
                                            Signal::Relay
                                        };
                                        (format!("{base} · {short}"), color)
                                    }
                                }
                            }
                            ViewPhase::Ended { status, .. } => (
                                status.text.clone(),
                                if status.error {
                                    Signal::Problem
                                } else {
                                    Signal::Idle
                                },
                            ),
                        };
                        let shown = pill(ui, &p, &state, color);
                        if let Some(hint) = net_hint {
                            let _ = shown.on_hover_text(hint);
                        } else if let ViewPhase::Ended { status, .. } = &self.phase {
                            // 结束的原文（详情）悬停可见
                            if !status.details().is_empty() {
                                let _ = shown.on_hover_text(status.details());
                            }
                        }
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
                                        .size(font::CAPTION)
                                        .color(p.muted),
                                    );
                                    ui.separator();
                                    // 只列对方系统有的快捷键，用对方系统的叫法，右边写组合键
                                    for sc in Shortcut::available(self.peer_os) {
                                        let events = sc.events(self.peer_os);
                                        let combo = sc.combo(self.peer_os).unwrap_or_default();
                                        let item = ui.add(
                                            egui::Button::new(sc.label_on(self.peer_os, s))
                                                .shortcut_text(combo)
                                                .frame(false),
                                        );
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
                        // 原始大小：一个远程像素对应一个屏幕像素，字最清楚；铺满窗口时放大会变软
                        if toolbar_toggle(
                            ui,
                            &p,
                            icon::NUMBER_SQUARE_ONE,
                            s.original_size,
                            self.original,
                            true,
                        )
                        .clicked()
                        {
                            self.original = !self.original;
                        }
                        if toolbar_toggle(ui, &p, icon::CHART_BAR, s.stats, self.overlay, true)
                            .clicked()
                        {
                            self.overlay = !self.overlay;
                        }
                        // 文件管理（对方支持时可点）
                        let files_ok = self.handle.files_link().is_some();
                        if toolbar_toggle(ui, &p, icon::FOLDER_OPEN, "文件管理", false, files_ok)
                            .clicked()
                        {
                            outcome.open_files = true;
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
                        .fill(p.danger_fill)
                        .corner_radius(12);
                        if ui.add(stop).clicked() {
                            self.stop();
                            outcome.open = false;
                        }
                        if icon_button(ui, &p, icon::CARET_UP, s.collapse, ControlSize::Small)
                            .clicked()
                        {
                            self.assistant = false;
                        }
                    });
                });
                // 剪贴板提示：工具条下方显示 6 秒
                if let Some((text, at)) = &self.clip_toast {
                    if at.elapsed() < Duration::from_secs(6) {
                        ui.add_space(space::S6);
                        Frame::new()
                            .fill(p.relay.text.gamma_multiply(0.9))
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
                // 告诉被控端显示区域多大（像素），它按这个大小编码；原始大小时不限
                let want = if self.original {
                    (0, 0)
                } else {
                    super::viewsize::pixels(avail.width(), avail.height(), ctx.pixels_per_point())
                };
                if let Some(size) = self.sizer.update(Instant::now(), want) {
                    self.handle.set_view(size);
                }
                if self.sizer.is_pending() {
                    ctx.request_repaint_after(super::viewsize::SETTLE);
                }
                let painter = ui.painter();
                if self.showing_yuv {
                    let rect = fit(avail, self.size, ctx.pixels_per_point(), self.original);
                    self.yuv.paint(painter, rect);
                    picture = Some(rect);
                } else if let Some(texture) = &self.texture {
                    let rect = fit(avail, self.size, ctx.pixels_per_point(), self.original);
                    let uv = Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0));
                    painter.image(texture.id(), rect, uv, Color32::WHITE);
                    picture = Some(rect);
                } else {
                    // 结束了：一句原因，下面一句办法
                    let headline = match &self.phase {
                        ViewPhase::Ended { status, .. } => match status.fix(s) {
                            Some(fix) => format!("{}\n{fix}", status.text),
                            None => status.text.clone(),
                        },
                        ViewPhase::Reconnecting { .. }
                        | ViewPhase::Connecting
                        | ViewPhase::Connected { .. } => self.phase.headline(s),
                    };
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

                // 重连中：盖一层提示，画面停在最后一帧。
                if matches!(self.phase, ViewPhase::Reconnecting { .. })
                    && (self.showing_yuv || self.texture.is_some())
                {
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
            notice: Some(_),
            status,
            ..
        } = &self.phase
        {
            // 原因和办法按界面语言说；服务器的原文（带要等几分钟）放在下面一行
            let text = format!(
                "{}\n{}\n{}",
                status.text,
                status.fix(s).unwrap_or_default(),
                status.details()
            );
            egui::Window::new(s.busy_title)
                .collapsible(false)
                .resizable(false)
                .anchor(Align2::CENTER_CENTER, vec2(0.0, 0.0))
                .show(ctx, |ui| {
                    ui.label(text);
                    ui.add_space(space::S8);
                    if ui.button(RichText::new(s.upgrade).strong()).clicked() {
                        self.show_upgrade = true;
                    }
                    if self.show_upgrade {
                        ui.label(s.upgrade_hint);
                    }
                });
        }

        // 详情面板：两端的网络、画面、编解码（工具条“详情”按钮或 F12 打开）
        if self.overlay {
            let route = route_name(self.phase.route(), s);
            let path = match self.phase.detail() {
                "" => route.to_owned(),
                detail => format!("{route} · {detail}"),
            };
            let fps = u32::try_from(self.fps.fps(now)).unwrap_or(u32::MAX);
            let rows = detail_rows(self.quality.as_ref(), &self.net, &path, fps, s);
            let p = Palette::of(ctx);
            let mut open = true;
            egui::Window::new(s.d_title)
                .open(&mut open)
                .collapsible(false)
                .resizable(false)
                .anchor(Align2::RIGHT_TOP, vec2(-12.0, 64.0))
                .show(ctx, |ui| {
                    for (section, items) in rows {
                        ui.label(RichText::new(section).strong());
                        egui::Grid::new(section)
                            .num_columns(2)
                            .spacing(vec2(18.0, 4.0))
                            .striped(true)
                            .show(ui, |ui| {
                                for (name, value) in items {
                                    ui.label(RichText::new(name).color(p.muted));
                                    ui.label(value);
                                    ui.end_row();
                                }
                            });
                        ui.add_space(space::S8);
                    }
                });
            if !open {
                self.overlay = false;
            }
        }
        self.forward_input(ctx, picture);
        if ctx.input(|i| i.viewport().close_requested()) {
            self.stop();
            outcome.open = false;
        }
        outcome
    }

    fn forward_input(&mut self, ctx: &egui::Context, picture: Option<Rect>) {
        let (mut events, modifiers, pointer) =
            ctx.input(|i| (i.events.clone(), i.modifiers, i.pointer.latest_pos()));
        // 指针在助手、菜单、详情窗口上时，移动、按下和滚轮是给它们的，不能再发给远程
        // （否则点助手的按钮，助手下面的远程画面也被点了）。抬起照常发，远程不会卡在按下状态。
        let over_ui = |pos: Pos2| {
            ctx.layer_id_at(pos)
                .is_some_and(|layer| layer.order != egui::Order::Background)
        };
        let wheel_blocked = pointer.is_some_and(over_ui);
        events.retain(|event| {
            let blocked = matches!(event, egui::Event::PointerMoved(pos) if over_ui(*pos))
                || matches!(event, egui::Event::PointerButton { pos, pressed: true, .. } if over_ui(*pos))
                || (wheel_blocked && matches!(event, egui::Event::MouseWheel { .. }));
            !blocked
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
            // 跨系统时 Ctrl 和 Command 对调（Windows 上 Ctrl+C 就是 Mac 上的复制）
            let local = PeerOs::current();
            for event in translated.events {
                let event = super::input::swap_ctrl_and_command(event, local, self.peer_os);
                let _ = self.handle.send(event);
            }
            // 不打开本机输入法：按键原样发给远程，由远程自己的输入法处理（和 ToDesk、Parsec 一样）。
            // 以前打开本机输入法在本机组字再发文字，结果在远程画面上打字会弹出本机的候选窗。
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
    let fill = if on { p.hover } else { Color32::TRANSPARENT };
    let color = p.ink;
    let button = egui::Button::new(RichText::new(glyph).size(font::ICON).color(color))
        .fill(fill)
        .stroke(egui::Stroke::NONE)
        .corner_radius(8)
        .min_size(vec2(32.0, 28.0));
    ui.add_enabled(enabled, button).on_hover_text(tip)
}
