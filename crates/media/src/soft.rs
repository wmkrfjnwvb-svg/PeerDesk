//! openh264 软件编码后端（[`crate::backend::EncoderBackend`]）。
//!
//! openh264 自己的码控关掉，每帧按调用方给的 QP 编（[`RateControl::PerFrameQp`]）：帧的大小由上面一层的
//! [`crate::qp::QpControl`] 负责，这里只执行。为什么不用它自带的码控见 [`crate::qp`]。
//!
//! 这个后端的脾气（都写在 [`BackendTraits`] 里，调用方按属性处理，不按“是不是 openh264”）：
//! - 新开的编码器第一帧只能按默认 QP（[`DEFAULT_LAYER_QP`]）编：Rust 封装到第一次编码时才真正初始化编码器，
//!   之前取不到、也改不了参数。调用方要别的 QP 时，这里先多编一次让它初始化（结果丢掉），再按要的 QP 编成关键帧，
//!   多出来的时间记在 [`Timing::setup_us`]；
//! - 同步：交一帧当场出一帧，从不攒帧、从不丢帧；
//! - 屏幕内容模式自带场景切换检测：整屏突变的帧会自己编成关键帧，即使没要求；
//! - 只收 I420。

use std::time::Instant;

use openh264::encoder::{
    BitRate, Complexity, Encoder, EncoderConfig, FrameRate, FrameType, IntraFramePeriod, Level,
    QpRange, RateControlMode, UsageType,
};
use openh264::formats::YUVSlices;
use openh264::OpenH264API;

use crate::backend::{
    BackendError, BackendInfo, BackendKind, BackendTraits, EncodedOut, EncoderBackend, Finished,
    Format, FrameAsk, FrameInput, Inputs, RateControl, Rates, Reconfigured, Submitted, Timing,
};
use crate::qp;

/// openh264 每层固定 QP 的默认值（`SVC_QUALITY_BASE_QP`）：新开的编码器第一帧就是按它编的。
pub const DEFAULT_LAYER_QP: u8 = 26;

/// openh264 软件编码后端。
pub struct OpenH264Backend {
    info: BackendInfo,
    /// 打开着的编码器和它的尺寸。
    enc: Option<(u16, u16, Encoder)>,
    /// 打开之后还没编过帧。
    first: bool,
    /// 编码器现在用的每层固定 QP（没变就不再设，省一次取参数、设参数）。
    layer_qp: Option<u8>,
    rates: Rates,
}

impl std::fmt::Debug for OpenH264Backend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OpenH264Backend")
            .field("size", &self.size())
            .field("first", &self.first)
            .field("layer_qp", &self.layer_qp)
            .finish_non_exhaustive()
    }
}

impl Default for OpenH264Backend {
    fn default() -> Self {
        Self::new()
    }
}

impl OpenH264Backend {
    /// 创建（还没打开编码器，先 [`EncoderBackend::reconfigure`]）。
    #[must_use]
    pub fn new() -> Self {
        Self {
            info: BackendInfo {
                name: "OpenH264".to_owned(),
                kind: BackendKind::Software,
                format: Format::H264,
                traits: BackendTraits {
                    rate_control: RateControl::PerFrameQp,
                    inputs: Inputs {
                        i420: true,
                        nv12: false,
                        gpu: false,
                        described: false,
                    },
                    max_held: 0,
                    keyframe_within: 1,
                    first_frame_qp: Some(DEFAULT_LAYER_QP),
                    // 屏幕内容模式自带场景切换检测：整屏变了会自己编成关键帧（用的还是我们给的 QP）
                    scene_keyframes: true,
                },
            },
            enc: None,
            first: true,
            layer_qp: None,
            rates: Rates { bps: 0, fps: 1 },
        }
    }

