//! 内容策略：一个使用模式（现在只有办公；以后的游戏、电影、音乐模式）就是一张参数表。
//! 发送端、帧率调节、编码器都从这里读参数，不各自写死常量；框架不变，加一个模式就是加一张表
//! （`docs/research/L-framework.md` 4.5 节；对应 WebRTC 的 `DegradationPreference` 和 W3C `contentHint`）。
//!
//! 只收现在真正用到的参数。游戏模式要的“保帧率”降级顺序、帧内刷新代替关键帧、电影模式的缓冲，
//! 做的时候再加字段，并在 e2e 里加对应的场景（例如 4K144、100 Mbps）守住。

/// 一个模式的参数。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ContentPolicy {
    /// 名字（日志、详情面板用）。
    pub name: &'static str,
    /// 帧率档位，从高到低：帧率调节只在这些档位里升降，最后一档是下限。
    pub fps_steps: &'static [u32],
    /// 起步码率（bps）：连接时还不知道链路多快，第一个关键帧按它编；之后按第一个关键帧的包串测得的
    /// 容量很快升上去（快链路上几百毫秒内）。
    pub start_bps: u64,
    /// 码率上限（bps）。
    pub max_bps: u64,
    /// 编码器的 QP 上限：大帧最多粗到哪（之后补清晰）。H.264 最大 51。
    pub max_qp: u8,
    /// 大帧规则的时限（微秒）：关键帧、整屏变化要在这么久里按码率发得完（照 Chromium 远程桌面
    /// `ShouldDropQualityForLargeFrame`，发不完就粗编、之后补清晰）。0 表示不限。
    pub big_frame_us: u64,
    /// 码率估计多久没被测量证实就算过期（微秒）；过期时大帧不按时限放宽（见
    /// [`crate::media::MediaSender`]）。
    pub rate_stale_after_us: u64,
}

impl ContentPolicy {
    /// 办公：看的是文字，清晰度最后让步（画质 → 帧率 → 分辨率），帧率 5–60。
    pub const OFFICE: Self = Self {
        name: "办公",
        fps_steps: &[60, 30, 20, 15, 10, 8, 5],
        // 1 Mbps（原来 4 Mbps）：窄带上按 4 Mbps 编第一个关键帧，微软 8 条真实轨迹首帧平均 935 ms、最慢 2.2 秒；
        // 1 Mbps 时平均 295 ms，卡顿也最少（500 kbps 首帧 74 ms 但卡顿多，2 Mbps 556 ms）；快链路上没有代价
        // （第一个关键帧的包串测出容量，很快升上去；e2e 局域网第一秒 PSNR 43.1 → 45.4 dB）。
        // 先探测再编第一帧、或者和第一帧并行探测，实测都不如它（docs/research/L-framework.md T7）。
        start_bps: 1_000_000,
        max_bps: 30_000_000,
        max_qp: 51,
        big_frame_us: 100_000,
        // 2 秒而不是 1 秒：操作之间停 1 秒很常见，链路 1 秒里变差的机会很小，而每次停顿后第一帧都要先粗后清；
        // 1 秒时办公负载的每次停顿都触发（微软轨迹 ms-05729 上改变了之后的丢包时序）。
        rate_stale_after_us: 2_000_000,
    };

    /// 帧率下限（档位的最后一档）。
    #[must_use]
    pub fn min_fps(&self) -> u32 {
        self.fps_steps.iter().copied().min().unwrap_or(1)
    }
}

impl Default for ContentPolicy {
    fn default() -> Self {
        Self::OFFICE
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn office_is_the_default_and_keeps_text_readable() {
        let p = ContentPolicy::default();
        assert_eq!(p, ContentPolicy::OFFICE);
        assert_eq!(p.min_fps(), 5);
        assert!(
            p.fps_steps.windows(2).all(|w| matches!(w, [a, b] if a > b)),
            "档位从高到低"
        );
        assert!(p.max_qp <= 51);
        let empty = ContentPolicy {
            fps_steps: &[],
            ..ContentPolicy::OFFICE
        };
        assert_eq!(empty.min_fps(), 1);
    }
}
