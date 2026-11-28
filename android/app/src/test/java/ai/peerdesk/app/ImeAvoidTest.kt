package ai.peerdesk.app

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Test

class ImeAvoidTest {
    private val margin = 48f
    private val top = 16f
    private val screenH = 2000f
    private val kbTop = 1200f // 可见区域下沿

    @Test fun openPointNotCoveredNoShift() {
        assertEquals(0f, imeShiftOnOpen(800f, kbTop, margin), 1e-4f)
        // 刚好离键盘 margin：不动
        assertEquals(0f, imeShiftOnOpen(kbTop - margin, kbTop, margin), 1e-4f)
    }

    @Test fun openPointCoveredShiftsUpToMarginAboveKeyboard() {
        val s = imeShiftOnOpen(1500f, kbTop, margin)
        assertEquals(1500f + margin - kbTop, s, 1e-4f)
        assertEquals(kbTop - margin, 1500f - s, 1e-4f) // 点现在离键盘 48
    }

    @Test fun openPointAtTheVeryBottom() {
        val s = imeShiftOnOpen(screenH, kbTop, margin)
        assertEquals(screenH + margin - kbTop, s, 1e-4f)
        // 桌面下沿（这里就是点的位置）被顶到键盘上方 margin 处
        assertEquals(kbTop - margin, screenH - s, 1e-4f)
    }

    @Test fun newPointInsideRangeLeavesShiftAlone() {
        // 当前顶起 300；新点 y=1300：范围 [1300+48-1200=148, 1300-16=1284]，300 在里面
        assertEquals(300f, imeShiftOnNewPoint(300f, 1300f, kbTop, margin, top), 1e-4f)
    }

    @Test fun newPointStillCoveredMovesMinimally() {
        // 新点 y=1700：范围 [548, 1684]，当前 300 太小 → 夹到下端 548
        assertEquals(548f, imeShiftOnNewPoint(300f, 1700f, kbTop, margin, top), 1e-4f)
    }

    @Test fun newPointNearTopComesBackDown() {
        // 当前顶起 900；新点 y=500：范围 [-652, 484]，900 太大 → 夹到上端 484，点离屏幕上沿 16
        val s = imeShiftOnNewPoint(900f, 500f, kbTop, margin, top)
        assertEquals(484f, s, 1e-4f)
        assertEquals(top, 500f - s, 1e-4f)
    }

    @Test fun newPointFarAboveNeverNegative() {
        // 点在 y=10：高端 = -6 → 顶起量不会小于 0
        assertEquals(0f, imeShiftOnNewPoint(0f, 10f, kbTop, margin, top), 1e-4f)
    }

    @Test fun keyboardHeightChangeRecomputesTheSameWay() {
        val p = 1500f
        val open = imeShiftOnOpen(p, kbTop, margin)         // 键盘顶 1200 → 348
        val taller = imeShiftOnNewPoint(open, p, 1000f, margin, top) // 键盘变高：下端上移，当前不够 → 548
        assertEquals(p + margin - 1000f, taller, 1e-4f)
        val shorter = imeShiftOnNewPoint(taller, p, 1400f, margin, top) // 键盘变矮：当前仍在范围内，不动
        assertEquals(taller, shorter, 1e-4f)
    }

    @Test fun pointFractionRoundTrip() {
        val shown = ViewRect(100f, 50f, 800f, 400f)
        val (fx, fy) = toFraction(shown, 500f, 250f)!!
        assertEquals(0.5f, fx, 1e-5f)
        assertEquals(0.5f, fy, 1e-5f)
        // 画面被顶起 100 以后，未顶起矩形里的 y
        val unshifted = shown.copy(top = shown.top + 100f)
        assertEquals(350f, inputPointY(unshifted, fy), 1e-4f)
        assertNull(toFraction(ViewRect(0f, 0f, 0f, 0f), 1f, 1f))
        assertNotNull(toFraction(shown, 0f, 0f))
    }
}
