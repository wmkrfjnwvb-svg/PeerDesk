//! PeerDesk 客户端的命令行逻辑。
//!
//! 现阶段提供探测、仿真和配置检查，以及一个测试用的图形界面（不带参数运行时打开）；
//! 正式的产品界面在 M1 用 Tauri 接入，复用同一个核心库。

use std::io::Write;

pub mod autostart;
pub mod crashreport;
pub mod diag_record;
pub mod guard;
pub mod gui;
pub mod instance;
pub mod keychain;
pub mod libcheck;
pub mod logfile;
pub mod process;

/// 默认的数据目录（身份、设置、日志都在这里）。
#[must_use]
pub fn data_dir() -> std::path::PathBuf {
    remote::default_data_dir()
}
mod hwcheck;
mod local;
mod remote;
mod screen_check;
mod source;
pub mod unattended;
pub mod vault;

use peerdesk_core::config::ClientConfig;
use peerdesk_core::crypto::{Policy, Suite};
use peerdesk_core::probe::CpuFeatures;
use peerdesk_core::region::rank;
use peerdesk_core::scenario::{self, Params};
use peerdesk_core::sim::Link;
use peerdesk_core::Error;

/// 成功。
pub const EXIT_OK: u8 = 0;
/// 运行失败。
pub const EXIT_FAILED: u8 = 1;
/// 用法错误。
pub const EXIT_USAGE: u8 = 2;
/// 重试也没用的错误（缺系统库、没有图形环境等，要用户先处理）：进程守护看到这个退出码就不再重启。
/// 取 BSD `sysexits.h` 的 `EX_CONFIG`；systemd 单元里的 `RestartPreventExitStatus` 要和它一致。
pub const EXIT_NO_RETRY: u8 = 78;

const USAGE: &str = "\
用法：peerdesk <命令> [参数]
      peerdesk                不带参数：打开图形界面（测试控制台）

命令：
  version                 显示版本
  probe                   探测本机硬件能力
  check-config <文件>     检查客户端配置，并显示服务器的选择顺序
  sim [选项]              在仿真网络上跑一次完整会话
  host [选项]             作为被控端等待连接
  connect <地址> [选项]   作为控制端直接连接被控端
  connect --server <地址> --device <设备码> [选项]
  netcheck [--server <地址> [--server-pin <指纹>]]
                          网络体检：NAT 类型、本机到路由器和服务器的延迟、IPv6
                          通过服务器找到被控端：先试直连（打洞），不通再走中继
  bench-transport         对比裸 UDP 和 QUIC 数据报在本机回环上的延迟
  screen-check [选项]     采集一帧本机屏幕，报告尺寸和耗时
  hw-check                编码器体检：软件和硬件编码的耗时、关键帧，结果存到日志目录
  service install|uninstall|status|password <密码>
                          Windows 无人值守：注册系统服务，锁屏和登录界面也能远程（需管理员）
  guard                   守护模式：运行图形界面，被杀或崩溃（退出码非 0）就重启，用户从托盘退出（退出码 0）则不重启
  autostart on|off        开启/关闭开机自启和崩溃自动恢复
  window show|close|quit  对正在运行的界面：显示窗口、关窗口（有托盘时隐藏）、退出（安装程序、更新脚本用）
  help                    显示本帮助

host 的选项：
  --listen <地址:端口>    监听地址（默认 0.0.0.0:4433）
  --server <地址:端口>    在这台信令服务器上登记，让控制端用指纹找到本机
  --server-pin <指纹>     信令服务器的指纹；给了就必须一致
  --data-dir <目录>       保存设备身份的目录
  --recv-dir <目录>       收到的文件放在哪里（默认 数据目录/received）
  --fps <N>               帧率（默认 60）
  --source <来源>         screen：采集本机屏幕并接受键鼠控制；demo：合成的演示桌面，不碰本机键鼠。
                          默认先试 screen，打不开时改用 demo 并给出警告
  --display <N>           采集第几个显示器（默认 0，主显示器）
  --once                  服务完一次会话后退出
  --trust <指纹>          这台设备不需要密码就能控制（可重复）
  --open                  不要密码，任何人都能连（只应在受信任的内网使用）
  --no-input              只允许观看，不接受键鼠输入
  --no-files              不接受文件
  密码通过环境变量 PEERDESK_PASSWORD 设置（至少 8 个字符）。
  密码、--trust、--open 至少要有一个。

