package ai.peerdesk.app

import kotlin.math.max

/** 远程画面在界面上的矩形（像素，界面坐标，已算上缩放和平移）。 */
data class ViewRect(val left: Float, val top: Float, val width: Float, val height: Float)

/** 缩放倍数和平移量。`panX/panY` 是画面中心相对容器中心的偏移（像素）。 */
data class ViewState(val zoom: Float = 1f, val panX: Float = 0f, val panY: Float = 0f)

const val MIN_ZOOM = 1f
const val MAX_ZOOM = 5f

/** 第一帧之前用 16:9。 */
const val DEFAULT_ASPECT = 16f / 9f

/** 被控端画面宽高比；还没收到有效尺寸时返回 16:9。 */
fun aspectOf(width: Int, height: Int): Float =
    if (width > 0 && height > 0) width.toFloat() / height.toFloat() else DEFAULT_ASPECT

/** 缩放 1 倍时，画面在容器里居中、完整放下（等比，留黑边）的尺寸。 */
fun fitSize(containerW: Float, containerH: Float, aspect: Float): Pair<Float, Float> {
    if (containerW <= 0f || containerH <= 0f || aspect <= 0f) return 0f to 0f
    return if (containerW / containerH > aspect) {
        containerH * aspect to containerH
    } else {
        containerW to containerW / aspect
    }
}

/** 平移不超出画面边缘：画面比容器小的方向固定居中；比容器大的方向，画面边缘不离开容器边缘。 */
private fun clampPan(pan: Float, shown: Float, container: Float): Float {
    val limit = max(0f, (shown - container) / 2f)
    return pan.coerceIn(-limit, limit)
}

/** 把缩放限制在 1–5 倍，并把平移夹在画面边缘内（容器或画面尺寸变了也用它重新夹）。 */
fun clampState(s: ViewState, containerW: Float, containerH: Float, aspect: Float): ViewState {
    val (bw, bh) = fitSize(containerW, containerH, aspect)
    val z = s.zoom.coerceIn(MIN_ZOOM, MAX_ZOOM)
    return ViewState(
        zoom = z,
        panX = clampPan(s.panX, bw * z, containerW),
        panY = clampPan(s.panY, bh * z, containerH),
    )
}

fun rectOf(s: ViewState, containerW: Float, containerH: Float, aspect: Float): ViewRect {
    val (bw, bh) = fitSize(containerW, containerH, aspect)
    val w = bw * s.zoom
    val h = bh * s.zoom
    return ViewRect((containerW - w) / 2f + s.panX, (containerH - h) / 2f + s.panY, w, h)
}

/** 以容器内一点 (cx, cy) 为中心缩放：这一点下面的画面内容缩放前后保持在原处。 */
fun applyZoom(
    s: ViewState, factor: Float, cx: Float, cy: Float,
    containerW: Float, containerH: Float, aspect: Float,
): ViewState {
    if (!factor.isFinite() || factor <= 0f) return s
    val old = rectOf(s, containerW, containerH, aspect)
    val newZoom = (s.zoom * factor).coerceIn(MIN_ZOOM, MAX_ZOOM)
    if (old.width <= 0f || old.height <= 0f) return s.copy(zoom = newZoom)
    val (bw, bh) = fitSize(containerW, containerH, aspect)
    val newW = bw * newZoom
    val newH = bh * newZoom
    // 焦点在画面里的相对位置（0..1）不变
    val fx = (cx - old.left) / old.width
    val fy = (cy - old.top) / old.height
    val newLeft = cx - fx * newW
    val newTop = cy - fy * newH
    val moved = ViewState(newZoom, newLeft - (containerW - newW) / 2f, newTop - (containerH - newH) / 2f)
    return clampState(moved, containerW, containerH, aspect)
}

fun applyPan(
    s: ViewState, dx: Float, dy: Float,
    containerW: Float, containerH: Float, aspect: Float,
): ViewState = clampState(s.copy(panX = s.panX + dx, panY = s.panY + dy), containerW, containerH, aspect)

/** 依次执行手势识别给的变换。 */
fun applyOps(
    s: ViewState, ops: List<ViewOp>,
    containerW: Float, containerH: Float, aspect: Float,
): ViewState = ops.fold(s) { acc, op ->
    when (op) {
        is ViewOp.Zoom -> applyZoom(acc, op.factor, op.cx, op.cy, containerW, containerH, aspect)
        is ViewOp.Pan -> applyPan(acc, op.dx, op.dy, containerW, containerH, aspect)
    }
}

/** 缩放后才要告诉手势识别“放大了”（放大时单指拖动先用来平移）。 */
fun isZoomed(s: ViewState): Boolean = s.zoom > MIN_ZOOM + 1e-3f

