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
        std::env::set_var("PEERDESK_LANG", "zh_CN");
        assert_eq!(system_lang(), Lang::Zh);
        std::env::set_var("PEERDESK_LANG", "en");
        assert_eq!(system_lang(), Lang::En);
        std::env::remove_var("PEERDESK_LANG");
        let _ = system_lang();
    }
}
