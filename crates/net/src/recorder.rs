//! 记录线程（M1 的 R2，设计见 `docs/research/Q-redesign/m1-recorder.md` 第 3 节）：
//! 会话线程把记录放进有界队列，单独的写盘线程取出、编码成块、写文件。
//!
//! 规矩：
//! - **会话线程永不阻塞**：[`Recorder::record`] 只做一次 `try_send`。放不进（队列满）就不放，给“这一类丢了几条”
//!   加一；写盘线程在下一个块的开头写一条缺口记录。写盘线程没了（`try_send` 报对端已关）也只计数。
//! - **不静默丢数据**：队列满、写盘出错、线程崩溃、目录清理失败，每一项都有计数，都进 [`Report`]，
//!   并且写进文字日志。结束记录里有各种类写了多少、丢了多少。
//! - **写盘出错**（磁盘满、没权限）：停止写这一份，之后取出的记录只计数；不重试、不影响会话。
//!   这份文件留着 `.part` 后缀（没有结束记录，读的一方能看出来）。下一次会话重新尝试。
//! - **不调用同步刷盘**：落盘时机由系统决定，断电丢最后几秒可以接受。
//! - 仿真里（虚拟时间）不起线程，直接用 `peerdesk_core::record::Sink` 写进内存，见 `crates/e2e`。

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{sync_channel, Receiver, RecvTimeoutError, SyncSender, TrySendError};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::Duration;

use peerdesk_core::record::{
    classify, encode_header, file_name, part_name, EndReason, GapInfo, Kind, Meta, Record, Sink,
};

/// 队列容量（条）：按最忙时每秒 200 条算，写盘线程停住 40 秒也不丢。
pub const QUEUE_CAPACITY: usize = 8_000;
/// 队列里这么久没有新记录时，写盘线程把还开着的块封上写出去。
const IDLE_FLUSH: Duration = Duration::from_secs(1);
/// 种类数。
const KINDS: usize = Kind::ALL.len();

/// 本机保留策略：最近 20 次或合计 200 MB，先到为准，从最旧的删。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Retention {
    /// 最多保留几份记录文件（含遗留的 `.part`）。
    pub max_files: usize,
    /// 记录文件合计最多多少字节。
    pub max_bytes: u64,
}

impl Default for Retention {
    fn default() -> Self {
        Self {
            max_files: 20,
            max_bytes: 200 * 1024 * 1024,
        }
    }
}

/// 记录设置：客户端传进会话选项。没有这一项（`None`）就不记录。
#[derive(Debug, Clone)]
pub struct RecordSetup {
    /// 记录放在哪个目录（数据目录下的 `records/`）。
    pub dir: PathBuf,
    /// 本机保留策略。
    pub retention: Retention,
    /// 程序版本号，写进文件头。
    pub program_version: Option<String>,
    /// 程序提交号，写进文件头。net 包里拿不到（它是客户端编译时定的），所以由客户端传进来。
    pub commit: Option<String>,
}

/// 控制端的记录设置：在 [`RecordSetup`] 之外，带着访问号和“接着哪一份”。
#[derive(Debug, Clone)]
pub struct ViewRecordSetup {
    /// 目录、保留策略、版本。
    pub setup: RecordSetup,
    /// 访问号：从用户点连接到最终断开不变，写进每份记录的文件头。
    pub visit_id: [u8; 16],
    /// 升级直连、断线重连产生的新会话，写上一份的会话号和原因；第一份为 `None`。
    pub previous: Option<peerdesk_core::record::Previous>,
}

/// 新的访问号：16 字节随机数，控制端从用户点连接到最终断开用同一个。
pub fn new_visit_id() -> io::Result<[u8; 16]> {
    use ring::rand::SecureRandom;
    let mut id = [0u8; 16];
    ring::rand::SystemRandom::new()
        .fill(&mut id)
        .map_err(|_| io::Error::other("系统随机数不可用"))?;
    Ok(id)
}

/// 一次清理的结果。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RetentionReport {
    /// 删掉的文件名。
    pub deleted: Vec<String>,
    /// 想删但没删成的文件数（原因写进了日志）。
    pub failed: u32,
    /// 列目录出错的原因；出错时没有清理。
    pub list_error: Option<String>,
    /// 清理之后剩下的记录文件数。
    pub remaining_files: usize,
    /// 清理之后剩下的字节数。
    pub remaining_bytes: u64,
}

