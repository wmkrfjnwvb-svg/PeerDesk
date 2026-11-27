//! 远程画面上的工具条背后的逻辑：收起和展开、按钮放不放得下、网络状态怎么说。
//! 只算不画，`viewer.rs` 拿结果去画。
//!
//! 设计依据：`docs` 里的调研（同行的工具条）和设计稿 Toolbar.dc.html。
//! - 默认不挡画面：展开后一段时间没用就收成一个小条（Parsec、ToDesk、CRD 的做法）；
//! - 小条上就带网络状态，变差时变色、写明原因，旁边有“怎么办”（同行没有谁主动说，这是 PeerDesk 的差异点）；
//! - 展开后只放高频按钮，其余收进一层“更多”菜单，不做多层嵌套（TeamViewer 新工具条被骂的点）；
//! - 窗口窄时，放不下的按钮自己挪进“更多”，不会盖住画面或超出窗口。

use std::time::Duration;

use peerdesk_net::controller::LinkQuality;

use super::i18n::Strings;
use super::state::{classify, net_texts, route_name, NetCause, NetLevel, NetMeter, ViewPhase};
use super::theme::Signal;

/// 展开的工具条这么久没碰（鼠标不在上面）就自动收成小条。只在连上以后、而且用户没有手动展开过时生效。
pub const AUTO_COLLAPSE: Duration = Duration::from_secs(6);
/// 窗口比这还窄时只显示小条：展开的工具条放不下。
pub const MIN_EXPANDED_WIDTH: f32 = 360.0;

/// 工具条上的功能。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Item {
    /// 快捷键菜单。
    Shortcuts,
    /// 声音开关。
    Sound,
    /// 全屏。
    Fullscreen,
    /// 文件管理。
    Files,
    /// 原始大小（1:1）。
    Original,
    /// 连接详情。
    Details,
}

/// 一个图标按钮占的宽度（含间距）。
pub const SLOT: f32 = 40.0;
/// 固定部分：拖动把手、分隔线、“断开”按钮、收起按钮、内边距。
pub const FIXED_WIDTH: f32 = 200.0;
/// 状态文字最多占这么宽（再长就截断）。
pub const STATUS_MAX: f32 = 260.0;

/// 展开时哪些按钮直接放在条上，哪些收进“更多”。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Arrangement {
    /// 直接放在条上的按钮（按顺序）。
    pub inline: Vec<Item>,
    /// “更多”菜单里的（按顺序）。
    pub menu: Vec<Item>,
}

/// 按可用宽度排按钮。快捷键、全屏和“更多”总是在条上；文件、声音放得下才放（文件比声音重要，先放）；
/// 原始大小、详情总在“更多”里（不常用，但要找得到）。
#[must_use]
pub fn arrange(avail_width: f32, status_width: f32) -> Arrangement {
    let mut used = FIXED_WIDTH + status_width.min(STATUS_MAX) + 3.0 * SLOT;
    let mut inline = vec![Item::Shortcuts, Item::Fullscreen];
    let mut menu = Vec::new();
    let mut overflow = Vec::new();
    for item in [Item::Files, Item::Sound] {
        if used + SLOT <= avail_width {
            used += SLOT;
            inline.push(item);
        } else {
            overflow.push(item);
        }
    }
    // 在条上的顺序固定：快捷键、声音、全屏、文件
    inline.sort_by_key(|i| match i {
        Item::Shortcuts => 0,
        Item::Sound => 1,
        Item::Fullscreen => 2,
        Item::Files => 3,
        Item::Original | Item::Details => 4,
    });
    // 放不下的排在菜单最前面，保持声音、文件的顺序
    overflow.sort_by_key(|i| u8::from(*i == Item::Files));
    menu.extend(overflow);
    menu.extend([Item::Original, Item::Details]);
    Arrangement { inline, menu }
}

/// 工具条的收起状态。
#[derive(Debug, Clone)]
pub struct Collapse {
    collapsed: bool,
    /// 还没被用户手动改过：可以自动收起。
    auto: bool,
    /// 最后一次“在用”的时刻（鼠标在条上、或还没连上）。
    active_at: Duration,
}

impl Default for Collapse {
    fn default() -> Self {
        Self {
            collapsed: false,
            auto: true,
            active_at: Duration::ZERO,
        }
    }
}

