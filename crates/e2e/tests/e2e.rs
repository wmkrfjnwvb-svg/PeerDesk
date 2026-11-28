//! 端到端场景。每个场景把截图、JSON 和表格写进 `target/e2e/`，CI 把这个目录作为构建产物上传，
//! 并把表格贴到运行摘要里。
//!
//! 用 release 跑：`cargo test --release -p peerdesk-e2e -- --nocapture`。

#![allow(clippy::unwrap_used, clippy::print_stdout, clippy::indexing_slicing)]

use std::path::PathBuf;

use peerdesk_core::adaptive::Profile;
use peerdesk_core::sim::{Busy, Doze, Link, Spikes};
use peerdesk_e2e::report::{markdown, save, TABLE_HEADER};
use peerdesk_e2e::{run, Action, CodecKind, EstimatorKind, Outcome, Phase, Scenario};

fn out_dir() -> PathBuf {
    std::env::var_os("PEERDESK_E2E_OUT").map_or_else(
        || PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/e2e"),
        PathBuf::from,
    )
}

fn phase(action: Action, frames: u32) -> Phase {
    Phase { action, frames }
}

/// 一段典型的办公操作，每段操作后停一秒。
fn office() -> Vec<Phase> {
    vec![
        phase(Action::Idle, 30),
        phase(Action::Typing, 90),
        phase(Action::Idle, 30),
        phase(Action::Scrolling, 60),
        phase(Action::Idle, 30),
        phase(Action::Dragging, 60),
        phase(Action::Idle, 30),
        phase(Action::Switching, 15),
        phase(Action::Idle, 30),
        phase(Action::Video, 90),
        phase(Action::Idle, 30),
    ]
}

/// 体验门槛：写进 `target/e2e/gates.md`（CI 贴到运行摘要里），不达标时测试失败。
/// 仿真是确定性的，同一份代码每次结果相同，所以可以当硬门槛。
fn gate(o: &Outcome, what: &str, ok: bool, detail: &str) {
    use std::io::Write as _;
    // 门槛只卡产品默认的估计器；另一个估计器跑同样的场景，只报告，用来对比。
    let product = EstimatorKind::PRODUCT
        .build(Profile::STABLE, 1, 1, 1)
        .name();
    let hard = o.estimator == product;
    let verdict = match (ok, hard) {
        (true, _) => "达标",
        (false, true) => "**未达标**",
        (false, false) => "未达标（对照，不卡）",
    };
    let line = format!(
        "| {} | {}/{} | {what} | {verdict} | {detail} |\n",
        o.name,
        o.codec.name(),
        o.estimator,
    );
    print!("{line}");
    std::fs::create_dir_all(out_dir()).unwrap();
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(out_dir().join("gates.md"))
        .unwrap();
    f.write_all(line.as_bytes()).unwrap();
    assert!(
        ok || !hard,
        "{} {}: {what} 未达标（{detail}）",
        o.name,
        o.codec.name()
    );
}

/// 所有场景共同的门槛。
fn gates(o: &Outcome) {
    // 硬门槛：任何时候都不能把解不了的帧交给解码器（花屏）。
    assert_eq!(o.bad, 0, "{} {}: 有帧解码失败", o.name, o.codec.name());
    let first = o.first_picture_ms.unwrap_or(f64::INFINITY);
    gate(
        o,
        "2 秒内看到第一幅画面",
        first < 2000.0,
        &format!("第一幅画面 {first:.0} ms"),
    );
    gate(
        o,
        "之后没有超过 1 秒的冻结",
        o.max_wait_ms < 1000.0,
        &format!("最长等待 {:.0} ms", o.max_wait_ms),
    );
    let idle: Vec<_> = o
        .phases
        .iter()
        .filter(|p| p.action == Action::Idle)
        .skip(1)
        .collect();
    let slow = idle
        .iter()
        .filter(|p| p.sharp_ms.is_none_or(|t| t > 500.0))
        .count();
    gate(
        o,
        "停下后 0.5 秒内清晰",
        slow == 0,
        &format!("{slow}/{} 次没做到", idle.len()),
    );
    // 已知问题（只报告，不卡）：接收端按“这一帧第一片到了 10 ms”请求重传，大帧后半段还在路上就被要重传。
    // 2026-11-26 用户日志：两分钟里重传 2628 片、实际丢包 97 个。改成按空洞判断后窄带上尾部丢包发现得太晚
    // （微软轨迹卡顿 75 → 101），先没合，见 docs/research/L-framework.md 第 6.5 节
    println!(
        "{} {}: 重传 {} 片，实际丢包 {} 个",
        o.name,
        o.codec.name(),
        o.retransmitted,
        o.net.lost.saturating_add(o.net.queue_dropped)
    );
    // 码率控制各状态的时间占比，和量尺的核对项（真实队列高时不该被判成应用受限），只报告
    println!("{} {}: {}", o.name, o.codec.name(), o.states.line());
    println!(
        "{} {}: {}",
        o.name,
        o.codec.name(),
        o.probes.line(o.sent_bytes)
    );
}

