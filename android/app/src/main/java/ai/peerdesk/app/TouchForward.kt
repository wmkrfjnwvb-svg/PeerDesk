package ai.peerdesk.app

import android.view.MotionEvent

/** `Native.touch` 的 phase 取值。 */
object TouchPhase {
    const val DOWN = 0
    const val MOVE = 1
    const val UP = 2
    const val CANCEL = 3
}

/** 一个手指的一次事件。 */
data class TouchOp(val id: Int, val phase: Int, val x: Float, val y: Float)

/**
 * 把一个 MotionEvent 拆成按手指（pointer id）逐个转发的事件。纯函数，参数是 MotionEvent 的原始数据，好在 JVM 上测。
 *
 * - ACTION_DOWN / ACTION_POINTER_DOWN：只转发刚按下的那一根（actionIndex）。
 * - ACTION_UP / ACTION_POINTER_UP：只转发抬起的那一根。
 * - ACTION_MOVE：所有手指各发一次移动。
 * - ACTION_CANCEL：所有手指各发一次取消。
 */
fun touchOps(
    action: Int, actionIndex: Int, ids: IntArray, xs: FloatArray, ys: FloatArray,
): List<TouchOp> {
    val n = minOf(ids.size, xs.size, ys.size)
    fun at(i: Int, phase: Int) = TouchOp(ids[i], phase, xs[i], ys[i])
    return when (action) {
        MotionEvent.ACTION_DOWN, MotionEvent.ACTION_POINTER_DOWN ->
            if (actionIndex in 0 until n) listOf(at(actionIndex, TouchPhase.DOWN)) else emptyList()
        MotionEvent.ACTION_UP, MotionEvent.ACTION_POINTER_UP ->
            if (actionIndex in 0 until n) listOf(at(actionIndex, TouchPhase.UP)) else emptyList()
        MotionEvent.ACTION_MOVE -> (0 until n).map { at(it, TouchPhase.MOVE) }
        MotionEvent.ACTION_CANCEL -> (0 until n).map { at(it, TouchPhase.CANCEL) }
        else -> emptyList()
    }
}

/** 设备码输入：只留数字，最多 9 位。 */
fun sanitizeCode(input: String): String = input.filter { it in '0'..'9' }.take(9)

/** 9 位设备码显示成 3-3-3（不足 9 位时显示已输入的部分）。 */
fun formatCode(digits: String): String = digits.chunked(3).joinToString("-")

/** 设备码显示成 3-3-3：输入框里存纯数字，显示加横线，光标位置按这个换算。 */
fun codeDisplayOffset(original: Int): Int = if (original <= 0) 0 else original + (original - 1) / 3

fun codeOriginalOffset(transformed: Int): Int = if (transformed <= 0) 0 else transformed - transformed / 4
