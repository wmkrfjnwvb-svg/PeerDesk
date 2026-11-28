//! 文件管理的传输队列：上传、下载任务排队，一次跑一个，按优先级取下一个；可以取消、置顶、清空记录。
//! 已经结束的任务存在数据目录里（重启后还能看到，失败的可以重试：文件协议会从断点续传）。
//!
//! 这里只有队列的逻辑和记录的读写；真正收发由 [`Runner`] 在后台线程里调用 `peerdesk_net::browse`。

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use num_traits::ToPrimitive;
use peerdesk_net::files::Progress;

/// 记录文件名（在数据目录里）。
pub const HISTORY_FILE: &str = "transfers.txt";
/// 最多保留多少条结束的记录。
const MAX_HISTORY: usize = 500;

/// 方向。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    /// 本机 → 对方。
    Upload,
    /// 对方 → 本机。
    Download,
}

/// 任务状态。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum State {
    /// 排队中。
    Queued,
    /// 正在传。
    Running,
    /// 传完了（文件数）。
    Done(usize),
    /// 失败（哪个文件、什么原因）。
    Failed(TransferFailure),
    /// 被取消。
    Cancelled,
}

impl State {
    /// 已经结束（成功、失败或取消）。
    #[must_use]
    pub const fn finished(&self) -> bool {
        matches!(self, State::Done(_) | State::Failed(_) | State::Cancelled)
    }
}

/// 文件传输为什么失败。界面按它说一句原因、一句办法（见 `reasons.rs`），核心的原文放进详情。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileProblem {
    /// 接收的一方磁盘满了。
    DiskFull,
    /// 没有权限读写这个文件或文件夹。
    NoPermission,
    /// 校验没通过（数据出错，或者源文件在传输中被改了）。
    Checksum,
    /// 源文件在传输中被改动、移走或删除。
    SourceChanged,
    /// 对方关了文件传输权限。
    AccessOff,
    /// 传输中断（连接断了，或者对方中止了传输）。
    Interrupted,
    /// 文件或目标文件夹不存在。
    Missing,
    /// 其他原因。
    Other,
}

impl FileProblem {
    /// 全部原因（界面文字逐个核对用）。
    pub const ALL: [Self; 8] = [
        Self::DiskFull,
        Self::NoPermission,
        Self::Checksum,
        Self::SourceChanged,
        Self::AccessOff,
        Self::Interrupted,
        Self::Missing,
        Self::Other,
    ];

    /// 存进记录的代号。
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::DiskFull => "disk_full",
            Self::NoPermission => "no_permission",
            Self::Checksum => "checksum",
            Self::SourceChanged => "source_changed",
            Self::AccessOff => "access_off",
            Self::Interrupted => "interrupted",
            Self::Missing => "missing",
            Self::Other => "other",
        }
    }

    fn from_code(code: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|p| p.code() == code)
    }

    /// 从核心的原文认原因（两端的说明都出自本仓库的代码，系统错误按常见的写法认）。
    #[must_use]
    pub fn of_text(text: &str) -> Self {
        use peerdesk_net::fs_store::{DISK_FULL, NO_PERMISSION};
        let has = |words: &[&str]| words.iter().any(|w| text.contains(w));
        if has(&["对方没有允许文件管理", "没有文件传输权限", "not permitted"]) {
            Self::AccessOff
        } else if has(&[
            DISK_FULL,
            "No space left",
            "not enough space",
            "os error 28)",
            "os error 112)",
        ]) {
            Self::DiskFull
        } else if has(&[
            NO_PERMISSION,
            "Permission denied",
            "Access is denied",
            "拒绝访问",
        ]) {
            Self::NoPermission
        } else if has(&["源文件"]) {
            Self::SourceChanged
        } else if has(&["校验"]) {
            Self::Checksum
        } else if has(&[
            "不存在",
            "找不到",
            "No such file",
            "not found",
            "os error 2)",
            "os error 3)",
        ]) {
            Self::Missing
        } else if has(&[
            peerdesk_net::files::PEER_ABORTED,
            "没有结果",
            "连接已关闭",
            "connection",
            "closed",
            "timed out",
            "reset",
        ]) {
            Self::Interrupted
        } else {
            Self::Other
        }
    }
}

