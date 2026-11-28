package ai.peerdesk.app

import android.view.MotionEvent
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

class TouchForwardTest {
    private val ids = intArrayOf(7, 3)
    private val xs = floatArrayOf(10f, 20f)
    private val ys = floatArrayOf(11f, 21f)

    @Test fun downForwardsOnlyTheNewFinger() {
        assertEquals(listOf(TouchOp(7, 0, 10f, 11f)), touchOps(MotionEvent.ACTION_DOWN, 0, intArrayOf(7), floatArrayOf(10f), floatArrayOf(11f)))
        assertEquals(listOf(TouchOp(3, 0, 20f, 21f)), touchOps(MotionEvent.ACTION_POINTER_DOWN, 1, ids, xs, ys))
    }

    @Test fun pointerUpForwardsOnlyTheLiftedFinger() {
        assertEquals(listOf(TouchOp(7, 2, 10f, 11f)), touchOps(MotionEvent.ACTION_POINTER_UP, 0, ids, xs, ys))
        assertEquals(listOf(TouchOp(7, 2, 10f, 11f)), touchOps(MotionEvent.ACTION_UP, 0, intArrayOf(7), floatArrayOf(10f), floatArrayOf(11f)))
    }

    @Test fun moveAndCancelForwardEveryFinger() {
        assertEquals(listOf(TouchOp(7, 1, 10f, 11f), TouchOp(3, 1, 20f, 21f)), touchOps(MotionEvent.ACTION_MOVE, 0, ids, xs, ys))
        assertEquals(listOf(TouchOp(7, 3, 10f, 11f), TouchOp(3, 3, 20f, 21f)), touchOps(MotionEvent.ACTION_CANCEL, 0, ids, xs, ys))
    }

    @Test fun otherActionsAndBadIndexAreIgnored() {
        assertTrue(touchOps(MotionEvent.ACTION_HOVER_MOVE, 0, ids, xs, ys).isEmpty())
        assertTrue(touchOps(MotionEvent.ACTION_POINTER_DOWN, 5, ids, xs, ys).isEmpty())
    }

    @Test fun codeFormatting() {
        assertEquals("123456789", sanitizeCode("12a3-456 789012"))
        assertEquals("123-456-789", formatCode("123456789"))
        assertEquals("123-4", formatCode("1234"))
        assertEquals("", formatCode(""))
    }

    @Test fun codeCursorMapping() {
        assertEquals(0, codeDisplayOffset(0))
        assertEquals(3, codeDisplayOffset(3))
        assertEquals(5, codeDisplayOffset(4))
        assertEquals(11, codeDisplayOffset(9))
        for (o in 0..9) assertEquals(o, codeOriginalOffset(codeDisplayOffset(o)))
    }
}
