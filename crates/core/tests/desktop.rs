//! 桌面会话仿真测试：用真实的编解码器和合成桌面，在各种网络上检查流量、收敛和自适应。
#![allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::print_stdout
)]

use peerdesk_core::adaptive::Profile;
use peerdesk_core::desktop_sim::{run, Params, Phase, RateMode, Report};
use peerdesk_core::screen::Workload;
use peerdesk_core::sim::Link;
use peerdesk_core::Error;

fn phase(workload: Workload, frames: u32) -> Phase {
    Phase { workload, frames }
}

fn kbps(bytes: u64, frames: u32, fps: u32) -> u64 {
    bytes * 8 * u64::from(fps) / u64::from(frames.max(1)) / 1000
}

fn show(name: &str, r: &Report) {
    println!(
        "{name}: sent {} delivered {} bytes {} converge {:?} p50 {} p95 {} rate {:?} repaired {} recovered {} qdrop {} lost {}",
        r.frames_sent, r.frames_delivered, r.bytes_sent, r.converge_us, r.latency_p50_us,
        r.latency_p95_us, r.rate_bps, r.frames_repaired, r.recovered, r.net.queue_dropped, r.net.lost
    );
}

#[test]
fn idle_desktop_costs_nothing_after_the_first_frame() {
    let r = run(&Params {
        phases: vec![phase(Workload::Idle, 300)],
        ..Params::default()
    })
    .unwrap();
    show("idle", &r);
    assert!(r.converged);
    // 首帧整屏约 7.6 KB，略超一帧的预算，所以分两帧发完；之后 10 秒没有任何数据。
    assert!(r.frames_sent <= 2, "{}", r.frames_sent);
    assert!(r.bytes_sent < 12_000, "{}", r.bytes_sent);
    assert_eq!(r.bad_input, 0);
}

#[test]
fn office_workloads_are_cheap_and_exact() {
    let fps = 30;
    let r = run(&Params {
        fps,
        phases: vec![
            phase(Workload::Idle, 30),
            phase(Workload::Typing, 300),
            phase(Workload::Scrolling, 60),
            phase(Workload::Dragging, 150),
        ],
        ..Params::default()
    })
    .unwrap();
    show("office", &r);
    assert!(r.converged, "diff {}", r.final_diff);
    assert_eq!(r.frames_delivered, r.frames_sent);
    let typing = kbps(r.phase_bytes[1], 300, fps);
    let scrolling = kbps(r.phase_bytes[2], 60, fps);
    let dragging = kbps(r.phase_bytes[3], 150, fps);
    println!("typing {typing} kbps, scrolling {scrolling} kbps, dragging {dragging} kbps");
    assert!(typing < 100, "打字 {typing} kbps");
    assert!(dragging < 800, "拖窗口 {dragging} kbps");
    assert!(typing < dragging);
    // 局域网上一帧的延迟在几毫秒以内
    assert!(r.latency_p95_us < 5_000, "{}", r.latency_p95_us);
    // 停止操作后很快完全一致
    assert!(r.converge_us.unwrap() < 50_000, "{:?}", r.converge_us);
}

#[test]
fn lossy_link_still_converges_to_the_exact_picture() {
    for (name, link) in [
        (
            "wifi 5%",
            Link {
                loss_permille: 50,
                ..Link::wifi()
            },
        ),
        ("cellular", Link::cellular()),
    ] {
        let params = Params {
            seed: 11,
            link,
            phases: vec![phase(Workload::Typing, 120), phase(Workload::Dragging, 120)],
            settle_frames: 60,
            rate: RateMode::Adaptive {
                profile: Profile::WIRELESS,
                start_bps: 2_000_000,
                max_bps: 20_000_000,
            },
            ..Params::default()
        };
        let r = run(&params).unwrap();
        show(name, &r);
        assert!(r.net.lost > 0, "{name}: 这个测试需要有丢包");
        assert!(
            r.converged,
            "{name}: 丢包后最终必须与源完全一致，还差 {} 像素",
            r.final_diff
        );
        assert!(
            r.converge_us.unwrap() < 1_000_000,
            "{name}: {:?}",
            r.converge_us
        );
        assert_eq!(r.bad_input, 0);
    }
}

#[test]
fn without_repair_heavy_loss_leaves_permanent_damage() {
    // 30% 丢包、不带纠错。打字时每帧只改一个块，丢了就没有后续的帧来覆盖它。
    let params = Params {
        seed: 2,
        link: Link {
            loss_permille: 300,
            ..Link::lan()
        },
        phases: vec![phase(Workload::Typing, 200)],
        settle_frames: 90,
        rate: RateMode::Fixed(60_000),
        fec_permille: 0,
        ..Params::default()
    };
    let repaired = run(&params).unwrap();
    show("repair on", &repaired);
    assert!(repaired.converged, "diff {}", repaired.final_diff);
    assert!(repaired.frames_sent > 200, "丢失的块被重新发送");

    let broken = run(&Params {
        repair: false,
        ..params
    })
    .unwrap();
    show("repair off", &broken);
    assert!(
        !broken.converged,
        "对照：没有修复机制时，丢掉的块一直停留在旧内容"
    );
    assert!(broken.final_diff > 0);
}

