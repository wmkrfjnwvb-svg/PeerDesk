package ai.peerdesk.app

import android.view.KeyEvent

/** 一次按键动作：Android 键码加按下/抬起。直接交给 `Native.key`。 */
data class KeyAction(val keycode: Int, val down: Boolean)

enum class ModKey(val keycode: Int) {
    Ctrl(KeyEvent.KEYCODE_CTRL_LEFT),
    Alt(KeyEvent.KEYCODE_ALT_LEFT),
    /** Windows 键 / Mac 的 Cmd：都是 META_LEFT，被控端按系统理解。 */
    Meta(KeyEvent.KEYCODE_META_LEFT),
}

/**
 * 快捷键栏的状态机。修饰键“按一下锁住，下一个键按完自动松开”：
 *
 * - 修饰键按钮：未锁 → 锁住（此时不发任何按键，只记状态）；已锁 → 再按一下取消锁定。
 * - 下一个键（栏上的键、软键盘打的字）：先按下所有锁住的修饰键，再按下并抬起这个键，再按相反顺序抬起修饰键，然后清空锁。
 *   这样每个动作的按下/抬起都成对，不会把对方的修饰键留在按住状态。
 * - 锁住期间退出会话/断开时，没有发出过任何按下，所以没有要释放的（`reset` 只清状态）。
 */
class ShortcutBarState {
    private val locked = linkedSetOf<ModKey>()

    fun isLocked(m: ModKey): Boolean = m in locked

    fun toggle(m: ModKey) {
        if (!locked.remove(m)) locked.add(m)
    }

    fun reset() = locked.clear()

    /** 栏上的普通键（Esc、Tab、方向键）或软键盘里的键：带上锁住的修饰键发出，并清掉锁。 */
    fun press(keycode: Int): List<KeyAction> {
        val mods = locked.toList()
        locked.clear()
        return chord(mods.map { it.keycode }, keycode)
    }

    /**
     * 软键盘打出的字。没有修饰键锁住时原样当文字发（返回 null）；
     * 锁住了修饰键，并且是字母/数字时，转成按键序列（例如 Ctrl 锁住后打 "c" 发出 Ctrl+C）。
     * 锁住修饰键时打了没法转成键的字符，修饰键照样释放（锁清掉），文字原样发。
     */
    fun typed(text: String): List<KeyAction>? {
        if (locked.isEmpty()) return null
        val key = if (text.length == 1) keycodeForChar(text[0]) else null
        if (key == null) {
            locked.clear()
            return null
        }
        return press(key)
    }

    companion object {
        /** 快捷组合（Ctrl+C、Ctrl+V，对方是 Mac 时用 Cmd）。不动当前的锁。 */
        fun shortcut(peer: PeerOs, keycode: Int): List<KeyAction> {
            val mod = if (peer == PeerOs.MacOs) ModKey.Meta else ModKey.Ctrl
            return chord(listOf(mod.keycode), keycode)
        }

        fun chord(modifiers: List<Int>, keycode: Int): List<KeyAction> = buildList {
            modifiers.forEach { add(KeyAction(it, true)) }
            add(KeyAction(keycode, true))
            add(KeyAction(keycode, false))
            modifiers.asReversed().forEach { add(KeyAction(it, false)) }
        }

        /** 字母和数字对应的 Android 键码；其它字符返回 null。 */
        fun keycodeForChar(c: Char): Int? = when (c) {
            in 'a'..'z' -> KeyEvent.KEYCODE_A + (c - 'a')
            in 'A'..'Z' -> KeyEvent.KEYCODE_A + (c - 'A')
            in '0'..'9' -> KeyEvent.KEYCODE_0 + (c - '0')
            else -> null
        }
    }
}

/** 对方系统决定 Meta 键上显示的字：Mac 显示 Cmd，其它显示 Win（都发 KEYCODE_META_LEFT）。 */
fun metaLabelIsCmd(peer: PeerOs): Boolean = peer == PeerOs.MacOs