/// 按保留策略清理 `dir` 里的记录文件（正式的和遗留的 `.part`，按文件名也就是开始时间排序，从最旧的删）。
/// `protect` 是正在写的那一份，不删；`reserve` 是接下来还要新建几份（开始前清理时留出位置）。
/// 不是记录文件的东西一律不碰。
pub fn apply_retention(
    dir: &Path,
    policy: &Retention,
    protect: Option<&str>,
    reserve: usize,
) -> RetentionReport {
    let mut report = RetentionReport::default();
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) => {
            crate::note!(
                warn,
                "会话记录：列目录 {} 出错，没有清理旧记录：{e}",
                dir.display()
            );
            report.list_error = Some(e.to_string());
            return report;
        }
    };
    let mut files: Vec<(String, u64)> = entries
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let name = entry.file_name().into_string().ok()?;
            classify(&name)?;
            let meta = entry.metadata().ok()?;
            Some((name, meta.len()))
        })
        .collect();
    files.sort();
    let mut count = files.len();
    let mut bytes: u64 = files.iter().map(|(_, len)| *len).sum();
    let count_limit = policy.max_files.saturating_sub(reserve);
    for (name, len) in &files {
        if count <= count_limit && bytes <= policy.max_bytes {
            break;
        }
        if protect == Some(name.as_str()) {
            continue;
        }
        match fs::remove_file(dir.join(name)) {
            Ok(()) => {
                count -= 1;
                bytes = bytes.saturating_sub(*len);
                report.deleted.push(name.clone());
            }
            Err(e) => {
                // 最旧的删不掉就停：绕过它去删更新的，丢的是更有价值的数据，还是满足不了策略
                crate::note!(warn, "会话记录：删除旧记录 {name} 失败，不再继续清理：{e}");
                report.failed += 1;
                break;
            }
        }
    }
    report.remaining_files = count;
    report.remaining_bytes = bytes;
    if !report.deleted.is_empty() {
        crate::note!(
            info,
            "会话记录：按保留策略删了 {} 份旧记录，剩 {} 份共 {} KB",
            report.deleted.len(),
            count,
            bytes / 1024
        );
    }
    report
}

/// 记录写到哪里。文件是一种实现，测试里还有可以注入故障的内存实现。
trait Output: Send {
    fn write_all(&mut self, bytes: &[u8]) -> io::Result<()>;
    /// 记录线程结束时调用。`clean` 为真表示写了结束记录、中间没出过错：文件可以改成正式文件名。
    fn finish(&mut self, clean: bool) -> io::Result<()>;
}

/// 本机文件。写的过程中叫 `.part`，正常结束后改成正式文件名，再清理旧记录。
struct FileOutput {
    file: File,
    dir: PathBuf,
    part: PathBuf,
    final_name: String,
    retention: Retention,
}

impl Output for FileOutput {
    fn write_all(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.file.write_all(bytes)
    }

    fn finish(&mut self, clean: bool) -> io::Result<()> {
        if clean {
            fs::rename(&self.part, self.dir.join(&self.final_name))?;
        }
        let _ = apply_retention(&self.dir, &self.retention, Some(&self.final_name), 0);
        Ok(())
    }
}

/// 队列满时丢掉的记录，记录线程每开一个新块取一次。
#[derive(Debug, Default)]
struct GapAcc {
    dropped: [u64; KINDS],
    first_us: u64,
    last_us: u64,
}

/// 会话线程和记录线程共享的东西。
#[derive(Debug, Default)]
struct Shared {
    gap: Mutex<GapAcc>,
    /// `finish` 交代的结束时间（微秒）和原因；没有说明会话没有正常收尾。
    end: Mutex<Option<(u64, EndReason)>>,
}

impl Shared {
    /// 丢了一条。只在队列满这条少见的路径上加锁，锁里只做几次加法。
    fn note_drop(&self, kind: Kind, at_us: u64) {
        let mut gap = self.gap.lock().unwrap_or_else(PoisonError::into_inner);
        let any = gap.dropped.iter().any(|n| *n > 0);
        if let Some(n) = gap.dropped.get_mut(kind.slot()) {
            *n = n.saturating_add(1);
        }
        gap.first_us = if any { gap.first_us.min(at_us) } else { at_us };
        gap.last_us = if any { gap.last_us.max(at_us) } else { at_us };
    }

    /// 取走累计的缺口（取走即清零）。
    fn take_gap(&self) -> Option<GapInfo> {
        let mut gap = self.gap.lock().unwrap_or_else(PoisonError::into_inner);
        if gap.dropped.iter().all(|n| *n == 0) {
            return None;
        }
        let info = GapInfo {
            first_us: gap.first_us,
            last_us: gap.last_us,
            dropped: gap.dropped,
        };
        *gap = GapAcc::default();
        Some(info)
    }
}

type Item = (u64, Record);

/// 记录线程交回的账。每一项都对得上：交给记录线程的 = 写进去的 + 写盘出错后丢的。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Report {
    /// 各种类交给写入器的条数（含缺口记录，不含结束记录），下标是 [`Kind::slot`]。
    pub written: [u64; KINDS],
    /// 各种类因为队列满被丢的条数（含最后没来得及写进缺口记录的），下标同上。
    pub dropped: [u64; KINDS],
    /// 写盘出错之后取出来、没有写的条数，下标同上。出错那一块里已经交给写入器的记录也没有写进文件。
    pub lost_to_write_error: [u64; KINDS],
    /// 写盘第一次出错的原因。
    pub write_error: Option<String>,
    /// 改名或清理出错的原因。
    pub finish_error: Option<String>,
    /// 写进文件的字节数（含文件头）。
    pub bytes: u64,
    /// 写出的块数。
    pub blocks: u64,
    /// 写了结束记录（会话正常收尾）。
    pub ended: bool,
    /// 记录线程自己崩了（`join` 失败）：这时其他项都不可信。
    pub thread_crashed: bool,
}