    /// 打开编码器：关掉 openh264 自己的码控，每帧的 QP 由调用方定。
    fn open(rates: Rates) -> Result<Encoder, BackendError> {
        let bps = u32::try_from(rates.bps).unwrap_or(u32::MAX);
        let fps = u16::try_from(rates.fps).unwrap_or(u16::MAX);
        let config = EncoderConfig::new()
            .usage_type(UsageType::ScreenContentRealTime)
            .rate_control_mode(RateControlMode::Off)
            .bitrate(BitRate::from_bps(bps))
            .max_frame_rate(FrameRate::from_hz(f32::from(fps)))
            // 帧率由发送端决定；编码器自己跳帧会让画面停住。
            .skip_frames(false)
            // 屏幕内容模式不支持这两项，关掉免得 OpenH264 打警告。
            .adaptive_quantization(false)
            .background_detection(false)
            // 不定期插关键帧：关键帧只在开始和丢包恢复时出现。
            .intra_frame_period(IntraFramePeriod::from_num_frames(u32::MAX))
            .qp(QpRange::new(qp::MIN_QP, qp::MAX_QP))
            // 级别决定码率上限；不指定时按分辨率自动选，小画面的上限很低。
            .level(Level::Level_5_2)
            .complexity(Complexity::Low)
            .debug(false);
        Encoder::with_api_config(OpenH264API::from_source(), config)
            .map_err(|e| BackendError::Failed(format!("创建 H.264 编码器失败：{e}")))
    }
}

impl EncoderBackend for OpenH264Backend {
    fn info(&self) -> &BackendInfo {
        &self.info
    }

    fn size(&self) -> Option<(u16, u16)> {
        self.enc.as_ref().map(|(w, h, _)| (*w, *h))
    }

    fn reconfigure(
        &mut self,
        size: (u16, u16),
        rates: Rates,
    ) -> Result<Reconfigured, BackendError> {
        // 换尺寸只能重建；先丢掉旧的，打不开时保持“没配置”
        self.enc = None;
        self.first = true;
        self.layer_qp = None;
        self.rates = rates;
        self.enc = Some((size.0, size.1, Self::open(rates)?));
        Ok(Reconfigured {
            rebuilt: true,
            keyframe_next: true,
        })
    }

    fn set_rates(&mut self, rates: Rates) -> Result<(), BackendError> {
        // 码控关着，码率、帧率只在打开时用来填静态参数：记下来，下次重建时用，现在什么都不用动
        self.rates = rates;
        Ok(())
    }

    fn submit(&mut self, input: FrameInput<'_>, ask: FrameAsk) -> Result<Submitted, BackendError> {
        let FrameInput::I420 { y, u, v } = input else {
            return Err(BackendError::Misuse("OpenH264 只收 I420 画面"));
        };
        let Some(want) = ask.qp else {
            return Err(BackendError::Misuse("OpenH264 按 QP 编，每帧都要给 QP"));
        };
        // 编码器先拿出来用，编完放回去；编码出错时丢掉（复位），调用方下一帧重新配置（从关键帧开始）。
        let Some((width, height, mut enc)) = self.enc.take() else {
            return Err(BackendError::Misuse("还没配置编码尺寸就交帧"));
        };
        let (w, h) = (usize::from(width), usize::from(height));
        // 必须刚好等于：openh264 的 YUVSlices::new 要求平面长度和尺寸完全一致，不一致会 panic
        // （C2 体检交一幅比配置大的画面时发现的；以前只拦了太小的）
        if y.len() != w.saturating_mul(h)
            || u.len() != w / 2 * (h / 2)
            || v.len() != w / 2 * (h / 2)
        {
            self.enc = Some((width, height, enc));
            return Err(BackendError::Misuse("画面的大小和配置的尺寸对不上"));
        }
        let failed = |e: openh264::Error| BackendError::Failed(format!("H.264 编码失败：{e}"));
        let yuv = YUVSlices::new((y, u, v), (w, h), (w, w / 2, w / 2));
        let first = self.first;
        // 新开的编码器第一帧本来就是关键帧，不用另外要
        if ask.keyframe && !first {
            enc.force_intra_frame();
        }
        let mut timing = Timing::default();
        if first && want != DEFAULT_LAYER_QP {
            // 先编一次让编码器初始化（结果丢掉），再按要的 QP 重新编成关键帧。
            let setup = Instant::now();
            enc.encode(&yuv).map_err(failed)?;
            enc.force_intra_frame();
            timing.setup_us = micros_since(setup);
        }
        let computing = Instant::now();
        if first {
            self.layer_qp = Some(DEFAULT_LAYER_QP);
        }
        if self.layer_qp != Some(want) && set_layer_qp(&mut enc, want) {
            self.layer_qp = Some(want);
        }
        let (key, data) = enc
            .encode(&yuv)
            .map(|stream| {
                (
                    matches!(stream.frame_type(), FrameType::IDR | FrameType::I),
                    stream.to_vec(),
                )
            })
            .map_err(failed)?;
        timing.compute_us = micros_since(computing);
        self.first = false;
        self.enc = Some((width, height, enc));
        Ok(Submitted::Ready(Finished::Frame(EncodedOut {
            token: ask.token,
            data,
            key,
            qp: Some(want),
            timing,
        })))
    }
}

