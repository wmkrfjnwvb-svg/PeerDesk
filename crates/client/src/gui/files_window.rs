//! 文件管理窗口（egui 接线）：左栏本机、右栏对方，下面是传输列表。
//!
//! 状态和逻辑在 [`super::files`]（两栏）和 [`super::transfers`]（队列、后台执行），这里只画控件、
//! 把点击转成那边的操作。和 `app.rs`、`viewer.rs` 一样不计入覆盖率（要有窗口系统才能跑）。

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::mpsc::Receiver;
use std::sync::Arc;
use std::time::{Duration, Instant};

use eframe::egui::{self, RichText, ViewportBuilder, ViewportId};
use num_traits::ToPrimitive;
use peerdesk_core::browse::{Reply, Request};
use peerdesk_net::Link;

use super::files::{child, duration_text, parent, size_text, time_text, Pane};
use super::theme::{font, icon, space, Palette};
use super::transfers::{Direction, Queue, Runner, State};

/// 哪一栏。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Side {
    Local,
    Remote,
}

/// 拖动中的条目。
#[derive(Debug, Clone)]
struct Dragged {
    side: Side,
    dir: String,
    names: Vec<String>,
}

/// 等对方回答的请求。
enum Pending {
    Places(Receiver<Result<Reply, String>>),
    List(String, Receiver<Result<Reply, String>>),
    Made(Receiver<Result<Reply, String>>),
}

#[derive(Debug)]
/// 一个可以连的会话：标题和连接（对方不支持文件管理或还没连上时为 `None`）。
pub struct Session {
    /// 窗口标题（设备码或地址）。
    pub title: String,
    /// 连接。
    pub link: Option<Arc<Link>>,
}

/// 文件管理窗口。
pub struct FilesWindow {
    /// 是否显示。
    pub open: bool,
    id: ViewportId,
    local: Pane,
    remote: Pane,
    queue: Queue,
    runner: Result<Runner, String>,
    data_dir: PathBuf,
    /// 选中的会话（标题）。
    session: Option<String>,
    /// 这个会话的连接（换了连接要重新列对方的目录）。
    link_seen: Option<usize>,
    pending: Vec<Pending>,
    /// 正在输入的新名字：（哪栏, 是不是文件夹, 名字）。
    naming: Option<(Side, bool, String)>,
    expanded: BTreeSet<u64>,
    note: Option<String>,
}

impl std::fmt::Debug for FilesWindow {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FilesWindow")
            .field("open", &self.open)
            .finish_non_exhaustive()
    }
}

fn local_list(pane: &mut Pane, path: &str) {
    pane.set_listing(path, peerdesk_net::browse::list(path));
}

impl FilesWindow {
    /// 创建（读回上次的传输记录，本机栏打开主目录）。
    #[must_use]
    pub fn new(data_dir: PathBuf) -> Self {
        let mut local = Pane {
            places: peerdesk_net::browse::places(),
            ..Pane::default()
        };
        if let Some(home) = local.places.first().map(|p| p.path.clone()) {
            local_list(&mut local, &home);
        }
        Self {
            open: false,
            id: ViewportId::from_hash_of("peerdesk-files"),
            local,
            remote: Pane::default(),
            queue: Queue::load(&data_dir),
            runner: Runner::new().map_err(|e| format!("起不了传输线程：{e}")),
            data_dir,
            session: None,
            link_seen: None,
            pending: Vec::new(),
            naming: None,
            expanded: BTreeSet::new(),
            note: None,
        }
    }

    /// 打开，并选中这个会话（助手上的“文件”按钮）。
    pub fn open_for(&mut self, title: &str) {
        self.open = true;
        if self.session.as_deref() != Some(title) {
            self.session = Some(title.to_owned());
            self.link_seen = None;
        }
    }

