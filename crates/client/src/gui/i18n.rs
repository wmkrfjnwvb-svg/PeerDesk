//! 界面语言：系统语言是中文时用中文，否则用英文。

/// 界面语言。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lang {
    /// 中文。
    Zh,
    /// 英文。
    En,
}

/// 按区域名选语言：`zh`、`zh-CN`、`zh_TW.UTF-8` 之类是中文，其余（包括取不到）是英文。
#[must_use]
pub fn lang_for(locale: Option<&str>) -> Lang {
    let primary = locale
        .unwrap_or_default()
        .split(['-', '_', '.', '@'])
        .next()
        .unwrap_or_default();
    if primary.eq_ignore_ascii_case("zh") {
        Lang::Zh
    } else {
        Lang::En
    }
}

impl Lang {
    /// 存盘用的代号。
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::Zh => "zh",
            Self::En => "en",
        }
    }

    /// 这种语言的用户是否优先走 IPv6 直连：英文（海外，IPv6 普及、质量好）是；
    /// 中文（国内跨运营商 IPv6 常常比 IPv4 差）只比延迟。
    #[must_use]
    pub const fn prefers_ipv6(self) -> bool {
        matches!(self, Self::En)
    }
}

/// 用户在界面里选的语言存在数据目录的这个文件里（`zh` 或 `en`）；没有时跟随系统。
pub const LANG_FILE: &str = "lang.txt";

/// 用户选过的语言；没选过（或文件读不懂）时为 `None`。
#[must_use]
pub fn saved_lang(dir: &std::path::Path) -> Option<Lang> {
    match std::fs::read_to_string(dir.join(LANG_FILE)).ok()?.trim() {
        "zh" => Some(Lang::Zh),
        "en" => Some(Lang::En),
        _ => None,
    }
}

/// 记住用户选的语言。
pub fn save_lang(dir: &std::path::Path, lang: Lang) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    std::fs::write(dir.join(LANG_FILE), lang.code())
}

/// 实际使用的语言：`PEERDESK_LANG` 环境变量优先，其次用户选过的，最后跟随系统。
#[must_use]
pub fn effective_lang(dir: &std::path::Path) -> Lang {
    let forced = std::env::var("PEERDESK_LANG")
        .ok()
        .filter(|v| !v.is_empty());
    match forced {
        Some(f) => lang_for(Some(&f)),
        None => saved_lang(dir).unwrap_or_else(system_lang),
    }
}

/// 当前系统的界面语言。`PEERDESK_LANG` 可以覆盖（例如 `en`、`zh`）。
#[must_use]
pub fn system_lang() -> Lang {
    let forced = std::env::var("PEERDESK_LANG")
        .ok()
        .filter(|v| !v.is_empty());
    lang_for(forced.or_else(sys_locale::get_locale).as_deref())
}

/// 这种语言的全部界面文字。
#[must_use]
pub const fn strings(lang: Lang) -> &'static Strings {
    match lang {
        Lang::Zh => &ZH,
        Lang::En => &EN,
    }
}

