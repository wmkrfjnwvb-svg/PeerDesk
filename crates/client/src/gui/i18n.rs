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
    /// 被控开启时端口不能改的说明。
    pub port_locked: &'static str,
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
    /// 网络状况：丢包（{n} 是百分数）。
    pub net_loss: &'static str,
    /// 丢包多时的建议。
    pub net_loss_hint: &'static str,
    /// 网络状况：延迟高（{n} 是毫秒）。
    pub net_latency: &'static str,
    /// 延迟高时的建议。
    pub net_latency_hint: &'static str,
    /// 网络状况：带宽不够。
    pub net_bandwidth: &'static str,
    /// 带宽不够时的说明。
    pub net_bandwidth_hint: &'static str,
    pub session_ended: &'static str,
    pub you_disconnected: &'static str,
    pub stats: &'static str,
    /// 详情面板。
    pub d_title: &'static str,
    /// 详情面板。
    pub d_local: &'static str,
    /// 详情面板。
    pub d_remote: &'static str,
    /// 详情面板。
    pub d_path: &'static str,
    /// 详情面板。
    pub d_rtt: &'static str,
    /// 详情面板。
    pub d_router: &'static str,
    /// 详情面板。
    pub d_loss: &'static str,
    /// 详情面板。
    pub d_queue: &'static str,
    /// 详情面板。
    pub d_net: &'static str,
    /// 详情面板。
    pub d_recv: &'static str,
    /// 详情面板。
    pub d_picture: &'static str,
    /// 详情面板。
    pub d_decoder: &'static str,
    /// 详情面板。
    pub d_reaction: &'static str,
    /// 详情面板。
    pub d_fec: &'static str,
    /// 详情面板。
    pub d_encoder: &'static str,
    /// 详情面板。
    pub d_encode: &'static str,
    /// 详情面板。
    pub d_fps: &'static str,
    /// 详情面板。
    pub d_bitrate: &'static str,
    /// 详情面板。
    pub d_profile: &'static str,
    /// 详情面板。
    pub d_capture: &'static str,
    /// 详情面板。
    pub d_unknown: &'static str,
    /// 详情面板。
    pub d_none: &'static str,
    /// 详情面板。
    pub d_old_peer: &'static str,
    /// 详情面板。
    pub d_good: &'static str,
    /// 详情面板。
    pub d_fair: &'static str,
    /// 详情面板。
    pub d_poor: &'static str,
    /// 详情面板。
    pub d_waiting: &'static str,
    /// 详情面板。
    pub d_slowest: &'static str,
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
    /// 远程控制
    pub nav_home: &'static str,
    /// 设备
    pub nav_devices: &'static str,
    /// 设置
    pub nav_settings: &'static str,
    /// 远程控制设备
    pub remote_card: &'static str,
    /// 把设备码和密码告诉对方，对方就能连接本机
    pub this_device_hint: &'static str,
    /// 输入对方设备码或 IP 地址
    pub code_hint: &'static str,
    /// 连接密码
    pub password_input_hint: &'static str,
    /// “记住密码”勾选框。
    pub remember_password: &'static str,
    /// 高级选项
    pub advanced: &'static str,
    /// 显示密码
    pub show_password: &'static str,
    /// 隐藏密码
    pub hide_password: &'static str,
    /// 服务器已连接
    pub server_online: &'static str,
    /// 服务器未连接
    pub server_offline: &'static str,
    /// 未设置服务器
    pub server_unset: &'static str,
    /// 服务器
    pub section_server: &'static str,
    /// 网络
    pub section_network: &'static str,
    /// 通用
    pub section_general: &'static str,
    /// 关于
    pub section_about: &'static str,
    /// 语言
    pub language: &'static str,
    /// 版本
    pub version: &'static str,
    /// 还没有连接过的设备。连接一次后会出现在这里。
    pub no_recent: &'static str,
    /// 快捷键
    pub shortcuts: &'static str,
    /// 收起工具条
    pub collapse: &'static str,
    /// 展开工具条
    pub expand: &'static str,
    /// 局域网地址
    pub lan_addresses: &'static str,
    /// 开启后，别人可以用设备码连接这台电脑
    pub host_off_hint: &'static str,
    /// 无人值守服务在运行时的状态标签。
    pub unattended: &'static str,
    /// 无人值守服务在运行时的说明。
    pub unattended_hint: &'static str,
    /// 连接
    pub connect_this: &'static str,
    /// 外观
    pub appearance: &'static str,
    /// 跟随系统
    pub appearance_system: &'static str,
    /// 浅色
    pub appearance_light: &'static str,
    /// 深色
    pub appearance_dark: &'static str,
    /// 授权页标题
    pub perm_title: &'static str,
    /// 授权页说明
    pub perm_subtitle: &'static str,
    /// 屏幕录制的作用
    pub perm_screen_what: &'static str,
    /// 辅助功能的作用
    pub perm_input_what: &'static str,
    /// 已授权
    pub perm_granted: &'static str,
    /// 未授权
    pub perm_missing: &'static str,
    /// 第几步
    pub perm_step: &'static str,
    /// 步骤：打开
    pub perm_open_hint: &'static str,
    /// 步骤：打开开关
    pub perm_toggle_hint: &'static str,
    /// 步骤：添加
    pub perm_add_hint: &'static str,
    /// 步骤：解锁
    pub perm_unlock_hint: &'static str,
    /// 步骤：重开
    pub perm_relaunch_hint: &'static str,
    /// 示意图提示
    pub perm_callout: &'static str,
    /// 示意图解锁
    pub perm_unlock: &'static str,
    /// 按钮：请求授权
    pub perm_request: &'static str,
    /// 按钮：打开设置
    pub perm_open_settings: &'static str,
    /// 按钮：重开
    pub perm_relaunch: &'static str,
    /// 完成标题
    pub perm_done_title: &'static str,
    /// 完成说明
    pub perm_done: &'static str,
    /// 首页提示
    pub perm_banner: &'static str,
    /// 首页按钮
    pub perm_start: &'static str,
    /// 设备码获取中
    pub code_pending: &'static str,
    /// IP 直连
    pub ip_direct: &'static str,
    /// 最近连接提示
    pub recent_hint: &'static str,
    /// 设备页说明
    pub devices_hint: &'static str,
    /// 删除记录
    pub forget: &'static str,
    /// 设备列表里的“文件传输”按钮。
    pub file_transfer: &'static str,
    /// 点了“文件传输”之后的提示。
    pub files_after_connect: &'static str,
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
    /// macOS 上的“开始菜单”：聚焦搜索。
    pub sc_spotlight: &'static str,
    /// macOS 上的“任务管理器”：强制退出。
    pub sc_force_quit: &'static str,
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
    /// 原始大小（1:1）的提示。
    pub original_size: &'static str,

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
    pub tray_open: &'static str,
    pub tray_disconnect: &'static str,
    pub tray_quit: &'static str,
    pub tray_tip_idle: &'static str,
    pub tray_tip_active: &'static str,
    pub tray_hint: &'static str,
    pub tray_unavailable: &'static str,
    pub autostart_label: &'static str,
    pub autostart_hint: &'static str,
    pub autostart_on: &'static str,
    pub autostart_off: &'static str,
    pub autostart_fallback: &'static str,
    pub autostart_failed: &'static str,
}