connect 的选项：
  --pin <指纹>            被控端的指纹；给了就必须一致，否则拒绝连接（直连时用）
  --server <地址:端口>    信令服务器
  --server-pin <指纹>     信令服务器的指纹；给了就必须一致
  --device <设备码|指纹>  要连接的被控端：服务器分配的 9 位设备码，或者它的指纹
  --account <会员令牌>    走中继时记在这个会员账户上，优先走付费集群（VIP 通道）
  --relay-only            只走中继
  --direct-only           只走直连，不用中继
  --data-dir <目录>       保存设备身份的目录
  --seconds <N>           会话时长（默认 5 秒）
  --send <路径>           上传一个文件或目录
  --snapshot <文件.bmp>   会话结束时把看到的画面存成图片
  --type-line <文字>      看到第一帧后把指针移到画面中央，输入这行字并回车（测试输入链路用）
  被控端需要密码时，通过环境变量 PEERDESK_PASSWORD 提供。

加密套件由配置决定；host 和 connect 目前使用推荐预设。
被控端默认采集本机屏幕（Windows、macOS、Linux X11；Wayland 暂不支持），也可以用 --source demo
提供一个合成的演示桌面（打字、拖窗口、滚动、视频），走的是同样的编码、传输和解码。

screen-check 的选项：
  --out <文件.bmp>        把采集到的画面存成图片
  --display <N>           采集第几个显示器（默认 0）

sim 的选项：
  --seed <N>              随机种子（默认 1），同一种子结果完全相同
  --link <预设>           lan | wifi | cellular | intercontinental（默认 lan）
  --loss <千分比>         覆盖预设的随机丢包率
  --delay-ms <毫秒>       覆盖预设的单向延迟
  --rate-kbps <N>         覆盖预设的带宽上限
  --frames <N>            发送帧数（默认 120）
  --frame-bytes <N>       每帧字节数（默认 6000）
  --fps <N>               帧率（默认 60）
  --suite <名字>          两端都只用这个加密套件：
                          none | aes-128-gcm | aes-256-gcm | chacha20-poly1305
  --host-suite <名字>     只改被控端的加密套件（用来演示协商失败）
  --corrupt <千分比>      数据报在途中被翻转一个比特的概率
  --trace                 输出完整的业务追踪
";

pub(crate) fn usage_error(out: &mut dyn Write, why: &str) -> u8 {
    let _ = writeln!(out, "错误：{why}\n\n{USAGE}");
    EXIT_USAGE
}

pub(crate) fn parse_number<T: std::str::FromStr>(
    flag: &str,
    value: Option<&String>,
) -> Result<T, String> {
    let value = value.ok_or_else(|| format!("{flag} 需要一个值"))?;
    value
        .parse()
        .map_err(|_| format!("{flag} 的值不是合法数字：{value}"))
}