/// 一次传输失败：哪个文件、什么原因、核心的原文（详情）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransferFailure {
    /// 出问题的文件（不知道具体是哪个时是任务里的条目）。
    pub file: String,
    /// 原因。
    pub problem: FileProblem,
    /// 核心的原文。
    pub details: String,
}

impl TransferFailure {
    /// 按原文认原因。
    #[must_use]
    pub fn new(file: impl Into<String>, details: impl Into<String>) -> Self {
        let details = details.into();
        Self {
            file: file.into(),
            problem: FileProblem::of_text(&details),
            details,
        }
    }
}

/// 一个传输任务：`local_dir` 和 `remote_dir` 下的 `names`（文件或文件夹）。
#[derive(Debug, Clone)]
pub struct Task {
    /// 编号。
    pub id: u64,
    /// 方向。
    pub direction: Direction,
    /// 本机目录（上传的来源、下载的目标）。
    pub local_dir: PathBuf,
    /// 对方目录（上传的目标、下载的来源）。
    pub remote_dir: String,
    /// 条目名字。
    pub names: Vec<String>,
    /// 状态。
    pub state: State,
    /// 进度（运行时由后台更新）。
    pub progress: Arc<Progress>,
    /// 开始的时刻（算速度）。
    pub started: Option<Instant>,
    /// 结束的时间（Unix 秒，记录里用）。
    pub finished_at: u64,
}

impl Task {
    /// （已完成, 总数）字节。
    #[must_use]
    pub fn bytes(&self) -> (u64, u64) {
        self.progress.get()
    }

    /// 任务里的条目（失败时不知道具体是哪个文件，就说这个）。
    #[must_use]
    pub fn label(&self) -> String {
        self.names.join(", ")
    }

    /// 平均速度（字节/秒）；还没开始或刚开始时为 `None`。
    #[must_use]
    pub fn speed(&self, now: Instant) -> Option<f64> {
        let secs = now.duration_since(self.started?).as_secs_f64();
        (secs >= 0.5).then(|| self.bytes().0 as f64 / secs)
    }

    /// 剩余时间（秒）；不知道时为 `None`。
    #[must_use]
    pub fn eta(&self, now: Instant) -> Option<u64> {
        let (done, total) = self.bytes();
        let speed = self.speed(now).filter(|s| *s > 0.0)?;
        let left = total.saturating_sub(done) as f64 / speed;
        left.ceil().to_u64()
    }

    /// 结束后打开哪个文件夹：上传看不到对方的文件夹，打开本机来源；下载打开本机目标。
    #[must_use]
    pub fn folder(&self) -> &Path {
        &self.local_dir
    }
}

/// 传输队列。
#[derive(Debug, Default)]
pub struct Queue {
    tasks: Vec<Task>,
    next_id: u64,
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

impl Queue {
    /// 全部任务（显示顺序：先排队和运行中的，按优先级；再结束的，新的在前）。
    #[must_use]
    pub fn tasks(&self) -> &[Task] {
        &self.tasks
    }

    /// 加一个任务，排在队尾。返回编号。
    pub fn add(
        &mut self,
        direction: Direction,
        local_dir: PathBuf,
        remote_dir: String,
        names: Vec<String>,
    ) -> u64 {
        self.next_id += 1;
        let task = Task {
            id: self.next_id,
            direction,
            local_dir,
            remote_dir,
            names,
            state: State::Queued,
            progress: Arc::default(),
            started: None,
            finished_at: 0,
        };
        // 插在最后一个未结束的任务后面（结束的记录在下面）
        let at = self
            .tasks
            .iter()
            .take_while(|t| !t.state.finished())
            .count();
        self.tasks.insert(at, task);
        self.next_id
    }

    fn index(&self, id: u64) -> Option<usize> {
        self.tasks.iter().position(|t| t.id == id)
    }

    /// 排队中的任务往前挪一位（不越过正在运行的）。
    pub fn move_up(&mut self, id: u64) {
        if let Some(i) = self.index(id).filter(|i| *i > 0) {
            let queued = |t: &Task| t.state == State::Queued;
            if self.tasks.get(i).is_some_and(queued) && self.tasks.get(i - 1).is_some_and(queued) {
                self.tasks.swap(i, i - 1);
            }
        }
    }

