package ai.peerdesk.app

/** 手势记录里的一个手指：编号和坐标（屏幕像素）。 */
data class TracePointer(val id: Int, val x: Float, val y: Float)

/** 记录里的一条。时间是 `SystemClock.uptimeMillis()` 同一个钟（MotionEvent.eventTime）。 */
sealed interface TraceEntry {
    val t: Long
    data class Motion(override val t: Long, val action: Int, val index: Int, val pointers: List<TracePointer>) : TraceEntry
    data class Rect(override val t: Long, val l: Float, val top: Float, val w: Float, val h: Float) : TraceEntry
    data class Container(override val t: Long, val w: Float, val h: Float) : TraceEntry
}

/**
 * 最近 [windowMs]（默认 120 秒）的原始触摸记录环形缓冲，导出成 JSON Lines，下次真机出问题时带数据回来。
 *
 * 内容：每个 MotionEvent 的 actionMasked、actionIndex 和所有手指的编号、坐标；每次画面矩形变化；容器尺寸变化。
 * 格式（每行一个 JSON）：
 * - 第一行 `{"v":1,"density":d,"container":[w,h]}`
 * - 触摸 `{"t":ms,"a":action,"i":index,"p":[[id,x,y],...]}`
 * - 画面矩形 `{"t":ms,"rect":[l,t,w,h]}`
 * - 容器尺寸变化 `{"t":ms,"container":[w,h]}`（比头部多出来的，头部只有导出时的尺寸）
 *
 * 线程安全：触摸在主线程记，矩形在主线程或事件线程记。
 */
class GestureTrace(private val windowMs: Long = 120_000L) {
    private val entries = ArrayDeque<TraceEntry>()

    @Synchronized
    fun addMotion(t: Long, action: Int, index: Int, ids: IntArray, xs: FloatArray, ys: FloatArray) {
        val n = minOf(ids.size, xs.size, ys.size)
        add(TraceEntry.Motion(t, action, index, List(n) { TracePointer(ids[it], xs[it], ys[it]) }))
    }

    @Synchronized
    fun addRect(t: Long, r: ViewRect) = add(TraceEntry.Rect(t, r.left, r.top, r.width, r.height))

    @Synchronized
    fun addContainer(t: Long, w: Float, h: Float) = add(TraceEntry.Container(t, w, h))

    @Synchronized
    fun clear() = entries.clear()

    @Synchronized
    fun size(): Int = entries.size

    private fun add(e: TraceEntry) {
        entries.addLast(e)
        // 以最新一条为准，丢掉比窗口更老的
        while (entries.isNotEmpty() && e.t - entries.first().t > windowMs) entries.removeFirst()
    }

    @Synchronized
    fun toJsonLines(density: Float, containerW: Float, containerH: Float): String = buildString {
        append("{\"v\":1,\"density\":").append(num(density)).append(",\"container\":[")
            .append(num(containerW)).append(',').append(num(containerH)).append("]}\n")
        for (e in entries) {
            when (e) {
                is TraceEntry.Motion -> {
                    append("{\"t\":").append(e.t).append(",\"a\":").append(e.action).append(",\"i\":").append(e.index)
                        .append(",\"p\":[")
                    e.pointers.forEachIndexed { k, p ->
                        if (k > 0) append(',')
                        append('[').append(p.id).append(',').append(num(p.x)).append(',').append(num(p.y)).append(']')
                    }
                    append("]}\n")
                }
                is TraceEntry.Rect ->
                    append("{\"t\":").append(e.t).append(",\"rect\":[").append(num(e.l)).append(',').append(num(e.top))
                        .append(',').append(num(e.w)).append(',').append(num(e.h)).append("]}\n")
                is TraceEntry.Container ->
                    append("{\"t\":").append(e.t).append(",\"container\":[").append(num(e.w)).append(',')
                        .append(num(e.h)).append("]}\n")
            }
        }
    }

    private fun num(f: Float): String {
        if (f.isNaN() || f.isInfinite()) return "0"
        // 最多三位小数，去掉多余的 0；不用科学计数法
        val s = String.format(java.util.Locale.ROOT, "%.3f", f).trimEnd('0').trimEnd('.')
        return if (s.isEmpty() || s == "-0") "0" else s
    }
}