fn micros_since(start: Instant) -> u64 {
    u64::try_from(start.elapsed().as_micros()).unwrap_or(u64::MAX)
}

/// 码控关掉时每层用的固定 QP 改成 `qp`（运行中可改、不重建编码器、不出额外的关键帧：
/// `WelsEncoderParamAdjust` 不重置时会拷贝 `iDLayerQp`）。返回是否生效。
#[allow(unsafe_code)]
fn set_layer_qp(enc: &mut Encoder, qp: u8) -> bool {
    let mut param = std::mem::MaybeUninit::<openh264_sys2::SEncParamExt>::zeroed();
    // SAFETY: 编码器已经初始化（编过一帧）；ENCODER_OPTION_SVC_ENCODE_PARAM_EXT 的参数就是 SEncParamExt，
    // 指针指向本函数里全零初始化的局部变量（纯 C 结构），调用期间有效，GetOption 只往里写。
    let got = unsafe {
        enc.raw_api().get_option(
            openh264_sys2::ENCODER_OPTION_SVC_ENCODE_PARAM_EXT,
            param.as_mut_ptr().cast(),
        )
    };
    if got != 0 {
        return false;
    }
    // SAFETY: 全零对这个纯 C 结构是合法值；GetOption 成功时填好了当前参数。
    let mut param = unsafe { param.assume_init() };
    for layer in &mut param.sSpatialLayers {
        layer.iDLayerQp = i32::from(qp);
    }
    // SAFETY: 参数是刚取回的当前参数，只改了每层的固定 QP（不重建编码器的那一类参数）；指针指向局部变量。
    let set = unsafe {
        enc.raw_api().set_option(
            openh264_sys2::ENCODER_OPTION_SVC_ENCODE_PARAM_EXT,
            std::ptr::addr_of_mut!(param).cast(),
        )
    };
    set == 0
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::indexing_slicing)]
    use super::*;
    use crate::backend::DropReason;

    const RATES: Rates = Rates {
        bps: 2_000_000,
        fps: 30,
    };

    /// 一幅 64×64 的 I420 画面（亮度有渐变）；`patch` 不为 0 时左上角 8×8 的一小块换成这个亮度（局部变化）。
    fn planes(patch: u8) -> (Vec<u8>, Vec<u8>, Vec<u8>) {
        let y = (0..64usize * 64)
            .map(|i| {
                if patch != 0 && i % 64 < 8 && i / 64 < 8 {
                    patch
                } else {
                    u8::try_from((i % 64) * 3 % 200).unwrap()
                }
            })
            .collect();
        (y, vec![128; 32 * 32], vec![128; 32 * 32])
    }

    fn ask(token: u64, qp: u8, keyframe: bool) -> FrameAsk {
        FrameAsk {
            token,
            budget: 100_000,
            qp: Some(qp),
            keyframe,
        }
    }

    /// 同步后端应当场出帧。
    fn frame(got: Result<Submitted, BackendError>) -> EncodedOut {
        let out = match got.unwrap() {
            Submitted::Ready(Finished::Frame(f)) => Some(f),
            Submitted::Ready(Finished::Dropped { .. }) | Submitted::Pending => None,
        };
        out.unwrap()
    }

    /// 契约逐条：每帧有下落并带回编号、同步不攒帧、要关键帧就给、改码率不重建不出关键帧、
    /// 换尺寸说明代价、时间分开报、用法不对不改变状态。
    #[test]
    fn openh264_keeps_the_backend_contract() {
        let mut b = OpenH264Backend::new();
        let info = b.info().clone();
        assert_eq!(
            (info.kind, info.format),
            (BackendKind::Software, Format::H264)
        );
        assert_eq!(info.traits.rate_control, RateControl::PerFrameQp);
        assert_eq!((info.traits.max_held, info.traits.keyframe_within), (0, 1));
        assert_eq!(info.traits.first_frame_qp, Some(DEFAULT_LAYER_QP));
        assert!(format!("{b:?}").contains("OpenH264Backend"));
        let (y, u, v) = planes(0);
        let input = FrameInput::I420 {
            y: &y,
            u: &u,
            v: &v,
        };
        // 还没配置就交帧：用法不对
        assert_eq!(b.size(), None);
        assert!(matches!(
            b.submit(input, ask(1, 26, true)),
            Err(BackendError::Misuse(_))
        ));
        // 配置：重建，下一帧一定是关键帧
        let r = b.reconfigure((64, 64), RATES).unwrap();
        assert!(r.rebuilt && r.keyframe_next);
        assert_eq!(b.size(), Some((64, 64)));
        // 第一帧没要关键帧也是关键帧；按默认 QP 编不用初始化
        let first = frame(b.submit(input, ask(10, DEFAULT_LAYER_QP, false)));
        assert!(first.key && !first.data.is_empty());
        assert_eq!((first.token, first.qp), (10, Some(DEFAULT_LAYER_QP)));
        assert_eq!(first.timing.setup_us, 0);
        assert_eq!(first.timing.wait_us, 0, "软件编码没有等待");
        assert!(info.traits.scene_keyframes);
        // 之后局部变化的帧不是关键帧
        let (y2, u2, v2) = planes(40);
        let changed = FrameInput::I420 {
            y: &y2,
            u: &u2,
            v: &v2,
        };
        let fine = frame(b.submit(changed, ask(11, 20, false)));
        assert!(!fine.key);
        // 改码率、帧率：不重建，不出关键帧
        b.set_rates(Rates {
            bps: 300_000,
            fps: 10,
        })
        .unwrap();
        assert_eq!(b.size(), Some((64, 64)));
        let after = frame(b.submit(input, ask(12, 40, false)));
        assert!(!after.key && after.token == 12);
        // 要关键帧：就是这一帧（keyframe_within = 1）
        let key = frame(b.submit(input, ask(13, 30, true)));
        assert!(key.key);
        // 同步后端没有攒着的帧
        assert_eq!(b.poll().unwrap(), None);
        b.flush().unwrap();
        // 用法不对：状态不变，接着还能编
        assert!(matches!(
            b.submit(FrameInput::Nv12(&y), ask(14, 30, false)),
            Err(BackendError::Misuse(_))
        ));
        let no_qp = FrameAsk {
            qp: None,
            ..ask(15, 30, false)
        };
        assert!(matches!(
            b.submit(input, no_qp),
            Err(BackendError::Misuse(_))
        ));
        let short = FrameInput::I420 {
            y: &y[..100],
            u: &u,
            v: &v,
        };
        assert!(matches!(
            b.submit(short, ask(16, 30, false)),
            Err(BackendError::Misuse(_))
        ));
        assert_eq!(b.size(), Some((64, 64)));
        assert!(!frame(b.submit(changed, ask(17, 30, false))).key);
        assert!(!b.accepts(&peerdesk_core::frame::TestGpuFrame::new(
            peerdesk_core::frame::Nv12::new(64, 64, vec![16; 64 * 64 * 3 / 2]).unwrap()
        )));
        // 丢帧原因的说明（这个后端从不丢帧，说明文字给日志用）
        assert!(!DropReason::Budget.name().is_empty());
    }

    /// 新开之后第一帧要别的 QP：先初始化一次（时间单独记），出来的仍然是关键帧，而且确实按要的 QP 编了
    /// （粗的比默认的小）。换尺寸重新来过。
    #[test]
    fn the_first_frame_can_be_coarser_at_the_cost_of_one_setup_encode() {
        let (y, u, v) = planes(0);
        let input = FrameInput::I420 {
            y: &y,
            u: &u,
            v: &v,
        };
        let mut plain = OpenH264Backend::default();
        plain.reconfigure((64, 64), RATES).unwrap();
        let default = frame(plain.submit(input, ask(1, DEFAULT_LAYER_QP, true)));
        let mut coarse = OpenH264Backend::new();
        coarse.reconfigure((64, 64), RATES).unwrap();
        let primed = frame(coarse.submit(input, ask(1, 48, true)));
        assert!(primed.key && primed.qp == Some(48));
        assert!(primed.timing.setup_us > 0, "多编的那一次单独计时");
        assert!(
            primed.data.len() < default.data.len(),
            "QP 48 的关键帧 {} 字节，应比 QP 26 的 {} 字节小",
            primed.data.len(),
            default.data.len()
        );
        // 重新配置（同样的尺寸也照做）：又是第一帧
        let r = coarse.reconfigure((64, 64), RATES).unwrap();
        assert!(r.keyframe_next);
        assert!(frame(coarse.submit(input, ask(2, DEFAULT_LAYER_QP, false))).key);
    }
}
