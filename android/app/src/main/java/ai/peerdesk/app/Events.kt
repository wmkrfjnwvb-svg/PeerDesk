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
    data class Reconnecting(val attempt: Int, val why: String) : CoreEvent
    data class Notice(val text: String) : CoreEvent
    /** 会话结束。之后不再有事件。 */
    data class Ended(val ok: Boolean, val text: String) : CoreEvent
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
            "reconnecting" -> CoreEvent.Reconnecting(o.optInt("attempt"), o.optString("why"))
            "notice" -> CoreEvent.Notice(o.optString("text"))
            "ended" -> CoreEvent.Ended(o.optBoolean("ok"), o.optString("text"))
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