    fn link<'a>(&self, sessions: &'a [Session]) -> Option<&'a Arc<Link>> {
        let title = self.session.as_deref()?;
        sessions.iter().find(|s| s.title == title)?.link.as_ref()
    }

    fn ask(
        &mut self,
        link: Option<&Arc<Link>>,
        request: Request,
    ) -> Option<Receiver<Result<Reply, String>>> {
        let (Some(link), Ok(runner)) = (link, &self.runner) else {
            return None;
        };
        Some(runner.request(link, request))
    }

    fn list_remote(&mut self, link: Option<&Arc<Link>>, path: String) {
        if let Some(rx) = self.ask(link, Request::List { path: path.clone() }) {
            self.remote.loading = true;
            self.pending.push(Pending::List(path, rx));
        }
    }

    /// 收对方的回答。
    fn collect(&mut self, link: Option<&Arc<Link>>) {
        let mut still = Vec::new();
        let mut relist = false;
        for p in self.pending.drain(..) {
            match p {
                Pending::Places(rx) => match rx.try_recv() {
                    Ok(Ok(Reply::Places(places))) => {
                        // 对方的主目录（第一个位置）作为起点
                        if let Some(home) = places.first() {
                            self.remote.path.clone_from(&home.path);
                            relist = true;
                        }
                        self.remote.places = places;
                    }
                    Ok(Ok(other)) => self.remote.error = Some(format!("对方的答复不对：{other:?}")),
                    Ok(Err(why)) => self.remote.error = Some(why),
                    Err(std::sync::mpsc::TryRecvError::Empty) => still.push(Pending::Places(rx)),
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => {}
                },
                Pending::List(path, rx) => match rx.try_recv() {
                    Ok(Ok(reply)) => self.remote.set_listing(&path, reply),
                    Ok(Err(why)) => {
                        self.remote.loading = false;
                        self.remote.error = Some(why);
                    }
                    Err(std::sync::mpsc::TryRecvError::Empty) => {
                        still.push(Pending::List(path, rx))
                    }
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => {}
                },
                Pending::Made(rx) => match rx.try_recv() {
                    Ok(Ok(Reply::Done)) => relist = true,
                    Ok(Ok(Reply::Failed(why)) | Err(why)) => self.remote.error = Some(why),
                    Ok(Ok(_)) => {}
                    Err(std::sync::mpsc::TryRecvError::Empty) => still.push(Pending::Made(rx)),
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => {}
                },
            }
        }
        self.pending = still;
        if relist {
            let path = self.remote.path.clone();
            self.list_remote(link, path);
        }
    }

    fn enqueue(
        &mut self,
        direction: Direction,
        local_dir: &str,
        remote_dir: &str,
        names: Vec<String>,
    ) {
        if names.is_empty() || remote_dir.is_empty() {
            return;
        }
        let _ = self.queue.add(
            direction,
            PathBuf::from(local_dir),
            remote_dir.to_owned(),
            names,
        );
    }

    /// 显示窗口。`sessions` 是当前的远程会话。
    pub fn show(&mut self, ctx: &egui::Context, sessions: &[Session]) {
        if !self.open {
            return;
        }
        if self.session.is_none() {
            self.session = sessions.first().map(|s| s.title.clone());
        }
        let link = self.link(sessions).cloned();
        // 换了连接（新会话或重连）：重新取对方的位置
        let key = link.as_ref().map(|l| Arc::as_ptr(l) as usize);
        if key != self.link_seen {
            self.link_seen = key;
            self.remote = Pane::default();
            if let Some(rx) = self.ask(link.as_ref(), Request::Places) {
                self.pending.push(Pending::Places(rx));
            }
        }
        self.collect(link.as_ref());
        if let Ok(runner) = &mut self.runner {
            if runner.tick(&mut self.queue, link.as_ref()) {
                if let Err(e) = self.queue.save(&self.data_dir) {
                    log::warn!("保存传输记录失败：{e}");
                }
                // 传完了：刷新两栏
                let (lp, rp) = (self.local.path.clone(), self.remote.path.clone());
                local_list(&mut self.local, &lp);
                self.list_remote(link.as_ref(), rp);
            }
        }
        let busy = self.runner.as_ref().is_ok_and(Runner::busy) || !self.pending.is_empty();
        let builder = ViewportBuilder::default()
            .with_title("PeerDesk — 文件管理")
            .with_inner_size([1100.0, 760.0])
            .with_min_inner_size([640.0, 420.0]);
        ctx.show_viewport_immediate(self.id, builder, |ctx, _| {
            if ctx.input(|i| i.viewport().close_requested()) {
                self.open = false;
            }
            self.ui(ctx, sessions, link.as_ref());
        });
        if busy {
            ctx.request_repaint_after(Duration::from_millis(200));
        }
    }

    fn ui(&mut self, ctx: &egui::Context, sessions: &[Session], link: Option<&Arc<Link>>) {
        let p = Palette::of(ctx);
        // 从系统资源管理器 / 访达拖进来的文件：上传到对方当前目录
        let dropped: Vec<PathBuf> = ctx.input(|i| {
            i.raw
                .dropped_files
                .iter()
                .filter_map(|f| f.path.clone())
                .collect()
        });
        for path in dropped {
            if let (Some(dir), Some(name)) = (path.parent(), path.file_name()) {
                let remote = self.remote.path.clone();
                self.enqueue(
                    Direction::Upload,
                    &dir.display().to_string(),
                    &remote,
                    vec![name.to_string_lossy().into_owned()],
                );
            }
        }

        egui::TopBottomPanel::top("files-top").show(ctx, |ui| {
            ui.add_space(space::S6);
            ui.horizontal(|ui| {
                ui.label("对方设备");
                let current = self
                    .session
                    .clone()
                    .unwrap_or_else(|| "（没有远程会话）".into());
                egui::ComboBox::from_id_salt("files-session")
                    .selected_text(current)
                    .show_ui(ui, |ui| {
                        for s in sessions {
                            if ui
                                .selectable_label(
                                    self.session.as_deref() == Some(&s.title),
                                    &s.title,
                                )
                                .clicked()
                            {
                                self.session = Some(s.title.clone());
                            }
                        }
                    });
                if sessions.is_empty() {
                    ui.label(
                        RichText::new("先连上一台设备：文件管理走远程会话的连接").color(p.muted),
                    );
                } else if link.is_none() {
                    ui.label(
                        RichText::new("对方版本太旧（不支持文件管理）或还没连上")
                            .color(p.relay.text),
                    );
                }
                if let Some(note) = &self.note {
                    ui.label(RichText::new(note).color(p.relay.text));
                }
            });
            ui.add_space(space::S4);
        });

        egui::TopBottomPanel::bottom("files-transfers")
            .resizable(true)
            .default_height(220.0)
            .show(ctx, |ui| self.transfers_ui(ui, &p));

        egui::CentralPanel::default().show(ctx, |ui| {
            ui.columns(2, |cols| {
                if let [left, right] = cols {
                    self.pane_ui(left, &p, Side::Local, link);
                    self.pane_ui(right, &p, Side::Remote, link);
                }
            });
        });
    }

    fn pane(&mut self, side: Side) -> &mut Pane {
        match side {
            Side::Local => &mut self.local,
            Side::Remote => &mut self.remote,
        }
    }

    fn go(&mut self, side: Side, path: String, link: Option<&Arc<Link>>) {
        match side {
            Side::Local => local_list(&mut self.local, &path),
            Side::Remote => self.list_remote(link, path),
        }
    }

    #[allow(clippy::too_many_lines)]
    fn pane_ui(&mut self, ui: &mut egui::Ui, p: &Palette, side: Side, link: Option<&Arc<Link>>) {
        let title = match side {
            Side::Local => "本机",
            Side::Remote => "对方",
        };
        ui.horizontal(|ui| {
            ui.label(RichText::new(title).strong().size(font::SUBTITLE));
            // 预置位置
            let places = self.pane(side).places.clone();
            egui::ComboBox::from_id_salt(("places", title))
                .selected_text("位置")
                .show_ui(ui, |ui| {
                    for place in &places {
                        if ui
                            .selectable_label(false, format!("{}  {}", place.label, place.path))
                            .clicked()
                        {
                            self.go(side, place.path.clone(), link);
                        }
                    }
                });
            if ui.button(icon::ARROW_UP).on_hover_text("上一级").clicked() {
                if let Some(up) = parent(&self.pane(side).path) {
                    self.go(side, up, link);
                }
            }
            if ui
                .button(icon::ARROWS_CLOCKWISE)
                .on_hover_text("刷新")
                .clicked()
            {
                let path = self.pane(side).path.clone();
                self.go(side, path, link);
            }
            if ui
                .button(icon::FOLDER_PLUS)
                .on_hover_text("新建文件夹")
                .clicked()
            {
                self.naming = Some((side, true, "新建文件夹".into()));
            }
            if ui
                .button(icon::FILE_PLUS)
                .on_hover_text("新建文件")
                .clicked()
            {
                self.naming = Some((side, false, "新建文件.txt".into()));
            }
        });
        // 路径（可以手输，回车跳过去）
        let mut path = self.pane(side).path.clone();
        let edit = ui.add(egui::TextEdit::singleline(&mut path).desired_width(f32::INFINITY));
        if edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
            self.go(side, path, link);
        }
        // 新建：输入名字
        if let Some((s, folder, name)) = self.naming.as_mut().filter(|(s, ..)| *s == side) {
            let (s, folder) = (*s, *folder);
            let mut confirm = false;
            let mut cancel = false;
            ui.horizontal(|ui| {
                ui.label(if folder { "文件夹名" } else { "文件名" });
                let r = ui.text_edit_singleline(name);
                confirm = ui.button("新建").clicked()
                    || (r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)));
                cancel = ui.button("取消").clicked();
            });
            if confirm {
                let name = name.clone();
                self.naming = None;
                let dir = self.pane(s).path.clone();
                match s {
                    Side::Local => match peerdesk_net::browse::make(&dir, &name, folder) {
                        Reply::Failed(why) => self.local.error = Some(why),
                        Reply::Places(_) | Reply::List { .. } | Reply::Done | Reply::Ready => {
                            local_list(&mut self.local, &dir);
                        }
                    },
                    Side::Remote => {
                        let request = if folder {
                            Request::MakeDir { dir, name }
                        } else {
                            Request::MakeFile { dir, name }
                        };
                        if let Some(rx) = self.ask(link, request) {
                            self.pending.push(Pending::Made(rx));
                        }
                    }
                }
            } else if cancel {
                self.naming = None;
            }
        }
        if let Some(err) = self.pane(side).error.clone() {
            ui.label(RichText::new(err).color(p.problem.text));
        }
        if self.pane(side).loading {
            ui.add(egui::Spinner::new());
        }
        // 传输按钮
        let selected = self.pane(side).selected_names();
        let (label, dir) = match side {
            Side::Local => (
                format!("{}  上传到对方", icon::UPLOAD_SIMPLE),
                Direction::Upload,
            ),
            Side::Remote => (
                format!("{}  下载到本机", icon::DOWNLOAD_SIMPLE),
                Direction::Download,
            ),
        };
        let can = !selected.is_empty() && link.is_some() && !self.remote.path.is_empty();
        if ui.add_enabled(can, egui::Button::new(label)).clicked() {
            let (l, r) = (self.local.path.clone(), self.remote.path.clone());
            self.enqueue(dir, &l, &r, selected);
        }
        if self.pane(side).truncated {
            ui.label(RichText::new("文件太多，只显示了前 20000 项").color(p.relay.text));
        }
        // 列表；整栏是放下区：从另一栏拖过来的条目传到这里
        let entries = self.pane(side).entries.clone();
        let here = self.pane(side).path.clone();
        let (_, dropped) = ui.dnd_drop_zone::<Dragged, _>(egui::Frame::new(), |ui| {
            egui::ScrollArea::vertical()
                .id_salt(("list", title))
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    egui::Grid::new(("grid", title))
                        .num_columns(3)
                        .striped(true)
                        .min_col_width(60.0)
                        .show(ui, |ui| {
                            ui.label(RichText::new("名称").color(p.muted));
                            ui.label(RichText::new("大小").color(p.muted));
                            ui.label(RichText::new("修改时间").color(p.muted));
                            ui.end_row();
                            for e in &entries {
                                let glyph = if e.dir { icon::FOLDER } else { icon::FILE };
                                let selected = self.pane(side).selected.contains(&e.name);
                                let id = egui::Id::new(("row", title, &e.name));
                                let names = if selected {
                                    self.pane(side).selected_names()
                                } else {
                                    vec![e.name.clone()]
                                };
                                let payload = Dragged {
                                    side,
                                    dir: here.clone(),
                                    names,
                                };
                                let r = ui
                                    .dnd_drag_source(id, payload, |ui| {
                                        ui.selectable_label(
                                            selected,
                                            format!("{glyph}  {}", e.name),
                                        )
                                    })
                                    .response;
                                if r.double_clicked() {
                                    if let Some(path) = self.pane(side).open(&e.name) {
                                        self.go(side, path, link);
                                    }
                                } else if r.clicked() {
                                    let add = ui.input(|i| i.modifiers.command);
                                    self.pane(side).click(&e.name, add);
                                }
                                ui.label(if e.dir {
                                    String::new()
                                } else {
                                    size_text(e.size)
                                });
                                ui.label(time_text(e.modified));
                                ui.end_row();
                            }
                        });
                });
        });
        if let Some(d) = dropped.filter(|d| d.side != side) {
            let (local_dir, remote_dir, direction) = match side {
                Side::Remote => (d.dir.clone(), self.remote.path.clone(), Direction::Upload),
                Side::Local => (self.local.path.clone(), d.dir.clone(), Direction::Download),
            };
            self.enqueue(direction, &local_dir, &remote_dir, d.names.clone());
        }
    }

    fn transfers_ui(&mut self, ui: &mut egui::Ui, p: &Palette) {
        ui.add_space(space::S4);
        ui.horizontal(|ui| {
            ui.label(RichText::new("传输").strong().size(font::SUBTITLE));
            if let Err(why) = &self.runner {
                ui.label(RichText::new(why).color(p.problem.text));
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button(format!("{}  清空记录", icon::TRASH)).clicked() {
                    self.queue.clear_finished();
                    if let Err(e) = self.queue.save(&self.data_dir) {
                        log::warn!("保存传输记录失败：{e}");
                    }
                }
            });
        });
        let now = Instant::now();
        let tasks = self.queue.tasks().to_vec();
        egui::ScrollArea::vertical()
            .id_salt("transfers")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                for t in &tasks {
                    ui.horizontal(|ui| {
                        let arrow = match t.direction {
                            Direction::Upload => icon::UPLOAD_SIMPLE,
                            Direction::Download => icon::DOWNLOAD_SIMPLE,
                        };
                        let open = self.expanded.contains(&t.id);
                        let caret = if open {
                            icon::CARET_UP
                        } else {
                            icon::ARROW_RIGHT
                        };
                        if ui.small_button(caret).on_hover_text("展开").clicked()
                            && !self.expanded.remove(&t.id)
                        {
                            self.expanded.insert(t.id);
                        }
                        let first = t.names.first().cloned().unwrap_or_default();
                        let what = if t.names.len() > 1 {
                            format!("{first} 等 {} 项", t.names.len())
                        } else {
                            first
                        };
                        ui.label(format!("{arrow}  {what}"));
                        let (done, total) = t.bytes();
                        let frac = if total == 0 {
                            0.0
                        } else {
                            done as f32 / total as f32
                        };
                        let (text, color) = match &t.state {
                            State::Queued => ("排队中".to_owned(), p.muted),
                            State::Running => {
                                let speed = t.speed(now).map_or_else(String::new, |s| {
                                    format!("  {}/s", size_text(s.to_u64().unwrap_or(0)))
                                });
                                let eta = t.eta(now).map_or_else(String::new, |e| {
                                    format!("  剩 {}", duration_text(e))
                                });
                                (
                                    format!(
                                        "{} / {}{speed}{eta}",
                                        size_text(done),
                                        size_text(total)
                                    ),
                                    p.ink,
                                )
                            }
                            State::Done(n) => (
                                format!("完成：{n} 个文件，{}", size_text(total)),
                                p.direct.text,
                            ),
                            State::Failed(why) => (format!("失败：{why}"), p.problem.text),
                            State::Cancelled => ("已取消".to_owned(), p.muted),
                        };
                        ui.add(egui::ProgressBar::new(frac).desired_width(220.0));
                        ui.label(RichText::new(text).color(color));
                        match t.state {
                            State::Queued => {
                                if ui
                                    .small_button(icon::ARROW_LINE_UP)
                                    .on_hover_text("置顶")
                                    .clicked()
                                {
                                    self.queue.to_top(t.id);
                                }
                                if ui
                                    .small_button(icon::CARET_UP)
                                    .on_hover_text("往前挪")
                                    .clicked()
                                {
                                    self.queue.move_up(t.id);
                                }
                                if ui.small_button(icon::X).on_hover_text("取消").clicked() {
                                    if let Ok(r) = &mut self.runner {
                                        r.cancel(&mut self.queue, t.id);
                                    }
                                }
                            }
                            State::Running => {
                                if ui.small_button(icon::X).on_hover_text("取消").clicked() {
                                    if let Ok(r) = &mut self.runner {
                                        r.cancel(&mut self.queue, t.id);
                                    }
                                }
                            }
                            State::Failed(_) | State::Cancelled => {
                                if ui
                                    .small_button(icon::ARROW_COUNTER_CLOCKWISE)
                                    .on_hover_text("重试（断点续传）")
                                    .clicked()
                                {
                                    self.queue.retry(t.id);
                                }
                            }
                            State::Done(_) => {}
                        }
                        if ui
                            .small_button(icon::FOLDER_OPEN)
                            .on_hover_text("打开本机文件夹")
                            .clicked()
                        {
                            if let Err(why) = super::app::open_folder(t.folder()) {
                                self.note = Some(why);
                            }
                        }
                    });
                    if self.expanded.contains(&t.id) {
                        ui.indent(("names", t.id), |ui| {
                            ui.label(
                                RichText::new(format!("本机：{}", t.local_dir.display()))
                                    .color(p.muted),
                            );
                            ui.label(
                                RichText::new(format!("对方：{}", t.remote_dir)).color(p.muted),
                            );
                            for n in &t.names {
                                ui.label(child(&t.local_dir.display().to_string(), n));
                            }
                        });
                    }
                    ui.separator();
                }
            });
    }
}