impl Collapse {
    /// 现在是收起的吗。
    #[must_use]
    pub const fn collapsed(&self) -> bool {
        self.collapsed
    }

    /// 每帧调用一次。`now` 是会话开始以来的时间，`live` 是否已连上，`hovered` 鼠标是否在工具条上。
    pub fn tick(&mut self, now: Duration, live: bool, hovered: bool) {
        if hovered || !live || self.collapsed {
            self.active_at = now;
        } else if self.auto && now.saturating_sub(self.active_at) >= AUTO_COLLAPSE {
            self.collapsed = true;
        }
    }

    /// 用户点了展开：之后不再自动收起（用户要它开着）。
    pub fn expand(&mut self, now: Duration) {
        self.collapsed = false;
        self.auto = false;
        self.active_at = now;
    }

    /// 用户点了收起。
    pub fn collapse(&mut self) {
        self.collapsed = true;
        self.auto = false;
    }

    /// 还要等多久才会自动收起（用来安排重绘）；不会自动收起时为 `None`。
    #[must_use]
    pub fn auto_collapse_in(&self, now: Duration) -> Option<Duration> {
        (!self.collapsed && self.auto)
            .then(|| AUTO_COLLAPSE.saturating_sub(now.saturating_sub(self.active_at)))
    }
}

/// 窗口太窄时不管用户怎么设，都只显示小条。
#[must_use]
pub fn show_collapsed(collapsed: bool, window_width: f32) -> bool {
    collapsed || window_width < MIN_EXPANDED_WIDTH
}

/// 网络变差时的说明：给用户看的原因和建议。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Advice {
    /// 差到什么程度。
    pub level: NetLevel,
    /// 一句话原因（“丢包 6%”）。
    pub short: String,
    /// 怎么办。
    pub hint: String,
}

/// 状态：一句话、颜色，网络差时带说明。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Status {
    /// 条上显示的字。
    pub text: String,
    /// 圆点和边框的颜色。
    pub signal: Signal,
    /// 网络差时的说明（有它就显示“怎么办”）。
    pub advice: Option<Advice>,
}

/// 工具条上的状态。`name` 是对方电脑名（老版本为空），`rtt` 是最近一秒的往返延迟（还没有数据时为 `None`）。
/// `with_name` 为真（展开的条）时写上电脑名；小条只写路径和延迟，越短越不挡画面。
#[must_use]
pub fn status(
    phase: &ViewPhase,
    net: &NetMeter,
    rtt: Option<u32>,
    name: &str,
    with_name: bool,
    s: &Strings,
) -> Status {
    let mut parts: Vec<String> = Vec::new();
    match phase {
        ViewPhase::Connecting => {
            return Status {
                text: s.connecting.to_owned(),
                signal: Signal::Idle,
                advice: None,
            }
        }
        ViewPhase::Reconnecting { .. } => {
            return Status {
                text: phase.headline(s),
                signal: Signal::Relay,
                advice: None,
            }
        }
        ViewPhase::Ended { status, .. } => {
            return Status {
                text: status.text.clone(),
                signal: if status.error {
                    Signal::Problem
                } else {
                    Signal::Idle
                },
                advice: None,
            }
        }
        ViewPhase::Connected { route, .. } => {
            if with_name && !name.is_empty() {
                parts.push(name.to_owned());
            }
            parts.push(route_name(Some(*route), s).to_owned());
        }
    }
    let (level, cause) = net.shown();
    match (level, cause) {
        (NetLevel::Good, _) | (_, None) => {
            if let Some(ms) = rtt {
                parts.push(format!("{ms} ms"));
            }
            Status {
                text: parts.join(" · "),
                signal: Signal::Direct,
                advice: None,
            }
        }
        (level, Some(cause)) => {
            let (short, hint) = net_texts(cause, s);
            parts.push(short.clone());
            Status {
                text: parts.join(" · "),
                signal: if level == NetLevel::Poor {
                    Signal::Problem
                } else {
                    Signal::Relay
                },
                advice: Some(Advice { level, short, hint }),
            }
        }
    }
}

/// “怎么办”面板上的一格：名称、数值，是不是问题所在。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Metric {
    /// 名称。
    pub label: &'static str,
    /// 数值（带单位）。
    pub value: String,
    /// 是造成网络差的那一项（标成警示色）。
    pub culprit: bool,
}