#[test]
fn parity_repairs_single_losses_without_waiting_for_feedback() {
    let base = Params {
        seed: 5,
        link: Link {
            loss_permille: 30,
            ..Link::lan()
        },
        phases: vec![phase(Workload::Video, 90)],
        settle_frames: 60,
        rate: RateMode::Fixed(60_000),
        ..Params::default()
    };
    let without = run(&Params {
        fec_permille: 0,
        ..base.clone()
    })
    .unwrap();
    let with = run(&Params {
        fec_permille: 200,
        ..base
    })
    .unwrap();
    show("no fec", &without);
    show("fec 20%", &with);
    assert!(with.recovered > 0);
    assert_eq!(without.recovered, 0);
    // 有纠错时完整送达的帧比例更高
    let ratio = |r: &Report| u64::from(r.frames_delivered) * 1000 / u64::from(r.frames_sent);
    assert!(
        ratio(&with) > ratio(&without),
        "{} vs {}",
        ratio(&with),
        ratio(&without)
    );
    assert!(with.converged && without.converged);
}

#[test]
fn rate_control_fits_video_into_a_narrow_link() {
    // 视频内容无损需要好几兆，链路只有 1.5 Mbps。
    let link = Link {
        rate_bps: Some(1_500_000),
        max_queue_us: 150_000,
        ..Link::lan()
    };
    let phases = vec![phase(Workload::Video, 240)];
    let fixed = run(&Params {
        link,
        phases: phases.clone(),
        settle_frames: 90,
        rate: RateMode::Fixed(usize::MAX / 2),
        ..Params::default()
    })
    .unwrap();
    let adaptive = run(&Params {
        link,
        phases,
        settle_frames: 90,
        rate: RateMode::Adaptive {
            profile: Profile::STABLE,
            start_bps: 4_000_000,
            max_bps: 20_000_000,
        },
        ..Params::default()
    })
    .unwrap();
    show("fixed", &fixed);
    show("adaptive", &adaptive);

    // 不控制码率：队列溢出，大量帧不完整
    assert!(fixed.net.queue_dropped > 100, "{}", fixed.net.queue_dropped);
    // 控制码率：码率降到链路能承受的水平，队列溢出少一个数量级以上
    assert!(
        adaptive.net.queue_dropped * 10 < fixed.net.queue_dropped,
        "{} vs {}",
        adaptive.net.queue_dropped,
        fixed.net.queue_dropped
    );
    let (min, _, last) = adaptive.rate_bps;
    assert!(min < 1_500_000, "码率应当降到链路带宽以下：{min}");
    assert!(last < 3_000_000, "结束时码率 {last}");
    assert!(
        u64::from(adaptive.frames_delivered) * 100 / u64::from(adaptive.frames_sent) >= 90,
        "{} / {}",
        adaptive.frames_delivered,
        adaptive.frames_sent
    );
    // 延迟有界：不到 200 毫秒
    assert!(
        adaptive.latency_p95_us < 200_000,
        "{}",
        adaptive.latency_p95_us
    );
    assert!(
        fixed.frames_delivered < 10,
        "不控制码率时几乎没有帧能完整送达"
    );
    // 视频停下后，画面补清晰直到与源完全一致
    assert!(adaptive.converged, "diff {}", adaptive.final_diff);
}

#[test]
fn bandwidth_drop_mid_session_is_absorbed() {
    let wide = Link {
        rate_bps: Some(8_000_000),
        max_queue_us: 150_000,
        ..Link::lan()
    };
    let narrow = Link {
        rate_bps: Some(800_000),
        ..wide
    };
    let r = run(&Params {
        seed: 3,
        link: wide,
        link_change: Some((90, narrow)),
        phases: vec![phase(Workload::Video, 240), phase(Workload::Typing, 60)],
        settle_frames: 120,
        rate: RateMode::Adaptive {
            profile: Profile::STABLE,
            start_bps: 4_000_000,
            max_bps: 20_000_000,
        },
        ..Params::default()
    })
    .unwrap();
    show("drop", &r);
    let (min, max, _) = r.rate_bps;
    assert!(max >= 4_000_000, "带宽充足时保持高码率：{max}");
    assert!(min < 1_000_000, "带宽骤降后码率跟着降：{min}");
    assert!(r.converged, "diff {}", r.final_diff);
    assert_eq!(r.bad_input, 0);
}

#[test]
fn deterministic_and_validates_arguments() {
    let params = Params {
        seed: 9,
        link: Link::wifi(),
        ..Params::default()
    };
    assert_eq!(run(&params).unwrap(), run(&params).unwrap());
    assert_ne!(
        run(&params).unwrap(),
        run(&Params { seed: 10, ..params }).unwrap()
    );
    assert_eq!(
        run(&Params {
            fps: 0,
            ..Params::default()
        }),
        Err(Error::InvalidArgument("fps 不能为 0"))
    );
    // 链路完全不通：不会卡死，也不会收敛
    let dead = run(&Params {
        link: Link {
            up: false,
            ..Link::lan()
        },
        ..Params::default()
    })
    .unwrap();
    assert!(!dead.converged);
    assert_eq!(dead.frames_delivered, 0);
    assert_eq!(dead.converge_us, None);
    assert_eq!(dead.rate_bps, (0, 0, 0));
}
