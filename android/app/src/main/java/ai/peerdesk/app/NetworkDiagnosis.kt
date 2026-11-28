package ai.peerdesk.app

/** 网络变差的原因。 */
sealed interface NetworkProblem {
    /** 本机到路由器这一段（Wi-Fi 信号、干扰）。 */
    data class LocalRouter(val routerMs: Int) : NetworkProblem
    /** 路由器之外（公网、对端网络）。 */
    data object Internet : NetworkProblem
    /** 没测到路由器：只能说多半在公网，但不确定。 */
    data object Unmeasured : NetworkProblem
}

/** 丢包超过 2%（20‰）或延迟超过 150 ms 才提示。 */
const val LOSS_WARN_PERMILLE = 20
const val RTT_WARN_MS = 150

/** 到路由器的往返达到这个值，就认为本机到路由器这一段有问题（正常 Wi-Fi 一般个位数到十几毫秒）。 */
const val ROUTER_SLOW_MS = 30

/**
 * 判断网络有没有问题、问题在哪一段。没问题返回 null。
 *
 * 路由器那段的判断：`routerMs` 达到 [ROUTER_SLOW_MS]，或者占了总往返的一半以上（延迟本身高时），
 * 说明本机到路由器就慢；否则问题在路由器之外。没测到 `routerMs` 时返回 [NetworkProblem.Unmeasured]，不假装知道。
 */
fun diagnoseNetwork(q: CoreEvent.Quality): NetworkProblem? {
    val bad = q.lossPermille > LOSS_WARN_PERMILLE || q.rttMs > RTT_WARN_MS
    if (!bad) return null
    val router = q.routerMs ?: return NetworkProblem.Unmeasured
    val localSlow = router >= ROUTER_SLOW_MS || (q.rttMs > 0 && router * 2 >= q.rttMs && router >= 10)
    return if (localSlow) NetworkProblem.LocalRouter(router) else NetworkProblem.Internet
}

/** 丢包千分比显示成百分比文字，如 3 -> "0.3%"。 */
fun formatLoss(permille: Int): String = "${permille / 10}.${permille % 10}%"

/** 码率显示：>= 1000 kbps 用 Mbps（一位小数）。返回 (值, 是否 Mbps)。 */
fun splitBitrate(kbps: Int): Pair<Double, Boolean> =
    if (kbps >= 1000) kbps / 1000.0 to true else kbps.toDouble() to false
