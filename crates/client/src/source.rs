//! 被控端的画面来源和输入去向：本机屏幕和键鼠（平台层），或者合成的演示桌面。

use std::io::Write;

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
}

impl Sources {
    /// 决定用本机屏幕还是演示桌面。指定了 `screen` 而屏幕打不开时报错；
    /// 没有指定时改用演示桌面并给出警告。
    pub(crate) fn choose(
        kind: SourceKind,
        opts: CaptureOptions,
        opener: Opener,
        out: &mut dyn Write,
    ) -> Result<Self, String> {
        let screen = match kind {
            SourceKind::Demo => false,
            SourceKind::Screen | SourceKind::Auto => match (opener.screen)(opts) {
                Ok(_) => true,
                Err(e) if kind == SourceKind::Auto => {
                    let _ = writeln!(out, "警告：无法采集屏幕（{e}），改为提供演示桌面。");
                    false
                }
                Err(e) => return Err(format!("无法采集屏幕：{e}")),
            },
        };
        Ok(Self {
            opts,
            opener,
            screen,
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

    /// 为一次会话打开画面来源和输入去向。`fallback` 是不注入、只计数的输入去向：
    /// 用演示桌面时用它（不能让对方控制本机却看着假画面），键鼠打不开时也用它（只能观看）。
    pub(crate) fn open(
        &self,
        fallback: Box<dyn InputSink>,
        out: &mut dyn Write,
    ) -> (Box<dyn ScreenSource>, Box<dyn InputSink>) {
        let demo: Box<dyn ScreenSource> = Box::new(DemoDesktop::new(1280, 720, 1));
        if !self.screen {
            return (demo, fallback);
        }
        let source = match (self.opener.screen)(self.opts) {
            Ok(source) => source,
            Err(e) => {
                let _ = writeln!(out, "警告：无法采集屏幕（{e}），这次会话提供演示桌面。");
                return (demo, fallback);
            }
        };
        let sink = (self.opener.input)(self.opts.display).unwrap_or_else(|e| {
            let _ = writeln!(out, "警告：无法控制键鼠（{e}），这次会话只能观看。");
            fallback
        });
        (source, sink)
    }
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

    fn fallback() -> Box<dyn InputSink> {
        Box::new(Tagged(&FALLBACK))
    }

    /// 打开一次会话，注入一个事件，返回画面宽度和输出。
    fn session(sources: &Sources) -> (u16, String) {
        let mut out = Vec::new();
        let (mut source, mut sink) = sources.open(fallback(), &mut out);
        sink.inject(&InputEvent::ReleaseAll);
        let width = source.capture().unwrap().width();
        (width, String::from_utf8(out).unwrap())
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
        let mut out = Vec::new();
        // 演示桌面：不碰屏幕和键鼠
        let demo = Sources::choose(SourceKind::Demo, OPTS, ok, &mut out).unwrap();
        assert_eq!(demo.describe(), "演示桌面");
        let before = FALLBACK.load(Ordering::Relaxed);
        assert_eq!(session(&demo).0, 1280);
        assert!(FALLBACK.load(Ordering::Relaxed) > before);
        // 屏幕能用
        for kind in [SourceKind::Auto, SourceKind::Screen] {
            let s = Sources::choose(kind, OPTS, ok, &mut out).unwrap();
            assert_eq!(s.describe(), "本机屏幕");
            let before = REAL.load(Ordering::Relaxed);
            let (width, text) = session(&s);
            assert_eq!(width, 320);
            assert!(text.is_empty());
            assert!(REAL.load(Ordering::Relaxed) > before);
        }
        assert!(out.is_empty());
    }

    #[test]
    fn falls_back_when_the_screen_is_unavailable() {
        let broken = Opener {
            screen: screen_fails,
            input: input_ok,
        };
        let mut out = Vec::new();
        let err = Sources::choose(SourceKind::Screen, OPTS, broken, &mut out).unwrap_err();
        assert!(err.contains("没有桌面"), "{err}");
        let s = Sources::choose(SourceKind::Auto, OPTS, broken, &mut out).unwrap();
        assert_eq!(s.describe(), "演示桌面");
        assert!(String::from_utf8(out)
            .unwrap()
            .contains("警告：无法采集屏幕"));

        // 选的时候屏幕还在，会话开始时不在了：这次会话用演示桌面
        let flaky = Opener {
            screen: screen_once,
            input: input_ok,
        };
        let s = Sources::choose(SourceKind::Screen, OPTS, flaky, &mut Vec::new()).unwrap();
        let (width, text) = session(&s);
        assert_eq!(width, 1280);
        assert!(text.contains("这次会话提供演示桌面"), "{text}");

        // 键鼠打不开：只能观看
        let view_only = Opener {
            screen: screen_ok,
            input: input_fails,
        };
        let s = Sources::choose(SourceKind::Screen, OPTS, view_only, &mut Vec::new()).unwrap();
        let (width, text) = session(&s);
        assert_eq!(width, 320);
        assert!(text.contains("只能观看"), "{text}");
    }
}
