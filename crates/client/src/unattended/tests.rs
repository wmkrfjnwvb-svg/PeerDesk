#![allow(clippy::unwrap_used, clippy::indexing_slicing)]

use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use super::*;

fn temp(name: &str) -> PathBuf {
    let dir =
        std::env::temp_dir().join(format!("peerdesk-unattended-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    // 打开日志（只第一次生效）：日志语句的覆盖率不随测试顺序变化
    crate::logfile::init(
        &std::env::temp_dir().join("peerdesk-unattended-logs"),
        false,
    );
    dir
}

#[test]
fn decides_per_session() {
    // 有人在控制台、没有代理：拉起
    assert_eq!(decide(Some(1), None), Action::Spawn(1));
    // 代理退出了：重拉
    assert_eq!(decide(Some(1), Some((1, false))), Action::Spawn(1));
    // 代理在当前会话里跑着：不动
    assert_eq!(decide(Some(1), Some((1, true))), Action::Wait);
    // 切换了用户：结束旧的（下一步再拉新的）
    assert_eq!(decide(Some(2), Some((1, true))), Action::Kill);
    // 没人在控制台
    assert_eq!(decide(None, Some((1, true))), Action::Kill);
    assert_eq!(decide(None, None), Action::Wait);
    assert_eq!(decide(None, Some((1, false))), Action::Wait);
}

#[derive(Default)]
struct Log {
    spawned: Vec<u32>,
    killed: usize,
    sas: usize,
}

struct FakeChild(Arc<AtomicBool>, Arc<Mutex<Log>>);

impl Running for FakeChild {
    fn running(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }
    fn kill(&self) {
        self.0.store(false, Ordering::Relaxed);
        self.1.lock().unwrap().killed += 1;
    }
}

/// 按脚本走的假系统：第 n 次查询活动会话时返回 `sessions[n]`，脚本走完时让服务停下。
struct FakeSystem {
    sessions: Vec<Option<u32>>,
    step: usize,
    fail_spawn: bool,
    log: Arc<Mutex<Log>>,
    stop: Arc<AtomicBool>,
    last: Option<Arc<AtomicBool>>,
    /// 第几步时让代理自己退出。
    crash_at: Option<usize>,
    /// 第几步时代理请求 Ctrl+Alt+Del。
    sas_at: Option<(usize, PathBuf)>,
}

impl System for FakeSystem {
    type Child = FakeChild;
    fn active_session(&mut self) -> Option<u32> {
        let s = self.sessions.get(self.step).copied().flatten();
        if self.crash_at == Some(self.step) {
            if let Some(alive) = &self.last {
                alive.store(false, Ordering::Relaxed);
            }
        }
        if let Some((at, file)) = &self.sas_at {
            if *at == self.step {
                std::fs::write(file, b"").unwrap();
            }
        }
        self.step += 1;
        if self.step >= self.sessions.len() {
            self.stop.store(true, Ordering::Relaxed);
        }
        s
    }
    fn spawn(&mut self, session: u32) -> Result<FakeChild, String> {
        if self.fail_spawn {
            return Err("拒绝访问".into());
        }
        self.log.lock().unwrap().spawned.push(session);
        let alive = Arc::new(AtomicBool::new(true));
        self.last = Some(Arc::clone(&alive));
        Ok(FakeChild(alive, Arc::clone(&self.log)))
    }
    fn send_sas(&mut self) {
        self.log.lock().unwrap().sas += 1;
    }
}

fn fake(sessions: Vec<Option<u32>>) -> FakeSystem {
    FakeSystem {
        sessions,
        step: 0,
        fail_spawn: false,
        log: Arc::default(),
        stop: Arc::default(),
        last: None,
        crash_at: None,
        sas_at: None,
    }
}

#[test]
fn supervisor_follows_the_console_session() {
    let base = temp("supervise");
    // 会话 1 → 切换到会话 2 → 没人 → 会话 2
    let mut sys = fake(vec![Some(1), Some(1), Some(2), Some(2), None, Some(2)]);
    std::fs::create_dir_all(agent_dir(&base)).unwrap();
    sys.sas_at = Some((1, agent_dir(&base).join(SAS_FILE)));
    let stop = Arc::clone(&sys.stop);
    supervise(&mut sys, &base, &stop);
    let log = sys.log.lock().unwrap();
    // 会话 1 拉起；换到 2 时结束旧的、下一步在 2 里拉起（换会话不等冷却）；
    // 没人在控制台时结束，回来后再拉起
    assert_eq!(log.spawned, [1, 2, 2]);
    // 两次结束，加上服务停止时结束正在跑的那个
    assert_eq!(log.killed, 3);
    assert_eq!(log.sas, 1, "代理请求了一次 Ctrl+Alt+Del");
    assert!(!agent_dir(&base).join(SAS_FILE).exists(), "请求文件已删掉");
}

#[test]
fn supervisor_respawns_a_crashed_agent_after_a_pause() {
    let base = temp("respawn");
    let steps = usize::try_from(RESPAWN_GAP).unwrap() + 3;
    let mut sys = fake(vec![Some(3); steps]);
    sys.crash_at = Some(1);
    let stop = Arc::clone(&sys.stop);
    supervise(&mut sys, &base, &stop);
    let log = sys.log.lock().unwrap();
    // 崩了以后等冷却期满才重拉（不会每 200 ms 拉一次）
    assert_eq!(log.spawned, [3, 3]);
    // 服务停止时结束正在跑的代理
    assert_eq!(log.killed, 1);
}

#[test]
fn supervisor_reports_why_the_agent_did_not_start() {
    let base = temp("spawnfail");
    let mut sys = fake(vec![Some(1)]);
    sys.fail_spawn = true;
    let stop = Arc::clone(&sys.stop);
    supervise(&mut sys, &base, &stop);
    let status = read_status(&base).unwrap();
    assert!(!status.running);
    assert!(status.detail.contains("拒绝访问"), "{}", status.detail);
}

#[test]
fn status_round_trips() {
    let base = temp("status");
    assert_eq!(read_status(&base), None);
    let status = Status {
        running: true,
        code: Some(123_456_789),
        detail: "等待连接\n第二行".into(),
    };
    write_status(&base, &status).unwrap();
    let back = read_status(&base).unwrap();
    assert!(back.running);
    assert_eq!(back.code, Some(123_456_789));
    assert_eq!(back.detail, "等待连接 第二行");
    write_status(&base, &Status::failed("坏了")).unwrap();
    assert_eq!(read_status(&base).unwrap().code, None);
}

#[test]
fn recognises_ctrl_alt_delete() {
    let mut d = SasDetector::default();
    // 只按 Delete
    assert!(!d.key(0x4C, true));
    assert!(!d.key(0x4C, false));
    // 左 Ctrl + 右 Alt + Delete
    assert!(!d.key(0xE0, true));
    assert!(!d.key(0xE6, true));
    assert!(d.key(0x4C, true));
    assert!(!d.key(0x4C, false), "抬起不算");
    // 松开 Alt 后再按 Delete 不算
    assert!(!d.key(0xE6, false));
    assert!(!d.key(0x4C, true));
    // 右 Ctrl + 左 Alt
    assert!(!d.key(0xE0, false));
    assert!(!d.key(0xE4, true));
    assert!(!d.key(0xE2, true));
    assert!(d.key(0x4C, true));
}

#[test]
fn install_registers_the_service_and_locks_the_agent_folder() {
    let agent = Path::new(r"C:\ProgramData\PeerDesk\agent");
    let commands = install_commands(r"C:\Program Files\PeerDesk\peerdesk.exe", agent);
    let lines: Vec<String> = commands.iter().map(|c| c.join(" ")).collect();
    assert!(lines
        .iter()
        .any(|l| l.contains(r#"binPath= "C:\Program Files\PeerDesk\peerdesk.exe" service run"#)));
    assert!(lines.iter().any(|l| l.contains("obj= LocalSystem")));
    assert!(lines.iter().any(|l| l.contains("SoftwareSASGeneration")));
    assert!(lines
        .iter()
        .any(|l| l.starts_with(r"icacls C:\ProgramData\PeerDesk\agent /inheritance:r")));
    assert_eq!(lines.last().unwrap(), "sc start PeerDesk");
    assert_eq!(uninstall_commands().len(), 2);
}

#[test]
fn install_tolerates_only_expected_failures() {
    let base = temp("install");
    let mut ran = Vec::new();
    let mut out = Vec::new();
    // 首次安装：sc stop 失败（没有服务），其他都成功
    let mut runner = |c: &Command| {
        ran.push(c.join(" "));
        Ok((c.get(1).map(String::as_str) != Some("stop"), String::new()))
    };
    let code = service_with(&["install".into()], &mut out, &base, &base, &mut runner).unwrap();
    assert_eq!(code, crate::EXIT_OK);
    assert_eq!(ran.len(), 8);
    let text = String::from_utf8(out).unwrap();
    assert!(text.contains("✗ sc stop PeerDesk"), "{text}");
    assert!(text.contains("还没有设置无人值守密码"), "{text}");
    // 改注册表失败（没有管理员身份）：停下并说明
    let mut out = Vec::new();
    let mut runner = |c: &Command| {
        Ok((
            c.first().map(String::as_str) != Some("reg"),
            "拒绝访问。".into(),
        ))
    };
    let err = service_with(&["install".into()], &mut out, &base, &base, &mut runner).unwrap_err();
    assert!(err.contains("拒绝访问"), "{err}");
    // 命令本身运行不了
    let mut runner = |_: &Command| Err("找不到 sc".to_owned());
    assert!(service_with(
        &["uninstall".into()],
        &mut Vec::new(),
        &base,
        &base,
        &mut runner
    )
    .is_err());
}

#[test]
fn service_subcommands() {
    let base = temp("subcommands");
    let mut never =
        |_: &Command| -> Result<(bool, String), String> { panic!("不应运行系统命令") };
    let mut run = |args: &[&str]| {
        let args: Vec<String> = args.iter().map(|s| (*s).to_owned()).collect();
        let mut out = Vec::new();
        let r = service_with(&args, &mut out, &base, &base, &mut never);
        (r, String::from_utf8(out).unwrap())
    };
    let (r, text) = run(&["status"]);
    assert!(r.is_ok() && text.contains("没有安装"), "{text}");
    assert!(run(&["password"]).0.is_err());
    assert!(run(&["password", "short"]).0.unwrap_err().contains("至少"));
    assert!(run(&["password", "long-enough-1"]).0.is_ok());
    assert_eq!(load_password(&agent_dir(&base)).unwrap(), "long-enough-1");
    write_status(
        &base,
        &Status {
            running: true,
            code: Some(987_654_321),
            detail: "等待连接".into(),
        },
    )
    .unwrap();
    let (_, text) = run(&["status"]);
    assert!(
        text.contains("运行中") && text.contains("987 654 321"),
        "{text}"
    );
    write_status(&base, &Status::failed("x")).unwrap();
    assert!(run(&["status"]).1.contains("（没有）"));
    assert!(run(&["bogus"]).0.is_err());
    assert!(run(&[]).0.is_err());
    // 非 Windows 上不能作为服务运行
    #[cfg(not(windows))]
    assert!(run(&["run"]).0.unwrap_err().contains("Windows"));
    // 卸载：状态文件删掉
    let mut ok = |_: &Command| Ok((true, String::new()));
    service_with(
        &["uninstall".into()],
        &mut Vec::new(),
        &base,
        &base,
        &mut ok,
    )
    .unwrap();
    assert_eq!(read_status(&base), None);
}

#[test]
fn agent_refuses_without_a_password() {
    let base = temp("nopass");
    let stop = AtomicBool::new(false);
    let err = run_agent(&base, &agent_dir(&base), 0, &stop).unwrap_err();
    assert!(err.contains("无人值守密码"), "{err}");
    assert!(read_status(&base).unwrap().detail.contains("无人值守密码"));
}

#[test]
fn agent_hosts_with_the_unattended_password() {
    let base = temp("agent");
    let agent = agent_dir(&base);
    save_password(&agent, "first-password").unwrap();
    // 不连真服务器：服务器设置指向本机一个没人的端口，登记失败不影响被控
    ServerSettings {
        addr: "127.0.0.1:9".into(),
        ..ServerSettings::default()
    }
    .save(&agent)
    .unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let thread = {
        let (base, agent, stop) = (base.clone(), agent.clone(), Arc::clone(&stop));
        std::thread::spawn(move || run_agent(&base, &agent, 0, &stop))
    };
    let deadline = Instant::now() + Duration::from_secs(20);
    while !read_status(&base).is_some_and(|s| s.running) {
        assert!(Instant::now() < deadline, "代理没有开始监听");
        std::thread::sleep(Duration::from_millis(50));
    }
    // 改密码：代理几秒内换上
    save_password(&agent, "second-password").unwrap();
    std::thread::sleep(Duration::from_secs(6));
    stop.store(true, Ordering::Relaxed);
    thread.join().unwrap().unwrap();
    assert!(!read_status(&base).unwrap().running);
    assert!(save_password(&agent, "short").is_err());
}

#[test]
fn ctrl_alt_delete_from_the_controller_calls_the_hook() {
    use peerdesk_core::input::InputEvent;
    let _guard = HOOK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let hits = Arc::new(AtomicU32::new(0));
    let counter = Arc::clone(&hits);
    crate::local::set_sas_hook(Box::new(move || {
        counter.fetch_add(1, Ordering::Relaxed);
    }));
    let mut sink = crate::local::local_input(Arc::default()).unwrap();
    for (code, down) in [(0xE0, true), (0xE2, true), (0x4C, true), (0x4C, false)] {
        sink.inject(&InputEvent::Key { code, down });
    }
    assert_eq!(hits.load(Ordering::Relaxed), 1);
}

use std::sync::atomic::AtomicU32;

/// 改全局 Ctrl+Alt+Del 钩子的测试串行跑。
static HOOK: Mutex<()> = Mutex::new(());

#[test]
fn agent_command() {
    let _guard = HOOK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let base = temp("agentcmd");
    let base_arg = base.display().to_string();
    let mut out = Vec::new();
    // 没设密码：说明原因并退出
    assert_eq!(
        crate::run(&["agent".into(), "--dir".into(), base_arg], &mut out),
        crate::EXIT_FAILED
    );
    assert!(String::from_utf8(out).unwrap().contains("无人值守密码"));
    assert!(cmd_agent(&["--bogus".into()], &mut Vec::new()).is_err());
    let mut out = Vec::new();
    assert_eq!(
        crate::run(&["service".into(), "bogus".into()], &mut out),
        crate::EXIT_FAILED
    );
}

#[test]
fn agent_says_why_it_stopped() {
    let base = temp("portbusy");
    save_password(&agent_dir(&base), "long-enough-pw").unwrap();
    ServerSettings {
        addr: "127.0.0.1:9".into(),
        ..ServerSettings::default()
    }
    .save(&agent_dir(&base))
    .unwrap();
    // 端口被别的程序占着：被控起不来，代理说明原因
    let busy = std::net::UdpSocket::bind("0.0.0.0:0").unwrap();
    let port = busy.local_addr().unwrap().port();
    // Windows 上同一个 UDP 端口可以重复绑定，被控照样起来：最多等 15 秒后让它停下，不能卡住测试
    let stop = Arc::new(AtomicBool::new(false));
    let timer = {
        let stop = Arc::clone(&stop);
        std::thread::spawn(move || {
            let until = Instant::now() + Duration::from_secs(15);
            while Instant::now() < until && !stop.load(Ordering::Relaxed) {
                std::thread::sleep(Duration::from_millis(50));
            }
            stop.store(true, Ordering::Relaxed);
        })
    };
    let result = run_agent(&base, &agent_dir(&base), port, &stop);
    stop.store(true, Ordering::Relaxed);
    timer.join().unwrap();
    // Linux、macOS：端口冲突，被控起不来，代理说明原因
    #[cfg(not(windows))]
    assert!(!result.unwrap_err().is_empty());
    #[cfg(windows)]
    let _ = result;
    assert!(!read_status(&base).unwrap().running);
    // 状态目录写不了：只记日志，照样说明原因
    let file = base.join("not-a-dir");
    std::fs::write(&file, b"").unwrap();
    assert!(run_agent(&file, &agent_dir(&file), 0, &AtomicBool::new(false)).is_err());
}

#[test]
fn base_dir_uses_program_data() {
    let data = Path::new("/home/u/.config/peerdesk");
    assert_eq!(
        base_dir_from(Some(r"C:\ProgramData".into()), data),
        Path::new(r"C:\ProgramData").join("PeerDesk")
    );
    assert_eq!(base_dir_from(Some("".into()), data), data.join("service"));
    assert_eq!(base_dir_from(None, data), data.join("service"));
    assert!(base_dir().ends_with("PeerDesk") || base_dir().ends_with("service"));
}

#[test]
fn host_events_update_the_status() {
    use peerdesk_net::NetError;
    let mut st = Status::failed("");
    apply(
        &mut st,
        HostEvent::Listening {
            port: 4433,
            ipv6: true,
        },
    )
    .unwrap();
    assert!(st.running && st.detail.contains("4433"));
    apply(
        &mut st,
        HostEvent::Registered {
            observed: "1.2.3.4:5".parse().unwrap(),
            code: 42,
        },
    )
    .unwrap();
    assert_eq!(st.code, Some(42));
    apply(
        &mut st,
        HostEvent::RegisterFailed(NetError::Io("超时".into())),
    )
    .unwrap();
    assert!(st.detail.contains("登记失败"));
    apply(&mut st, HostEvent::Established).unwrap();
    assert!(st.detail.contains("正在被远程"));
    apply(&mut st, HostEvent::SessionEnded(Ok(String::new()))).unwrap();
    assert_eq!(st.detail, "等待连接");
    apply(&mut st, HostEvent::Refused("屏幕已锁定".into())).unwrap();
    assert_eq!(st.detail, "屏幕已锁定");
    apply(&mut st, HostEvent::Network(Default::default())).unwrap();
    apply(&mut st, HostEvent::Established).unwrap();
    apply(
        &mut st,
        HostEvent::HandshakeFailed(NetError::Io("x".into())),
    )
    .unwrap();
    assert!(st.detail.contains("正在被远程"), "无关事件不改状态");
    assert_eq!(
        apply(&mut st, HostEvent::Stopped(None)).unwrap_err(),
        "被控已停止"
    );
    assert!(apply(
        &mut st,
        HostEvent::Stopped(Some(NetError::Io("端口被占用".into())))
    )
    .unwrap_err()
    .contains("端口被占用"));
}

#[test]
fn runs_real_commands() {
    assert!(system_runner(&vec![]).is_err());
    let (ok, _) = system_runner(&cmd(&["cargo", "--version"])).unwrap();
    assert!(ok);
    let (ok, _) = system_runner(&cmd(&["cargo", "no-such-subcommand-xyz"])).unwrap();
    assert!(!ok);
    assert!(system_runner(&cmd(&["peerdesk-no-such-program"])).is_err());
    // 用法错误不碰系统
    assert!(cmd_service(&["bogus".into()], &mut Vec::new()).is_err());
}

#[test]
fn install_copies_the_installers_server() {
    let base = temp("copyserver");
    let user = temp("copyserver-user");
    ServerSettings {
        addr: "10.0.0.1:4433".into(),
        ..ServerSettings::default()
    }
    .save(&user)
    .unwrap();
    let mut ok = |_: &Command| Ok((true, String::new()));
    service_with(&["install".into()], &mut Vec::new(), &base, &user, &mut ok).unwrap();
    assert_eq!(
        ServerSettings::load(&agent_dir(&base)).addr,
        "10.0.0.1:4433"
    );
}

#[test]
fn sas_request_file() {
    let base = temp("sasfile");
    let file = base.join(SAS_FILE);
    sas_requester(file.clone())();
    assert!(file.exists());
    // 目录不存在：只记日志
    sas_requester(base.join("missing").join(SAS_FILE))();
}