impl Report {
    /// 各种类的条数加起来。
    fn total(counts: &[u64; KINDS]) -> u64 {
        counts.iter().sum()
    }

    /// 队列满丢的总条数。
    #[must_use]
    pub fn dropped_total(&self) -> u64 {
        Self::total(&self.dropped)
    }

    /// 写盘出错之后丢的总条数。
    #[must_use]
    pub fn lost_to_write_error_total(&self) -> u64 {
        Self::total(&self.lost_to_write_error)
    }

    fn log(&self) {
        let by_kind: Vec<String> = Kind::ALL
            .iter()
            .zip(self.written)
            .filter(|(_, n)| *n > 0)
            .map(|(kind, n)| format!("{} {n}", kind.name()))
            .collect();
        crate::note!(
            info,
            "会话记录：{} 块 {} KB，{}；{}",
            self.blocks,
            self.bytes / 1024,
            by_kind.join("、"),
            if self.ended {
                "正常结束"
            } else {
                "没有结束记录（留作 .part）"
            }
        );
        if self.dropped_total() > 0 {
            crate::note!(
                warn,
                "会话记录：队列满了，丢了 {} 条记录（缺口已写进记录）",
                self.dropped_total()
            );
        }
        if self.lost_to_write_error_total() > 0 {
            crate::note!(
                warn,
                "会话记录：写盘出错之后丢了 {} 条记录",
                self.lost_to_write_error_total()
            );
        }
    }
}

/// 记录线程里的状态。
struct Writer {
    out: Box<dyn Output>,
    sink: Sink,
    /// 写入器封好、等着写盘的字节。
    buf: Vec<u8>,
    shared: Arc<Shared>,
    report: Report,
}

impl Writer {
    fn push(&mut self, at_us: u64, record: &Record) {
        if self.report.write_error.is_some() {
            if let Some(n) = self
                .report
                .lost_to_write_error
                .get_mut(record.kind().slot())
            {
                *n += 1;
            }
            return;
        }
        self.sink
            .push(at_us, record, || self.shared.take_gap(), &mut self.buf);
        self.drain();
    }

    fn flush(&mut self) {
        if self.report.write_error.is_none() {
            self.sink.flush(&mut self.buf);
            self.drain();
        }
    }

    /// 把封好的块写出去。出错就记下原因、写日志，之后不再写。
    fn drain(&mut self) {
        if self.buf.is_empty() {
            return;
        }
        match self.out.write_all(&self.buf) {
            Ok(()) => {
                self.report.bytes += u64::try_from(self.buf.len()).unwrap_or(u64::MAX);
            }
            Err(e) => {
                crate::note!(warn, "会话记录：写盘出错，这一份不再写：{e}");
                self.report.write_error = Some(e.to_string());
            }
        }
        self.buf.clear();
    }

    /// 队列关闭之后收尾，返回账。
    fn finish(mut self) -> Report {
        let end = *self
            .shared
            .end
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let pending = self.shared.take_gap();
        if self.report.write_error.is_none() {
            match end {
                Some((at_us, reason)) => {
                    self.sink
                        .finish(at_us, reason, at_us, pending, &mut self.buf);
                    self.report.ended = true;
                }
                None => {
                    // 会话没有交代怎么结束（发送端被丢掉了）：不写结束记录，让读的一方看出来
                    self.sink.flush(&mut self.buf);
                }
            }
            self.drain();
        }
        self.report.written = *self.sink.written();
        // 结束记录自己不算
        if let Some(n) = self.report.written.get_mut(Kind::End.slot()) {
            *n = 0;
        }
        self.report.dropped = *self.sink.dropped();
        if let Some(gap) = pending.filter(|_| !self.report.ended) {
            // 没写进缺口记录的部分也要算进账
            for (total, n) in self.report.dropped.iter_mut().zip(gap.dropped) {
                *total += n;
            }
        }
        self.report.blocks = self.sink.blocks();
        let clean = self.report.ended && self.report.write_error.is_none();
        if let Err(e) = self.out.finish(clean) {
            crate::note!(warn, "会话记录：收尾（改名或清理）出错：{e}");
            self.report.finish_error = Some(e.to_string());
        }
        self.report.log();
        self.report
    }
}

fn run(rx: &Receiver<Item>, mut writer: Writer, flush_every: Duration) -> Report {
    loop {
        match rx.recv_timeout(flush_every) {
            Ok((at_us, record)) => writer.push(at_us, &record),
            Err(RecvTimeoutError::Timeout) => writer.flush(),
            Err(RecvTimeoutError::Disconnected) => return writer.finish(),
        }
    }
}