    /// 排队中的任务置顶（下一个就跑它）。
    pub fn to_top(&mut self, id: u64) {
        let Some(i) = self.index(id) else { return };
        if self.tasks.get(i).is_some_and(|t| t.state == State::Queued) {
            let task = self.tasks.remove(i);
            let at = self
                .tasks
                .iter()
                .take_while(|t| t.state == State::Running)
                .count();
            self.tasks.insert(at, task);
        }
    }

    /// 下一个该跑的任务（没有正在运行的、并且有排队的时候）。
    #[must_use]
    pub fn next(&self) -> Option<u64> {
        if self.tasks.iter().any(|t| t.state == State::Running) {
            return None;
        }
        self.tasks
            .iter()
            .find(|t| t.state == State::Queued)
            .map(|t| t.id)
    }

    /// 标成运行中。
    pub fn start(&mut self, id: u64, now: Instant) -> Option<&Task> {
        let i = self.index(id)?;
        let task = self.tasks.get_mut(i)?;
        task.state = State::Running;
        task.started = Some(now);
        Some(task)
    }

    /// 结束（成功、失败或取消）：挪到结束记录的最前面。
    pub fn finish(&mut self, id: u64, state: State) {
        let Some(i) = self.index(id) else { return };
        let mut task = self.tasks.remove(i);
        task.state = state;
        task.finished_at = now_secs();
        let at = self
            .tasks
            .iter()
            .take_while(|t| !t.state.finished())
            .count();
        self.tasks.insert(at, task);
    }

    /// 取消：排队中的直接取消；运行中的返回 `true`，由调用方中止后台任务再调 [`Self::finish`]。
    pub fn cancel(&mut self, id: u64) -> bool {
        match self
            .tasks
            .iter()
            .find(|t| t.id == id)
            .map(|t| t.state.clone())
        {
            Some(State::Queued) => {
                self.finish(id, State::Cancelled);
                false
            }
            Some(State::Running) => true,
            Some(State::Done(_) | State::Failed(_) | State::Cancelled) | None => false,
        }
    }

    /// 失败或取消的任务重新排队（文件协议会从断点续传）。
    pub fn retry(&mut self, id: u64) {
        let Some(task) = self.tasks.iter().find(|t| t.id == id) else {
            return;
        };
        if matches!(task.state, State::Failed(_) | State::Cancelled) {
            let (d, l, r, n) = (
                task.direction,
                task.local_dir.clone(),
                task.remote_dir.clone(),
                task.names.clone(),
            );
            self.tasks.retain(|t| t.id != id);
            let _ = self.add(d, l, r, n);
        }
    }

    /// 清空结束的记录。
    pub fn clear_finished(&mut self) {
        self.tasks.retain(|t| !t.state.finished());
    }

    /// 结束的记录写到 `dir` 里（最多 [`MAX_HISTORY`] 条）。
    ///
    /// # Errors
    /// 写不了文件。
    pub fn save(&self, dir: &Path) -> std::io::Result<()> {
        let lines: String = self
            .tasks
            .iter()
            .filter(|t| t.state.finished())
            .take(MAX_HISTORY)
            .map(|t| encode(t) + "\n")
            .collect();
        std::fs::create_dir_all(dir)?;
        std::fs::write(dir.join(HISTORY_FILE), lines)
    }

    /// 读 `dir` 里的记录（读不了或格式不对的行跳过）。
    #[must_use]
    pub fn load(dir: &Path) -> Self {
        let text = std::fs::read_to_string(dir.join(HISTORY_FILE)).unwrap_or_default();
        let tasks: Vec<Task> = text.lines().filter_map(decode).collect();
        let next_id = tasks.iter().map(|t| t.id).max().unwrap_or(0);
        Self { tasks, next_id }
    }
}

/// 后台执行器：自己的 tokio 运行时，一次跑一个任务（在会话的连接上开文件浏览流）。
pub struct Runner {
    rt: tokio::runtime::Runtime,
    current: Option<(u64, tokio::task::JoinHandle<Result<usize, TransferFailure>>)>,
}

impl std::fmt::Debug for Runner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Runner")
            .field("current", &self.current.as_ref().map(|(id, _)| *id))
            .finish_non_exhaustive()
    }
}