/// 中文。
pub const ZH: Strings = Strings {
    app_title: "PeerDesk 测试版",
    this_device: "本机",
    allow_control: "允许远程控制",
    port: "端口",
    port_locked: "先在首页关闭“本机”被控，才能修改端口",
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
    net_loss: "丢包 {n}%",
    net_loss_hint: "网络丢包较多，画面可能卡顿。多半是 Wi-Fi 信号弱或有干扰：靠近路由器、改用 5 GHz Wi-Fi 或网线。",
    net_latency: "延迟 {n} ms",
    net_latency_hint: "网络延迟较高，操作会慢半拍。对方离得远、网络拥堵，或者没能直连（走中继）。",
    net_bandwidth: "带宽不足",
    net_bandwidth_hint: "网络带宽不够，画面已自动降低清晰度和帧率。关掉占用网络的下载、视频可以改善。",
    session_ended: "会话已结束",
    you_disconnected: "你断开了连接",
    stats: "详情 (F12)",
    d_title: "连接详情",
    d_local: "本机（控制端）",
    d_remote: "对方（被控端）",
    d_path: "连接路径",
    d_rtt: "往返延迟",
    d_router: "到路由器",
    d_loss: "丢包",
    d_queue: "排队延迟",
    d_net: "网络状况",
    d_recv: "接收码率",
    d_picture: "画面",
    d_decoder: "解码",
    d_reaction: "操作到画面",
    d_fec: "纠错",
    d_encoder: "编码器",
    d_encode: "编码耗时",
    d_fps: "帧率",
    d_bitrate: "码率（实际 / 目标）",
    d_profile: "码率策略",
    d_capture: "采集分辨率",
    d_unknown: "测不出",
    d_none: "—",
    d_old_peer: "对方是老版本，没有发送状态",
    d_good: "正常",
    d_fair: "一般",
    d_poor: "差",
    d_waiting: "等待数据…",
    d_slowest: "最慢",
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
    nav_home: "远程控制",
    nav_devices: "设备",
    nav_settings: "设置",
    remote_card: "远程控制设备",
    this_device_hint: "把设备码和密码告诉对方，对方就能连接本机",
    code_hint: "输入对方设备码或 IP 地址",
    password_input_hint: "连接密码",
    remember_password: "记住密码（加密保存在本机）",
    advanced: "高级选项",
    show_password: "显示密码",
    hide_password: "隐藏密码",
    server_online: "服务器已连接",
    server_offline: "服务器未连接",
    server_unset: "未设置服务器",
    section_server: "服务器",
    section_network: "网络",
    section_general: "通用",
    section_about: "关于",
    language: "语言",
    version: "版本",
    no_recent: "还没有连接过的设备。连接一次后会出现在这里。",
    shortcuts: "快捷键",
    collapse: "收起工具条",
    expand: "展开工具条",
    lan_addresses: "局域网地址",
    host_off_hint: "开启后，别人可以用设备码连接这台电脑",
    unattended: "无人值守",
    unattended_hint: "系统服务在运行：锁屏和登录界面也能远程。连接用管理员设置的无人值守密码。",
    connect_this: "连接",
    appearance: "外观",
    appearance_system: "跟随系统",
    appearance_light: "浅色",
    appearance_dark: "深色",
    perm_title: "授权",
    perm_subtitle: "这台 Mac 需要以下两项权限，别人才能看到屏幕、操作鼠标键盘。只需设置一次。",
    perm_screen_what: "让对方看到这台电脑的屏幕",
    perm_input_what: "让对方操作这台电脑的鼠标和键盘",
    perm_granted: "已授权",
    perm_missing: "未授权",
    perm_step: "第 {n} 步",
    perm_open_hint: "点下面的按钮，系统会弹出授权提示或打开设置",
    perm_toggle_hint: "在列表里找到 PeerDesk，打开右边的开关",
    perm_add_hint: "列表里没有 PeerDesk？点列表下面的“+”，在“应用程序”里选择 PeerDesk",
    perm_unlock_hint: "先点左下角的锁，输入这台电脑的密码",
    perm_relaunch_hint: "系统提示“退出并重新打开”时点它；也可以点下面的按钮",
    perm_callout: "打开这个开关",
    perm_unlock: "点锁解锁",
    perm_request: "请求授权",
    perm_open_settings: "打开系统设置",
    perm_relaunch: "重新打开 PeerDesk",
    perm_done_title: "全部设置好了",
    perm_done: "现在别人可以远程控制这台 Mac 了。",
    perm_banner: "这台 Mac 还需要授权，别人才能远程控制它",
    perm_start: "开始设置",
    code_pending: "正在获取设备码…",
    ip_direct: "IP 直连",
    recent_hint: "单击填入，双击直接连接",
    devices_hint: "连接过的设备会出现在这里",
    forget: "删除",
    file_transfer: "文件传输",
    files_after_connect: "连接这台设备后会自动打开文件传输（需要时输入密码）",
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
    sc_spotlight: "聚焦搜索",
    sc_force_quit: "强制退出",
    os_unknown: "未知（按 Windows 发送）",
    remote_os: "对方系统",
    sound: "声音",
    clipboard_sync: "剪贴板同步",
    fullscreen: "全屏",
    original_size: "原始大小（1:1，最清楚）；再点一下铺满窗口",
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
    tray_open: "打开 PeerDesk",
    tray_disconnect: "断开连接",
    tray_quit: "退出 PeerDesk",
    tray_tip_idle: "PeerDesk：没有人连接",
    tray_tip_active: "PeerDesk：有人正在连接这台电脑",
    tray_hint: "PeerDesk 还在后台运行，这台电脑仍可被连接。从托盘（菜单栏）图标可以打开窗口或退出。",
    tray_unavailable: "这个桌面没有系统托盘，关闭窗口会退出 PeerDesk（被控也会停止）。",
    autostart_label: "开机自启，被杀或崩溃后自动恢复",
    autostart_hint: "从托盘选“退出”才会真正退出，不会被自动拉起。",
    autostart_on: "已开启开机自启和自动恢复",
    autostart_off: "已关闭开机自启和自动恢复",
    autostart_fallback: "系统的服务管理器用不了，已改用 PeerDesk 自带的守护进程开机自启（守护进程本身被杀时不会恢复）",
    autostart_failed: "设置开机自启失败",
};