fn finish(o: &Outcome) {
    save(&out_dir(), o).unwrap();
    // 打开 PEERDESK_EVENTS 时把发送端的决定记下来（门槛没过时先看它）
    if !o.trace.is_empty() {
        let dir = out_dir().join(format!("{}-{}-{}", o.name, o.codec.name(), o.estimator));
        std::fs::write(dir.join("trace.txt"), o.trace.join("\n")).unwrap();
    }
    gates(o);
    println!("\n{TABLE_HEADER}{}", markdown(o));
    println!(
        "keyframes {}, recoveries {}, skipped {}, bad {}, net lost {}, queue dropped {}",
        o.keyframes, o.recoveries, o.skipped, o.bad, o.net.lost, o.net.queue_dropped
    );
}

/// 用产品默认的估计器（卡门槛）和另一个估计器（对照，只报告）各跑一遍。
fn compare(s: &Scenario) {
    for estimator in [
        EstimatorKind::Legacy,
        EstimatorKind::ClassicV1,
        EstimatorKind::GccV1,
    ] {
        let o = run(&Scenario {
            estimator,
            ..s.clone()
        })
        .unwrap();
        finish(&o);
    }
}

/// 局域网上的办公操作：两种编码器对比。
#[test]
fn office_on_lan() {
    compare(&Scenario::new("office-lan", office()));
    let tile = run(&Scenario {
        codec: CodecKind::Tile,
        ..Scenario::new("office-lan", office())
    })
    .unwrap();
    finish(&tile);
}

/// 带宽阶梯：20 → 3 → 20 Mbps（ToDesk 切清晰度时卡几秒的场景）。
#[test]
fn bandwidth_steps() {
    let at = |mbps: u64| Link {
        rate_bps: Some(mbps * 1_000_000),
        ..Link::lan()
    };
    let s = Scenario {
        network: vec![(0, at(20)), (4_000_000, at(3)), (8_000_000, at(20))],
        ..Scenario::new(
            "bandwidth-steps",
            vec![
                phase(Action::Scrolling, 120),
                phase(Action::Scrolling, 120),
                phase(Action::Scrolling, 120),
                phase(Action::Idle, 30),
            ],
        )
    };
    compare(&s);
}

/// 突然掉 90% 的带宽，持续 2 秒，再恢复。
#[test]
fn sudden_drop() {
    let full = Link {
        rate_bps: Some(20_000_000),
        ..Link::wifi()
    };
    let low = Link {
        rate_bps: Some(2_000_000),
        ..full
    };
    let s = Scenario {
        network: vec![(0, full), (3_000_000, low), (5_000_000, full)],
        profile: Profile::WIRELESS,
        ..Scenario::new(
            "sudden-drop",
            vec![phase(Action::Dragging, 240), phase(Action::Idle, 45)],
        )
    };
    compare(&s);
}

/// 蜂窝网络上的办公操作。
#[test]
fn office_on_cellular() {
    compare(&Scenario {
        network: vec![(0, Link::cellular())],
        profile: Profile::CELLULAR,
        start_bps: 2_000_000,
        ..Scenario::new("office-cellular", office())
    });
}

/// 起步码率很低、链路很宽：估计器多快把码率提上去（画面多快变清楚）。
#[test]
fn fast_ramp_on_a_fat_link() {
    compare(&Scenario {
        network: vec![(
            0,
            Link {
                rate_bps: Some(50_000_000),
                ..Link::lan()
            },
        )],
        start_bps: 500_000,
        ..Scenario::new(
            "fast-ramp",
            vec![phase(Action::Scrolling, 90), phase(Action::Idle, 30)],
        )
    });
}