/// 一组文件结果的汇总：都成功时为文件数，否则是第一个失败的文件和原因。
fn summarize(results: &[peerdesk_net::files::FileResult]) -> Result<usize, TransferFailure> {
    use peerdesk_core::file::Outcome;
    let failed = results.iter().find_map(|r| match r.outcome {
        Some(Outcome::Done | Outcome::Skipped) => None,
        Some(Outcome::Failed) => Some(TransferFailure {
            file: r.path.clone(),
            problem: FileProblem::Checksum,
            details: format!("{}：校验失败", r.path),
        }),
        None => Some(TransferFailure {
            file: r.path.clone(),
            problem: FileProblem::Interrupted,
            details: format!("{}：没有结果（连接断开，或者对方中止了传输）", r.path),
        }),
    });
    failed.map_or(Ok(results.len()), Err)
}

impl Runner {
    /// 创建（起一个后台线程池）。
    ///
    /// # Errors
    /// 起不了线程。
    pub fn new() -> std::io::Result<Self> {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()?;
        Ok(Self { rt, current: None })
    }

    /// 是否有任务在跑。
    #[must_use]
    pub const fn busy(&self) -> bool {
        self.current.is_some()
    }

    /// 每帧调用：收尾跑完的任务；空闲并且有连接时开始下一个。返回有没有任务刚结束（要存记录）。
    pub fn tick(&mut self, queue: &mut Queue, link: Option<&Arc<peerdesk_net::Link>>) -> bool {
        let mut ended = false;
        if let Some((id, handle)) = self.current.take_if(|(_, h)| h.is_finished()) {
            let label = queue
                .tasks()
                .iter()
                .find(|t| t.id == id)
                .map(Task::label)
                .unwrap_or_default();
            let state = match self.rt.block_on(handle) {
                Ok(Ok(files)) => State::Done(files),
                Ok(Err(failure)) => State::Failed(failure),
                Err(e) => State::Failed(TransferFailure::new(label, e.to_string())),
            };
            queue.finish(id, state);
            ended = true;
        }
        if self.current.is_none() {
            if let (Some(link), Some(id)) = (link, queue.next()) {
                if let Some(task) = queue.start(id, Instant::now()) {
                    let (link, task) = (Arc::clone(link), task.clone());
                    let handle = self.rt.spawn(async move {
                        let results = match task.direction {
                            Direction::Upload => {
                                peerdesk_net::browse::upload(
                                    &link,
                                    &task.local_dir,
                                    &task.names,
                                    &task.remote_dir,
                                    &task.progress,
                                )
                                .await
                            }
                            Direction::Download => {
                                peerdesk_net::browse::download(
                                    &link,
                                    &task.remote_dir,
                                    &task.names,
                                    &task.local_dir,
                                    &task.progress,
                                )
                                .await
                            }
                        };
                        results
                            .map_err(|e| TransferFailure::new(task.label(), e.to_string()))
                            .and_then(|r| summarize(&r))
                    });
                    self.current = Some((id, handle));
                }
            }
        }
        ended
    }

    /// 在后台发一个浏览请求（列目录、位置、新建），结果从返回的通道取（不卡界面）。
    pub fn request(
        &self,
        link: &Arc<peerdesk_net::Link>,
        request: peerdesk_core::browse::Request,
    ) -> std::sync::mpsc::Receiver<Result<peerdesk_core::browse::Reply, String>> {
        let (tx, rx) = std::sync::mpsc::channel();
        let link = Arc::clone(link);
        // 结果走通道，任务本身不用等
        drop(self.rt.spawn(async move {
            let reply = peerdesk_net::browse::request(&link, &request).await;
            let _ = tx.send(reply.map_err(|e| e.to_string()));
        }));
        rx
    }

    /// 取消任务：运行中的中止（关流；对方留下的半截文件下次续传），排队中的直接取消。
    pub fn cancel(&mut self, queue: &mut Queue, id: u64) {
        if queue.cancel(id) {
            if let Some((_, handle)) = self.current.take_if(|(running, _)| *running == id) {
                handle.abort();
            }
            queue.finish(id, State::Cancelled);
        }
    }
}

/// 字段里的 `%`、制表符、换行和分隔符转义（名字里什么字符都可能有）。
fn escape(s: &str) -> String {
    s.replace('%', "%25")
        .replace('\t', "%09")
        .replace('\n', "%0A")
        .replace('\r', "%0D")
        .replace('\u{1f}', "%1F")
}