/// 英文。
pub const EN: Strings = Strings {
    app_title: "PeerDesk (test build)",
    this_device: "This device",
    allow_control: "Allow remote control",
    port: "Port",
    port_locked: "Turn off hosting on the Home page to change the port",
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
    net_loss: "{n}% loss",
    net_loss_hint: "The network is dropping packets, so the picture may stutter. Usually weak or noisy Wi-Fi: move closer to the router, or use 5 GHz Wi-Fi or a cable.",
    net_latency: "{n} ms delay",
    net_latency_hint: "High network delay, so actions lag a little. The other computer is far away, the network is congested, or the connection is relayed.",
    net_bandwidth: "Low bandwidth",
    net_bandwidth_hint: "Not enough bandwidth: picture quality and frame rate were lowered automatically. Pausing downloads or videos helps.",
    session_ended: "Session ended",
    you_disconnected: "You disconnected",
    stats: "Details (F12)",
    d_title: "Connection details",
    d_local: "This computer (controller)",
    d_remote: "Remote computer (host)",
    d_path: "Path",
    d_rtt: "Round trip",
    d_router: "To router",
    d_loss: "Packet loss",
    d_queue: "Queueing delay",
    d_net: "Network",
    d_recv: "Received bitrate",
    d_picture: "Picture",
    d_decoder: "Decoder",
    d_reaction: "Action to picture",
    d_fec: "Error correction",
    d_encoder: "Encoder",
    d_encode: "Encode time",
    d_fps: "Frame rate",
    d_bitrate: "Bitrate (actual / target)",
    d_profile: "Rate profile",
    d_capture: "Capture size",
    d_unknown: "n/a",
    d_none: "—",
    d_old_peer: "The remote computer runs an older version and sends no status",
    d_good: "Good",
    d_fair: "Fair",
    d_poor: "Poor",
    d_waiting: "Waiting for data…",
    d_slowest: "slowest",
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
    nav_home: "Remote control",
    nav_devices: "Devices",
    nav_settings: "Settings",
    remote_card: "Control a remote device",
    this_device_hint: "Share this code and password so others can connect",
    code_hint: "Enter a device code or IP address",
    password_input_hint: "Password",
    remember_password: "Remember password (encrypted on this computer)",
    advanced: "Advanced",
    show_password: "Show password",
    hide_password: "Hide password",
    server_online: "Server connected",
    server_offline: "Server not connected",
    server_unset: "No server set",
    section_server: "Server",
    section_network: "Network",
    section_general: "General",
    section_about: "About",
    language: "Language",
    version: "Version",
    no_recent: "No devices yet. Devices you connect to will appear here.",
    shortcuts: "Shortcuts",
    collapse: "Hide toolbar",
    expand: "Show toolbar",
    lan_addresses: "LAN addresses",
    host_off_hint: "Turn on to let others connect to this computer",
    unattended: "Unattended",
    unattended_hint: "The system service is running: the lock and sign-in screens can be controlled too. Connect with the unattended password set by the administrator.",
    connect_this: "Connect",
    appearance: "Appearance",
    appearance_system: "System",
    appearance_light: "Light",
    appearance_dark: "Dark",
    perm_title: "Permissions",
    perm_subtitle: "This Mac needs two permissions so others can see the screen and use the mouse and keyboard. You only do this once.",
    perm_screen_what: "Lets the other side see this screen",
    perm_input_what: "Lets the other side use the mouse and keyboard",
    perm_granted: "Granted",
    perm_missing: "Not granted",
    perm_step: "Step {n}",
    perm_open_hint: "Click the button below. macOS will ask, or open Settings",
    perm_toggle_hint: "Find PeerDesk in the list and turn on its switch",
    perm_add_hint: "PeerDesk not in the list? Click “+” below the list and choose PeerDesk in Applications",
    perm_unlock_hint: "First click the lock at the bottom left and enter your password",
    perm_relaunch_hint: "When macOS asks to “Quit & Reopen”, click it, or use the button below",
    perm_callout: "Turn this on",
    perm_unlock: "Unlock",
    perm_request: "Ask for permission",
    perm_open_settings: "Open System Settings",
    perm_relaunch: "Reopen PeerDesk",
    perm_done_title: "All set",
    perm_done: "Others can now control this Mac remotely.",
    perm_banner: "This Mac needs permission before others can control it",
    perm_start: "Set up",
    code_pending: "Getting device code…",
    ip_direct: "Direct IP",
    recent_hint: "Click to fill in, double-click to connect",
    devices_hint: "Devices you have connected to",
    forget: "Remove",
    file_transfer: "File transfer",
    files_after_connect: "File transfer opens once you connect to this device (enter the password if asked)",
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
    sc_spotlight: "Spotlight",
    sc_force_quit: "Force Quit",
    os_unknown: "unknown (sent as Windows)",
    remote_os: "Remote system",
    sound: "Sound",
    clipboard_sync: "Clipboard sync",
    fullscreen: "Full screen",
    original_size: "Original size (1:1, sharpest); click again to fill the window",
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
    tray_open: "Open PeerDesk",
    tray_disconnect: "Disconnect",
    tray_quit: "Quit PeerDesk",
    tray_tip_idle: "PeerDesk: nobody connected",
    tray_tip_active: "PeerDesk: someone is connected to this computer",
    tray_hint: "PeerDesk is still running in the background and this computer can still be reached. Use the tray (menu bar) icon to open the window or quit.",
    tray_unavailable: "This desktop has no system tray, so closing the window quits PeerDesk (and stops hosting).",
    autostart_label: "Start at login and restart automatically after a crash or kill",
    autostart_hint: "Quitting from the tray really quits, and it is not restarted.",
    autostart_on: "Start at login and auto-restart are on",
    autostart_off: "Start at login and auto-restart are off",
    autostart_fallback: "The system service manager is not available, so PeerDesk uses its own guard process at login (it is not restarted if the guard itself is killed)",
    autostart_failed: "Could not set up start at login",
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
