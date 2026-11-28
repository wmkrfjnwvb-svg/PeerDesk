package ai.peerdesk.app

import kotlin.math.max

/**
 * 软键盘弹出时让“输入位置”（最后一次左键点击的位置）留在可见区域里，思路同 ToDesk 的 adjustPan。
 *
 * 所有坐标是没有被顶起之前的屏幕 y（像素）；`shift` 是画面向上顶起的距离（>= 0）。
 * `visibleBottom` = 键盘顶（有快捷键栏时是快捷键栏顶）。
 * 顶起量不受平移夹边限制：输入点在桌面最下面时，桌面下沿会停在键盘上方。
 */

/** 点要在可见区域内至少需要的顶起量（下限）。可以是负数（点离键盘很远）。 */
fun imeShiftLow(pointY: Float, visibleBottom: Float, marginPx: Float): Float = pointY + marginPx - visibleBottom

/** 点不能被顶出屏幕上沿：顶起量的上限。 */
fun imeShiftHigh(pointY: Float, topMarginPx: Float): Float = pointY - topMarginPx

/** 键盘刚弹出：被盖住才顶起，刚好让点离键盘 margin。 */
fun imeShiftOnOpen(pointY: Float, visibleBottom: Float, marginPx: Float): Float =
    max(0f, imeShiftLow(pointY, visibleBottom, marginPx))

/**
 * 键盘已经在、来了新的输入位置（或键盘高度变了）：允许范围 [low, high]。
 * 当前顶起量在范围内就不动；否则夹到最近的一端（移动最少，不来回跳）。结果不小于 0。
 */
fun imeShiftOnNewPoint(
    current: Float, pointY: Float, visibleBottom: Float, marginPx: Float, topMarginPx: Float,
): Float {
    val low = imeShiftLow(pointY, visibleBottom, marginPx)
    val high = imeShiftHigh(pointY, topMarginPx)
    // 键盘特别高、可见区域比两个边距之和还小时 low > high：以“不被盖住”为准
    val v = if (low > high) low else current.coerceIn(low, high)
    return max(0f, v)
}

/** 输入位置（画面矩形里的比例）换成没顶起前的屏幕 y。 */
fun inputPointY(unshifted: ViewRect, fy: Float): Float = unshifted.top + fy * unshifted.height

/** 屏幕坐标换成画面矩形里的比例（0..1）；矩形是当前显示的（已顶起的）矩形。 */
fun toFraction(shown: ViewRect, x: Float, y: Float): Pair<Float, Float>? {
    if (shown.width <= 0f || shown.height <= 0f) return null
    return ((x - shown.left) / shown.width).coerceIn(0f, 1f) to ((y - shown.top) / shown.height).coerceIn(0f, 1f)
}