/// 用户实测（2026-11-26，Mac 2880×1800 被控、Windows 控制）：会话开头画面很糊，要过好几秒才清楚。
/// 宽链路（30 Mbps、往返 30 ms、Wi-Fi 抖动）、Retina 大屏、产品默认的起步码率，开头停着的画面要尽快变清晰。
#[test]
fn start_is_sharp_quickly_on_a_fat_link_with_a_big_screen() {
    let o = run(&Scenario {
        width: 2880,
        height: 1800,
        network: vec![(
            0,
            Link {
                rate_bps: Some(30_000_000),
                delay_us: 15_000,
                // 用户的 Wi-Fi（2026-11-26 日志）：连续发包时排队读数 0–2 ms，画面静止时零星小包几乎每次都是
                // 12–25 ms（省电：空闲后网卡睡着），偶尔一阵尖峰；延迟趋势正常、没有丢包；接收端偶尔忙一下、一帧一起读走
                jitter_us: 1_000,
                fifo_jitter: true,
                doze: Some(Doze {
                    idle_us: 50_000,
                    wake_us: 25_000,
                }),
                spikes: Some(Spikes {
                    every_us: 2_000_000,
                    length_us: 40_000,
                    max_us: 25_000,
                }),
                busy: Some(Busy {
                    every_us: 500_000,
                    length_us: 20_000,
                }),
                ..Link::lan()
            },
        )],
        ..Scenario::new(
            "big-screen-start",
            vec![
                phase(Action::Idle, 60),
                phase(Action::Typing, 90),
                phase(Action::Idle, 30),
                phase(Action::Typing, 90),
                phase(Action::Idle, 30),
            ],
        )
    })
    .unwrap();
    finish(&o);
    if std::env::var_os("PEERDESK_EVENTS").is_some() {
        for line in o.trace.iter().filter(|l| !l.contains("大帧")).take(40) {
            println!("{line}");
        }
    }
    // 码率不能被 Wi-Fi 的零星延迟打到底（改前：30 Mbps → 100 kbps，之后一直贴着 100 kbps）：2 秒之后码率的中位数
    // 不低于链路的四分之一
    let mut later: Vec<u64> = o
        .features
        .iter()
        .filter(|f| f.t_ms >= 2_000)
        .map(|f| f.rate_bps)
        .collect();
    later.sort_unstable();
    let median = later.get(later.len() / 2).copied().unwrap_or(0);
    gate(
        &o,
        "码率不被 Wi-Fi 延迟打到底",
        median >= 30_000_000 / 4,
        &format!(
            "2 秒之后码率中位数 {} kbps（链路 30000 kbps）",
            median / 1000
        ),
    );
    // 开头的静止画面要尽快清晰（这个场景以前一直没清晰：码率被 Wi-Fi 抖动打到底、1 Mbps 起步爬得慢）
    let start = o.phases.first().and_then(|p| p.sharp_ms);
    gate(
        &o,
        "开头 2 秒内清晰",
        start.is_some_and(|t| t <= 2000.0),
        &format!("开头变清晰 {start:?} ms"),
    );
}

/// 轻操作（打字、小改动，发送端用不满码率）时 Wi-Fi 的延迟尖峰不能把码率打到底。
///
/// 2026-11-26 用户日志（Mac 2880×1800 被控、Wi-Fi 约 30 Mbps、RTT 30 ms）：连上几分钟后反复“画面不动”。
/// 轻操作时每秒只发 40–240 kbps，一次 13–27 ms 的排队读数（延迟趋势是平的）被当成拥塞，按送达速率的 85% 降，
/// 码率从 17 Mbps 掉到底线 100 kbps；100 kbps 下编码器出 0 KB 的帧，帧照常到、内容不更新，6 秒后才爬回来。
/// 那次会话 168 秒里降码率 59 次，39 次发生在发送端没用满码率时，11 次降到 300 kbps 以下。
fn wifi_light_activity(name: &str) -> Scenario {
    Scenario {
        width: 2880,
        height: 1800,
        network: vec![(
            0,
            Link {
                rate_bps: Some(30_000_000),
                delay_us: 15_000,
                jitter_us: 1_000,
                fifo_jitter: true,
                doze: Some(Doze {
                    idle_us: 50_000,
                    wake_us: 25_000,
                }),
                // 按那次会话的排队读数标定：p50 3 ms、p90 11、p99 57 ms，每秒都有机会碰上一次尖峰
                spikes: Some(Spikes {
                    every_us: 1_000_000,
                    length_us: 30_000,
                    max_us: 60_000,
                }),
                busy: Some(Busy {
                    every_us: 500_000,
                    length_us: 20_000,
                }),
                ..Link::lan()
            },
        )],
        ..Scenario::new(
            name,
            // 轻操作（指针每帧动一点）20 秒，中间切一次窗口、打字、滚动
            vec![
                phase(Action::Idle, 60),
                phase(Action::Pointer, 600),
                phase(Action::Switching, 15),
                phase(Action::Idle, 30),
                phase(Action::Pointer, 300),
                phase(Action::Typing, 90),
                phase(Action::Scrolling, 60),
                phase(Action::Idle, 30),
            ],
        )
    }
}

