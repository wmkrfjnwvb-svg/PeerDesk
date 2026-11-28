package ai.peerdesk.app

import org.json.JSONArray
import org.json.JSONObject

/** Rust 核心发来的事件（见 crates/mobile/src/lib.rs 的 `Event::to_json`）。 */
sealed interface CoreEvent {
    data class Connected(val route: Route, val detail: String) : CoreEvent
    data class Peer(val os: PeerOs, val name: String) : CoreEvent
    data class Quality(
        val rttMs: Int,
        val lossPermille: Int,
        val queueMs: Int,
        val fps: Int,
        val recvKbps: Int,
        val decodeMs: Int,
        val width: Int,
        val height: Int,
        /** 本机到路由器的往返；没测到是 null。 */
        val routerMs: Int?,
        val decoder: String,
        val fec: String,
    ) : CoreEvent
    /** 正在重连：第几次，核心的原文（详情），为什么在重连。 */
    data class Reconnecting(val attempt: Int, val why: String, val reason: Reason = Reason.NetworkLost) : CoreEvent
    /** 服务器的通知（例如免费中继额度用完）：原文和原因。 */
    data class Notice(val text: String, val reason: Reason = Reason.Other) : CoreEvent
    /** 会话结束。之后不再有事件。`text` 是核心的原文（放进详情），界面按 [reason] 说人话。 */
    data class Ended(val ok: Boolean, val text: String, val reason: Reason = Reason.Other) : CoreEvent
}

enum class Route { Direct, Relay, Unknown }
enum class PeerOs { Windows, MacOs, Linux, Unknown }

/** 解析一行事件 JSON。格式不对或类型不认识返回 null（调用方忽略，不当作错误弹出来）。 */
fun parseEvent(line: String?): CoreEvent? {
    if (line.isNullOrBlank()) return null
    return try {
        val o = JSONObject(line)
        when (o.optString("type")) {
            "connected" -> CoreEvent.Connected(
                route = when (o.optString("route")) {
                    "Direct" -> Route.Direct
                    "Relay" -> Route.Relay
                    else -> Route.Unknown
                },
                detail = o.optString("detail"),
            )
            "peer" -> CoreEvent.Peer(
                os = when (o.optString("os")) {
                    "Windows" -> PeerOs.Windows
                    "MacOs" -> PeerOs.MacOs
                    "Linux" -> PeerOs.Linux
                    else -> PeerOs.Unknown
                },
                name = o.optString("name"),
            )
            "quality" -> CoreEvent.Quality(
                rttMs = o.optInt("rtt_ms"),
                lossPermille = o.optInt("loss_permille"),
                queueMs = o.optInt("queue_ms"),
                fps = o.optInt("fps"),
                recvKbps = o.optInt("recv_kbps"),
                decodeMs = o.optInt("decode_ms"),
                width = o.optInt("width"),
                height = o.optInt("height"),
                routerMs = if (o.isNull("router_ms") || !o.has("router_ms")) null else o.optInt("router_ms"),
                decoder = o.optString("decoder"),
                fec = o.optString("fec"),
            )
            // 老核心没有 reason：重连当作网络断了，其他当作“其他原因”（原文仍在）
            "reconnecting" -> CoreEvent.Reconnecting(
                o.optInt("attempt"), o.optString("why"), Reason.of(o.optString("reason", Reason.NetworkLost.code)),
            )
            "notice" -> CoreEvent.Notice(o.optString("text"), Reason.of(o.optString("reason")))
            "ended" -> CoreEvent.Ended(o.optBoolean("ok"), o.optString("text"), Reason.of(o.optString("reason")))
            else -> null
        }
    } catch (_: org.json.JSONException) {
        null
    }
}

/** 手势识别要界面执行的画面变换（像素）。 */
sealed interface ViewOp {
    data class Zoom(val factor: Float, val cx: Float, val cy: Float) : ViewOp
    data class Pan(val dx: Float, val dy: Float) : ViewOp
    /** 长按到点了：震一下。 */
    data object Haptic : ViewOp
    /** 触屏点击落在屏幕 (x, y)（像素）；button 0 左键、1 右键。界面画水波、记输入位置。 */
    data class Tap(val x: Float, val y: Float, val button: Int) : ViewOp
}

/** 解析 `touch`/`tick` 返回的变换数组；null、空串、坏格式都当作“没有变换”。 */
fun parseViewOps(json: String?): List<ViewOp> {
    if (json.isNullOrBlank()) return emptyList()
    return try {
        val arr = JSONArray(json)
        buildList {
            for (i in 0 until arr.length()) {
                val o = arr.optJSONObject(i) ?: continue
                when (o.optString("type")) {
                    "zoom" -> add(
                        ViewOp.Zoom(
                            o.optDouble("factor", 1.0).toFloat(),
                            o.optDouble("cx", 0.0).toFloat(),
                            o.optDouble("cy", 0.0).toFloat(),
                        ),
                    )
                    "haptic" -> add(ViewOp.Haptic)
                    "tap" -> add(
                        ViewOp.Tap(
                            o.optDouble("x", 0.0).toFloat(),
                            o.optDouble("y", 0.0).toFloat(),
                            if (o.optInt("button", 0) == 1) 1 else 0,
                        ),
                    )
                    "pan" -> add(
                        ViewOp.Pan(o.optDouble("dx", 0.0).toFloat(), o.optDouble("dy", 0.0).toFloat()),
                    )
                }
            }
        }
    } catch (_: org.json.JSONException) {
        emptyList()
    }
}