/// 会话线程这一侧的记录器。
///
/// 没有调用 [`Self::finish`] 就被丢掉（会话的 future 被取消：升级直连时丢掉旧连接、用户取消）时，
/// 写一条原因为 [`EndReason::Cancelled`] 的结束记录，时间取最后放进来的那条记录的时间——
/// 这样“本端放弃了这次会话”和“进程崩了或被杀”（没有结束记录）读回来是两回事。
#[derive(Debug)]
pub struct Recorder {
    tx: Option<SyncSender<Item>>,
    shared: Arc<Shared>,
    thread: Option<JoinHandle<Report>>,
    /// 记录线程没了之后，又放了多少条（放不进去，只计数）。
    after_exit: std::sync::atomic::AtomicU64,
    /// 放进来的记录里最大的时间（微秒）。
    last_at: std::sync::atomic::AtomicU64,
}

/// 等记录线程收尾。会话结束时不用等（直接丢掉它，线程自己收尾）；测试和需要账的地方用 [`Self::wait`]。
#[derive(Debug)]
pub struct Finishing(Option<JoinHandle<Report>>);

impl Finishing {
    /// 等记录线程写完、改名、清理，返回账。
    #[must_use]
    pub fn wait(self) -> Report {
        let Some(thread) = self.0 else {
            return Report::default();
        };
        thread.join().unwrap_or_else(|_| {
            crate::note!(warn, "会话记录：记录线程崩溃了");
            Report {
                thread_crashed: true,
                ..Report::default()
            }
        })
    }
}

impl Recorder {
    /// 在 `dir` 里开始记一份：先按保留策略为新文件腾位置，再建 `<文件名>.part`，写文件头，启动记录线程。
    /// 目录建不出来、文件建不出来（或同名文件已经有了）、文件头写不进去都返回错误，由调用方写日志后
    /// 不记录继续会话。
    pub fn start_in_dir(dir: &Path, meta: &Meta, retention: Retention) -> io::Result<Self> {
        fs::create_dir_all(dir)?;
        let _ = apply_retention(dir, &retention, None, 1);
        let final_name = file_name(meta);
        let part = dir.join(part_name(&final_name));
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&part)?;
        let out = FileOutput {
            file,
            dir: dir.to_owned(),
            part: part.clone(),
            final_name,
            retention,
        };
        Self::start_with(Box::new(out), meta, IDLE_FLUSH).inspect_err(|_| {
            // 文件头没写进去：不留一个空的 .part
            let _ = fs::remove_file(&part);
        })
    }

    fn start_with(
        mut out: Box<dyn Output>,
        meta: &Meta,
        flush_every: Duration,
    ) -> io::Result<Self> {
        let header = encode_header(meta);
        out.write_all(&header)?;
        let shared = Arc::new(Shared::default());
        let (tx, rx) = sync_channel::<Item>(QUEUE_CAPACITY);
        let writer = Writer {
            out,
            sink: Sink::new(),
            buf: Vec::new(),
            shared: Arc::clone(&shared),
            report: Report {
                bytes: u64::try_from(header.len()).unwrap_or(u64::MAX),
                ..Report::default()
            },
        };
        let thread = std::thread::Builder::new()
            .name("peerdesk-recorder".to_owned())
            .spawn(move || run(&rx, writer, flush_every))?;
        Ok(Self {
            tx: Some(tx),
            shared,
            thread: Some(thread),
            after_exit: std::sync::atomic::AtomicU64::new(0),
            last_at: std::sync::atomic::AtomicU64::new(0),
        })
    }

    /// 放一条记录。**永不阻塞**：队列满就丢掉并计数，记录线程没了也只计数。`at_us` 是本端单调时钟从会话开始算的微秒。
    pub fn record(&self, at_us: u64, record: Record) {
        let kind = record.kind();
        self.last_at
            .fetch_max(at_us, std::sync::atomic::Ordering::Relaxed);
        let Some(tx) = &self.tx else {
            self.after_exit
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            return;
        };
        match tx.try_send((at_us, record)) {
            Ok(()) => {}
            Err(TrySendError::Full(_)) => self.shared.note_drop(kind, at_us),
            Err(TrySendError::Disconnected(_)) => {
                self.after_exit
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            }
        }
    }

    /// 记录线程没了之后放的条数（正常是 0）。
    #[must_use]
    pub fn lost_after_thread_exit(&self) -> u64 {
        self.after_exit.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// 会话结束：交代结束时间（微秒，从会话开始算）和原因，关闭队列。不等记录线程写完。
    #[must_use]
    pub fn finish(mut self, at_us: u64, reason: EndReason) -> Finishing {
        *self
            .shared
            .end
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some((at_us, reason));
        drop(self.tx.take());
        Finishing(self.thread.take())
    }
}