/// 界面文字。
#[allow(missing_docs)]
#[derive(Debug)]
pub struct Strings {
    pub app_title: &'static str,
    pub this_device: &'static str,
    pub allow_control: &'static str,
    pub port: &'static str,
    pub addresses: &'static str,
    pub no_addresses: &'static str,
    pub refresh: &'static str,
    pub copy: &'static str,
    pub copied: &'static str,
    pub fingerprint: &'static str,
    pub password: &'static str,
    pub new_password: &'static str,
    pub password_hint: &'static str,
    pub host_off: &'static str,
    pub host_starting: &'static str,
    pub host_listening: &'static str,
    pub host_handshaking: &'static str,
    pub host_controlled: &'static str,
    /// 屏幕或键鼠打不开、不能开启被控时的提示（后面接原因）。
    pub host_unavailable: &'static str,
    pub host_stopping: &'static str,
    pub controller: &'static str,
    pub disconnect: &'static str,
    pub inputs_received: &'static str,
    pub register_server: &'static str,
    pub server_addr: &'static str,
    pub server_pin: &'static str,
    pub registered: &'static str,
    pub connect_title: &'static str,
    pub remote_addr: &'static str,
    pub pin: &'static str,
    pub connect: &'static str,
    pub recent: &'static str,
    pub via_server: &'static str,
    pub device: &'static str,
    pub connect_self: &'static str,
    pub ready: &'static str,
    pub connecting: &'static str,
    pub connected: &'static str,
    pub session_ended: &'static str,
    pub you_disconnected: &'static str,
    pub stats: &'static str,
    pub fps: &'static str,
    pub bitrate: &'static str,
    pub rtt: &'static str,
    pub latency: &'static str,
    pub path: &'static str,
    pub resolution: &'static str,
    pub direct: &'static str,
    pub relay: &'static str,
    pub not_available: &'static str,
    pub live_stats_pending: &'static str,
    pub waiting_frame: &'static str,
    pub close: &'static str,
    pub fps_limit: &'static str,
    pub err_bad_port: &'static str,
    pub err_bad_addr: &'static str,
    pub err_bad_pin: &'static str,
    pub err_bad_server: &'static str,
    pub err_need_target: &'static str,
    /// 用设备码连接但没填服务器。
    pub err_need_server: &'static str,
    /// 设备码。
    pub device_code: &'static str,
    /// 保存。
    pub save: &'static str,
    /// 已保存。
    pub saved: &'static str,
    /// 打开日志文件夹。
    pub open_logs: &'static str,
    /// 网络体检。
    pub network_check: &'static str,
    /// 网络重连中。
    pub reconnecting: &'static str,
    /// 免费额度用完的弹框标题。
    pub busy_title: &'static str,
    /// 升级会员按钮。
    pub upgrade: &'static str,
    /// 升级说明。
    pub upgrade_hint: &'static str,
    /// 会员令牌。
    pub account_token: &'static str,
    /// 本机复制的内容太大没有同步（`{mb}` 换成大小）。
    pub clip_local_too_large: &'static str,
    /// 对方复制的内容太大没有同步（`{mb}` 换成大小）。
    pub clip_remote_too_large: &'static str,
    /// 复制了文件但对方版本不支持。
    pub clip_files_unsupported: &'static str,
    /// 助手：助手。
    pub assistant: &'static str,
    /// 助手：开始菜单。
    pub sc_start: &'static str,
    /// 助手：切换程序。
    pub sc_switch_app: &'static str,
    /// 助手：关闭窗口。
    pub sc_close_window: &'static str,
    /// 助手：任务管理器。
    pub sc_task_manager: &'static str,
    /// 助手：锁屏。
    pub sc_lock: &'static str,
    /// 助手：显示桌面。
    pub sc_show_desktop: &'static str,
    /// 助手：截图。
    pub sc_screenshot: &'static str,
    /// 助手：复制。
    pub sc_copy: &'static str,
    /// 助手：粘贴。
    pub sc_paste: &'static str,
    /// 助手：全选。
    pub sc_select_all: &'static str,
    /// 助手：撤销。
    pub sc_undo: &'static str,
    /// 助手：未知（按 Windows 发送）。
    pub os_unknown: &'static str,
    /// 助手：对方系统。
    pub remote_os: &'static str,
    /// 助手：声音。
    pub sound: &'static str,
    /// 助手：剪贴板同步。
    pub clipboard_sync: &'static str,
    /// 助手：全屏。
    pub fullscreen: &'static str,
    /// 助手：对方系统没有这个操作。
    pub not_on_this_os: &'static str,
    pub err_port_in_use: &'static str,
    pub err_timeout: &'static str,
    pub err_refused: &'static str,
    pub err_wrong_password: &'static str,
    pub err_locked: &'static str,
    pub err_busy: &'static str,
    pub err_pin_mismatch: &'static str,
    pub err_offline: &'static str,
    pub err_closed: &'static str,
    pub err_identity: &'static str,
    pub err_other: &'static str,
    pub warn_unpinned: &'static str,
}

