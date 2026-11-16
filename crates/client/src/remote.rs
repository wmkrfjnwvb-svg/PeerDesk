//! 真实连接的命令：`host`、`connect`、`bench-transport`。

use std::io::Write;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use peerdesk_core::auth::{Auth, Limiter};
use peerdesk_core::crypto::Policy;
use peerdesk_core::input::InputEvent;
use peerdesk_core::proto::Caps;
use peerdesk_core::session::SessionConfig;
use peerdesk_net::controller::{self, ControllerOptions, Upload};
use peerdesk_net::demo::{to_bmp, CountSink, DemoDesktop};
use peerdesk_net::fs_store::scan;
use peerdesk_net::host::{self, HostOptions, InputSink, Permissions};
use peerdesk_net::identity::{parse_hex, to_hex};
use peerdesk_net::{bench, Fingerprint, Identity, NetError, Node};

use crate::{parse_number, usage_error, EXIT_FAILED, EXIT_OK};

/// 默认的数据目录：优先用系统的每用户配置目录。
fn default_data_dir() -> PathBuf {
    let from_env = |name: &str| {
        std::env::var_os(name)
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
    };
    from_env("PEERDESK_DATA_DIR")
        .or_else(|| from_env("APPDATA").map(|p| p.join("PeerDesk")))
        .or_else(|| from_env("XDG_CONFIG_HOME").map(|p| p.join("peerdesk")))
        .or_else(|| from_env("HOME").map(|p| p.join(".config").join("peerdesk")))
        .unwrap_or_else(|| PathBuf::from(".peerdesk"))
}

fn runtime() -> Result<tokio::runtime::Runtime, NetError> {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|e| NetError::Io(format!("启动运行时失败：{e}")))
}

fn finish(out: &mut dyn Write, result: Result<u8, NetError>) -> u8 {
    result.unwrap_or_else(|e| {
        let _ = writeln!(out, "错误：{e}");
        EXIT_FAILED
    })
}

fn session(identity: &Identity, policy: &Policy, auth: Auth) -> SessionConfig {
    SessionConfig {
        device_id: identity.device_id(),
        region: String::new(),
        caps: Caps::H264,
        policy: policy.clone(),
        auth,
        // 由网络层替换成真实连接的通道绑定。
        binding: Vec::new(),
    }
}

/// 输入注入接入之前：只计数。
struct CountInput(Arc<AtomicU64>);

impl InputSink for CountInput {
    fn inject(&mut self, _event: &InputEvent) {
        self.0.fetch_add(1, Ordering::Relaxed);
    }
}

struct HostArgs {
    listen: SocketAddr,
    data_dir: PathBuf,
    recv_dir: Option<PathBuf>,
    fps: u32,
    once: bool,
    auth: Auth,
    trusted: Vec<Fingerprint>,
    open: bool,
    permissions: Permissions,
}

/// 密码从环境变量读，避免出现在进程列表和命令历史里；`--password` 仅为方便测试。
fn password_from_env() -> Option<String> {
    std::env::var("PEERDESK_PASSWORD")
        .ok()
        .filter(|p| !p.is_empty())
}