fn parse_sim(args: &[String]) -> Result<(Params, bool), String> {
    let mut params = Params::default();
    let mut trace = false;
    let mut it = args.iter();
    while let Some(flag) = it.next() {
        match flag.as_str() {
            "--seed" => params.seed = parse_number(flag, it.next())?,
            "--link" => {
                let name = it.next().ok_or("--link 需要一个值")?;
                params.link = match name.as_str() {
                    "lan" => Link::lan(),
                    "wifi" => Link::wifi(),
                    "cellular" => Link::cellular(),
                    "intercontinental" => Link::intercontinental(),
                    other => return Err(format!("未知的链路预设：{other}")),
                };
            }
            "--loss" => {
                let loss: u16 = parse_number(flag, it.next())?;
                if loss > 1000 {
                    return Err("--loss 的范围是 0 到 1000".into());
                }
                params.link.loss_permille = loss;
            }
            "--delay-ms" => {
                let ms: u64 = parse_number(flag, it.next())?;
                params.link.delay_us = ms.saturating_mul(1000);
            }
            "--rate-kbps" => {
                let kbps: u64 = parse_number(flag, it.next())?;
                params.link.rate_bps = Some(kbps.saturating_mul(1000));
            }
            "--frames" => params.frames = parse_number(flag, it.next())?,
            "--frame-bytes" => params.frame_bytes = parse_number(flag, it.next())?,
            "--fps" => params.fps = parse_number(flag, it.next())?,
            "--suite" => {
                let name = it.next().ok_or("--suite 需要一个值")?;
                let suite = Suite::parse(name).map_err(|e| e.to_string())?;
                let policy = Policy::custom(&[suite]).map_err(|e| e.to_string())?;
                params.controller_policy = policy.clone();
                params.host_policy = policy;
            }
            "--host-suite" => {
                let name = it.next().ok_or("--host-suite 需要一个值")?;
                let suite = Suite::parse(name).map_err(|e| e.to_string())?;
                params.host_policy = Policy::custom(&[suite]).map_err(|e| e.to_string())?;
            }
            "--corrupt" => {
                let permille: u16 = parse_number(flag, it.next())?;
                if permille > 1000 {
                    return Err("--corrupt 的范围是 0 到 1000".into());
                }
                params.corrupt_permille = permille;
            }
            "--trace" => trace = true,
            other => return Err(format!("未知选项：{other}")),
        }
    }
    Ok((params, trace))
}

fn cmd_sim(args: &[String], out: &mut dyn Write) -> Result<u8, Error> {
    let (params, trace) = match parse_sim(args) {
        Ok(parsed) => parsed,
        Err(why) => return Ok(usage_error(out, &why)),
    };
    let report = scenario::run(&params)?;
    let suite = report.suite.map_or("-", Suite::name);
    let lines = [
        format!(
            "会话建立      {}",
            if report.established { "是" } else { "否" }
        ),
        format!("加密套件      {suite}"),
        format!(
            "握手耗时      {} us（Hello 发送 {} 次）",
            report.handshake_us, report.hello_attempts
        ),
        format!(
            "帧            发出 {}，送达 {}，内容损坏 {}",
            report.frames_sent, report.frames_delivered, report.frames_corrupted
        ),
        format!(
            "帧延迟        p50 {} us，p95 {} us",
            report.latency_p50_us, report.latency_p95_us
        ),
        format!(
            "数据包        发出 {}，丢失 {}，队列溢出 {}",
            report.net.sent, report.net.lost, report.net.queue_dropped
        ),
        format!("异常数据报    {}", report.bad_datagrams),
    ];
    for line in lines {
        let _ = writeln!(out, "{line}");
    }
    if let Some(reason) = report.rejected {
        let _ = writeln!(out, "被拒绝        {reason:?}");
    }
    if trace {
        let _ = write!(out, "\n{}", report.trace_dump);
    }
    Ok(if report.established {
        EXIT_OK
    } else {
        EXIT_FAILED
    })
}

fn cmd_check_config(args: &[String], out: &mut dyn Write) -> Result<u8, Error> {
    let [path] = args else {
        return Ok(usage_error(out, "check-config 需要且只需要一个文件路径"));
    };
    let text = std::fs::read_to_string(path)
        .map_err(|e| Error::Config(format!("读取 {path} 失败：{e}")))?;
    let cfg = ClientConfig::from_toml(&text)?;
    let policy = cfg.crypto.policy()?;
    let names: Vec<&str> = policy.suites().iter().map(|s| s.name()).collect();
    let _ = writeln!(out, "配置有效。区域：{}", cfg.region);
    let _ = writeln!(out, "加密套件（按偏好）：{}", names.join(", "));
    if policy.allows_plaintext() {
        let _ = writeln!(out, "警告：策略允许不加密，只应在受信任的内网使用。");
    }
    let _ = writeln!(out, "服务器选择顺序：");
    for candidate in rank(&cfg.region, &cfg.region_fallback, &cfg.candidates()) {
        let index = usize::try_from(candidate.id.0).unwrap_or(usize::MAX);
        let addr = cfg.servers.get(index).map_or("?", |s| s.addr.as_str());
        let lan = if candidate.lan { "，内网" } else { "" };
        let _ = writeln!(out, "  {addr}（{}{lan}）", candidate.region);
    }
    Ok(EXIT_OK)
}