#[test]
// 2880×1800 一分多钟，CI 额度有限，不进 CI（调试构建插桩后更要一个小时以上）。改了传输、码率控制或编码之后，
// 合并前在本地手动跑（和 microsoft_traces 的 144 次一样）：
// cargo test -p peerdesk-e2e --release --test e2e light_activity_on_wifi_keeps_the_rate -- --ignored
#[ignore = "大屏场景耗时长，不进 CI；改传输后本地手动跑（见上面的注释）"]
fn light_activity_on_wifi_keeps_the_rate() {
    let o = run(&wifi_light_activity("wifi-light")).unwrap();
    finish(&o);
    if std::env::var_os("PEERDESK_EVENTS").is_some() {
        for line in o
            .trace
            .iter()
            .filter(|l| l.contains("降码率") || l.contains("升码率"))
            .take(60)
        {
            println!("{line}");
        }
    }
    // 2 秒之后（起步结束）：尖峰只是延迟，链路容量一直是 30 Mbps。中位数看不见塌陷（改前 22% 的时间
    // 低于 10 Mbps 中位数照样过），看最低值和 5% 分位
    let mut after: Vec<u64> = o
        .features
        .iter()
        .filter(|f| f.t_ms >= 2_000)
        .map(|f| f.rate_bps)
        .collect();
    after.sort_unstable();
    let min = after.first().copied().unwrap_or(0);
    let p5 = after.get(after.len() / 20).copied().unwrap_or(0);
    gate(
        &o,
        "轻操作时码率不被 Wi-Fi 尖峰打到底",
        min >= 3_000_000 && p5 >= 15_000_000,
        &format!(
            "2 秒之后最低 {} kbps、5% 分位 {} kbps（链路 30000 kbps，要求不低于 3000 和 15000）",
            min / 1000,
            p5 / 1000
        ),
    );
}

/// 真实用户日志的操作节奏（回归，`docs/research/P-fps-bitrate.md` M1、W3）。
///
/// 2026-11-27 三份日志（Mac 公网 Wi-Fi、两台 Windows 局域网 Wi-Fi）：大部分时间静止或只有零星小帧，突然来一个大帧
/// （切窗口、滚动），又停下。链路没有丢包、容量远高于要发的量，排队读数平时 0–5 ms、每秒有一两份 12–28 ms。
/// 修之前：大帧一出来状态切回“跟踪”，碰上一份 12 ms 以上的读数就按“送达速率 × 0.85”降，而那时的送达速率只是
/// 静止时要发的量——46 分钟里降到 1 Mbps 以下 317 次，降帧率 82 次里 67 次跟在后面；局域网上画面因此变糊。
/// 这里按那个节奏重放：静止 → 切窗口 → 指针 → 打字 → 静止 → 滚动 → 指针，重复 4 遍，局域网 Wi-Fi（往返 16 ms）。
fn real_log_rhythm(name: &str) -> Scenario {
    let mut phases = Vec::new();
    for _ in 0..4 {
        phases.extend([
            phase(Action::Idle, 90),
            // 切过去马上接着动鼠标、打字：大帧之后紧跟着零星小帧（日志里降码率就发生在这种时候）
            phase(Action::Switching, 6),
            phase(Action::Pointer, 150),
            phase(Action::Typing, 90),
            phase(Action::Idle, 60),
            phase(Action::Scrolling, 60),
            phase(Action::Pointer, 120),
        ]);
    }
    Scenario {
        network: vec![(
            0,
            Link {
                rate_bps: Some(50_000_000),
                delay_us: 8_000,
                jitter_us: 1_000,
                fifo_jitter: true,
                doze: Some(Doze {
                    idle_us: 50_000,
                    wake_us: 25_000,
                }),
                // 按日志的排队读数标定：中位 3 ms，95% 分位 13–20 ms，每秒有机会碰上一次十几到二十几毫秒的尖峰
                spikes: Some(Spikes {
                    every_us: 700_000,
                    length_us: 30_000,
                    max_us: 28_000,
                }),
                busy: Some(Busy {
                    every_us: 500_000,
                    length_us: 20_000,
                }),
                ..Link::lan()
            },
        )],
        trace: true,
        // 和产品一样 60 帧：每帧预算减半，滚动、切窗口的帧顶着预算，状态才会像日志里那样频繁回到“跟踪”
        fps: 60,
        // 日志里是 2880×1800、上限 30 Mbps；这里画面小 5.6 倍（省仿真时间），上限按像素数折算，
        // 帧和预算的比例才和日志里差不多（预算 24–50 KB，大帧 100 KB 以上）
        max_bps: 6_000_000,
        ..Scenario::new(name, phases)
    }
}

