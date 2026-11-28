//! Android 解码器（MediaCodec）的设置：低延迟开关、码流参数（纯逻辑，Linux 上能测）。
//!
//! 默认的 MediaCodec 会攒几帧再出画面，远程桌面要关掉。各家芯片的开关不一样，有的设了反而 `configure` 失败，
//! 所以分几档：从最激进的一档开始，`configure` 失败就退一档再试，直到什么都不设。
//!
//! 键名和“哪档更冒险”参考 Moonlight 的 `MediaCodecHelper.setDecoderLowLatencyOptions`（只取事实，不抄代码，
//! 它是 GPL-3）：
//! - `low-latency`：Android 11 起的官方键（`MediaFormat.KEY_LOW_LATENCY`）。
//! - `vdec-lowlatency`：联发科（和 Amlogic）改过的 ACodec 认这个键。
//! - `priority = 0`（实时）：高通的解码器上不设——Moonlight 记录过高通平台上配合“最高处理速率”会崩溃，
//!   我们两个都不设，只用高通自己的低延迟键。
//! - 各家的厂商键（`vendor.<扩展名>.<参数>`，Android 8.0 起支持）：按解码器名字的前缀认芯片。

/// 一个格式键和它的整数值。
pub type Opt = (&'static str, i32);

/// 芯片厂商（按解码器名字认）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Vendor {
    /// 高通（`OMX.qcom`、`c2.qti`）。
    Qualcomm,
    /// 三星 Exynos。
    Exynos,
    /// 华为麒麟。
    Kirin,
    /// Amlogic（电视盒子）。
    Amlogic,
    /// 别的（联发科、谷歌软件解码器等）。
    Other,
}

impl Vendor {
    /// 按解码器名字认芯片（不分大小写，看前缀）。
    #[must_use]
    pub fn of(decoder: &str) -> Self {
        let name = decoder.to_ascii_lowercase();
        let any = |prefixes: &[&str]| prefixes.iter().any(|p| name.starts_with(p));
        if any(&["omx.qcom", "c2.qti"]) {
            Self::Qualcomm
        } else if any(&["omx.exynos", "c2.exynos"]) {
            Self::Exynos
        } else if any(&["omx.hisi", "c2.hisi"]) {
            Self::Kirin
        } else if any(&["omx.amlogic", "c2.amlogic"]) {
            Self::Amlogic
        } else {
            Self::Other
        }
    }
}

/// 一共几档（第 `TRIES - 1` 档什么都不设，一定能 `configure`，除非解码器本身坏了）。
pub const TRIES: u32 = 6;

/// 第 `attempt` 档（从 0 开始）要设的键。档数越大设得越少；超过 [`TRIES`] 也是什么都不设。
#[must_use]
pub fn options(decoder: &str, attempt: u32) -> Vec<Opt> {
    let vendor = Vendor::of(decoder);
    let mut out = Vec::new();
    if attempt < 1 {
        out.push(("low-latency", 1));
    }
    if attempt < 2 {
        out.push(("vdec-lowlatency", 1));
    }
    if attempt < 3 && vendor != Vendor::Qualcomm {
        out.push(("priority", 0));
    }
    match vendor {
        Vendor::Qualcomm => {
            if attempt < 4 {
                out.push(("vendor.qti-ext-dec-picture-order.enable", 1));
            }
            if attempt < 5 {
                out.push(("vendor.qti-ext-dec-low-latency.enable", 1));
            }
        }
        Vendor::Kirin => {
            if attempt < 4 {
                out.push((
                    "vendor.hisi-ext-low-latency-video-dec.video-scene-for-low-latency-req",
                    1,
                ));
                out.push((
                    "vendor.hisi-ext-low-latency-video-dec.video-scene-for-low-latency-rdy",
                    -1,
                ));
            }
        }
        Vendor::Exynos => {
            if attempt < 4 {
                out.push(("vendor.rtc-ext-dec-low-latency.enable", 1));
            }
        }
        Vendor::Amlogic => {
            if attempt < 4 {
                out.push(("vendor.low-latency.enable", 1));
            }
        }
        Vendor::Other => {}
    }
    out
}

/// 从关键帧（Annex B）里取出 SPS 和 PPS，各自带上四字节起始码：MediaCodec 的 `csd-0`、`csd-1`。
/// 没有就是 `None`（这一帧不能用来打开解码器）。
#[must_use]
pub fn codec_params(data: &[u8]) -> Option<(Vec<u8>, Vec<u8>)> {
    let nals = peerdesk_media::annexb::nal_units(data);
    let find = |kind: u8| {
        nals.iter()
            .find(|n| peerdesk_media::annexb::nal_type(n) == kind)
            .map(|n| [&[0u8, 0, 0, 1][..], n].concat())
    };
    Some((find(7)?, find(8)?))
}

