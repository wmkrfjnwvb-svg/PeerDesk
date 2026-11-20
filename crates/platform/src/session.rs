//! Linux 图形会话的判断：只支持 X11，Wayland 给出明确的错误。
//!
//! 在 Wayland 桌面里，XWayland 也提供 `DISPLAY`，但通过它只能看到、操作 X11 程序，
//! 原生 Wayland 窗口是黑的、收不到注入的输入。与其给出一个看似能用、实际一片黑的画面，不如直接报错。

use crate::PlatformError;

/// 图形会话相关的环境变量。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SessionEnv {
    /// `DISPLAY`
    pub display: Option<String>,
    /// `WAYLAND_DISPLAY`
    pub wayland_display: Option<String>,
    /// `XDG_SESSION_TYPE`
    pub session_type: Option<String>,
}

impl SessionEnv {
    /// 读取当前进程的环境变量。空值当作没有设置。
    #[must_use]
    pub fn current() -> Self {
        let var = |name: &str| std::env::var(name).ok().filter(|v| !v.is_empty());
        Self {
            display: var("DISPLAY"),
            wayland_display: var("WAYLAND_DISPLAY"),
            session_type: var("XDG_SESSION_TYPE"),
        }
    }

    /// 能否使用 X11。不能时说明原因。
    pub fn check_x11(&self) -> Result<(), PlatformError> {
        let wayland = self.wayland_display.is_some()
            || self
                .session_type
                .as_deref()
                .is_some_and(|t| t.eq_ignore_ascii_case("wayland"));
        if wayland {
            return Err(PlatformError::Unsupported(
                "这是 Wayland 桌面，暂时只支持 X11。请在登录界面选择 X11（Xorg）会话后再试".into(),
            ));
        }
        if self.display.is_none() {
            return Err(PlatformError::Unsupported(
                "没有图形界面：环境变量 DISPLAY 没有设置（需要 X11 桌面会话）".into(),
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(display: Option<&str>, wayland: Option<&str>, kind: Option<&str>) -> SessionEnv {
        SessionEnv {
            display: display.map(Into::into),
            wayland_display: wayland.map(Into::into),
            session_type: kind.map(Into::into),
        }
    }

    #[test]
    fn x11_sessions_are_accepted() {
        assert_eq!(env(Some(":0"), None, None).check_x11(), Ok(()));
        assert_eq!(env(Some(":0"), None, Some("x11")).check_x11(), Ok(()));
        // 只要能读环境变量就不会出错；结果取决于运行环境。
        let _ = SessionEnv::current().check_x11();
    }

    #[test]
    fn wayland_is_rejected_clearly() {
        for e in [
            env(Some(":0"), Some("wayland-0"), None),
            env(Some(":0"), None, Some("Wayland")),
            env(None, Some("wayland-0"), Some("wayland")),
        ] {
            let err = e.check_x11().unwrap_err();
            assert!(err.to_string().contains("Wayland"), "{err}");
        }
    }

    #[test]
    fn missing_display_is_explained() {
        let err = env(None, None, Some("tty")).check_x11().unwrap_err();
        assert!(err.to_string().contains("DISPLAY"), "{err}");
    }
}
