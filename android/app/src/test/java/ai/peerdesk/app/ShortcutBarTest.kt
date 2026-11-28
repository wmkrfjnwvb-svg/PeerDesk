package ai.peerdesk.app

import android.view.KeyEvent
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class ShortcutBarTest {
    private val ctrl = KeyEvent.KEYCODE_CTRL_LEFT
    private val c = KeyEvent.KEYCODE_C

    @Test fun ctrlLockedThenCSendsFullSequence() {
        val bar = ShortcutBarState()
        bar.toggle(ModKey.Ctrl)
        assertTrue(bar.isLocked(ModKey.Ctrl))
        val seq = bar.typed("c")
        assertEquals(
            listOf(KeyAction(ctrl, true), KeyAction(c, true), KeyAction(c, false), KeyAction(ctrl, false)),
            seq,
        )
        assertFalse("按完自动松开", bar.isLocked(ModKey.Ctrl))
        assertNull("下一个字不再带 Ctrl", bar.typed("c"))
    }

    @Test fun toggleTwiceUnlocks() {
        val bar = ShortcutBarState()
        bar.toggle(ModKey.Alt)
        bar.toggle(ModKey.Alt)
        assertFalse(bar.isLocked(ModKey.Alt))
        assertEquals(listOf(KeyAction(KeyEvent.KEYCODE_TAB, true), KeyAction(KeyEvent.KEYCODE_TAB, false)), bar.press(KeyEvent.KEYCODE_TAB))
    }

    @Test fun twoModifiersReleaseInReverseOrder() {
        val bar = ShortcutBarState()
        bar.toggle(ModKey.Ctrl)
        bar.toggle(ModKey.Alt)
        val seq = bar.press(KeyEvent.KEYCODE_DPAD_LEFT)
        val alt = KeyEvent.KEYCODE_ALT_LEFT
        val left = KeyEvent.KEYCODE_DPAD_LEFT
        assertEquals(
            listOf(
                KeyAction(ctrl, true), KeyAction(alt, true),
                KeyAction(left, true), KeyAction(left, false),
                KeyAction(alt, false), KeyAction(ctrl, false),
            ),
            seq,
        )
    }

    @Test fun lockedWithNonKeyCharReleasesAndSendsText() {
        val bar = ShortcutBarState()
        bar.toggle(ModKey.Ctrl)
        assertNull(bar.typed("好"))
        assertFalse(bar.isLocked(ModKey.Ctrl))
    }

    @Test fun uppercaseAndDigitMapToKeycodes() {
        assertEquals(KeyEvent.KEYCODE_A, ShortcutBarState.keycodeForChar('A'))
        assertEquals(KeyEvent.KEYCODE_Z, ShortcutBarState.keycodeForChar('z'))
        assertEquals(KeyEvent.KEYCODE_5, ShortcutBarState.keycodeForChar('5'))
        assertNull(ShortcutBarState.keycodeForChar('!'))
    }

    @Test fun copyPasteUseCtrlOnWindowsAndCmdOnMac() {
        val win = ShortcutBarState.shortcut(PeerOs.Windows, KeyEvent.KEYCODE_V)
        assertEquals(KeyAction(KeyEvent.KEYCODE_CTRL_LEFT, true), win.first())
        val mac = ShortcutBarState.shortcut(PeerOs.MacOs, KeyEvent.KEYCODE_C)
        assertEquals(
            listOf(
                KeyAction(KeyEvent.KEYCODE_META_LEFT, true), KeyAction(c, true),
                KeyAction(c, false), KeyAction(KeyEvent.KEYCODE_META_LEFT, false),
            ),
            mac,
        )
        assertEquals(KeyAction(KeyEvent.KEYCODE_CTRL_LEFT, true), ShortcutBarState.shortcut(PeerOs.Unknown, c).first())
    }

    @Test fun metaLabel() {
        assertTrue(metaLabelIsCmd(PeerOs.MacOs))
        assertFalse(metaLabelIsCmd(PeerOs.Windows))
        assertFalse(metaLabelIsCmd(PeerOs.Linux))
    }

    @Test fun resetClearsLocks() {
        val bar = ShortcutBarState()
        bar.toggle(ModKey.Meta)
        bar.reset()
        assertFalse(bar.isLocked(ModKey.Meta))
    }
}