/// 中文。
pub const ZH: Strings = Strings {
    app_title: "PeerDesk 测试版",
    this_device: "本机",
    allow_control: "允许远程控制",
    port: "端口",
    addresses: "本机地址",
    no_addresses: "没有找到局域网地址",
    refresh: "刷新",
    copy: "复制",
    copied: "已复制",
    fingerprint: "设备指纹",
    password: "密码",
    new_password: "换一个",
    password_hint: "把地址和密码告诉控制端。每次启动都会换新密码。",
    host_off: "未开启",
    host_starting: "正在开启…",
    host_listening: "等待连接",
    host_handshaking: "对方正在验证",
    host_controlled: "正在被控制",
    host_unavailable: "不能开启远程控制：没有采集屏幕或控制键鼠的权限。macOS 请在“系统设置 → 隐私与安全性”里给 PeerDesk 打开“屏幕录制”和“辅助功能”，然后重新打开 PeerDesk",
    host_stopping: "正在关闭…",
    controller: "控制端",
    disconnect: "断开",
    inputs_received: "收到的输入",
    register_server: "服务器（用设备码连接）",
    server_addr: "服务器地址:端口",
    server_pin: "服务器指纹（可选）",
    registered: "已在服务器登记，设备码",
    connect_title: "连接",
    remote_addr: "对方设备码或地址",
    pin: "核对指纹（可选）",
    connect: "连接",
    recent: "最近连接",
    via_server: "填一次就行。本机会在服务器上登记、得到设备码；连接别人时也用它按设备码查找。",
    device: "被控端指纹",
    connect_self: "连接本机（自测）",
    ready: "就绪",
    connecting: "正在连接…",
    connected: "已连接",
    session_ended: "会话已结束",
    you_disconnected: "你断开了连接",
    stats: "统计 (F12)",
    fps: "帧率",
    bitrate: "码率",
    rtt: "往返延迟",
    latency: "画面延迟",
    path: "路径",
    resolution: "分辨率",
    direct: "直连",
    relay: "中继",
    not_available: "暂无",
    live_stats_pending: "会话结束时给出",
    waiting_frame: "等待第一帧画面…",
    close: "关闭",
    fps_limit: "帧率上限",
    err_bad_port: "端口必须是 1 到 65535 之间的数字",
    err_bad_addr: "地址的格式应当是 IP:端口，例如 192.168.1.20:4433",
    err_bad_pin: "指纹应当是 64 个十六进制字符（可以从对方界面上复制）",
    err_bad_server: "服务器地址的格式应当是 IP:端口",
    err_need_target: "请填写对方的设备码或地址",
    err_need_server: "用设备码连接，需要先在“服务器”里填写服务器地址",
    device_code: "设备码",
    save: "保存",
    saved: "服务器设置已保存",
    open_logs: "打开日志文件夹",
    network_check: "网络体检（NAT、路由器、服务器、IPv6、端口映射）",
    reconnecting: "网络重连中，正在切换到其他通道",
    busy_title: "免费服务器拥挤",
    upgrade: "升级会员，走 VIP 通道",
    upgrade_hint: "会员走付费集群的专用通道，不受免费额度限制。会员购买即将开放；已有会员令牌的，在窗口下方“服务器”里填写“会员令牌”。",
    account_token: "会员令牌（可选）",
    clip_local_too_large: "复制的内容太大（{mb} MB），没有同步到对方；文件超过 20 MB 请用文件传输",
    clip_files_unsupported: "对方的 PeerDesk 版本不支持复制粘贴文件，请升级对方，或用文件传输",
    clip_remote_too_large: "对方复制的内容太大（{mb} MB），没有同步过来；文件超过 20 MB 请用文件传输",
    assistant: "助手",
    sc_start: "开始菜单",
    sc_switch_app: "切换程序",
    sc_close_window: "关闭窗口",
    sc_task_manager: "任务管理器",
    sc_lock: "锁屏",
    sc_show_desktop: "显示桌面",
    sc_screenshot: "截图",
    sc_copy: "复制",
    sc_paste: "粘贴",
    sc_select_all: "全选",
    sc_undo: "撤销",
    os_unknown: "未知（按 Windows 发送）",
    remote_os: "对方系统",
    sound: "声音",
    clipboard_sync: "剪贴板同步",
    fullscreen: "全屏",
    not_on_this_os: "对方系统没有这个操作",
    err_port_in_use: "端口已被占用（也许另一个 PeerDesk 正在运行），换一个端口试试",
    err_timeout: "对方没有响应。请检查地址和端口、防火墙，以及对方是否打开了“允许远程控制”",
    err_refused: "连接失败",
    err_wrong_password: "密码不对",
    err_locked: "对方因为多次密码错误暂时锁定了，请稍后再试",
    err_busy: "对方正在被其他设备控制",
    err_pin_mismatch: "对方的指纹和填写的不一致：可能连错了设备，也可能有人冒充",
    err_offline: "这台设备现在不在服务器上",
    err_closed: "连接已断开",
    err_identity: "无法读取或创建本机身份",
    err_other: "出错了",
    warn_unpinned: "没有核对指纹：无法确认连到的是你的设备",
};

