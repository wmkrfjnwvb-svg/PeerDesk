//! 观看窗口（eframe 接线）：每个连接一个系统窗口。只画画面和工具栏、转发输入，
//! 逻辑在 `coords`、`input`、`state`、`stats`、`session` 里。

use std::time::{Duration, Instant};

use peerdesk_core::frame::Picture;
use peerdesk_core::proto::PeerOs;

use super::assistant::{os_label, Shortcut};
use super::theme::{
    self, dot, font, font_id, icon, link_button, radius, space, text, Palette, Signal, Weight,
};
use super::toolbar::{self, show_collapsed, Collapse, Item};

use eframe::egui::{
    self, pos2, vec2, Align2, Color32, FontId, Frame, Pos2, Rect, RichText, TextureHandle,
    TextureOptions, ViewportBuilder, ViewportId,
};

use super::coords::fit;
use super::i18n::Strings;
use super::input::InputState;
use super::session::ViewHandle;
use super::state::{detail_rows, route_name, NetMeter, ViewPhase};
use super::stats::FpsMeter;

/// 截图用：`PEERDESK_GUI_TOOLBAR=expanded` 工具条一直展开（不自动收起），`collapsed` 一开始就是小条，
/// `advice` 展开并打开“怎么办”面板（有网络问题时才有内容）。
const TOOLBAR_ENV: &str = "PEERDESK_GUI_TOOLBAR";