/// 一行“降码率 A → B kbps”里的两个数。
fn cut_of(line: &str) -> Option<(u64, u64)> {
    let rest = line.split("降码率 ").nth(1)?;
    let mut parts = rest.split(' ');
    let from = parts.next()?.parse().ok()?;
    let to = parts.nth(1)?.parse().ok()?;
    Some((from, to))
}

#[test]
fn real_log_rhythm_keeps_the_rate_and_the_frame_rate() {
    let o = run(&real_log_rhythm("real-log-rhythm")).unwrap();
    finish(&o);
    gates(&o);
    // 链路没有拥塞：不该有“趋势正常”却把码率砍掉一半以上的降码率（修之前这个场景里有十几次）
    let deep: Vec<&String> = o
        .trace
        .iter()
        .filter(|l| l.contains("趋势正常"))
        .filter(|l| cut_of(l).is_some_and(|(from, to)| to.saturating_mul(2) < from))
        .collect();
    gate(
        &o,
        "没有拥塞证据时不把码率砍掉一半以上",
        deep.len() <= 1,
        &format!(
            "{} 次：{:?}",
            deep.len(),
            deep.iter().take(3).collect::<Vec<_>>()
        ),
    );
    let mut after: Vec<u64> = o
        .features
        .iter()
        .filter(|f| f.t_ms >= 2_000)
        .map(|f| f.rate_bps)
        .collect();
    after.sort_unstable();
    let min = after.first().copied().unwrap_or(0);
    let p5 = after.get(after.len() / 20).copied().unwrap_or(0);
    gate(
        &o,
        "码率不被 Wi-Fi 的抖动打下去",
        min >= 1_000_000 && p5 >= 3_000_000,
        &format!(
            "2 秒之后最低 {} kbps、5% 分位 {} kbps（链路 50000 kbps、上限 6000，要求不低于 1000 和 3000）",
            min / 1000,
            p5 / 1000
        ),
    );
    // 帧率档位不来回跳：链路宽，内容装得进预算
    let steps = o
        .trace
        .iter()
        .filter(|l| l.contains("内容装不进预算"))
        .count();
    gate(
        &o,
        "帧率不因为码率误降而降档",
        steps <= 1,
        &format!("降档 {steps} 次"),
    );
}

// ---- 运行中探测的四个场景（状态机第 2 步）----

/// 一条不乱序、往返 30 ms 的链路。
fn steady_link(rate_bps: u64) -> Link {
    Link {
        rate_bps: Some(rate_bps),
        delay_us: 15_000,
        jitter_us: 1_000,
        fifo_jitter: true,
        ..Link::lan()
    }
}

/// `t_ms` 时刻（之前最近一份反馈）的码率。
fn rate_at(o: &Outcome, t_ms: u64) -> u64 {
    o.features
        .iter()
        .take_while(|f| f.t_ms <= t_ms)
        .last()
        .map_or(0, |f| f.rate_bps)
}