fn unescape(s: &str) -> String {
    s.replace("%1F", "\u{1f}")
        .replace("%0D", "\r")
        .replace("%0A", "\n")
        .replace("%09", "\t")
        .replace("%25", "%")
}

/// 一行：编号、方向、状态、结束时间、已完成、总数、本机目录、对方目录、名字（\x1f 分隔）、说明。
fn encode(t: &Task) -> String {
    let (done, total) = t.bytes();
    let (state, note) = match &t.state {
        State::Done(n) => ("done", n.to_string()),
        State::Failed(f) => (
            "failed",
            [f.problem.code(), &f.file, &f.details].join("\u{1f}"),
        ),
        State::Cancelled => ("cancelled", String::new()),
        State::Queued | State::Running => ("cancelled", String::new()),
    };
    let names: Vec<String> = t.names.iter().map(|n| escape(n)).collect();
    [
        t.id.to_string(),
        match t.direction {
            Direction::Upload => "up".into(),
            Direction::Download => "down".into(),
        },
        state.into(),
        t.finished_at.to_string(),
        done.to_string(),
        total.to_string(),
        escape(&t.local_dir.display().to_string()),
        escape(&t.remote_dir),
        names.join("\u{1f}"),
        escape(&note),
    ]
    .join("\t")
}

fn decode(line: &str) -> Option<Task> {
    let f: Vec<&str> = line.split('\t').collect();
    let [id, dir, state, at, done, total, local, remote, names, note] = f.as_slice() else {
        return None;
    };
    let note = unescape(note);
    let names: Vec<String> = names.split('\u{1f}').map(unescape).collect();
    let state = match *state {
        "done" => State::Done(note.parse().ok()?),
        "failed" => State::Failed(decode_failure(&note, &names)),
        "cancelled" => State::Cancelled,
        _ => return None,
    };
    let progress = Progress::default();
    progress
        .done
        .store(done.parse().ok()?, std::sync::atomic::Ordering::Relaxed);
    progress
        .total
        .store(total.parse().ok()?, std::sync::atomic::Ordering::Relaxed);
    Some(Task {
        id: id.parse().ok()?,
        direction: match *dir {
            "up" => Direction::Upload,
            "down" => Direction::Download,
            _ => return None,
        },
        local_dir: PathBuf::from(unescape(local)),
        remote_dir: unescape(remote),
        names,
        state,
        progress: Arc::new(progress),
        started: None,
        finished_at: at.parse().ok()?,
    })
}

