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

/** 容器四边被盖住的宽度（像素，容器坐标）：工具条、刘海等。画面可以被拉到不被盖住的区域里。 */
data class Edges(val left: Float, val top: Float, val right: Float, val bottom: Float) {
    companion object {
        val ZERO = Edges(0f, 0f, 0f, 0f)
    }
}

/**
 * 被盖住的边距：上边是工具条（显示时）的下沿和刘海上沿里大的那个，左右下边是刘海。
 * 键盘不在这里算（由 imeShift 单独处理），避免算两次。
 */
fun safeInsets(toolbarVisible: Boolean, toolbarBottom: Float, cutout: Edges): Edges =
    Edges(
        left = cutout.left.coerceAtLeast(0f),
        top = max(if (toolbarVisible) toolbarBottom else 0f, cutout.top).coerceAtLeast(0f),
        right = cutout.right.coerceAtLeast(0f),
        bottom = cutout.bottom.coerceAtLeast(0f),
    )

/**
 * 一个方向上的平移范围。C 容器长度、h 画面长度、S/E 起止边距，c0 = (C-h)/2（居中时画面起点）。
 * - h >= C-S-E：画面盖住整个安全区，起点在 [C-E-h, S]，即 pan ∈ [(C-E-h)-c0, S-c0]：
 *   画面起点最多拉到 S，终点最少停在 C-E。
 * - h < C-S-E：画面比安全区小，居中在安全区里：pan = (S-E)/2。
 * 边距都是 0 时和原来一样（h>=C 时 ±(h-C)/2，否则 0）。
 */
private fun clampPan(pan: Float, shown: Float, container: Float, start: Float, end: Float): Float {
    val c0 = (container - shown) / 2f
    if (shown < container - start - end) return (start - end) / 2f
    val lo = (container - end - shown) - c0
    val hi = start - c0
    return pan.coerceIn(lo, hi)
}

/** 把缩放限制在 1–5 倍，并把平移夹在允许范围内（容器、画面尺寸或边距变了也用它重新夹）。 */
fun clampState(
    s: ViewState, containerW: Float, containerH: Float, aspect: Float, insets: Edges = Edges.ZERO,
): ViewState {
    val (bw, bh) = fitSize(containerW, containerH, aspect)
    val z = s.zoom.coerceIn(MIN_ZOOM, MAX_ZOOM)
    return ViewState(
        zoom = z,
        panX = clampPan(s.panX, bw * z, containerW, insets.left, insets.right),
        panY = clampPan(s.panY, bh * z, containerH, insets.top, insets.bottom),
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
    insets: Edges = Edges.ZERO,
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
    return clampState(moved, containerW, containerH, aspect, insets)
}

fun applyPan(
    s: ViewState, dx: Float, dy: Float,
    containerW: Float, containerH: Float, aspect: Float,
    insets: Edges = Edges.ZERO,
): ViewState = clampState(s.copy(panX = s.panX + dx, panY = s.panY + dy), containerW, containerH, aspect, insets)

/** 依次执行手势识别给的变换。 */
fun applyOps(
    s: ViewState, ops: List<ViewOp>,
    containerW: Float, containerH: Float, aspect: Float,
    /** 键盘把画面往上顶了多少像素：手势给的是屏幕坐标，换成没顶之前的坐标再算缩放中心。 */
    imeShift: Float = 0f,
    insets: Edges = Edges.ZERO,
): ViewState = ops.fold(s) { acc, op ->
    when (op) {
        is ViewOp.Zoom -> applyZoom(acc, op.factor, op.cx, op.cy + imeShift, containerW, containerH, aspect, insets)
        is ViewOp.Pan -> applyPan(acc, op.dx, op.dy, containerW, containerH, aspect, insets)
        ViewOp.Haptic -> acc
        is ViewOp.Tap -> acc
    }
}

/** 缩放后才要告诉手势识别“放大了”（放大时单指拖动先用来平移）。 */
fun isZoomed(s: ViewState): Boolean = s.zoom > MIN_ZOOM + 1e-3f