/// 工具条的初始状态（见 [`TOOLBAR_ENV`]）。
fn initial_collapse() -> Collapse {
    let mut collapse = Collapse::default();
    match std::env::var(TOOLBAR_ENV).as_deref() {
        Ok("expanded" | "advice") => collapse.expand(Duration::ZERO),
        Ok("collapsed") => collapse.collapse(),
        _ => {}
    }
    collapse
}

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
    /// 工具条收起和展开（自动收起的逻辑见 `toolbar.rs`）。
    collapse: Collapse,
    /// “怎么办”面板是否展开。
    advice_open: bool,
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
            collapse: initial_collapse(),
            advice_open: std::env::var(TOOLBAR_ENV).is_ok_and(|v| v == "advice"),
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
                Picture::Nv12(_) | Picture::Rgb(_) => picture.to_screen().ok(),
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

        let mut picture = None;
        // 远程画面上的悬浮工具条
        self.toolbar(ctx, s, &mut outcome);

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

    /// 远程画面上的悬浮工具条。默认在顶部居中，按住左边的把手可以拖到任何位置。
    /// 展开时是一排按钮；一段时间没用自动收成一个带网络状态的小条，不挡画面（逻辑见 `toolbar.rs`）。
    /// 网络变差时小条变色并写上原因，点“怎么办”在原地展开说明，不弹窗。
    fn toolbar(&mut self, ctx: &egui::Context, s: &'static Strings, outcome: &mut Outcome) {
        let p = Palette::of(ctx);
        let live = matches!(self.phase, ViewPhase::Connected { .. });
        let now = self.started.elapsed();
        let screen = ctx.screen_rect();
        let collapsed = show_collapsed(self.collapse.collapsed(), screen.width());
        let rtt = self.quality.as_ref().map(|q| q.rtt_ms);
        let status = toolbar::status(&self.phase, &self.net, rtt, &self.peer_name, !collapsed, s);
        if status.advice.is_none() {
            self.advice_open = false;
        }
        // 位置自己记：按住左边的把手拖（egui 的可拖动区域会被按钮、文字盖住，拖不动）
        let at = self
            .assistant_at
            .unwrap_or_else(|| screen.center_top() + vec2(0.0, space::S8));
        let area = egui::Area::new(egui::Id::new(("assistant", self.id)))
            .pivot(Align2::CENTER_TOP)
            .current_pos(at)
            .constrain(true)
            .order(egui::Order::Foreground)
            .show(ctx, |ui| {
                // 工具条上的字不能被选中（选字会吃掉拖动）
                ui.style_mut().interaction.selectable_labels = false;
                let edge = if collapsed && status.advice.is_some() {
                    p.signal(status.signal).solid
                } else {
                    p.line
                };
                let frame = Frame::new()
                    .fill(p.surface.gamma_multiply(0.96))
                    .stroke(egui::Stroke::new(1.0_f32, edge))
                    .corner_radius(if collapsed { radius::PILL } else { 12.0 })
                    .inner_margin(egui::Margin::symmetric(8, 4))
                    .shadow(egui::epaint::Shadow {
                        offset: [0, 4],
                        blur: 16,
                        spread: 0,
                        color: Color32::from_black_alpha(60),
                    });
                frame.show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = space::S4;
                        // 条上的按钮都不带边框（和图标按钮一致），鼠标停上去才有底色
                        ui.visuals_mut().widgets.inactive.weak_bg_fill = Color32::TRANSPARENT;
                        ui.visuals_mut().widgets.inactive.bg_stroke = egui::Stroke::NONE;
                        ui.spacing_mut().button_padding = vec2(space::S8, space::S4);
                        self.grip(ui, ctx, &p, at, screen);
                        if collapsed {
                            self.collapsed_bar(ui, &p, &status, now, s);
                        } else {
                            self.expanded_bar(
                                ui,
                                ctx,
                                &p,
                                &status,
                                live,
                                screen.width(),
                                s,
                                outcome,
                            );
                        }
                    });
                });
                if self.advice_open {
                    if let Some(advice) = &status.advice {
                        ui.add_space(space::S6);
                        self.advice_panel(ui, &p, advice, status.signal, s);
                    }
                }
                // 剪贴板提示：工具条下方显示 6 秒
                self.clipboard_toast(ui, ctx, &p);
            });
        // 鼠标在条上（或它的菜单开着）就不自动收起
        let hovered = area.response.contains_pointer() || egui::Popup::is_any_open(ctx);
        self.collapse.tick(now, live, hovered);
        if let Some(left) = self.collapse.auto_collapse_in(now) {
            ctx.request_repaint_after(left.max(Duration::from_millis(100)));
        }
    }

    /// 拖动把手：按住拖整个工具条（收起时也能拖）。
    fn grip(
        &mut self,
        ui: &mut egui::Ui,
        ctx: &egui::Context,
        p: &Palette,
        at: Pos2,
        screen: Rect,
    ) {
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
    }

    /// 收起的小条：展开箭头、状态点、路径和延迟；网络差时写原因，后面有“怎么办”。点一下展开。
    fn collapsed_bar(
        &mut self,
        ui: &mut egui::Ui,
        p: &Palette,
        status: &toolbar::Status,
        now: Duration,
        s: &'static Strings,
    ) {
        let group = ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = space::S8;
            ui.label(
                RichText::new(icon::CARET_DOWN)
                    .size(font::SMALL)
                    .color(p.muted),
            );
            let _ = dot(ui, p, status.signal);
            ui.label(text(&status.text, font::SMALL, Weight::Regular).color(p.ink));
        });
        let id = egui::Id::new(("toolbar-expand", self.id));
        let click = ui
            .interact(group.response.rect, id, egui::Sense::click())
            .on_hover_cursor(egui::CursorIcon::PointingHand)
            .on_hover_text(s.expand);
        if click.clicked() {
            self.collapse.expand(now);
        }
        if status.advice.is_some() {
            let label = text(s.tb_what_to_do, font::SMALL, Weight::Medium)
                .color(p.signal(status.signal).text);
            if ui.add(egui::Button::new(label).frame(false)).clicked() {
                self.advice_open = !self.advice_open;
            }
        }
    }

    /// 展开的工具条：状态、快捷键、声音、全屏、文件、更多、断开、收起。放不下的按钮挪进“更多”。
    #[allow(clippy::too_many_arguments)]
    fn expanded_bar(
        &mut self,
        ui: &mut egui::Ui,
        ctx: &egui::Context,
        p: &Palette,
        status: &toolbar::Status,
        live: bool,
        window_width: f32,
        s: &'static Strings,
        outcome: &mut Outcome,
    ) {
        // 状态：点和文字；网络差时后面有“怎么办”
        let label = text(&status.text, font::SMALL, Weight::Regular).color(p.ink);
        let status_width = ui
            .painter()
            .layout_no_wrap(
                status.text.clone(),
                font_id(font::SMALL, Weight::Regular),
                p.ink,
            )
            .size()
            .x
            + space::S24;
        let _ = dot(ui, p, status.signal);
        ui.add(egui::Label::new(label).truncate());
        if status.advice.is_some() {
            let label = text(s.tb_what_to_do, font::SMALL, Weight::Medium)
                .color(p.signal(status.signal).text);
            if ui.add(egui::Button::new(label).frame(false)).clicked() {
                self.advice_open = !self.advice_open;
            }
        }
        ui.separator();
        let layout = toolbar::arrange(window_width, status_width);
        for item in &layout.inline {
            match item {
                Item::Shortcuts => self.shortcuts_menu(ui, p, live, s),
                Item::Sound => {
                    let sound = self.handle.sound();
                    let glyph = if sound {
                        icon::SPEAKER_HIGH
                    } else {
                        icon::SPEAKER_SLASH
                    };
                    if toolbar_toggle(ui, p, glyph, s.sound, sound, live).clicked() {
                        self.handle.set_sound(!sound);
                    }
                }
                Item::Fullscreen => {
                    let glyph = if self.fullscreen {
                        icon::CORNERS_IN
                    } else {
                        icon::CORNERS_OUT
                    };
                    if toolbar_toggle(ui, p, glyph, s.fullscreen, self.fullscreen, true).clicked() {
                        self.fullscreen = !self.fullscreen;
                        ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(self.fullscreen));
                    }
                }
                Item::Files => {
                    let ok = self.handle.files_link().is_some();
                    if toolbar_toggle(ui, p, icon::FOLDER_OPEN, s.open_files, false, ok).clicked() {
                        outcome.open_files = true;
                    }
                }
                Item::Original | Item::Details => {}
            }
        }
        ui.menu_button(RichText::new(icon::DOTS_THREE).size(font::ICON), |ui| {
            for item in &layout.menu {
                self.more_item(ui, *item, live, s, outcome);
            }
        })
        .response
        .on_hover_text(s.tb_more);
        ui.separator();
        let (label, glyph) = if self.phase.is_ended() {
            (s.close, icon::X_CIRCLE)
        } else {
            (s.disconnect, icon::PHONE_DISCONNECT)
        };
        let stop =
            egui::Button::new(RichText::new(format!("{glyph}  {label}")).color(theme::ON_DANGER))
                .fill(p.danger_fill)
                .corner_radius(radius::CONTROL);
        if ui.add(stop).clicked() {
            self.stop();
            outcome.open = false;
        }
        if toolbar_toggle(ui, p, icon::CARET_UP, s.collapse, false, true).clicked() {
            self.collapse.collapse();
        }
    }

    /// “更多”菜单里的一项：开关类的前面打勾。
    fn more_item(
        &mut self,
        ui: &mut egui::Ui,
        item: Item,
        live: bool,
        s: &'static Strings,
        outcome: &mut Outcome,
    ) {
        let check = |on: bool, label: &str| {
            if on {
                format!("{}  {label}", icon::CHECK)
            } else {
                format!("     {label}")
            }
        };
        match item {
            Item::Sound => {
                let sound = self.handle.sound();
                let button = egui::Button::new(check(sound, s.sound)).frame(false);
                if ui.add_enabled(live, button).clicked() {
                    self.handle.set_sound(!sound);
                    ui.close();
                }
            }
            Item::Files => {
                let ok = self.handle.files_link().is_some();
                let button = egui::Button::new(format!("     {}", s.open_files)).frame(false);
                if ui.add_enabled(ok, button).clicked() {
                    outcome.open_files = true;
                    ui.close();
                }
            }
            Item::Original => {
                let button = egui::Button::new(check(self.original, s.tb_original)).frame(false);
                if ui.add(button).on_hover_text(s.original_size).clicked() {
                    self.original = !self.original;
                    ui.close();
                }
            }
            Item::Details => {
                let button = egui::Button::new(check(self.overlay, s.d_title))
                    .shortcut_text("F12")
                    .frame(false);
                if ui.add(button).clicked() {
                    self.overlay = !self.overlay;
                    ui.close();
                }
            }
            Item::Shortcuts | Item::Fullscreen => {}
        }
    }

    /// 快捷键菜单（按被控端系统选键）。
    fn shortcuts_menu(&mut self, ui: &mut egui::Ui, p: &Palette, live: bool, s: &'static Strings) {
        ui.add_enabled_ui(live, |ui| {
            let button = ui.menu_button(
                RichText::new(icon::KEYBOARD).size(font::ICON).color(p.ink),
                |ui| {
                    ui.label(
                        RichText::new(format!("{}：{}", s.remote_os, os_label(self.peer_os, s)))
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
            button.response.on_hover_text(s.shortcuts);
        });
    }

    /// 网络情况面板（点“怎么办”展开）：四个数、原因和办法，还能打开全部详情。
    fn advice_panel(
        &mut self,
        ui: &mut egui::Ui,
        p: &Palette,
        advice: &toolbar::Advice,
        signal: Signal,
        s: &'static Strings,
    ) {
        let colors = p.signal(signal);
        Frame::new()
            .fill(p.surface.gamma_multiply(0.96))
            .stroke(egui::Stroke::new(1.0_f32, colors.solid))
            .corner_radius(12.0)
            .inner_margin(egui::Margin::symmetric(16, 12))
            .show(ui, |ui| {
                ui.set_max_width(380.0);
                ui.spacing_mut().item_spacing.y = space::S8;
                ui.label(
                    text(
                        format!("{}：{}", s.tb_net_title, advice.short),
                        font::BODY,
                        Weight::Semibold,
                    )
                    .color(colors.text),
                );
                if let Some(q) = &self.quality {
                    let fps =
                        u32::try_from(self.fps.fps(self.started.elapsed())).unwrap_or(u32::MAX);
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = space::S24;
                        for m in toolbar::metrics(q, fps, s) {
                            ui.vertical(|ui| {
                                ui.spacing_mut().item_spacing.y = 0.0;
                                ui.label(
                                    text(m.label, font::CAPTION, Weight::Regular).color(p.muted),
                                );
                                let color = if m.culprit { colors.text } else { p.ink };
                                ui.label(text(m.value, font::BODY, Weight::Semibold).color(color));
                            });
                        }
                    });
                }
                ui.add(
                    egui::Label::new(text(&advice.hint, font::SMALL, Weight::Regular).color(p.ink))
                        .wrap(),
                );
                ui.horizontal(|ui| {
                    if link_button(ui, p, s.tb_all_details).clicked() {
                        self.overlay = true;
                    }
                    if link_button(ui, p, s.close).clicked() {
                        self.advice_open = false;
                    }
                });
            });
    }

    /// 剪贴板提示：工具条下方显示 6 秒。
    fn clipboard_toast(&mut self, ui: &mut egui::Ui, ctx: &egui::Context, p: &Palette) {
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