impl Drop for Recorder {
    fn drop(&mut self) {
        if self.tx.is_some() {
            let at = self.last_at.load(std::sync::atomic::Ordering::Relaxed);
            *self
                .shared
                .end
                .lock()
                .unwrap_or_else(PoisonError::into_inner) = Some((at, EndReason::Cancelled));
            drop(self.tx.take());
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::indexing_slicing)]
    use super::*;
    use peerdesk_core::proto::PeerOs;
    use peerdesk_core::record::{part_name, InputLevel, Recording, Retransmit, Role};
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("peerdesk-rec-{}-{tag}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn meta(start_s: u64) -> Meta {
        let mut m = Meta::new(Role::Host, start_s * 1_000_000);
        m.session_id = Some([0xab; 16]);
        m.os = Some(PeerOs::Linux);
        m
    }

    fn input(n: u64) -> Record {
        Record::Retransmit(Retransmit {
            requested: u32::try_from(n).unwrap(),
            resent: 0,
            capped: 0,
            lost_frames: 0,
        })
    }

    /// 内存里的输出，可以让第 N 次写入卡住或出错。
    #[derive(Clone, Default)]
    struct Probe {
        bytes: Arc<Mutex<Vec<u8>>>,
        writes: Arc<AtomicUsize>,
        /// 第几次写（从 1 数，文件头是第 1 次）开始失败；0 表示不失败。
        fail_from: usize,
        /// 第几次写开始卡住，直到 `release` 为真；0 表示不卡。
        stall_at: usize,
        stalled: Arc<AtomicBool>,
        release: Arc<AtomicBool>,
        finished: Arc<Mutex<Option<bool>>>,
        fail_finish: bool,
    }

    impl Output for Probe {
        fn write_all(&mut self, bytes: &[u8]) -> io::Result<()> {
            let n = self.writes.fetch_add(1, Ordering::SeqCst) + 1;
            if self.stall_at != 0 && n == self.stall_at {
                self.stalled.store(true, Ordering::SeqCst);
                while !self.release.load(Ordering::SeqCst) {
                    std::thread::sleep(Duration::from_millis(1));
                }
            }
            if self.fail_from != 0 && n >= self.fail_from {
                return Err(io::Error::other("磁盘满了（测试注入）"));
            }
            self.bytes.lock().unwrap().extend_from_slice(bytes);
            Ok(())
        }

        fn finish(&mut self, clean: bool) -> io::Result<()> {
            *self.finished.lock().unwrap() = Some(clean);
            if self.fail_finish {
                Err(io::Error::other("改名失败（测试注入）"))
            } else {
                Ok(())
            }
        }
    }

    const QUICK: Duration = Duration::from_millis(20);

    fn start(probe: &Probe) -> Recorder {
        start_flushing(probe, QUICK)
    }

    fn start_flushing(probe: &Probe, flush_every: Duration) -> Recorder {
        Recorder::start_with(Box::new(probe.clone()), &meta(1_000), flush_every).unwrap()
    }

    #[test]
    fn a_full_queue_never_blocks_and_the_gap_adds_up() {
        let probe = Probe {
            // 第 1 次是文件头；第 2 次是第一个块封上时写：卡在这里，记录线程就不再取队列
            stall_at: 2,
            ..Probe::default()
        };
        // 不让定时封块抢在“超过 1 秒封块”之前
        let rec = start_flushing(&probe, Duration::from_secs(60));
        rec.record(0, input(0));
        // 超过 1 秒：记录线程封上第一块，卡在写盘上
        rec.record(2_000_000, input(1));
        let t0 = std::time::Instant::now();
        while !probe.stalled.load(Ordering::SeqCst) {
            assert!(t0.elapsed() < Duration::from_secs(10), "记录线程没有卡住");
            std::thread::sleep(Duration::from_millis(1));
        }
        // 记录线程卡着：放满队列，再多放 500 条。放的那一侧用另一个线程跑，卡住了就超时失败
        let extra = 500u64;
        let total = u64::try_from(QUEUE_CAPACITY).unwrap() + extra;
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let sender = std::thread::spawn(move || {
            for i in 0..total {
                rec.record(3_000_000 + i, input(i + 2));
            }
            let _ = done_tx.send(());
            rec
        });
        assert!(
            done_rx.recv_timeout(Duration::from_secs(10)).is_ok(),
            "队列满了，record 不能阻塞"
        );
        let rec = sender.join().unwrap();
        probe.release.store(true, Ordering::SeqCst);
        let report = rec.finish(9_000_000, EndReason::Normal).wait();
        assert!(report.ended && report.write_error.is_none());
        let slot = Kind::Retransmit.slot();
        assert_eq!(report.dropped[slot], extra);
        // 交给记录线程的 2 + 满的队列 = 写进去的；其余都是丢的
        assert_eq!(
            report.written[slot],
            2 + u64::try_from(QUEUE_CAPACITY).unwrap()
        );
        assert_eq!(report.written[slot] + report.dropped[slot], total + 2);
        // 读回来：缺口记录加起来等于丢的条数，结束记录里的账一致
        let parsed = Recording::parse(&probe.bytes.lock().unwrap()).unwrap();
        let gap_sum: u64 = parsed
            .gaps()
            .map(|(_, g)| g.dropped.iter().map(|(_, n)| *n).sum::<u64>())
            .sum();
        assert_eq!(gap_sum, extra);
        assert_eq!(parsed.count(Kind::Retransmit), QUEUE_CAPACITY + 2);
        let end = parsed.end().unwrap();
        let t = end
            .tallies
            .iter()
            .find(|t| t.kind == Kind::Retransmit.code())
            .unwrap();
        assert_eq!((t.written, t.dropped), (report.written[slot], extra));
        assert_eq!(*probe.finished.lock().unwrap(), Some(true));
    }

