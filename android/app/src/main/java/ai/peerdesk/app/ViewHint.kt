package ai.peerdesk.app

/**
 * 给被控端的“编码尺寸”提示（`setView` 的 viewW/viewH）的防抖。
 *
 * 画面矩形每次变化都要告诉核心（触点换算要用），但被控端按这个尺寸编码：每变一次就多一帧，网络弱时被控端
 * 已经缩小了画面还会切换编码分辨率。所以尺寸提示只在最后一次变化后 [delayMs] 才更新，之前一直用上一次的值。
 *
 * 纯逻辑，时钟由调用方注入，好在 JVM 上测。第一次提交立刻生效（会话开头要有个尺寸）。
 */
class SizeHintDebouncer(private val delayMs: Long, private val clock: () -> Long) {
    private var applied: Pair<Int, Int>? = null
    private var pending: Pair<Int, Int>? = null
    private var changedAt = 0L

    /** 提交最新的尺寸，返回现在该告诉被控端的尺寸（可能还是旧值）。 */
    @Synchronized
    fun submit(w: Int, h: Int): Pair<Int, Int> {
        val size = w to h
        val cur = applied
        if (cur == null) {
            applied = size
            return size
        }
        if (size == cur) {
            pending = null // 变回原来的值：不用改
        } else if (size != pending) {
            pending = size
            changedAt = clock()
        }
        return settle(cur)
    }

    /** 不提交新值，只看有没有已经等够时间的待定值；返回当前该用的尺寸（没提交过为 null）。 */
    @Synchronized
    fun poll(): Pair<Int, Int>? {
        val cur = applied ?: return null
        return settle(cur)
    }

    /** 还有待定的值在等。 */
    @Synchronized
    fun hasPending(): Boolean = pending != null

    private fun settle(cur: Pair<Int, Int>): Pair<Int, Int> {
        val p = pending
        if (p != null && clock() - changedAt >= delayMs) {
            applied = p
            pending = null
            return p
        }
        return cur
    }

    @Synchronized
    fun reset() {
        applied = null
        pending = null
    }
}