/// 面板上的四个数：延迟、丢包、码率、帧率。`fps` 是本机实际显示的帧率。
/// 标出问题所在的一项，和 [`classify`] 用同一个判断。
#[must_use]
pub fn metrics(q: &LinkQuality, fps: u32, s: &Strings) -> [Metric; 4] {
    let cause = classify(q).1;
    let loss = f64::from(q.loss_permille) / 10.0;
    [
        Metric {
            label: s.d_rtt,
            value: format!("{} ms", q.rtt_ms),
            culprit: matches!(cause, Some(NetCause::Latency(_))),
        },
        Metric {
            label: s.d_loss,
            value: format!("{loss:.1}%"),
            culprit: matches!(cause, Some(NetCause::Loss(_))),
        },
        Metric {
            label: s.d_recv,
            value: format!("{:.1} Mbps", f64::from(q.recv_kbps) / 1000.0),
            culprit: matches!(cause, Some(NetCause::Bandwidth(_))),
        },
        Metric {
            label: s.d_fps,
            value: fps.to_string(),
            culprit: false,
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gui::i18n::{EN, ZH};
    use crate::gui::state::Status as Note;
    use peerdesk_net::rendezvous::Path;

    fn connected(route: Path) -> ViewPhase {
        ViewPhase::Connected {
            peer: String::new(),
            remote: "127.0.0.1:1".parse().unwrap(),
            route,
            detail: String::new(),
        }
    }

    fn lossy(permille: u16) -> NetMeter {
        let mut meter = NetMeter::default();
        meter.push(&LinkQuality {
            loss_permille: permille,
            ..LinkQuality::default()
        });
        meter
    }

    #[test]
    fn wide_window_shows_all_main_buttons_and_the_menu_keeps_the_rest() {
        let a = arrange(1200.0, 160.0);
        assert_eq!(
            a.inline,
            [Item::Shortcuts, Item::Sound, Item::Fullscreen, Item::Files]
        );
        assert_eq!(a.menu, [Item::Original, Item::Details]);
    }

    #[test]
    fn narrow_window_moves_buttons_into_the_menu_files_last() {
        // 刚好放不下声音：先挪声音，文件还在条上
        let fixed = FIXED_WIDTH + 100.0 + 3.0 * SLOT;
        let a = arrange(fixed + SLOT, 100.0);
        assert_eq!(a.inline, [Item::Shortcuts, Item::Fullscreen, Item::Files]);
        assert_eq!(a.menu, [Item::Sound, Item::Original, Item::Details]);
        // 一个都放不下：快捷键和全屏仍在条上
        let a = arrange(0.0, 100.0);
        assert_eq!(a.inline, [Item::Shortcuts, Item::Fullscreen]);
        assert_eq!(
            a.menu,
            [Item::Sound, Item::Files, Item::Original, Item::Details]
        );
        // 每个功能恰好出现一次
        for width in [0.0, 400.0, 500.0, 600.0, 800.0, 2000.0] {
            let a = arrange(width, 150.0);
            let mut all: Vec<Item> = a.inline.iter().chain(&a.menu).copied().collect();
            all.sort_by_key(|i| format!("{i:?}"));
            all.dedup();
            assert_eq!(all.len(), 6, "{width}");
        }
        // 状态文字再长也只算 STATUS_MAX
        assert_eq!(arrange(900.0, 5000.0), arrange(900.0, STATUS_MAX));
    }

    #[test]
    fn collapses_by_itself_only_when_live_and_untouched() {
        let at = Duration::from_secs;
        let mut c = Collapse::default();
        assert!(!c.collapsed());
        // 还没连上：不收起
        c.tick(at(100), false, false);
        assert!(!c.collapsed());
        // 连上后鼠标在条上：不收起
        c.tick(at(101), true, true);
        c.tick(at(105), true, true);
        assert!(!c.collapsed());
        assert_eq!(c.auto_collapse_in(at(105)), Some(AUTO_COLLAPSE));
        // 鼠标离开，过了 AUTO_COLLAPSE 才收起
        c.tick(at(108), true, false);
        assert!(!c.collapsed());
        assert_eq!(c.auto_collapse_in(at(108)), Some(at(3)));
        c.tick(at(111), true, false);
        assert!(c.collapsed());
        assert_eq!(c.auto_collapse_in(at(111)), None);
        // 手动展开后不再自动收起
        c.expand(at(112));
        c.tick(at(200), true, false);
        assert!(!c.collapsed());
        assert_eq!(c.auto_collapse_in(at(200)), None);
        // 手动收起
        c.collapse();
        assert!(c.collapsed());
        c.tick(at(300), true, true);
        assert!(c.collapsed());
    }

    #[test]
    fn narrow_windows_only_show_the_small_bar() {
        assert!(show_collapsed(true, 2000.0));
        assert!(!show_collapsed(false, 1296.0));
        assert!(show_collapsed(false, MIN_EXPANDED_WIDTH - 1.0));
        assert!(!show_collapsed(false, MIN_EXPANDED_WIDTH));
    }

    #[test]
    fn status_says_route_delay_and_the_name_only_when_expanded() {
        let s = &ZH;
        let good = NetMeter::default();
        let direct = connected(Path::Direct);
        let st = status(&direct, &good, Some(18), "办公室 PC", false, s);
        assert_eq!(st.text, format!("{} · 18 ms", s.direct));
        assert_eq!((st.signal, st.advice), (Signal::Direct, None));
        let st = status(&direct, &good, Some(18), "办公室 PC", true, s);
        assert_eq!(st.text, format!("办公室 PC · {} · 18 ms", s.direct));
        // 老版本没有电脑名、还没有延迟数据
        let st = status(&connected(Path::Relay), &good, None, "", true, s);
        assert_eq!(st.text, s.relay);
    }

    #[test]
    fn status_explains_a_bad_network() {
        let s = &EN;
        let st = status(&connected(Path::Direct), &lossy(60), Some(40), "", false, s);
        assert_eq!(st.text, format!("{} · 6% loss", s.direct));
        assert_eq!(st.signal, Signal::Problem);
        let advice = st.advice.unwrap();
        assert_eq!(advice.level, NetLevel::Poor);
        assert_eq!(advice.short, "6% loss");
        assert_eq!(advice.hint, s.net_loss_hint);
        // 一般：琥珀色
        let st = status(&connected(Path::Direct), &lossy(20), None, "x", true, s);
        assert_eq!(st.signal, Signal::Relay);
        assert_eq!(st.advice.unwrap().level, NetLevel::Fair);
    }

    #[test]
    fn status_in_other_phases() {
        let s = &ZH;
        let net = NetMeter::default();
        let st = status(&ViewPhase::Connecting, &net, None, "", true, s);
        assert_eq!((st.text.as_str(), st.signal), (s.connecting, Signal::Idle));
        let again = ViewPhase::Reconnecting {
            attempt: 1,
            why: "网络断了".into(),
        };
        let st = status(&again, &net, None, "", true, s);
        assert_eq!((st.text, st.signal), (again.headline(s), Signal::Relay));
        let ended = |note| ViewPhase::Ended {
            status: note,
            summary: None,
            route: None,
            notice: None,
        };
        let st = status(&ended(Note::error("密码不对")), &net, None, "", true, s);
        assert_eq!((st.text.as_str(), st.signal), ("密码不对", Signal::Problem));
        let st = status(&ended(Note::info("已断开")), &net, None, "", true, s);
        assert_eq!(st.signal, Signal::Idle);
        assert!(st.advice.is_none());
    }

    #[test]
    fn metrics_mark_the_culprit() {
        let s = &ZH;
        let q = LinkQuality {
            rtt_ms: 42,
            loss_permille: 60,
            recv_kbps: 8200,
            ..LinkQuality::default()
        };
        let m = metrics(&q, 30, s);
        assert_eq!(m[0].value, "42 ms");
        assert_eq!(m[1].value, "6.0%");
        assert_eq!(m[2].value, "8.2 Mbps");
        assert_eq!(m[3].value, "30");
        assert_eq!(
            m.iter().map(|x| x.culprit).collect::<Vec<_>>(),
            [false, true, false, false]
        );
        let slow = LinkQuality {
            rtt_ms: 250,
            ..LinkQuality::default()
        };
        assert!(metrics(&slow, 30, s)[0].culprit);
        let full = LinkQuality {
            queue_ms: 300,
            ..LinkQuality::default()
        };
        assert!(metrics(&full, 30, s)[2].culprit);
        let fine = metrics(&LinkQuality::default(), 60, s);
        assert!(fine.iter().all(|x| !x.culprit));
    }
}