/// 强行误降一次：轻操作时链路延迟突然多 60 ms、持续 300 ms（容量没变）。应用受限时这算常驻队列，码率会降；
/// 延迟恢复之后要核实（探测），1 秒内回到降前的四分之三以上，日志里有“证实”。
#[test]
fn a_false_cut_while_app_limited_is_verified_and_undone() {
    let base = steady_link(30_000_000);
    let o = run(&Scenario {
        network: vec![
            (0, base),
            (
                6_000_000,
                Link {
                    delay_us: 75_000,
                    ..base
                },
            ),
            (6_300_000, base),
        ],
        trace: true,
        ..Scenario::new(
            "false-cut",
            vec![phase(Action::Idle, 30), phase(Action::Pointer, 420)],
        )
    })
    .unwrap();
    finish(&o);
    let before = rate_at(&o, 5_990);
    let (low_at, low) = o
        .features
        .iter()
        .filter(|f| (6_000..8_000).contains(&f.t_ms))
        .map(|f| (f.t_ms, f.rate_bps))
        .min_by_key(|(_, r)| *r)
        .unwrap();
    let after = rate_at(&o, low_at + 1_000);
    let verdicts = o.probes.run_verdicts;
    gate(
        &o,
        "应用受限时误降之后 1 秒内核实并恢复",
        low * 100 < before * 66 && after * 4 >= before * 3 && verdicts[0] >= 1,
        &format!(
            "降前 {} kbps，最低 {} kbps（{low_at} ms），1 秒后 {} kbps；核实 {} 轮，证实 {}、否定 {}、超时 {}、中止 {}",
            before / 1000,
            low / 1000,
            after / 1000,
            o.probes.verify_rounds,
            verdicts[0],
            verdicts[1],
            verdicts[2],
            verdicts[3]
        ),
    );
    assert!(
        o.trace.iter().any(|l| l.contains("核实探测结论 证实")),
        "日志里要有核实的结论"
    );
}

/// 轻操作中链路真的从 30 降到 3 Mbps。轻操作每秒只发一两百 kbps，3 Mbps 也装得下，所以变窄的那一刻没有任何
/// 证据；证据在下一次有真实流量时（这里是切窗口）才出现。要求：那之后 300 ms 内码率降到链路以下，之后不反弹
/// （没有一轮探测被“证实”）。
#[test]
fn a_real_drop_during_light_activity_is_followed_without_bouncing_back() {
    let o = run(&Scenario {
        network: vec![
            (0, steady_link(30_000_000)),
            (6_000_000, steady_link(3_000_000)),
        ],
        // 固定 720p：调试构建默认的小画面切窗口只有十几 KB，3 Mbps 也装得下，就没有“真实流量”这个证据了
        width: 1280,
        height: 720,
        trace: true,
        ..Scenario::new(
            "real-drop",
            vec![
                phase(Action::Idle, 30),
                phase(Action::Pointer, 240),
                phase(Action::Switching, 15),
                phase(Action::Pointer, 300),
            ],
        )
    })
    .unwrap();
    finish(&o);
    let switch_ms = 9_000;
    let soon = rate_at(&o, switch_ms + 300);
    let later = o
        .features
        .iter()
        .filter(|f| f.t_ms >= switch_ms + 300)
        .map(|f| f.rate_bps)
        .max()
        .unwrap_or(0);
    gate(
        &o,
        "链路真变窄：有真实流量后 300 ms 内降下来，不反弹",
        soon <= 3_000_000 && later <= 3_300_000 && o.probes.run_verdicts[0] == 0,
        &format!(
            "切窗口后 300 ms 码率 {} kbps，之后最高 {} kbps（链路 3000 kbps）；探测证实 {} 轮",
            soon / 1000,
            later / 1000,
            o.probes.run_verdicts[0]
        ),
    );
}

/// 1 Mbps 的链路上指针轻操作：运行中探测一个字节都不发（窄带保护）。
#[test]
fn no_run_probes_on_a_narrow_link() {
    let o = run(&Scenario {
        network: vec![(0, steady_link(1_000_000))],
        ..Scenario::new(
            "narrow-light",
            vec![phase(Action::Idle, 30), phase(Action::Pointer, 600)],
        )
    })
    .unwrap();
    finish(&o);
    gate(
        &o,
        "1 Mbps 上轻操作不发运行中探测",
        o.probes.run_bytes == 0 && o.probes.run_sent == 0,
        &format!(
            "运行中探测 {} 轮、{} 字节（全部探测 {} 字节）",
            o.probes.run_sent,
            o.probes.run_bytes,
            o.probes.bytes()
        ),
    );
}

