package ai.peerdesk.app

/** 点击水波：左键一圈，右键双圈。 */
data class Ripple(val id: Long, val x: Float, val y: Float, val button: Int, val startMs: Long)

const val RIPPLE_DURATION_MS = 350L
const val RIPPLE_MIN_DP = 8f
const val RIPPLE_MAX_DP = 28f
const val RIPPLE_START_ALPHA = 0.35f
/** 同时最多留这么多个（连点时旧的先丢）。 */
const val RIPPLE_MAX_COUNT = 12

/** 水波的进度 0..1（还没开始是 0，过期是 1）。 */
fun rippleProgress(r: Ripple, nowMs: Long): Float =
    ((nowMs - r.startMs).toFloat() / RIPPLE_DURATION_MS).coerceIn(0f, 1f)

/** 进度 p 时的半径（dp）：先快后慢（ease-out）从 8 扩到 28。 */
fun rippleRadiusDp(p: Float): Float {
    val eased = 1f - (1f - p) * (1f - p)
    return RIPPLE_MIN_DP + (RIPPLE_MAX_DP - RIPPLE_MIN_DP) * eased
}

/** 进度 p 时的透明度：从 0.35 线性降到 0。 */
fun rippleAlpha(p: Float): Float = RIPPLE_START_ALPHA * (1f - p)

fun isExpired(r: Ripple, nowMs: Long): Boolean = nowMs - r.startMs >= RIPPLE_DURATION_MS

/** 加一个水波：先清掉已过期的，超过上限就丢最旧的。可以叠加。 */
fun addRipple(list: List<Ripple>, r: Ripple, nowMs: Long): List<Ripple> {
    val live = list.filterNot { isExpired(it, nowMs) } + r
    return if (live.size > RIPPLE_MAX_COUNT) live.takeLast(RIPPLE_MAX_COUNT) else live
}

/** 清掉已过期的。 */
fun pruneRipples(list: List<Ripple>, nowMs: Long): List<Ripple> = list.filterNot { isExpired(it, nowMs) }

/** 按下大鼠标按键时箭头缩小的倍数。 */
const val ARROW_PRESSED_SCALE = 0.8f
