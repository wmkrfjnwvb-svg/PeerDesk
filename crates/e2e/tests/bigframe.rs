//! 测量用（不进 CI）：大帧——开头的整屏关键帧、整屏切换——在不同每帧预算、不同最大 QP 下编出来多大、
//! 多清晰，以及画面停下后补清晰要多少字节、补到多清晰。用来定“大帧规则”（见 docs/research/M-bigframe.md）。
//!
//! 手动运行：`cargo test -p peerdesk-e2e --release --test bigframe -- --ignored --nocapture`
#![allow(clippy::unwrap_used, clippy::print_stdout, clippy::indexing_slicing)]

use peerdesk_core::video::{FrameHeader, VideoDecoder, VideoEncoder};
use peerdesk_e2e::{luma_psnr, Action, RealisticDesktop};
use peerdesk_media::{H264Decoder, H264Encoder, HardwareEncoder};

fn software_only(_: u32, _: u32, _: u32, _: u64) -> Result<Box<dyn HardwareEncoder>, String> {
    Err("测量只用软件编码".to_owned())
}

/// 编一帧、解出来，返回 (字节数, 是不是关键帧, 亮度 PSNR)。
fn round(
    enc: &mut H264Encoder,
    dec: &mut H264Decoder,
    id: u32,
    desk: &RealisticDesktop,
    changed: bool,
    budget: usize,
) -> Option<(usize, bool, f64)> {
    let frame = enc.encode(id, desk.screen(), changed, budget).unwrap()?;
    let (header, data) = FrameHeader::parse(&frame.payload).unwrap();
    let shown = dec.decode(&header, data).unwrap()?;
    Some((
        frame.payload.len(),
        frame.key,
        luma_psnr(shown, desk.screen()),
    ))
}

#[test]
#[ignore = "测量用，手动运行"]
fn big_frames_by_budget_and_max_qp() {
    const FPS: u32 = 30;
    println!("| 大帧固定 QP | 每帧预算 B（码率） | 首帧 B / PSNR | 整屏切换 B / PSNR | 停下后补清晰 帧数 / B / 最后 PSNR | 关键帧数 |");
    println!("|---|---|---|---|---|---|");
    for coarse in [None, Some(38u8), Some(42), Some(46), Some(51)] {
        for budget in [1_200usize, 2_500, 10_000] {
            let mut desk = RealisticDesktop::new(1280, 720, 1).unwrap();
            let mut enc = H264Encoder::with_max_bps(FPS, 30_000_000)
                .with_hardware(software_only)
                .with_max_qp(51);
            let mut dec = H264Decoder::new().unwrap();
            let mut keys = 0;
            if let Some(qp) = coarse {
                enc.coarse_next(qp);
            }
            let first = round(&mut enc, &mut dec, 1, &desk, true, budget).unwrap();
            keys += u32::from(first.1);
            // 整屏切换（换窗口）：第一帧突变
            desk.step(Action::Switching).unwrap();
            if let Some(qp) = coarse {
                enc.coarse_next(qp);
            }
            let switch = round(&mut enc, &mut dec, 2, &desk, true, budget).unwrap();
            keys += u32::from(switch.1);
            // 画面停下：按时调用，直到编码器说补完了（最多 3 秒）
            let (mut n, mut bytes, mut last) = (0u32, 0usize, switch.2);
            for id in 3..(3 + 3 * FPS) {
                if let Some((b, k, p)) = round(&mut enc, &mut dec, id, &desk, false, budget) {
                    n += 1;
                    bytes += b;
                    last = p;
                    keys += u32::from(k);
                }
                if !enc.wants_refine() {
                    break;
                }
            }
            println!(
                "| {} | {budget}（{} kbps） | {} / {:.1} | {} / {:.1} | {n} / {bytes} / {last:.1} | {keys} |",
                coarse.map_or("码控".to_owned(), |q| q.to_string()),
                budget * 8 * 30 / 1000,
                first.0,
                first.2,
                switch.0,
                switch.2,
            );
        }
    }
}