    #[test]
    fn a_write_error_stops_writing_and_counts_everything_after() {
        let probe = Probe {
            // 文件头写成功，第一个块写失败
            fail_from: 2,
            ..Probe::default()
        };
        let rec = start(&probe);
        rec.record(0, input(0));
        rec.record(2_000_000, input(1)); // 封块、写盘失败
        for i in 0..10 {
            rec.record(3_000_000 + i, Record::Input(InputLevel::Few));
        }
        // 会话这一侧什么都没发生：record 照常返回
        let report = rec.finish(5_000_000, EndReason::Failed).wait();
        assert!(report.write_error.as_deref().unwrap().contains("磁盘满了"));
        assert!(!report.ended, "出错之后不再写结束记录");
        assert_eq!(
            report.bytes,
            u64::try_from(encode_header(&meta(1_000)).len()).unwrap()
        );
        // 失败的那一次写之后取出来的 10 条输入记录都算在“出错后丢的”里
        assert_eq!(report.lost_to_write_error[Kind::Input.slot()], 10);
        assert_eq!(report.lost_to_write_error_total(), 10);
        assert_eq!(
            *probe.finished.lock().unwrap(),
            Some(false),
            "出错的文件不改名"
        );
        // 文件里只有文件头：能读，没有记录，没有结束
        let parsed = Recording::parse(&probe.bytes.lock().unwrap()).unwrap();
        assert!(parsed.entries.is_empty() && parsed.end().is_none());
    }

    #[test]
    fn a_failing_header_write_is_an_error_for_the_caller() {
        let probe = Probe {
            fail_from: 1,
            ..Probe::default()
        };
        let err = Recorder::start_with(Box::new(probe), &meta(1), QUICK).unwrap_err();
        assert!(err.to_string().contains("磁盘满了"));
    }

    #[test]
    fn a_finish_error_is_reported() {
        let probe = Probe {
            fail_finish: true,
            ..Probe::default()
        };
        let rec = start(&probe);
        rec.record(0, input(0));
        let report = rec.finish(10, EndReason::Normal).wait();
        assert!(report.ended);
        assert!(report.finish_error.as_deref().unwrap().contains("改名失败"));
    }

    #[test]
    fn dropping_the_recorder_without_finish_leaves_no_end_record() {
        let probe = Probe::default();
        let rec = start(&probe);
        rec.record(0, input(0));
        let mut rec = rec;
        drop(rec.tx.take());
        let report = Finishing(rec.thread.take()).wait();
        assert!(!report.ended);
        assert_eq!(report.written[Kind::Retransmit.slot()], 1);
        assert_eq!(*probe.finished.lock().unwrap(), Some(false));
        let parsed = Recording::parse(&probe.bytes.lock().unwrap()).unwrap();
        assert_eq!(parsed.count(Kind::Retransmit), 1);
        assert!(parsed.end().is_none());
    }

    #[test]
    fn a_cancelled_session_ends_with_a_cancelled_record_not_a_missing_one() {
        let probe = Probe::default();
        let rec = start(&probe);
        rec.record(10, input(1));
        rec.record(30, input(2));
        let thread = rec.thread.as_ref().map(|t| t.thread().id());
        assert!(thread.is_some());
        drop(rec);
        // 记录线程自己收尾：等它把结束记录写进去
        let t0 = std::time::Instant::now();
        let parsed = loop {
            let parsed = Recording::parse(&probe.bytes.lock().unwrap()).unwrap();
            if parsed.end().is_some() {
                break parsed;
            }
            assert!(t0.elapsed() < Duration::from_secs(10), "没有写结束记录");
            std::thread::sleep(Duration::from_millis(5));
        };
        let end = parsed.end().unwrap();
        assert_eq!((end.reason, end.duration_us), (EndReason::Cancelled, 30));
        assert_eq!(parsed.count(Kind::Retransmit), 2);
    }

    #[test]
    fn recording_after_finish_only_counts() {
        let probe = Probe::default();
        let mut rec = start(&probe);
        drop(rec.tx.take());
        rec.record(1, input(1));
        assert_eq!(rec.lost_after_thread_exit(), 1);
        let _ = Finishing(rec.thread.take()).wait();
    }