/// 硬件解码器都打不开时，按顺序试这几个系统自带的软件解码器（厂商付过专利许可，不是我们带的 OpenH264）。
pub const SOFTWARE_DECODERS: [&str; 2] = ["c2.android.avc.decoder", "OMX.google.h264.decoder"];

/// 是不是系统自带的软件解码器（工具条和日志里写明“软件解码”）。
#[must_use]
pub fn is_software(decoder: &str) -> bool {
    let name = decoder.to_ascii_lowercase();
    name.starts_with("c2.android.") || name.starts_with("omx.google.")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys(decoder: &str, attempt: u32) -> Vec<&'static str> {
        options(decoder, attempt)
            .into_iter()
            .map(|(k, _)| k)
            .collect()
    }

    #[test]
    fn vendors_are_known_by_the_decoder_name() {
        assert_eq!(
            Vendor::of("c2.qti.avc.decoder.low_latency"),
            Vendor::Qualcomm
        );
        assert_eq!(Vendor::of("OMX.qcom.video.decoder.avc"), Vendor::Qualcomm);
        assert_eq!(Vendor::of("c2.exynos.h264.decoder"), Vendor::Exynos);
        assert_eq!(Vendor::of("OMX.hisi.video.decoder.avc"), Vendor::Kirin);
        assert_eq!(
            Vendor::of("OMX.amlogic.avc.decoder.awesome2"),
            Vendor::Amlogic
        );
        assert_eq!(Vendor::of("c2.mtk.avc.decoder"), Vendor::Other);
    }

    #[test]
    fn each_try_sets_less_until_nothing() {
        // 高通：不设 priority，有自己的两个键，最后一个退得最晚
        assert_eq!(
            keys("c2.qti.avc.decoder", 0),
            [
                "low-latency",
                "vdec-lowlatency",
                "vendor.qti-ext-dec-picture-order.enable",
                "vendor.qti-ext-dec-low-latency.enable"
            ]
        );
        assert_eq!(
            keys("c2.qti.avc.decoder", 4),
            ["vendor.qti-ext-dec-low-latency.enable"]
        );
        assert!(keys("c2.qti.avc.decoder", 5).is_empty());
        // 联发科等：官方键、联发科的键、实时优先级
        assert_eq!(
            keys("c2.mtk.avc.decoder", 0),
            ["low-latency", "vdec-lowlatency", "priority"]
        );
        assert_eq!(options("c2.mtk.avc.decoder", 2), [("priority", 0)]);
        assert!(keys("c2.mtk.avc.decoder", 3).is_empty());
        // 三星、麒麟、Amlogic 的厂商键在第 3 档还在、第 4 档没了
        assert!(
            keys("c2.exynos.h264.decoder", 3).contains(&"vendor.rtc-ext-dec-low-latency.enable")
        );
        assert!(keys("c2.exynos.h264.decoder", 4).is_empty());
        let kirin = options("OMX.hisi.video.decoder.avc", 3);
        assert!(kirin.contains(&(
            "vendor.hisi-ext-low-latency-video-dec.video-scene-for-low-latency-rdy",
            -1
        )));
        assert!(keys("OMX.amlogic.avc.decoder", 3).contains(&"vendor.low-latency.enable"));
        // 超过档数：什么都不设
        for name in [
            "c2.qti.avc.decoder",
            "c2.mtk.avc.decoder",
            "OMX.hisi.x",
            "OMX.amlogic.x",
        ] {
            assert!(options(name, TRIES).is_empty(), "{name}");
            assert!(options(name, TRIES - 1).is_empty(), "{name}");
        }
    }

    #[test]
    fn parameter_sets_come_out_of_a_keyframe_with_start_codes() {
        // SPS（7）、PPS（8）、IDR（5），三字节和四字节起始码混着
        let frame = [
            0, 0, 0, 1, 0x67, 1, 2, 0, 0, 1, 0x68, 3, 0, 0, 0, 1, 0x65, 9, 9,
        ];
        let (sps, pps) = codec_params(&frame).unwrap_or_default();
        assert_eq!(sps, [0, 0, 0, 1, 0x67, 1, 2]);
        assert_eq!(pps, [0, 0, 0, 1, 0x68, 3]);
        // 增量帧没有参数集
        assert_eq!(codec_params(&[0, 0, 1, 0x41, 7]), None);
        assert_eq!(codec_params(&[0, 0, 1, 0x67, 7]), None, "缺 PPS");
    }

    #[test]
    fn software_decoders_are_named() {
        assert!(SOFTWARE_DECODERS.iter().all(|n| is_software(n)));
        assert!(!is_software("c2.qti.avc.decoder"));
    }
}