/// 英文。
pub const EN: Strings = Strings {
    app_title: "PeerDesk (test build)",
    this_device: "This device",
    allow_control: "Allow remote control",
    port: "Port",
    addresses: "Addresses",
    no_addresses: "No LAN address found",
    refresh: "Refresh",
    copy: "Copy",
    copied: "Copied",
    fingerprint: "Fingerprint",
    password: "Password",
    new_password: "New",
    password_hint: "Give an address and the password to the controller. \
                    A new password is generated on every launch.",
    host_off: "Off",
    host_starting: "Starting…",
    host_listening: "Waiting for a connection",
    host_handshaking: "Verifying the controller",
    host_controlled: "Being controlled",
    host_unavailable: "Remote control is not available: PeerDesk may not capture the screen or control the keyboard and mouse. On macOS, allow PeerDesk under System Settings → Privacy & Security → Screen Recording and Accessibility, then reopen PeerDesk",
    host_stopping: "Stopping…",
    controller: "Controller",
    disconnect: "Disconnect",
    inputs_received: "Input received",
    register_server: "Server (connect by device code)",
    server_addr: "Server address:port",
    server_pin: "Server fingerprint (optional)",
    registered: "Registered on the server, device code",
    connect_title: "Connect",
    remote_addr: "Device code or address",
    pin: "Pin fingerprint (optional)",
    connect: "Connect",
    recent: "Recent",
    via_server: "Set once. This device registers on the server and gets a device code; it is also used to look up others by code.",
    device: "Device fingerprint",
    connect_self: "Connect to this device (self-test)",
    ready: "Ready",
    connecting: "Connecting…",
    connected: "Connected",
    session_ended: "Session ended",
    you_disconnected: "You disconnected",
    stats: "Stats (F12)",
    fps: "fps",
    bitrate: "bitrate",
    rtt: "rtt",
    latency: "frame latency",
    path: "path",
    resolution: "resolution",
    direct: "direct",
    relay: "relay",
    not_available: "n/a",
    live_stats_pending: "shown when the session ends",
    waiting_frame: "Waiting for the first frame…",
    close: "Close",
    fps_limit: "Max fps",
    err_bad_port: "The port must be a number from 1 to 65535",
    err_bad_addr: "Use the form IP:port, for example 192.168.1.20:4433",
    err_bad_pin: "A fingerprint is 64 hex characters (copy it from the other device)",
    err_bad_server: "Use the form IP:port for the server",
    err_need_target: "Enter the other device's code or address",
    err_need_server: "Connecting by device code needs a server; fill it in under Server",
    device_code: "Device code",
    save: "Save",
    saved: "Server settings saved",
    open_logs: "Open log folder",
    network_check: "Network check (NAT, router, server, IPv6, port mapping)",
    reconnecting: "Network lost, reconnecting through another channel",
    busy_title: "Free servers are busy",
    upgrade: "Upgrade to VIP",
    upgrade_hint: "VIP members use dedicated paid relays without the free-tier limit. Membership is coming soon; if you already have a member token, enter it under Server below.",
    account_token: "Member token (optional)",
    clip_local_too_large: "Copied content is too large ({mb} MB) to sync; use file transfer for files over 20 MB",
    clip_files_unsupported: "The remote PeerDesk is too old to copy and paste files; update it, or use file transfer",
    clip_remote_too_large: "Content copied on the remote ({mb} MB) is too large to sync; use file transfer for files over 20 MB",
    assistant: "Assistant",
    sc_start: "Start menu",
    sc_switch_app: "Switch app",
    sc_close_window: "Close window",
    sc_task_manager: "Task manager",
    sc_lock: "Lock screen",
    sc_show_desktop: "Show desktop",
    sc_screenshot: "Screenshot",
    sc_copy: "Copy",
    sc_paste: "Paste",
    sc_select_all: "Select all",
    sc_undo: "Undo",
    os_unknown: "unknown (sent as Windows)",
    remote_os: "Remote system",
    sound: "Sound",
    clipboard_sync: "Clipboard sync",
    fullscreen: "Full screen",
    not_on_this_os: "Not available on the remote system",
    err_port_in_use:
        "The port is already in use (maybe another PeerDesk is running). Try another port",
    err_timeout: "No response. Check the address, the port, the firewall, and that \
                  \"Allow remote control\" is on at the other side",
    err_refused: "Could not connect",
    err_wrong_password: "Wrong password",
    err_locked: "The other device is locked for a while after too many wrong passwords",
    err_busy: "The other device is being controlled by someone else",
    err_pin_mismatch:
        "The fingerprint does not match: wrong device, or someone is impersonating it",
    err_offline: "That device is not online on the server",
    err_closed: "The connection was closed",
    err_identity: "Cannot read or create this device's identity",
    err_other: "Something went wrong",
    warn_unpinned: "Fingerprint not pinned: cannot confirm this is your device",
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn locale_selection() {
        for zh in ["zh", "zh-CN", "zh_TW.UTF-8", "ZH-hans", "zh@pinyin"] {
            assert_eq!(lang_for(Some(zh)), Lang::Zh, "{zh}");
        }
        for en in ["en-US", "de_DE.UTF-8", "", "C", "POSIX", "zu"] {
            assert_eq!(lang_for(Some(en)), Lang::En, "{en}");
        }
        assert_eq!(lang_for(None), Lang::En);
        assert_eq!(strings(Lang::Zh).this_device, "本机");
        assert_eq!(strings(Lang::En).this_device, "This device");
    }

    #[test]
    fn environment_overrides_the_system() {
        // 只有这个测试改 PEERDESK_LANG。
        let dir = std::env::temp_dir().join(format!("peerdesk-lang-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::env::set_var("PEERDESK_LANG", "zh_CN");
        assert_eq!(system_lang(), Lang::Zh);
        save_lang(&dir, Lang::En).unwrap();
        assert_eq!(effective_lang(&dir), Lang::Zh, "环境变量优先于用户选择");
        std::env::set_var("PEERDESK_LANG", "en");
        assert_eq!(system_lang(), Lang::En);
        std::env::remove_var("PEERDESK_LANG");
        let _ = system_lang();
        // 没有环境变量时用用户选的
        assert_eq!(effective_lang(&dir), Lang::En);
        save_lang(&dir, Lang::Zh).unwrap();
        assert_eq!(effective_lang(&dir), Lang::Zh);
        std::fs::write(dir.join(LANG_FILE), "fr").unwrap();
        assert_eq!(saved_lang(&dir), None);
        let _ = effective_lang(&dir);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn language_codes_and_ipv6_preference() {
        assert_eq!((Lang::Zh.code(), Lang::En.code()), ("zh", "en"));
        assert!(Lang::En.prefers_ipv6());
        assert!(!Lang::Zh.prefers_ipv6());
    }
}
