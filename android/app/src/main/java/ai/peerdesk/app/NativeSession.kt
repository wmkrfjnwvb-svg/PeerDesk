package ai.peerdesk.app

import java.util.concurrent.locks.ReentrantReadWriteLock
import kotlin.concurrent.read
import kotlin.concurrent.write

/**
 * 包住一个 Rust 句柄，保证 `free` 只调一次、之后任何调用都安全地什么也不做。
 *
 * 普通调用拿读锁（可以并发：事件线程在 `nextEvent` 里最多等 200 ms，不能挡住触摸）；
 * `free` 拿写锁，等所有在途调用返回后再释放，所以不会出现“释放了还在用”。
 */
class NativeSession(private val handle: Long) {
    private val lock = ReentrantReadWriteLock()
    private var freed = false

    private inline fun <T> use(whenFreed: T, block: (Long) -> T): T =
        lock.read { if (freed) whenFreed else block(handle) }

    fun nextEvent(timeoutMs: Int): String? = use(null) { Native.nextEvent(it, timeoutMs) }
    fun touch(id: Int, phase: Int, x: Float, y: Float, tMs: Long): String? =
        use(null) { Native.touch(it, id, phase, x, y, tMs) }
    fun tick(tMs: Long): String? = use(null) { Native.tick(it, tMs) }
    fun setView(rect: ViewRect, viewW: Int, viewH: Int) = use(Unit) {
        Native.setView(it, rect.left, rect.top, rect.width, rect.height, viewW, viewH)
    }
    fun button(button: Int, down: Boolean) = use(Unit) { Native.button(it, button, down) }
    fun pointAt(x: Float, y: Float) = use(Unit) { Native.pointAt(it, x, y) }
    fun key(keycode: Int, down: Boolean): Boolean = use(false) { Native.key(it, keycode, down) }
    fun text(text: String) = use(Unit) { Native.text(it, text) }
    fun releaseAll() = use(Unit) { Native.releaseAll(it) }
    fun stop() = use(Unit) { Native.stop(it) }

    /** 释放句柄。可以重复调用，只有第一次生效。 */
    fun free() {
        lock.write {
            if (!freed) {
                freed = true
                Native.free(handle)
            }
        }
    }
}
