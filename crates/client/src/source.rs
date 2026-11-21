//! 被控端的画面来源和输入去向：本机屏幕和键鼠（平台层），或者合成的演示桌面。

use peerdesk_net::demo::DemoDesktop;
use peerdesk_net::host::{InputSink, ScreenSource};
use peerdesk_platform::{CaptureOptions, PlatformError};

/// `--source` 指定的画面来源。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SourceKind {
    /// 没有指定：先试本机屏幕，打不开时用演示桌面。
    Auto,
    /// 本机屏幕和键鼠。
    Screen,
    /// 合成的演示桌面，不碰本机键鼠。
    Demo,
}

impl SourceKind {
    /// 解析 `--source` 的值。
    pub(crate) fn parse(value: Option<&String>) -> Result<Self, String> {
        match value.map(String::as_str) {
            Some("screen") => Ok(Self::Screen),
            Some("demo") => Ok(Self::Demo),
            Some(other) => Err(format!("未知的画面来源：{other}（screen 或 demo）")),
            None => Err("--source 需要一个值".into()),
        }
    }
}

/// 一次会话的画面来源和输入去向。
pub(crate) type Opened = (Box<dyn ScreenSource>, Box<dyn InputSink>);

type OpenScreen = fn(CaptureOptions) -> Result<Box<dyn ScreenSource>, PlatformError>;
type OpenInput = fn(usize) -> Result<Box<dyn InputSink>, PlatformError>;

/// 打开本机屏幕和键鼠的方式；测试时换成假的。
#[derive(Debug, Clone, Copy)]
pub(crate) struct Opener {
    screen: OpenScreen,
    input: OpenInput,
}

impl Opener {
    /// 真实的平台层。
    pub(crate) const SYSTEM: Self = Self {
        screen: peerdesk_platform::open_screen,
        input: peerdesk_platform::open_input_on,
    };
}

/// 选定的画面来源。屏幕只在会话期间采集，没人连着时不占资源。
#[derive(Debug, Clone, Copy)]
pub(crate) struct Sources {
    opts: CaptureOptions,
    opener: Opener,
    screen: bool,
    need_input: bool,
}

impl Sources {
    /// 决定画面来源。默认（`Auto`）和 `Screen` 一样要求本机屏幕；`need_input` 时还要求能控制键鼠。
    /// 打不开就报错：没有权限就不提供远程，绝不拿演示画面顶替真实屏幕。
    /// 演示桌面只在明确指定 `--source demo` 时使用（测试、调试）。
    pub(crate) fn choose(
        kind: SourceKind,
        opts: CaptureOptions,
        opener: Opener,
        need_input: bool,
    ) -> Result<Self, String> {
        let screen = match kind {
            SourceKind::Demo => false,
            SourceKind::Screen | SourceKind::Auto => {
                check(opener, opts, need_input)?;
                true
            }
        };
        Ok(Self {
            opts,
            opener,
            screen,
            need_input,
        })
    }

    /// 给用户看的名字。
    pub(crate) const fn describe(&self) -> &'static str {
        if self.screen {
            "本机屏幕"
        } else {
            "演示桌面"
        }
    }

    /// 为一次会话打开画面来源和输入去向。`record` 是不注入、只计数的输入去向：
    /// 用演示桌面时、或者被控端设置了不允许输入时用它（不能让对方控制本机却看着假画面）。
    /// 会话开始时屏幕或键鼠打不开了（例如权限被撤销）：报错，这次不提供远程。
    pub(crate) fn open(&self, record: Box<dyn InputSink>) -> Result<Opened, String> {
        if !self.screen {
            return Ok((Box::new(DemoDesktop::new(1280, 720, 1)), record));
        }
        let source = (self.opener.screen)(self.opts).map_err(|e| format!("无法采集屏幕：{e}"))?;
        let sink = if self.need_input {
            (self.opener.input)(self.opts.display).map_err(|e| format!("无法控制键鼠：{e}"))?
        } else {
            record
        };
        Ok((source, sink))
    }
}

