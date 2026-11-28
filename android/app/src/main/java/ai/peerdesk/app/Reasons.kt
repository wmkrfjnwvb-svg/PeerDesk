package ai.peerdesk.app

/** 失败提示里可以放的按钮（和 crates/net/src/reason.rs 的 `Action` 一一对应）。 */
enum class ReasonAction { Retry, ServerSettings, Diagnostics }

/**
 * 连接失败、会话结束、重连的原因（核心事件里的 `reason` 代号，见 crates/net/src/reason.rs 的 `Reason::code`）。
 * 界面按它说一句原因（[cause]）、一句办法（[fix]），最多两个按钮（[actions]，和核心的 `Reason::actions` 相同）；
 * 核心的原文放进可以复制的“详情”。文字和桌面端（crates/client/src/gui/reasons.rs）相同，
 * crates/mobile/tests/reason_texts.rs 核对两边的代号、按钮和中英文字。
 */
enum class Reason(val code: String, val cause: Int, val fix: Int, val actions: List<ReasonAction>) {
    NoRoute("no_route", R.string.reason_no_route_cause, R.string.reason_no_route_fix, listOf(ReasonAction.Retry, ReasonAction.Diagnostics)),
    DeviceOffline("device_offline", R.string.reason_device_offline_cause, R.string.reason_device_offline_fix, listOf(ReasonAction.Retry)),
    ServerUnreachable("server_unreachable", R.string.reason_server_unreachable_cause, R.string.reason_server_unreachable_fix, listOf(ReasonAction.Retry, ReasonAction.ServerSettings)),
    WrongPassword("wrong_password", R.string.reason_wrong_password_cause, R.string.reason_wrong_password_fix, listOf(ReasonAction.Retry)),
    Locked("locked", R.string.reason_locked_cause, R.string.reason_locked_fix, listOf(ReasonAction.Retry)),
    Busy("busy", R.string.reason_busy_cause, R.string.reason_busy_fix, listOf(ReasonAction.Retry)),
    Declined("declined", R.string.reason_declined_cause, R.string.reason_declined_fix, listOf(ReasonAction.Retry)),
    HostPermission("host_permission", R.string.reason_host_permission_cause, R.string.reason_host_permission_fix, listOf(ReasonAction.Retry)),
    Version("version", R.string.reason_version_cause, R.string.reason_version_fix, listOf()),
    IdentityMismatch("identity_mismatch", R.string.reason_identity_mismatch_cause, R.string.reason_identity_mismatch_fix, listOf(ReasonAction.Diagnostics)),
    PeerClosed("peer_closed", R.string.reason_peer_closed_cause, R.string.reason_peer_closed_fix, listOf(ReasonAction.Retry)),
    PeerQuit("peer_quit", R.string.reason_peer_quit_cause, R.string.reason_peer_quit_fix, listOf(ReasonAction.Retry)),
    NetworkLost("network_lost", R.string.reason_network_lost_cause, R.string.reason_network_lost_fix, listOf(ReasonAction.Retry, ReasonAction.Diagnostics)),
    RelayQuota("relay_quota", R.string.reason_relay_quota_cause, R.string.reason_relay_quota_fix, listOf(ReasonAction.Retry)),
    PortInUse("port_in_use", R.string.reason_port_in_use_cause, R.string.reason_port_in_use_fix, listOf()),
    Cancelled("cancelled", R.string.reason_cancelled_cause, R.string.reason_cancelled_fix, listOf()),
    Other("other", R.string.reason_other_cause, R.string.reason_other_fix, listOf(ReasonAction.Retry, ReasonAction.Diagnostics));

    /** 是不是“对方或自己正常结束”：提示用普通颜色，不用错误的红色。 */
    val calm: Boolean get() = this == PeerClosed || this == PeerQuit || this == Cancelled

    companion object {
        /** 代号换回原因。不认识的（更新的核心加的）、没有代号的（老核心）当作 [Other]：原文仍在详情里。 */
        fun of(code: String?): Reason = entries.firstOrNull { it.code == code } ?: Other
    }
}
