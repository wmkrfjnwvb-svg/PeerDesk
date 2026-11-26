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