    #[test]
    fn drops_that_never_reached_a_gap_record_are_still_counted() {
        // 队列满过、但之后再没有新块：没有 finish 的话缺口记录写不出去，账里要有
        let probe = Probe::default();
        let rec = start(&probe);
        rec.shared.note_drop(Kind::Frame, 50);
        rec.shared.note_drop(Kind::Frame, 40);
        rec.shared.note_drop(Kind::Feedback, 70);
        let mut rec = rec;
        drop(rec.tx.take());
        let report = Finishing(rec.thread.take()).wait();
        assert_eq!(report.dropped[Kind::Frame.slot()], 2);
        assert_eq!(report.dropped[Kind::Feedback.slot()], 1);
        assert_eq!(report.dropped_total(), 3);
    }

    #[test]
    fn drops_are_written_as_one_gap_with_the_right_times() {
        let probe = Probe::default();
        let rec = start(&probe);
        rec.shared.note_drop(Kind::Frame, 500);
        rec.shared.note_drop(Kind::Frame, 300);
        rec.shared.note_drop(Kind::Feedback, 900);
        rec.record(1_000, input(1));
        let report = rec.finish(2_000, EndReason::Normal).wait();
        let parsed = Recording::parse(&probe.bytes.lock().unwrap()).unwrap();
        let (at, gap) = parsed.gaps().next().unwrap();
        let gap = (at, gap.clone());
        assert_eq!(gap.0, 900, "缺口的时间是最后一条被丢的记录的时间");
        assert_eq!(gap.1.span_us, 600);
        assert_eq!(
            gap.1.dropped,
            vec![(Kind::Feedback.code(), 1), (Kind::Frame.code(), 2)]
        );
        assert_eq!(report.dropped_total(), 3);
    }

    #[test]
    fn an_idle_recorder_still_gets_its_block_written() {
        let probe = Probe::default();
        let rec = start(&probe);
        rec.record(0, input(0));
        // 没有后续记录，记录线程靠定时封块写盘
        let t0 = std::time::Instant::now();
        while probe.writes.load(Ordering::SeqCst) < 2 {
            assert!(t0.elapsed() < Duration::from_secs(10), "没有写盘");
            std::thread::sleep(Duration::from_millis(5));
        }
        let parsed = Recording::parse(&probe.bytes.lock().unwrap()).unwrap();
        assert_eq!(parsed.count(Kind::Retransmit), 1);
        let _ = rec.finish(1, EndReason::Normal).wait();
    }

    #[test]
    fn a_dead_recorder_thread_only_counts() {
        let (tx, rx) = sync_channel::<Item>(4);
        drop(rx);
        let thread = std::thread::spawn(Report::default);
        let rec = Recorder {
            tx: Some(tx),
            shared: Arc::new(Shared::default()),
            thread: Some(thread),
            after_exit: std::sync::atomic::AtomicU64::new(0),
            last_at: std::sync::atomic::AtomicU64::new(0),
        };
        rec.record(1, input(1));
        rec.record(2, input(2));
        assert_eq!(rec.lost_after_thread_exit(), 2);
        assert_eq!(rec.finish(3, EndReason::Normal).wait(), Report::default());
    }

    #[test]
    fn a_crashed_recorder_thread_is_reported() {
        // resume_unwind 不经过 panic 钩子，不会往测试输出里打一堆栈
        let thread =
            std::thread::spawn(|| -> Report { std::panic::resume_unwind(Box::new("崩了")) });
        let report = Finishing(Some(thread)).wait();
        assert!(report.thread_crashed);
        assert_eq!(Finishing(None).wait(), Report::default());
    }

