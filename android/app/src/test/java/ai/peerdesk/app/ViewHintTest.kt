package ai.peerdesk.app

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class ViewHintTest {
    private var now = 0L
    private val d = SizeHintDebouncer(300) { now }

    @Test fun firstSubmitIsImmediate() {
        assertNull(d.poll())
        assertEquals(1000 to 500, d.submit(1000, 500))
        assertFalse(d.hasPending())
    }

    @Test fun changeIsHeldBackUntil300msAfterTheLastChange() {
        d.submit(1000, 500)
        now = 100
        assertEquals(1000 to 500, d.submit(1100, 550))
        assertTrue(d.hasPending())
        // 连续变化：每次都重新计时
        now = 350
        assertEquals(1000 to 500, d.submit(1200, 600))
        now = 600
        assertEquals("离最后一次变化只有 250 ms", 1000 to 500, d.submit(1200, 600))
        now = 650
        assertEquals(1200 to 600, d.submit(1200, 600))
        assertFalse(d.hasPending())
    }

    @Test fun pollSettlesWithoutNewSubmit() {
        d.submit(1000, 500)
        now = 10
        d.submit(1500, 750)
        now = 309
        assertEquals(1000 to 500, d.poll())
        now = 310
        assertEquals(1500 to 750, d.poll())
        assertEquals(1500 to 750, d.poll())
    }

    @Test fun changingBackCancelsThePending() {
        d.submit(1000, 500)
        now = 10
        d.submit(1500, 750)
        now = 20
        assertEquals(1000 to 500, d.submit(1000, 500))
        assertFalse(d.hasPending())
        now = 1000
        assertEquals(1000 to 500, d.poll())
    }

    @Test fun sameValueDoesNotRestartTheTimer() {
        d.submit(1000, 500)
        now = 10
        d.submit(1500, 750)
        now = 200
        d.submit(1500, 750) // 值没变，不重新计时
        now = 310
        assertEquals(1500 to 750, d.submit(1500, 750))
    }

    @Test fun resetForgetsEverything() {
        d.submit(1000, 500)
        d.reset()
        assertNull(d.poll())
        assertEquals(800 to 400, d.submit(800, 400))
    }
}