/// 记录里的失败：`代号 \x1f 文件 \x1f 原文`；老版本的记录只有一句原文，按原文认原因。
fn decode_failure(note: &str, names: &[String]) -> TransferFailure {
    let parts: Vec<&str> = note.split('\u{1f}').collect();
    match parts.as_slice() {
        [code, file, details] => TransferFailure {
            file: (*file).to_owned(),
            problem: FileProblem::from_code(code).unwrap_or_else(|| FileProblem::of_text(details)),
            details: (*details).to_owned(),
        },
        _ => TransferFailure::new(names.join(", "), note),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::indexing_slicing)]
    use super::*;

    fn ids(q: &Queue) -> Vec<u64> {
        q.tasks().iter().map(|t| t.id).collect()
    }

    #[test]
    fn priority_and_lifecycle() {
        let mut q = Queue::default();
        let a = q.add(
            Direction::Upload,
            "/l".into(),
            "/r".into(),
            vec!["a".into()],
        );
        let b = q.add(
            Direction::Download,
            "/l".into(),
            "/r".into(),
            vec!["b".into()],
        );
        let c = q.add(
            Direction::Upload,
            "/l".into(),
            "/r".into(),
            vec!["c".into()],
        );
        assert_eq!(q.next(), Some(a));
        // 置顶 c、把 b 往前挪：c, b, a
        q.to_top(c);
        q.move_up(b);
        assert_eq!(ids(&q), [c, b, a]);
        let t0 = Instant::now();
        assert_eq!(q.start(c, t0).unwrap().state, State::Running);
        assert_eq!(q.next(), None, "一次只跑一个");
        // 运行中的不能被越过
        q.to_top(a);
        assert_eq!(ids(&q), [c, a, b]);
        q.move_up(a);
        assert_eq!(ids(&q)[0], c);
        // 取消：运行中的要调用方中止；排队的直接取消
        assert!(q.cancel(c));
        assert!(!q.cancel(b));
        assert_eq!(
            q.tasks().iter().find(|t| t.id == b).unwrap().state,
            State::Cancelled
        );
        q.finish(c, State::Done(3));
        assert_eq!(q.next(), Some(a));
        // 失败的重试：重新排队（新编号）
        q.start(a, t0);
        q.finish(
            a,
            State::Failed(TransferFailure::new("a", "connection lost")),
        );
        q.retry(a);
        assert!(q
            .tasks()
            .iter()
            .any(|t| t.state == State::Queued && t.names == ["a"]));
        q.retry(c); // 成功的不重试
        assert!(!q.cancel(c));
        // 清空记录：只剩排队的
        q.clear_finished();
        assert!(q.tasks().iter().all(|t| t.state == State::Queued));
        // 不存在的编号：什么都不做
        q.move_up(999);
        q.to_top(999);
        q.finish(999, State::Cancelled);
        q.retry(999);
        assert!(q.start(999, t0).is_none());
    }

    #[test]
    fn speed_and_eta() {
        let mut q = Queue::default();
        let id = q.add(
            Direction::Upload,
            "/l".into(),
            "/r".into(),
            vec!["a".into()],
        );
        let t0 = Instant::now();
        let task = q.start(id, t0).unwrap().clone();
        task.progress
            .total
            .store(1000, std::sync::atomic::Ordering::Relaxed);
        task.progress
            .done
            .store(250, std::sync::atomic::Ordering::Relaxed);
        assert_eq!(task.speed(t0), None, "刚开始不算速度");
        let later = t0 + std::time::Duration::from_secs(1);
        assert_eq!(task.speed(later), Some(250.0));
        assert_eq!(task.eta(later), Some(3));
        assert_eq!(task.folder(), Path::new("/l"));
    }

    #[test]
    fn history_survives_a_restart() {
        let dir = std::env::temp_dir().join(format!("peerdesk-transfers-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut q = Queue::default();
        let weird = "名字\t带\n怪%字符\u{1f}".to_owned();
        let a = q.add(
            Direction::Download,
            "/本机".into(),
            "C:\\对方".into(),
            vec![weird.clone(), "b".into()],
        );
        let b = q.add(
            Direction::Upload,
            "/l".into(),
            "/r".into(),
            vec!["x".into()],
        );
        let c = q.add(
            Direction::Upload,
            "/l".into(),
            "/r".into(),
            vec!["y".into()],
        );
        let _queued = q.add(
            Direction::Upload,
            "/l".into(),
            "/r".into(),
            vec!["z".into()],
        );
        q.finish(a, State::Done(2));
        q.finish(
            b,
            State::Failed(TransferFailure::new(
                "y",
                "存储错误：没有权限：写入 /r/y.part",
            )),
        );
        q.finish(c, State::Cancelled);
        q.save(&dir).unwrap();
        let back = Queue::load(&dir);
        assert_eq!(back.tasks().len(), 3, "排队的不存");
        let got = back.tasks().iter().find(|t| t.id == a).unwrap();
        assert_eq!(got.names, [weird, "b".to_owned()]);
        assert_eq!(got.remote_dir, "C:\\对方");
        assert_eq!(got.state, State::Done(2));
        assert!(back.tasks().iter().any(|t| t.state
            == State::Failed(TransferFailure {
                file: "y".into(),
                problem: FileProblem::NoPermission,
                details: "存储错误：没有权限：写入 /r/y.part".into(),
            })));
        // 新任务的编号接着往下
        let mut back = back;
        assert!(back.add(Direction::Upload, "/".into(), "/".into(), vec![]) > c);
        // 坏行跳过；没有文件时为空
        std::fs::write(
            dir.join(HISTORY_FILE),
            "垃圾\n1\tsideways\tdone\t0\t0\t0\t/\t/\ta\t1\n",
        )
        .unwrap();
        assert!(Queue::load(&dir).tasks().is_empty());
        assert!(Queue::load(&dir.join("没有")).tasks().is_empty());
        // 正在运行的被存时按取消算（重启后不会还在跑）
        let mut q = Queue::default();
        let r = q.add(
            Direction::Upload,
            "/l".into(),
            "/r".into(),
            vec!["r".into()],
        );
        q.start(r, Instant::now());
        assert!(encode(&q.tasks()[0]).contains("\tcancelled\t"));
    }

    /// 一对连好的端点，被控端回答所有文件浏览流。
    fn connected(
        rt: &tokio::runtime::Runtime,
    ) -> (
        Arc<peerdesk_net::Link>,
        peerdesk_net::Node,
        peerdesk_net::Node,
    ) {
        use peerdesk_core::crypto::Policy;
        use peerdesk_net::stream::read_kind;
        use peerdesk_net::{Identity, Node};
        rt.block_on(async {
            let policy = Policy::recommended();
            let (hid, cid) = (Identity::generate().unwrap(), Identity::generate().unwrap());
            let any: std::net::SocketAddr = ([127, 0, 0, 1], 0).into();
            let host = Node::bind(any, &hid, &policy).unwrap();
            let ctrl = Node::bind(any, &cid, &policy).unwrap();
            let addr = host.local_addr().unwrap();
            let pin = hid.fingerprint();
            let (c, h) = tokio::join!(ctrl.connect(addr, Some(&pin)), host.accept());
            let host_link = Arc::new(h.unwrap().unwrap());
            tokio::spawn(async move {
                while let Ok((send, mut recv)) = host_link.accept_stream().await {
                    let _ = read_kind(&mut recv).await;
                    let _ =
                        peerdesk_net::browse::serve(send, recv, true, &Progress::default()).await;
                }
            });
            (Arc::new(c.unwrap()), host, ctrl)
        })
    }

    fn run_until_idle(runner: &mut Runner, q: &mut Queue, link: &Arc<peerdesk_net::Link>) {
        let until = Instant::now() + std::time::Duration::from_secs(30);
        loop {
            let _ = runner.tick(q, Some(link));
            if !runner.busy() && q.next().is_none() {
                return;
            }
            assert!(Instant::now() < until, "传输 30 秒没结束");
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }

    #[test]
    fn runner_uploads_downloads_fails_and_cancels() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let (link, _h, _c) = connected(&rt);
        let base = std::env::temp_dir().join(format!("peerdesk-runner-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let (local, remote, back) = (base.join("local"), base.join("remote"), base.join("back"));
        for d in [&local, &remote, &back] {
            std::fs::create_dir_all(d).unwrap();
        }
        std::fs::create_dir_all(local.join("夹")).unwrap();
        std::fs::write(local.join("夹/a.txt"), b"hello").unwrap();
        std::fs::write(local.join("big.bin"), vec![1u8; 2_000_000]).unwrap();
        let mut runner = Runner::new().unwrap();
        let mut q = Queue::default();
        let remote_s = remote.display().to_string();
        // 没有连接：不开始
        let up = q.add(
            Direction::Upload,
            local.clone(),
            remote_s.clone(),
            vec!["夹".into(), "big.bin".into()],
        );
        assert!(!runner.tick(&mut q, None));
        assert!(!runner.busy());
        // 上传，再下载回来
        let down = q.add(
            Direction::Download,
            back.clone(),
            remote_s.clone(),
            vec!["夹".into()],
        );
        let missing = q.add(
            Direction::Download,
            back.clone(),
            remote_s.clone(),
            vec!["没有".into()],
        );
        run_until_idle(&mut runner, &mut q, &link);
        let state = |q: &Queue, id| q.tasks().iter().find(|t| t.id == id).unwrap().state.clone();
        assert_eq!(state(&q, up), State::Done(2));
        assert_eq!(state(&q, down), State::Done(1));
        // 下载不存在的条目：说出是哪一项、原因是不存在
        let State::Failed(failure) = state(&q, missing) else {
            panic!("应该失败");
        };
        assert_eq!(
            (failure.file.as_str(), failure.problem),
            ("没有", FileProblem::Missing),
            "{failure:?}"
        );
        assert_eq!(std::fs::read(back.join("夹/a.txt")).unwrap(), b"hello");
        assert_eq!(
            std::fs::metadata(remote.join("big.bin")).unwrap().len(),
            2_000_000
        );
        // 取消正在跑的任务
        std::fs::write(local.join("huge.bin"), vec![2u8; 30_000_000]).unwrap();
        let big = q.add(
            Direction::Upload,
            local.clone(),
            remote_s.clone(),
            vec!["huge.bin".into()],
        );
        let _ = runner.tick(&mut q, Some(&link));
        assert!(runner.busy());
        runner.cancel(&mut q, big);
        assert_eq!(state(&q, big), State::Cancelled);
        assert!(!runner.busy());
        assert!(format!("{runner:?}").contains("Runner"));
        let hook = peerdesk_net::controller::LinkHook(Arc::new(|_| {}));
        assert_eq!(format!("{hook:?}"), "LinkHook");
        // 浏览请求在后台做，结果从通道取
        let rx = runner.request(
            &link,
            peerdesk_core::browse::Request::List {
                path: remote_s.clone(),
            },
        );
        let reply = rx
            .recv_timeout(std::time::Duration::from_secs(10))
            .unwrap()
            .unwrap();
        assert!(matches!(reply, peerdesk_core::browse::Reply::List { .. }));
        // 测完删掉临时目录：不删的话每跑一次在临时目录里留 33 MB（2026-11-27 积了 92 份、约 3 GB）
        drop(runner);
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn summary_of_results() {
        use peerdesk_core::file::Outcome;
        use peerdesk_net::files::FileResult;
        let ok = |p: &str| FileResult {
            path: p.into(),
            outcome: Some(Outcome::Done),
        };
        assert_eq!(summarize(&[ok("a"), ok("b")]), Ok(2));
        let bad = FileResult {
            path: "c".into(),
            outcome: None,
        };
        let failure = summarize(&[ok("a"), bad]).unwrap_err();
        assert_eq!(
            (failure.file.as_str(), failure.problem),
            ("c", FileProblem::Interrupted)
        );
        // 不是调试输出（Some(Failed) 之类）
        assert!(!failure.details.contains("None") && !failure.details.contains("Some"));
        let broken = FileResult {
            path: "d".into(),
            outcome: Some(Outcome::Failed),
        };
        let failure = summarize(&[broken]).unwrap_err();
        assert_eq!(
            (failure.file.as_str(), failure.problem),
            ("d", FileProblem::Checksum)
        );
        assert!(!failure.details.contains("Failed"), "{}", failure.details);
    }

    #[test]
    fn file_problems_are_recognized_from_both_sides() {
        let cases = [
            ("对方没有允许文件管理", FileProblem::AccessOff),
            (
                "对方中止了传输：file transfer not permitted",
                FileProblem::AccessOff,
            ),
            (
                "对方中止了传输：存储错误：磁盘空间不足：写入 /x",
                FileProblem::DiskFull,
            ),
            (
                "写入 /x：No space left on device (os error 28)",
                FileProblem::DiskFull,
            ),
            (
                "There is not enough space on the disk. (os error 112)",
                FileProblem::DiskFull,
            ),
            ("存储错误：没有权限：打开 /x", FileProblem::NoPermission),
            (
                "打开 /x：Permission denied (os error 13)",
                FileProblem::NoPermission,
            ),
            (
                "对方中止了传输：源文件在传输过程中被修改",
                FileProblem::SourceChanged,
            ),
            ("源文件不存在：a.txt", FileProblem::SourceChanged),
            ("a：校验失败", FileProblem::Checksum),
            ("目标文件夹不存在：/x", FileProblem::Missing),
            ("对方中止了传输：别的原因", FileProblem::Interrupted),
            ("连接已关闭：timed out", FileProblem::Interrupted),
            ("怪事", FileProblem::Other),
        ];
        for (text, problem) in cases {
            assert_eq!(FileProblem::of_text(text), problem, "{text}");
        }
        for p in FileProblem::ALL {
            assert_eq!(FileProblem::from_code(p.code()), Some(p));
        }
        // 老版本记录里只有一句原文：按原文认；新记录的代号认不出时也按原文认
        let names = ["a".to_owned(), "b".to_owned()];
        assert_eq!(
            decode_failure("a 没有传完（None）", &names),
            TransferFailure {
                file: "a, b".into(),
                problem: FileProblem::Other,
                details: "a 没有传完（None）".into(),
            }
        );
        assert_eq!(
            decode_failure("from_future\u{1f}a\u{1f}校验失败", &names).problem,
            FileProblem::Checksum
        );
    }
}
