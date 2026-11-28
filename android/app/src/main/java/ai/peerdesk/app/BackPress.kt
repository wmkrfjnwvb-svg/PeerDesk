package ai.peerdesk.app

enum class BackAction {
    /** 第一次返回：显示工具条和“再按一次返回键断开”的提示。 */
    ShowHint,
    /** 2 秒内第二次返回：断开。 */
    Disconnect,
}

/**
 * 会话中的返回键：边缘滑动手势操作时容易误触，所以第一次只提示，[windowMs] 内再按一次才断开。
 * 纯逻辑，时钟由调用方注入。
 */
class BackPressTracker(private val windowMs: Long = 2000L, private val clock: () -> Long) {
    private var firstAt: Long? = null

    fun onBack(): BackAction {
        val now = clock()
        val first = firstAt
        return if (first != null && now - first <= windowMs) {
            firstAt = null
            BackAction.Disconnect
        } else {
            firstAt = now
            BackAction.ShowHint
        }
    }

    fun reset() {
        firstAt = null
    }
}