/// 检查屏幕和键鼠能不能打开（打开后马上关掉）。
fn check(opener: Opener, opts: CaptureOptions, need_input: bool) -> Result<(), String> {
    drop((opener.screen)(opts).map_err(|e| format!("无法采集屏幕：{e}"))?);
    if need_input {
        drop((opener.input)(opts.display).map_err(|e| format!("无法控制键鼠：{e}"))?);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use peerdesk_core::input::InputEvent;
    use std::sync::atomic::{AtomicU32, Ordering};

    /// 记下注入了几次的输入去向。
    struct Tagged(&'static AtomicU32);

    impl InputSink for Tagged {
        fn inject(&mut self, _event: &InputEvent) {
            self.0.fetch_add(1, Ordering::Relaxed);
        }
    }

    static REAL: AtomicU32 = AtomicU32::new(0);
    static FALLBACK: AtomicU32 = AtomicU32::new(0);
    static OPENED: AtomicU32 = AtomicU32::new(0);

    fn screen_ok(_: CaptureOptions) -> Result<Box<dyn ScreenSource>, PlatformError> {
        Ok(Box::new(DemoDesktop::new(320, 200, 1)))
    }
    fn screen_fails(_: CaptureOptions) -> Result<Box<dyn ScreenSource>, PlatformError> {
        Err(PlatformError::Unsupported("没有桌面".into()))
    }
    /// 第一次（选择来源时）成功，之后失败：会话开始时屏幕不见了。
    fn screen_once(opts: CaptureOptions) -> Result<Box<dyn ScreenSource>, PlatformError> {
        if OPENED.fetch_add(1, Ordering::Relaxed) == 0 {
            screen_ok(opts)
        } else {
            screen_fails(opts)
        }
    }
    fn input_ok(_: usize) -> Result<Box<dyn InputSink>, PlatformError> {
        Ok(Box::new(Tagged(&REAL)))
    }
    fn input_fails(_: usize) -> Result<Box<dyn InputSink>, PlatformError> {
        Err(PlatformError::Permission("辅助功能".into()))
    }

    const OPTS: CaptureOptions = CaptureOptions {
        display: 0,
        max_fps: 30,
    };

    fn record() -> Box<dyn InputSink> {
        Box::new(Tagged(&FALLBACK))
    }

    /// 打开一次会话，注入一个事件，返回画面宽度。
    fn session(sources: &Sources) -> Result<u16, String> {
        let (mut source, mut sink) = sources.open(record())?;
        sink.inject(&InputEvent::ReleaseAll);
        Ok(source.capture().unwrap().width())
    }

    #[test]
    fn parses_the_flag() {
        let v = |s: &str| Some(s.to_owned());
        assert_eq!(
            SourceKind::parse(v("screen").as_ref()),
            Ok(SourceKind::Screen)
        );
        assert_eq!(SourceKind::parse(v("demo").as_ref()), Ok(SourceKind::Demo));
        assert!(SourceKind::parse(v("camera").as_ref()).is_err());
        assert!(SourceKind::parse(None).is_err());
    }

    #[test]
    fn chooses_and_opens_sources() {
        let ok = Opener {
            screen: screen_ok,
            input: input_ok,
        };
        // 演示桌面：只在明确指定时用，不碰屏幕和键鼠
        let demo = Sources::choose(SourceKind::Demo, OPTS, ok, true).unwrap();
        assert_eq!(demo.describe(), "演示桌面");
        let before = FALLBACK.load(Ordering::Relaxed);
        assert_eq!(session(&demo), Ok(1280));
        assert!(FALLBACK.load(Ordering::Relaxed) > before);
        // 屏幕和键鼠都能用
        for kind in [SourceKind::Auto, SourceKind::Screen] {
            let s = Sources::choose(kind, OPTS, ok, true).unwrap();
            assert_eq!(s.describe(), "本机屏幕");
            let before = REAL.load(Ordering::Relaxed);
            assert_eq!(session(&s), Ok(320));
            assert!(REAL.load(Ordering::Relaxed) > before);
        }
        // 被控端不允许输入：不需要键鼠权限，输入只记录
        let view = Opener {
            screen: screen_ok,
            input: input_fails,
        };
        let s = Sources::choose(SourceKind::Auto, OPTS, view, false).unwrap();
        let before = FALLBACK.load(Ordering::Relaxed);
        assert_eq!(session(&s), Ok(320));
        assert!(FALLBACK.load(Ordering::Relaxed) > before);
    }

    #[test]
    fn refuses_without_permission_instead_of_showing_a_demo() {
        let no_screen = Opener {
            screen: screen_fails,
            input: input_ok,
        };
        for kind in [SourceKind::Auto, SourceKind::Screen] {
            let err = Sources::choose(kind, OPTS, no_screen, true).unwrap_err();
            assert!(
                err.contains("无法采集屏幕") && err.contains("没有桌面"),
                "{err}"
            );
        }
        let no_input = Opener {
            screen: screen_ok,
            input: input_fails,
        };
        let err = Sources::choose(SourceKind::Auto, OPTS, no_input, true).unwrap_err();
        assert!(err.contains("无法控制键鼠"), "{err}");

        // 选的时候屏幕还在，会话开始时不在了：这次会话拒绝
        let flaky = Opener {
            screen: screen_once,
            input: input_ok,
        };
        let s = Sources::choose(SourceKind::Screen, OPTS, flaky, true).unwrap();
        assert!(session(&s).unwrap_err().contains("无法采集屏幕"));
        // 会话开始时键鼠不在了：也拒绝
        let s = Sources {
            opts: OPTS,
            opener: no_input,
            screen: true,
            need_input: true,
        };
        assert!(session(&s).unwrap_err().contains("无法控制键鼠"));
    }
}