/// 持续小动静 60 秒、码率起步很低（没做起步探测）、链路 30 Mbps：应用受限时码率自己不升，靠周期探测探上去；
/// 每一轮都有结论；探到已知链路能力的七成以上就不再探。对端是老版本时不探，行为和以前一样。
#[test]
fn periodic_probes_climb_during_a_minute_of_light_activity() {
    let scenario = Scenario {
        network: vec![(0, steady_link(30_000_000))],
        start_probe: false,
        trace: true,
        ..Scenario::new(
            "light-minute",
            vec![phase(Action::Idle, 30), phase(Action::Pointer, 1_800)],
        )
    };
    let o = run(&scenario).unwrap();
    finish(&o);
    let p = o.probes;
    let verdicts: u32 = p.run_verdicts.iter().sum();
    let end = rate_at(&o, u64::MAX);
    // 最后一轮探测是什么时候发的
    let last_probe_ms = o
        .trace
        .iter()
        .filter(|l| l.contains("周期探测：按"))
        .filter_map(|l| l.get(1..7).and_then(|t| t.trim().parse::<u64>().ok()))
        .max()
        .unwrap_or(0);
    // 起步之后（第一阵流量发完，2 秒时）码率还不到 15 Mbps，才需要周期探测把它探上去。#71 第 2 步之前，第一阵流量
    // 收尾时会按送达速率误降到几 Mbps，非探不可；不再误降以后起步就到了，不要求探（探了也每轮都要有结论）。
    let after_start = rate_at(&o, 2_000);
    gate(
        &o,
        "轻操作时码率到得了位（起步没到就靠周期探测），每轮探测都有结论，到了就不再探",
        (p.run_sent >= 3 || after_start >= 15_000_000)
            && verdicts >= p.run_sent
            && verdicts - p.run_sent <= p.run_verdicts[3]
            && end >= 15_000_000
            && last_probe_ms < 45_000,
        &format!(
            "起步之后 {} kbps；发出 {} 轮，结论 {verdicts} 个（证实 {}、否定 {}、超时 {}、中止 {}）；码率 1000 → {} kbps；\
             最后一轮在 {last_probe_ms} ms；探测 {} 字节，占 {}‰",
            after_start / 1000,
            p.run_sent,
            p.run_verdicts[0],
            p.run_verdicts[1],
            p.run_verdicts[2],
            p.run_verdicts[3],
            end / 1000,
            p.bytes(),
            p.bytes() * 1000 / o.sent_bytes.max(1)
        ),
    );
    // 对端是老版本（反馈里探测结果不带组号）：不做运行中探测
    let old = run(&Scenario {
        probe_ids: false,
        name: "light-old-peer".to_owned(),
        phases: vec![phase(Action::Idle, 30), phase(Action::Pointer, 450)],
        ..scenario
    })
    .unwrap();
    finish(&old);
    gate(
        &old,
        "对端是老版本时不做运行中探测",
        old.probes.run_sent == 0 && old.probes.run_bytes == 0,
        &format!("运行中探测 {} 轮", old.probes.run_sent),
    );
}

/// Wi-Fi 上 3% 的随机丢包，但链路并不拥塞：估计器不该因此降码率。
#[test]
fn random_loss_without_congestion() {
    compare(&Scenario {
        network: vec![(
            0,
            Link {
                delay_us: 3_000,
                jitter_us: 2_000,
                loss_permille: 30,
                rate_bps: Some(30_000_000),
                ..Link::lan()
            },
        )],
        profile: Profile::WIRELESS,
        ..Scenario::new(
            "random-loss",
            vec![phase(Action::Dragging, 150), phase(Action::Idle, 30)],
        )
    });
}

/// 往返短的 Wi-Fi：会话开始按往返选了“稳定”画像（不带纠错），实际有 5% 随机丢包。
/// 应当自动加纠错、换成“无线”画像（2026-11-23 用户日志：原来丢一个包就等一个往返重传、重发关键帧）。
#[test]
fn random_loss_on_a_link_that_looked_stable() {
    compare(&Scenario {
        network: vec![(
            0,
            Link {
                delay_us: 10_000,
                jitter_us: 2_000,
                loss_permille: 50,
                rate_bps: Some(30_000_000),
                ..Link::lan()
            },
        )],
        profile: Profile::STABLE,
        ..Scenario::new(
            "lossy-stable",
            vec![
                phase(Action::Typing, 90),
                phase(Action::Dragging, 90),
                phase(Action::Idle, 30),
            ],
        )
    });
}