fn parse_host(args: &[String]) -> Result<HostArgs, String> {
    let mut parsed = HostArgs {
        listen: ([0, 0, 0, 0], 4433).into(),
        data_dir: default_data_dir(),
        recv_dir: None,
        fps: 60,
        once: false,
        auth: password_from_env().map_or(Auth::Open, Auth::Password),
        trusted: Vec::new(),
        open: false,
        permissions: Permissions::default(),
    };
    let mut it = args.iter();
    while let Some(flag) = it.next() {
        match flag.as_str() {
            "--listen" => parsed.listen = parse_number(flag, it.next())?,
            "--data-dir" => parsed.data_dir = it.next().ok_or("--data-dir 需要一个值")?.into(),
            "--recv-dir" => {
                parsed.recv_dir = Some(it.next().ok_or("--recv-dir 需要一个值")?.into())
            }
            "--fps" => parsed.fps = parse_number(flag, it.next())?,
            "--once" => parsed.once = true,
            "--password" => {
                parsed.auth = Auth::Password(it.next().ok_or("--password 需要一个值")?.clone());
            }
            "--trust" => {
                let hex = it.next().ok_or("--trust 需要一个值")?;
                parsed
                    .trusted
                    .push(parse_hex(hex).map_err(|e| e.to_string())?);
            }
            "--open" => parsed.open = true,
            "--no-input" => parsed.permissions.input = false,
            "--no-files" => parsed.permissions.files = false,
            other => return Err(format!("未知选项：{other}")),
        }
    }
    if parsed.fps == 0 || parsed.fps > 240 {
        return Err("--fps 的范围是 1 到 240".into());
    }
    match (&parsed.auth, parsed.open) {
        (Auth::Password(p), false) if p.len() < 8 => {
            return Err("密码至少 8 个字符".into());
        }
        (Auth::Password(_), true) => return Err("--open 不能和密码一起用".into()),
        (Auth::Open, false) if parsed.trusted.is_empty() => {
            return Err(
                "必须指定谁可以控制这台设备：设置环境变量 PEERDESK_PASSWORD，\
                 或用 --trust <指纹> 指定受信任的设备，或用 --open 明确表示任何人都可以连"
                    .into(),
            );
        }
        (Auth::Password(_) | Auth::Open, _) => {}
    }
    if parsed.open {
        // 完全开放时，受信任列表没有意义。
        parsed.trusted.clear();
    } else if matches!(parsed.auth, Auth::Open) {
        // 只有受信任的设备可以连：其他设备面对的是一个没人知道的随机密码。
        let random = Identity::generate().map_err(|e| e.to_string())?;
        parsed.auth = Auth::Password(to_hex(&random.fingerprint()));
    }
    Ok(parsed)
}

async fn host_async(args: HostArgs, out: &mut dyn Write) -> Result<u8, NetError> {
    let policy = Policy::recommended();
    let identity = Identity::load_or_create(&args.data_dir)?;
    let node = Node::bind(args.listen, &identity, &policy)?;
    let recv_root = args
        .recv_dir
        .unwrap_or_else(|| args.data_dir.join("received"));
    let _ = writeln!(out, "监听 {}", node.local_addr()?);
    let _ = writeln!(out, "指纹 {}", to_hex(&identity.fingerprint()));
    let _ = writeln!(out, "控制端连接时请核对这个指纹。按 Ctrl+C 退出。");
    if args.open {
        let _ = writeln!(
            out,
            "警告：--open 模式，任何能连到这个端口的人都可以控制这台设备。"
        );
    }
    let limiter = Arc::new(Mutex::new(Limiter::default()));
    let _ = out.flush();

    while let Some(incoming) = node.accept().await {
        let link = match incoming {
            Ok(link) => link,
            Err(e) => {
                let _ = writeln!(out, "连接握手失败：{e}");
                continue;
            }
        };
        let _ = writeln!(
            out,
            "已连接 {}，对端指纹 {}",
            link.remote_addr(),
            to_hex(link.peer_fingerprint())
        );
        let _ = out.flush();
        let inputs = Arc::new(AtomicU64::new(0));
        let result = host::serve(
            link,
            HostOptions {
                trusted: args.trusted.clone(),
                permissions: args.permissions,
                limiter: Arc::clone(&limiter),
                ..HostOptions::new(
                    session(&identity, &policy, args.auth.clone()),
                    args.fps,
                    recv_root.clone(),
                )
            },
            Box::new(DemoDesktop::new(1280, 720, 1)),
            Box::new(CountInput(Arc::clone(&inputs))),
        )
        .await;
        match result {
            Ok(report) => {
                let _ = writeln!(
                    out,
                    "会话结束（{}）：发出 {} 帧共 {} 字节，结束时码率 {} kbps（{}），\
                     注入 {} 个输入事件（拒绝 {} 个），收到 {} 个文件",
                    report.end,
                    report.frames_sent,
                    report.video_bytes,
                    report.final_rate_bps / 1000,
                    report.profile,
                    report.input_events,
                    report.input_denied,
                    report.files.len()
                );
                for file in &report.files {
                    let _ = writeln!(out, "  {} {:?}", file.path, file.outcome);
                }
            }
            Err(e) => {
                // 没建立起来的会话（例如对端核对指纹后放弃）不算数，继续等下一个。
                let _ = writeln!(out, "会话失败：{e}");
                let _ = out.flush();
                continue;
            }
        }
        let _ = out.flush();
        if args.once {
            break;
        }
    }
    node.close();
    node.wait_idle().await;
    Ok(EXIT_OK)
}