/// 输出失败原因，返回退出码。
fn report(out: &mut dyn Write, result: Result<u8, String>) -> u8 {
    result.unwrap_or_else(|why| {
        let _ = writeln!(out, "错误：{why}");
        EXIT_FAILED
    })
}

/// 执行命令行，返回进程退出码。
pub fn run(args: &[String], out: &mut dyn Write) -> u8 {
    let Some((command, rest)) = args.split_first() else {
        return usage_error(out, "缺少命令");
    };
    let result = match command.as_str() {
        "version" | "--version" | "-V" => {
            let _ = writeln!(out, "peerdesk {}", peerdesk_core::VERSION);
            Ok(EXIT_OK)
        }
        "help" | "--help" | "-h" => {
            let _ = write!(out, "{USAGE}");
            Ok(EXIT_OK)
        }
        "probe" => {
            let cpu = CpuFeatures::detect();
            let _ = writeln!(out, "CPU 指令集    {}", cpu.describe());
            let _ = writeln!(
                out,
                "宽向量指令    {}",
                if cpu.has_wide_simd() { "有" } else { "无" }
            );
            let _ = writeln!(out, "GPU 与编码器  M1 接入平台层后提供");
            Ok(EXIT_OK)
        }
        "sim" => cmd_sim(rest, out),
        "host" => return remote::host(rest, out),
        "connect" => return remote::connect(rest, out),
        "bench-transport" => return remote::bench(rest, out),
        "netcheck" => return remote::netcheck(rest, out),
        "screen-check" => return screen_check::screen_check(rest, out),
        "check-config" => cmd_check_config(rest, out),
        "hw-check" => return hwcheck::hw_check(out, &data_dir().join("logs")),
        "service" => {
            let result = unattended::cmd_service(rest, out);
            return report(out, result);
        }
        "guard" => return guard::cmd_guard(out),
        "window" => return instance::cmd_window(rest, out, &data_dir()),
        "autostart" => return autostart::cmd_autostart(rest, out),
        "agent" => {
            let result = unattended::cmd_agent(rest, out);
            return report(out, result);
        }
        other => Ok(usage_error(out, &format!("未知命令：{other}"))),
    };
    match result {
        Ok(code) => code,
        Err(e) => {
            let _ = writeln!(out, "错误：{e}");
            EXIT_FAILED
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn exec(args: &[&str]) -> (u8, String) {
        let args: Vec<String> = args.iter().map(|s| (*s).to_owned()).collect();
        let mut out = Vec::new();
        let code = run(&args, &mut out);
        (code, String::from_utf8(out).unwrap())
    }

    #[test]
    fn version_help_and_probe() {
        for flag in ["version", "--version", "-V"] {
            let (code, text) = exec(&[flag]);
            assert_eq!(code, EXIT_OK);
            assert!(text.starts_with("peerdesk "));
        }
        for flag in ["help", "--help", "-h"] {
            let (code, text) = exec(&[flag]);
            assert_eq!(code, EXIT_OK);
            assert!(text.contains("check-config"));
        }
        let (code, text) = exec(&["probe"]);
        assert_eq!(code, EXIT_OK);
        assert!(text.contains("CPU 指令集"));
    }

    #[test]
    fn usage_errors() {
        assert_eq!(exec(&[]).0, EXIT_USAGE);
        let (code, text) = exec(&["frobnicate"]);
        assert_eq!(code, EXIT_USAGE);
        assert!(text.contains("未知命令：frobnicate"));
        for bad in [
            vec!["sim", "--bogus"],
            vec!["sim", "--seed"],
            vec!["sim", "--seed", "x"],
            vec!["sim", "--link"],
            vec!["sim", "--link", "carrier-pigeon"],
            vec!["sim", "--loss", "1001"],
            vec!["sim", "--suite"],
            vec!["sim", "--suite", "rot13"],
            vec!["sim", "--host-suite"],
            vec!["sim", "--host-suite", "rot13"],
            vec!["sim", "--corrupt", "1001"],
            vec!["check-config"],
            vec!["check-config", "a", "b"],
        ] {
            assert_eq!(exec(&bad).0, EXIT_USAGE, "{bad:?}");
        }
    }

    #[test]
    fn sim_default_run_succeeds_and_is_reproducible() {
        let (code, a) = exec(&["sim", "--frames", "30"]);
        assert_eq!(code, EXIT_OK);
        assert!(a.contains("发出 30，送达 30，内容损坏 0"));
        assert!(a.contains("aes-256-gcm"));
        assert_eq!(exec(&["sim", "--frames", "30"]).1, a);
    }

    #[test]
    fn sim_accepts_every_option() {
        for link in ["lan", "wifi", "cellular", "intercontinental"] {
            assert_eq!(
                exec(&["sim", "--link", link, "--frames", "5"]).0,
                EXIT_OK,
                "{link}"
            );
        }
        let (code, text) = exec(&[
            "sim",
            "--seed",
            "9",
            "--loss",
            "20",
            "--delay-ms",
            "15",
            "--rate-kbps",
            "20000",
            "--frames",
            "10",
            "--frame-bytes",
            "3000",
            "--fps",
            "30",
            "--suite",
            "none",
            "--trace",
        ]);
        assert_eq!(code, EXIT_OK);
        assert!(text.contains("加密套件      none"));
        assert!(text.contains("hello_sent"));
    }

    #[test]
    fn sim_reports_failure() {
        // 帧率为 0 是核心库的参数错误
        let (code, text) = exec(&["sim", "--fps", "0"]);
        assert_eq!(code, EXIT_FAILED);
        assert!(text.contains("fps"));
        // 两端没有共同套件：被拒绝
        let (code, text) = exec(&["sim", "--suite", "none", "--host-suite", "aes-256-gcm"]);
        assert_eq!(code, EXIT_FAILED);
        assert!(text.contains("被拒绝        NoCommonSuite"));
        // 加密时，途中损坏的数据报被认证拦下，不会交付损坏的帧
        let (code, text) = exec(&["sim", "--corrupt", "100", "--frames", "60"]);
        assert_eq!(code, EXIT_OK);
        assert!(text.contains("内容损坏 0"));
        assert!(!text.contains("异常数据报    0"));
        // 100% 丢包：握手不成功
        let (code, text) = exec(&["sim", "--loss", "1000"]);
        assert_eq!(code, EXIT_FAILED);
        assert!(text.contains("会话建立      否"));
        assert!(text.contains("加密套件      -"));
    }

    fn temp_file(name: &str, content: &str) -> String {
        let path = std::env::temp_dir().join(format!("peerdesk-{}-{name}", std::process::id()));
        std::fs::write(&path, content).unwrap();
        path.to_string_lossy().into_owned()
    }

    #[test]
    fn check_config_shows_server_order() {
        let path = temp_file(
            "ok.toml",
            "region = \"eu\"\n\
             [[servers]]\naddr = \"203.0.113.1:4433\"\nregion = \"us\"\n\
             [[servers]]\naddr = \"10.0.0.2:4433\"\nregion = \"dc\"\n\
             [crypto]\nsuites = [\"aes-256-gcm\", \"none\"]\n",
        );
        let (code, text) = exec(&["check-config", &path]);
        std::fs::remove_file(&path).unwrap();
        assert_eq!(code, EXIT_OK, "{text}");
        assert!(text.contains("aes-256-gcm, none"));
        assert!(text.contains("警告"));
        let lan = text.find("10.0.0.2").unwrap();
        let wan = text.find("203.0.113.1").unwrap();
        assert!(lan < wan, "内网服务器排在前面");
        assert!(text.contains("dc，内网"));
    }

    #[test]
    fn check_config_reports_errors() {
        let (code, text) = exec(&["check-config", "/nonexistent/peerdesk.toml"]);
        assert_eq!(code, EXIT_FAILED);
        assert!(text.contains("读取"));
        let path = temp_file("bad.toml", "region = \"\"");
        let (code, text) = exec(&["check-config", &path]);
        std::fs::remove_file(&path).unwrap();
        assert_eq!(code, EXIT_FAILED);
        assert!(text.contains("配置错误"));
    }
}