    #[test]
    fn files_are_written_as_part_then_renamed() {
        let dir = temp_dir("rename");
        let rec = Recorder::start_in_dir(&dir.join("records"), &meta(1_000), Retention::default())
            .unwrap();
        let final_name = file_name(&meta(1_000));
        let part = dir.join("records").join(part_name(&final_name));
        assert!(part.exists(), "写的过程中是 .part");
        rec.record(10, input(1));
        rec.record(20, input(2));
        let report = rec.finish(30, EndReason::Normal).wait();
        assert!(report.ended && report.finish_error.is_none());
        assert!(!part.exists());
        let bytes = fs::read(dir.join("records").join(&final_name)).unwrap();
        assert_eq!(u64::try_from(bytes.len()).unwrap(), report.bytes);
        let parsed = Recording::parse(&bytes).unwrap();
        assert_eq!(parsed.count(Kind::Retransmit), 2);
        assert_eq!(parsed.end().unwrap().reason, EndReason::Normal);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_crashed_session_leaves_a_readable_part_file_that_is_kept() {
        let dir = temp_dir("leftover");
        let rec = Recorder::start_in_dir(&dir, &meta(1_000), Retention::default()).unwrap();
        rec.record(10, input(1));
        // 没有 finish：进程被杀，队列关闭，.part 留着
        let mut rec = rec;
        drop(rec.tx.take());
        let _ = Finishing(rec.thread.take()).wait();
        let part = dir.join(part_name(&file_name(&meta(1_000))));
        assert!(part.exists());
        // 下一次会话开始：遗留的 .part 原样保留，照样读得出来
        let next = Recorder::start_in_dir(&dir, &meta(2_000), Retention::default()).unwrap();
        let _ = next.finish(1, EndReason::Normal).wait();
        let parsed = Recording::parse(&fs::read(&part).unwrap()).unwrap();
        assert_eq!(parsed.count(Kind::Retransmit), 1);
        assert!(parsed.end().is_none());
        assert_eq!(
            classify(part.file_name().unwrap().to_str().unwrap()),
            Some(peerdesk_core::record::FileState::Part)
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn opening_fails_loudly_when_the_directory_cannot_be_made() {
        let dir = temp_dir("blocked");
        let file = dir.join("not-a-dir");
        fs::write(&file, b"x").unwrap();
        assert!(
            Recorder::start_in_dir(&file.join("records"), &meta(1), Retention::default()).is_err()
        );
        // 同名文件已经有了（同一秒同一会话号）：不覆盖，报错
        let first = Recorder::start_in_dir(&dir, &meta(5), Retention::default()).unwrap();
        assert!(Recorder::start_in_dir(&dir, &meta(5), Retention::default()).is_err());
        let _ = first.finish(1, EndReason::Normal).wait();
        let _ = fs::remove_dir_all(&dir);
    }

    fn make(dir: &Path, name: &str, len: usize) {
        fs::write(dir.join(name), vec![0u8; len]).unwrap();
    }

    #[test]
    fn retention_deletes_the_oldest_first_by_count() {
        let dir = temp_dir("count");
        let names: Vec<String> = (1..=5)
            .map(|i| format!("2026112{i}T000000Z-00000000-host.pdsr"))
            .collect();
        for n in &names {
            make(&dir, n, 10);
        }
        make(&dir, "notes.txt", 1_000_000);
        make(&dir, "20261120T000000Z-00000000-host.pdsr.part", 10); // 遗留的，最旧
        let policy = Retention {
            max_files: 4,
            max_bytes: u64::MAX,
        };
        let report = apply_retention(&dir, &policy, None, 0);
        assert_eq!(
            report.deleted,
            vec![
                "20261120T000000Z-00000000-host.pdsr.part".to_owned(),
                names[0].clone()
            ]
        );
        assert_eq!((report.remaining_files, report.remaining_bytes), (4, 40));
        assert!(dir.join("notes.txt").exists(), "不是记录文件的不碰");
        // 为新文件留一个位置：再删一份
        let report = apply_retention(&dir, &policy, None, 1);
        assert_eq!(report.deleted, vec![names[1].clone()]);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn retention_by_size_never_deletes_the_protected_file() {
        let dir = temp_dir("bytes");
        make(&dir, "20261121T000000Z-00000000-host.pdsr", 100);
        make(&dir, "20261122T000000Z-00000000-host.pdsr", 100);
        make(&dir, "20261123T000000Z-00000000-host.pdsr", 100);
        let policy = Retention {
            max_files: 20,
            max_bytes: 150,
        };
        // 保护最旧的那份：删中间的，再还不够也不动保护的
        let report = apply_retention(
            &dir,
            &policy,
            Some("20261121T000000Z-00000000-host.pdsr"),
            0,
        );
        assert_eq!(
            report.deleted,
            vec![
                "20261122T000000Z-00000000-host.pdsr".to_owned(),
                "20261123T000000Z-00000000-host.pdsr".to_owned()
            ]
        );
        assert_eq!((report.remaining_files, report.remaining_bytes), (1, 100));
        assert!(dir.join("20261121T000000Z-00000000-host.pdsr").exists());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn retention_failures_are_counted_not_hidden() {
        let dir = temp_dir("fail");
        // 名字像记录文件、其实是个目录：remove_file 删不掉
        fs::create_dir_all(dir.join("20261121T000000Z-00000000-host.pdsr")).unwrap();
        make(&dir, "20261122T000000Z-00000000-host.pdsr", 10);
        let policy = Retention {
            max_files: 1,
            max_bytes: u64::MAX,
        };
        let report = apply_retention(&dir, &policy, None, 0);
        assert_eq!(report.failed, 1);
        assert!(report.deleted.is_empty());
        // 目录不存在：列目录出错，写进报告
        let missing = apply_retention(&dir.join("nope"), &policy, None, 0);
        assert!(missing.list_error.is_some());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn visit_ids_are_random() {
        let (a, b) = (new_visit_id().unwrap(), new_visit_id().unwrap());
        assert_ne!(a, b);
    }

    #[test]
    fn defaults_follow_the_design() {
        let r = Retention::default();
        assert_eq!((r.max_files, r.max_bytes), (20, 200 * 1024 * 1024));
        assert_eq!(QUEUE_CAPACITY, 8_000);
    }
}