/// `peerdesk host`。
pub fn host(args: &[String], out: &mut dyn Write) -> u8 {
    let parsed = match parse_host(args) {
        Ok(parsed) => parsed,
        Err(why) => return usage_error(out, &why),
    };
    let result = runtime().and_then(|rt| rt.block_on(host_async(parsed, out)));
    finish(out, result)
}

struct ConnectArgs {
    addr: SocketAddr,
    pin: Option<Fingerprint>,
    data_dir: PathBuf,
    seconds: u64,
    send: Option<PathBuf>,
    auth: Auth,
    snapshot: Option<PathBuf>,
}

fn parse_connect(args: &[String]) -> Result<ConnectArgs, String> {
    let (addr, rest) = args.split_first().ok_or("connect 需要被控端的 地址:端口")?;
    let mut parsed = ConnectArgs {
        addr: addr
            .parse()
            .map_err(|_| format!("不是合法的 地址:端口：{addr}"))?,
        pin: None,
        data_dir: default_data_dir(),
        seconds: 5,
        send: None,
        auth: password_from_env().map_or(Auth::Open, Auth::Password),
        snapshot: None,
    };
    let mut it = rest.iter();
    while let Some(flag) = it.next() {
        match flag.as_str() {
            "--pin" => {
                let hex = it.next().ok_or("--pin 需要一个值")?;
                parsed.pin = Some(parse_hex(hex).map_err(|e| e.to_string())?);
            }
            "--data-dir" => parsed.data_dir = it.next().ok_or("--data-dir 需要一个值")?.into(),
            "--seconds" => parsed.seconds = parse_number(flag, it.next())?,
            "--send" => parsed.send = Some(it.next().ok_or("--send 需要一个值")?.into()),
            "--snapshot" => {
                parsed.snapshot = Some(it.next().ok_or("--snapshot 需要一个值")?.into());
            }
            "--password" => {
                parsed.auth = Auth::Password(it.next().ok_or("--password 需要一个值")?.clone());
            }
            other => return Err(format!("未知选项：{other}")),
        }
    }
    Ok(parsed)
}

fn plan_upload(path: &Path) -> Result<Upload, NetError> {
    let bad = || NetError::Io(format!("无法上传：{}", path.display()));
    let name = path.file_name().and_then(|n| n.to_str()).ok_or_else(bad)?;
    let root = match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent.to_path_buf(),
        Some(_) | None => PathBuf::from("."),
    };
    let files = scan(&root, name)?;
    Ok(Upload { root, files })
}

async fn connect_async(args: ConnectArgs, out: &mut dyn Write) -> Result<u8, NetError> {
    let policy = Policy::recommended();
    let identity = Identity::load_or_create(&args.data_dir)?;
    let upload = args.send.as_deref().map(plan_upload).transpose()?;
    let node = Node::bind(([0, 0, 0, 0], 0).into(), &identity, &policy)?;
    let link = node.connect(args.addr, args.pin.as_ref()).await?;
    let _ = writeln!(out, "已连接 {}", link.remote_addr());
    let _ = writeln!(out, "对端指纹 {}", to_hex(link.peer_fingerprint()));
    if args.pin.is_none() {
        let _ = writeln!(
            out,
            "警告：没有用 --pin 指定指纹，无法确认连到的是不是你的设备。"
        );
    }
    let _ = out.flush();

    let sink = CountSink::new();
    // 输入采集接入之前没有事件可发；发送端保持打开直到会话结束。
    let (_input_tx, input_rx) = tokio::sync::mpsc::channel(64);
    let report = controller::run(
        link,
        ControllerOptions {
            session: session(&identity, &policy, args.auth.clone()),
            deadline_us: 100_000,
            duration: Some(Duration::from_secs(args.seconds)),
        },
        Box::new(sink.clone()),
        input_rx,
        upload,
    )
    .await?;
    node.close();

    let lines = [
        format!("会话结束      {}", report.end),
        format!("加密套件      {}", report.suite),
        format!(
            "帧            送达 {}，作废 {}，解码失败 {}，纠错恢复 {} 个分片",
            report.frames_delivered, report.frames_expired, report.bad_frames, report.recovered
        ),
        format!("画面更新      {} 次", sink.count()),
        format!("画面数据      {} 字节", report.video_bytes),
        format!("往返延迟      {} us", report.rtt.as_micros()),
        format!("异常数据报    {}", report.bad_datagrams),
    ];
    for line in lines {
        let _ = writeln!(out, "{line}");
    }
    if let Some(screen) = &report.screen {
        let _ = writeln!(out, "画面尺寸      {}×{}", screen.width(), screen.height());
        if let Some(path) = &args.snapshot {
            std::fs::write(path, to_bmp(screen))
                .map_err(|e| NetError::Io(format!("写入 {} 失败：{e}", path.display())))?;
            let _ = writeln!(out, "画面已保存    {}", path.display());
        }
    }
    if !report.files.is_empty() {
        let _ = writeln!(out, "上传          {} 字节", report.file_bytes);
        for file in &report.files {
            let _ = writeln!(out, "  {} {:?}", file.path, file.outcome);
        }
    }
    Ok(EXIT_OK)
}