/// 窄带宽上播放视频：每帧都装不进预算。应当降帧率保住画面，而不是每帧都超预算、排长队卡住。
#[test]
fn video_on_a_narrow_link() {
    compare(&Scenario {
        network: vec![(
            0,
            Link {
                rate_bps: Some(1_500_000),
                delay_us: 15_000,
                ..Link::lan()
            },
        )],
        start_bps: 1_000_000,
        ..Scenario::new(
            "narrow-video",
            vec![phase(Action::Video, 150), phase(Action::Idle, 30)],
        )
    });
}

/// 带宽随时间变化的蜂窝网络（合成的，不是录下来的真实轨迹）：每半秒在 2–12 Mbps 之间随机游走，
/// 40 ms 延迟、抖动、1% 丢包，跑一整段办公操作。种子固定，结果可复现。
#[test]
fn office_on_a_varying_cellular_link() {
    let mut seed = 7u64;
    let mut mbps = 6i64;
    let network: Vec<(u64, Link)> = (0..36u64)
        .map(|i| {
            seed = seed
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            let step = i64::try_from(seed >> 62).unwrap() - 1; // -1、0、1、2
            mbps = (mbps + step * 2).clamp(2, 12);
            let link = Link {
                delay_us: 40_000,
                jitter_us: 15_000,
                loss_permille: 10,
                rate_bps: Some(u64::try_from(mbps).unwrap() * 1_000_000),
                ..Link::lan()
            };
            (i * 500_000, link)
        })
        .collect();
    compare(&Scenario {
        network,
        profile: Profile::CELLULAR,
        start_bps: 2_000_000,
        ..Scenario::new("varying-cellular", office())
    });
}

/// 很窄的链路（400 kbps，排队上限 200 ms，像差的蜂窝网或限速的公司网络）上打字：
/// 真实二进制在这样的限速上曾经 8 秒 0 帧（720p 满屏文字的关键帧约 150 KB，光传就要 3 秒，
/// 浅缓冲又让突发大量丢包）。第一幅画面必须 2 秒内出来，之后不能长时间冻结。
#[test]
fn typing_on_a_very_narrow_link() {
    compare(&Scenario {
        network: vec![(
            0,
            Link {
                rate_bps: Some(400_000),
                delay_us: 20_000,
                max_queue_us: 200_000,
                ..Link::lan()
            },
        )],
        ..Scenario::new(
            "very-narrow",
            vec![phase(Action::Typing, 120), phase(Action::Idle, 60)],
        )
    });
}

/// 停着的时候网络变了：空闲时没有数据，带宽估计冻结在旧值上。
/// 先在 20 Mbps 上打字，停 3 秒（期间掉到 2 Mbps），然后切换窗口（大面积突变）：
/// 按旧的估计一下子发出去会把瓶颈塞满、长时间冻结。
#[test]
fn link_drops_while_idle() {
    let fast = Link {
        rate_bps: Some(20_000_000),
        delay_us: 20_000,
        ..Link::lan()
    };
    let slow = Link {
        rate_bps: Some(2_000_000),
        ..fast
    };
    compare(&Scenario {
        network: vec![(0, fast), (3_500_000, slow)],
        ..Scenario::new(
            "drop-while-idle",
            vec![
                phase(Action::Typing, 60),
                phase(Action::Idle, 90),
                phase(Action::Switching, 30),
                phase(Action::Scrolling, 60),
                phase(Action::Idle, 30),
            ],
        )
    });
}

/// 停着的时候网络变好了（2 → 20 Mbps）：之后滚动，要很快用上新带宽（画质跟上）。
#[test]
fn link_improves_while_idle() {
    let slow = Link {
        rate_bps: Some(2_000_000),
        delay_us: 20_000,
        ..Link::lan()
    };
    let fast = Link {
        rate_bps: Some(20_000_000),
        ..slow
    };
    compare(&Scenario {
        network: vec![(0, slow), (3_500_000, fast)],
        start_bps: 1_000_000,
        ..Scenario::new(
            "rise-while-idle",
            vec![
                phase(Action::Typing, 60),
                phase(Action::Idle, 90),
                phase(Action::Scrolling, 90),
                phase(Action::Idle, 30),
            ],
        )
    });
}
