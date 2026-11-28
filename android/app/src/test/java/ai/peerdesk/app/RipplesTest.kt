package ai.peerdesk.app

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class RipplesTest {
    private fun r(id: Long, t: Long, button: Int = 0) = Ripple(id, 10f, 20f, button, t)

    @Test fun progressRadiusAlpha() {
        val a = r(1, 1000)
        assertEquals(0f, rippleProgress(a, 1000), 1e-6f)
        assertEquals(0.5f, rippleProgress(a, 1175), 1e-3f)
        assertEquals(1f, rippleProgress(a, 5000), 1e-6f)
        assertEquals(0f, rippleProgress(a, 900), 1e-6f)
        assertEquals(8f, rippleRadiusDp(0f), 1e-4f)
        assertEquals(28f, rippleRadiusDp(1f), 1e-4f)
        assertTrue("先快后慢", rippleRadiusDp(0.5f) > 18f)
        assertEquals(0.35f, rippleAlpha(0f), 1e-6f)
        assertEquals(0f, rippleAlpha(1f), 1e-6f)
    }

    @Test fun expiresAfterDuration() {
        val a = r(1, 1000)
        assertFalse(isExpired(a, 1000 + RIPPLE_DURATION_MS - 1))
        assertTrue(isExpired(a, 1000 + RIPPLE_DURATION_MS))
    }

    @Test fun overlapAndCleanup() {
        var list = addRipple(emptyList(), r(1, 1000), 1000)
        list = addRipple(list, r(2, 1100, 1), 1100) // 叠加
        assertEquals(listOf(1L, 2L), list.map { it.id })
        // 第一个过期后新加一个：旧的被清掉
        list = addRipple(list, r(3, 1400), 1400)
        assertEquals(listOf(2L, 3L), list.map { it.id })
        assertEquals(emptyList<Ripple>(), pruneRipples(list, 5000))
        assertEquals(listOf(3L), pruneRipples(list, 1460).map { it.id })
    }

    @Test fun capDropsTheOldest() {
        var list = emptyList<Ripple>()
        for (i in 0 until RIPPLE_MAX_COUNT + 5) list = addRipple(list, r(i.toLong(), 1000), 1000)
        assertEquals(RIPPLE_MAX_COUNT, list.size)
        assertEquals(5L, list.first().id)
    }

    @Test fun arrowShrinksToEightyPercent() {
        assertEquals(0.8f, ARROW_PRESSED_SCALE, 1e-6f)
    }
}