/// `peerdesk connect`。
pub fn connect(args: &[String], out: &mut dyn Write) -> u8 {
    let parsed = match parse_connect(args) {
        Ok(parsed) => parsed,
        Err(why) => return usage_error(out, &why),
    };
    let result = runtime().and_then(|rt| rt.block_on(connect_async(parsed, out)));
    finish(out, result)
}

/// `peerdesk bench-transport`。
pub fn bench(args: &[String], out: &mut dyn Write) -> u8 {
    let packets = match args {
        [] => 1200usize,
        [flag, value] if flag == "--packets" => match value.parse::<usize>() {
            Ok(n) if (1..=100_000).contains(&n) => n,
            Ok(_) | Err(_) => return usage_error(out, "--packets 的范围是 1 到 100000"),
        },
        _ => return usage_error(out, "bench-transport 只接受 --packets <N>"),
    };
    let result = runtime()
        .and_then(|rt| rt.block_on(bench::compare(packets, 1100)))
        .map(|c| {
            let _ = writeln!(
                out,
                "本机回环，{} 个包，每包 {} 字节（单向延迟，微秒）",
                c.packets, c.payload_bytes
            );
            let _ = writeln!(out, "              收到    p50    p99    最大");
            for (name, l) in [("裸 UDP", c.udp), ("QUIC 数据报", c.quic)] {
                let _ = writeln!(
                    out,
                    "{name:<12}{:>6} {:>6} {:>6} {:>7}",
                    l.received, l.p50_us, l.p99_us, l.max_us
                );
            }
            let _ = writeln!(
                out,
                "QUIC 相对裸 UDP 的中位数差值：{} us",
                i128::from(c.quic.p50_us) - i128::from(c.udp.p50_us)
            );
            EXIT_OK
        });
    finish(out, result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    fn strings(args: &[&str]) -> Vec<String> {
        args.iter().map(|s| (*s).to_owned()).collect()
    }

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("peerdesk-cli-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// 可以在另一个线程里边写边读的输出。
    #[derive(Clone, Default)]
    struct Shared(Arc<Mutex<Vec<u8>>>);
    impl Write for Shared {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    impl Shared {
        fn text(&self) -> String {
            String::from_utf8(self.0.lock().unwrap().clone()).unwrap()
        }
        fn wait_for(&self, needle: &str) -> String {
            let deadline = std::time::Instant::now() + Duration::from_secs(20);
            loop {
                let text = self.text();
                if let Some(line) = text.lines().find(|l| l.starts_with(needle)) {
                    return line.trim_start_matches(needle).trim().to_owned();
                }
                assert!(
                    std::time::Instant::now() < deadline,
                    "等待 {needle:?} 超时：{text}"
                );
                std::thread::sleep(Duration::from_millis(20));
            }
        }
    }

    fn run(args: &[&str]) -> (u8, String) {
        let mut out = Vec::new();
        let code = crate::run(&strings(args), &mut out);
        (code, String::from_utf8(out).unwrap())
    }

    #[test]
    fn host_and_connect_end_to_end_with_upload() {
        let (host_dir, ctrl_dir, files) = (temp_dir("host"), temp_dir("ctrl"), temp_dir("files"));
        std::fs::create_dir_all(files.join("docs")).unwrap();
        std::fs::write(files.join("docs/a.txt"), b"alpha").unwrap();
        std::fs::write(files.join("docs/b.bin"), vec![7u8; 200_000]).unwrap();

        let host_out = Shared::default();
        let mut writer = host_out.clone();
        let host_args = strings(&[
            "host",
            "--listen",
            "127.0.0.1:0",
            "--data-dir",
            host_dir.to_str().unwrap(),
            "--fps",
            "100",
            "--once",
            "--open",
        ]);
        let host_thread = std::thread::spawn(move || crate::run(&host_args, &mut writer));
        let addr = host_out.wait_for("监听");
        let pin = host_out.wait_for("指纹");

        let (code, text) = run(&[
            "connect",
            &addr,
            "--pin",
            &pin,
            "--data-dir",
            ctrl_dir.to_str().unwrap(),
            "--snapshot",
            ctrl_dir.join("seen.bmp").to_str().unwrap(),
            "--seconds",
            "1",
            "--send",
            files.join("docs").to_str().unwrap(),
        ]);
        assert_eq!(code, EXIT_OK, "{text}");
        assert!(text.contains(&format!("对端指纹 {pin}")));
        assert!(text.contains("加密套件      aes-256-gcm"));
        assert!(text.contains("解码失败 0"));
        assert!(text.contains("画面尺寸      1280×720"));
        // 看到的画面存成了一张 1280×720 的 BMP
        let bmp = std::fs::read(ctrl_dir.join("seen.bmp")).unwrap();
        assert_eq!(&bmp[..2], b"BM");
        assert_eq!(bmp.len(), 54 + 1280 * 720 * 3);
        assert!(text.contains("docs/a.txt Some(Done)"));
        assert!(text.contains("docs/b.bin Some(Done)"));
        assert!(!text.contains("警告"));

        assert_eq!(host_thread.join().unwrap(), EXIT_OK);
        let host_text = host_out.text();
        assert!(host_text.contains("已连接"), "{host_text}");
        assert!(host_text.contains("收到 2 个文件"), "{host_text}");
        assert_eq!(
            std::fs::read(host_dir.join("received/docs/a.txt")).unwrap(),
            b"alpha"
        );
        assert_eq!(
            std::fs::read(host_dir.join("received/docs/b.bin"))
                .unwrap()
                .len(),
            200_000
        );

        // 身份是持久的：重启被控端后指纹不变
        let again = Identity::load_or_create(&host_dir).unwrap();
        assert_eq!(to_hex(&again.fingerprint()), pin);
        for dir in [host_dir, ctrl_dir, files] {
            let _ = std::fs::remove_dir_all(dir);
        }
    }

    #[test]
    fn connect_without_pin_warns_and_wrong_pin_is_refused() {
        let (host_dir, ctrl_dir) = (temp_dir("host2"), temp_dir("ctrl2"));
        let host_out = Shared::default();
        let mut writer = host_out.clone();
        let host_args = strings(&[
            "host",
            "--listen",
            "127.0.0.1:0",
            "--data-dir",
            host_dir.to_str().unwrap(),
            "--recv-dir",
            host_dir.join("in").to_str().unwrap(),
            "--once",
            "--open",
        ]);
        let host_thread = std::thread::spawn(move || crate::run(&host_args, &mut writer));
        let addr = host_out.wait_for("监听");

        // 指纹不对：拒绝，不进入会话
        let wrong = "ab".repeat(32);
        let (code, text) = run(&[
            "connect",
            &addr,
            "--pin",
            &wrong,
            "--data-dir",
            ctrl_dir.to_str().unwrap(),
        ]);
        assert_eq!(code, EXIT_FAILED);
        assert!(text.contains("对端身份与预期不符"), "{text}");

        // 不给指纹：能连，但有警告
        let (code, text) = run(&[
            "connect",
            &addr,
            "--data-dir",
            ctrl_dir.to_str().unwrap(),
            "--seconds",
            "1",
        ]);
        assert_eq!(code, EXIT_OK, "{text}");
        assert!(text.contains("警告"));
        assert!(!text.contains("上传"));
        assert_eq!(host_thread.join().unwrap(), EXIT_OK);
        let host_text = host_out.text();
        assert!(host_text.contains("会话失败"), "{host_text}");
        assert!(host_text.contains("会话结束"), "{host_text}");
        for dir in [host_dir, ctrl_dir] {
            let _ = std::fs::remove_dir_all(dir);
        }
    }

    fn start_host(
        extra: &[&str],
        dir: &Path,
    ) -> (Shared, std::thread::JoinHandle<u8>, String, String) {
        let out = Shared::default();
        let mut writer = out.clone();
        let mut args = strings(&[
            "host",
            "--listen",
            "127.0.0.1:0",
            "--data-dir",
            dir.to_str().unwrap(),
            "--once",
        ]);
        args.extend(strings(extra));
        let thread = std::thread::spawn(move || crate::run(&args, &mut writer));
        let addr = out.wait_for("监听");
        let pin = out.wait_for("指纹");
        (out, thread, addr, pin)
    }

    #[test]
    fn password_protected_host_end_to_end() {
        let (host_dir, ctrl_dir) = (temp_dir("pw-host"), temp_dir("pw-ctrl"));
        let (host_out, host_thread, addr, pin) = start_host(
            &["--password", "correct horse", "--no-input", "--no-files"],
            &host_dir,
        );
        let c = ctrl_dir.to_str().unwrap();

        // 密码不对：进不去
        let (code, text) = run(&[
            "connect",
            &addr,
            "--pin",
            &pin,
            "--data-dir",
            c,
            "--password",
            "wrong guess",
        ]);
        assert_eq!(code, EXIT_FAILED);
        assert!(text.contains("密码错误"), "{text}");
        // 没带密码：进不去
        let (code, _) = run(&["connect", &addr, "--pin", &pin, "--data-dir", c]);
        assert_eq!(code, EXIT_FAILED);
        // 密码正确
        let (code, text) = run(&[
            "connect",
            &addr,
            "--pin",
            &pin,
            "--data-dir",
            c,
            "--password",
            "correct horse",
            "--seconds",
            "1",
        ]);
        assert_eq!(code, EXIT_OK, "{text}");
        assert_eq!(host_thread.join().unwrap(), EXIT_OK);
        let host_text = host_out.text();
        assert!(host_text.contains("会话失败"), "{host_text}");
        assert!(host_text.contains("会话结束"), "{host_text}");
        assert!(!host_text.contains("警告：--open"));
        for dir in [host_dir, ctrl_dir] {
            let _ = std::fs::remove_dir_all(dir);
        }
    }

    #[test]
    fn trusted_device_connects_without_password_and_strangers_cannot() {
        let (host_dir, friend_dir, stranger_dir) = (
            temp_dir("tr-host"),
            temp_dir("tr-friend"),
            temp_dir("tr-stranger"),
        );
        let friend = Identity::load_or_create(&friend_dir).unwrap();
        let friend_fp = to_hex(&friend.fingerprint());
        let (host_out, host_thread, addr, pin) = start_host(&["--trust", &friend_fp], &host_dir);

        // 陌生设备：没有密码可用（被控端内部是一个随机密码）
        let (code, text) = run(&[
            "connect",
            &addr,
            "--pin",
            &pin,
            "--data-dir",
            stranger_dir.to_str().unwrap(),
            "--password",
            "anything at all",
        ]);
        assert_eq!(code, EXIT_FAILED, "{text}");
        // 受信任的设备：不需要密码
        let (code, text) = run(&[
            "connect",
            &addr,
            "--pin",
            &pin,
            "--data-dir",
            friend_dir.to_str().unwrap(),
            "--seconds",
            "1",
        ]);
        assert_eq!(code, EXIT_OK, "{text}");
        assert_eq!(host_thread.join().unwrap(), EXIT_OK);
        assert!(host_out.text().contains(&friend_fp));
        for dir in [host_dir, friend_dir, stranger_dir] {
            let _ = std::fs::remove_dir_all(dir);
        }
    }

    #[test]
    fn host_refuses_to_start_without_an_access_rule() {
        use crate::EXIT_USAGE;
        let d = temp_dir("norule");
        let dir = d.to_str().unwrap();
        let (code, text) = run(&["host", "--data-dir", dir]);
        assert_eq!(code, EXIT_USAGE);
        assert!(text.contains("必须指定谁可以控制这台设备"), "{text}");
        for bad in [
            vec!["host", "--data-dir", dir, "--password", "short"],
            vec![
                "host",
                "--data-dir",
                dir,
                "--password",
                "long enough",
                "--open",
            ],
            vec!["host", "--data-dir", dir, "--password"],
            vec!["host", "--data-dir", dir, "--trust"],
            vec!["host", "--data-dir", dir, "--trust", "nothex"],
            vec!["connect", "127.0.0.1:1", "--password"],
        ] {
            assert_eq!(run(&bad).0, EXIT_USAGE, "{bad:?}");
        }
        // --open 时受信任列表被清空，并给出警告
        let parsed = parse_host(&strings(&["--open", "--trust", &"ab".repeat(32)])).unwrap();
        assert!(parsed.trusted.is_empty() && parsed.open);
        // 只有 --trust 时，内部生成一个随机密码
        let parsed = parse_host(&strings(&["--trust", &"ab".repeat(32)])).unwrap();
        assert!(matches!(parsed.auth, Auth::Password(ref p) if p.len() == 64));
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn argument_errors() {
        use crate::EXIT_USAGE;
        for bad in [
            vec!["host", "--bogus"],
            vec!["host", "--listen"],
            vec!["host", "--listen", "nope"],
            vec!["host", "--data-dir"],
            vec!["host", "--recv-dir"],
            vec!["host", "--fps", "0"],
            vec!["host", "--fps", "999"],
            vec!["connect"],
            vec!["connect", "nope"],
            vec!["connect", "127.0.0.1:1", "--pin"],
            vec!["connect", "127.0.0.1:1", "--pin", "short"],
            vec!["connect", "127.0.0.1:1", "--data-dir"],
            vec!["connect", "127.0.0.1:1", "--seconds", "x"],
            vec!["connect", "127.0.0.1:1", "--send"],
            vec!["connect", "127.0.0.1:1", "--snapshot"],
            vec!["connect", "127.0.0.1:1", "--bogus"],
            vec!["bench-transport", "--packets", "0"],
            vec!["bench-transport", "--packets", "x"],
            vec!["bench-transport", "extra"],
        ] {
            assert_eq!(run(&bad).0, EXIT_USAGE, "{bad:?}");
        }
    }

    #[test]
    fn runtime_failures_are_reported() {
        let dir = temp_dir("fail");
        let d = dir.to_str().unwrap();
        // 连不上
        let (code, text) = run(&["connect", "127.0.0.1:9", "--data-dir", d]);
        assert_eq!(code, EXIT_FAILED);
        assert!(text.contains("错误"));
        // 要上传的路径不存在
        let (code, text) = run(&[
            "connect",
            "127.0.0.1:9",
            "--data-dir",
            d,
            "--send",
            "/nonexistent/x.bin",
        ]);
        assert_eq!(code, EXIT_FAILED);
        assert!(text.contains("错误"), "{text}");
        // 没有文件名的路径
        assert!(plan_upload(Path::new("/")).is_err());
        // 监听端口被占
        let taken = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        let addr = taken.local_addr().unwrap().to_string();
        let (code, text) = run(&["host", "--listen", &addr, "--data-dir", d, "--open"]);
        assert_eq!(code, EXIT_FAILED);
        assert!(text.contains("绑定"), "{text}");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn relative_upload_path_uses_current_directory_as_root() {
        let name = format!("peerdesk-rel-{}.tmp", std::process::id());
        std::fs::write(&name, b"x").unwrap();
        let upload = plan_upload(Path::new(&name)).unwrap();
        std::fs::remove_file(&name).unwrap();
        assert_eq!(upload.root, PathBuf::from("."));
        assert_eq!(upload.files.len(), 1);
    }

    #[test]
    fn bench_prints_both_transports() {
        let (code, text) = run(&["bench-transport", "--packets", "120"]);
        assert_eq!(code, EXIT_OK, "{text}");
        assert!(text.contains("裸 UDP"));
        assert!(text.contains("QUIC 数据报"));
        assert!(text.contains("中位数差值"));
    }

    #[test]
    fn data_dir_has_a_default() {
        assert!(!default_data_dir().as_os_str().is_empty());
        let count = Arc::new(AtomicU64::new(0));
        CountInput(Arc::clone(&count)).inject(&InputEvent::ReleaseAll);
        assert_eq!(count.load(Ordering::Relaxed), 1);
    }
}
