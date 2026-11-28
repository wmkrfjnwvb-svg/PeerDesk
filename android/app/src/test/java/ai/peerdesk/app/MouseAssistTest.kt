package ai.peerdesk.app

import android.view.MotionEvent
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class MouseAssistTest {
    private val sizes = MouseSizes(width = 100f, height = 150f, arrowH = 30f, closeSize = 30f, miniSize = 50f)
    private val cw = 1000f
    private val ch = 600f

    /** 放在容器正中间偏下。 */
    private val m = vmouseLayout(cw, ch, 0.5f, 0.7f, sizes)

    @Test fun tipIsAboveTheLeftButtonCenter() {
        assertEquals(450f + 25f, m.tipX, 0.01f)           // (1000-100)*0.5 + 100/4
        assertEquals(m.left.left + m.left.width / 2, m.tipX, 0.01f)
        assertEquals((600f - 180f) * 0.7f, m.tipY, 0.01f) // 顶边
    }

    @Test fun hitZones() {
        val l = m.bounds.left
        val t = m.bounds.top
        assertEquals(Zone.Left, m.hit(l + 10f, t + 30f + 10f))
        assertEquals(Zone.Right, m.hit(l + 90f, t + 30f + 10f))
        assertEquals(Zone.Body, m.hit(l + 50f, t + 30f + 120f))
        assertEquals(Zone.Close, m.hit(l + 90f, t + 10f))
        assertNull("箭头那一条除了关闭按钮都穿透", m.hit(l + 20f, t + 10f))
        assertNull(m.hit(l - 5f, t + 100f))
    }

    @Test fun leftRightSplitAtMiddleAndButtonsAreUpperHalfOfBody() {
        // 本体高 150，按键占 45% = 67.5
        assertEquals(67.5f, m.left.height, 0.01f)
        assertEquals(m.left.left + m.left.width, m.right.left, 0.01f)
        assertEquals(m.left.top + m.left.height, m.body.top, 0.01f)
        assertEquals(m.bounds.top + m.bounds.height, m.body.top + m.body.height, 0.01f)
    }

    @Test fun dragMovesTipAndStaysInsideContainer() {
        val (fx, fy) = dragMouseGroup(0.5f, 0.7f, 100f, -50f, cw, ch, sizes.width, sizes.arrowH + sizes.height)
        val moved = vmouseLayout(cw, ch, fx, fy, sizes)
        assertEquals(m.tipX + 100f, moved.tipX, 0.01f)
        assertEquals(m.tipY - 50f, moved.tipY, 0.01f)
        // 拖出容器：夹在边上
        val (ex, ey) = dragMouseGroup(0.5f, 0.7f, 100000f, 100000f, cw, ch, sizes.width, sizes.arrowH + sizes.height)
        val edge = vmouseLayout(cw, ch, ex, ey, sizes)
        assertEquals(cw, edge.bounds.left + edge.bounds.width, 0.01f)
        assertEquals(ch, edge.bounds.top + edge.bounds.height, 0.01f)
        val (sx, sy) = dragMouseGroup(0.5f, 0.7f, -100000f, -100000f, cw, ch, sizes.width, sizes.arrowH + sizes.height)
        val tl = vmouseLayout(cw, ch, sx, sy, sizes)
        assertEquals(0f, tl.bounds.left, 0.01f)
        assertEquals(0f, tl.tipY, 0.01f)
    }

    @Test fun miniIsInsideContainerAndDraggable() {
        val b = miniLayout(cw, ch, 1f, 1f, sizes)
        assertEquals(cw, b.left + b.width, 0.01f)
        assertEquals(ch, b.top + b.height, 0.01f)
        val (fx, fy) = dragMouseGroup(1f, 1f, -200f, -100f, cw, ch, sizes.miniSize, sizes.miniSize)
        val moved = miniLayout(cw, ch, fx, fy, sizes)
        assertEquals(b.left - 200f, moved.left, 0.01f)
        assertEquals(b.top - 100f, moved.top, 0.01f)
    }

    // ---- 路由 ----

    private fun route(r: TouchRouter, action: Int, idx: Int, vararg pts: Triple<Int, Float, Float>) =
        r.route(action, idx, pts.map { it.first }.toIntArray(), pts.map { it.second }.toFloatArray(), pts.map { it.third }.toFloatArray())

    private fun expandedRouter() = TouchRouter(8f).also { r -> r.hitTest = { x, y -> m.hit(x, y) } }

    private val lx get() = m.left.left + 10f
    private val ly get() = m.left.top + 10f

    @Test fun leftButtonDownMoveUp() {
        val r = expandedRouter()
        var res = route(r, MotionEvent.ACTION_DOWN, 0, Triple(1, lx, ly))
        assertTrue(res.touches.isEmpty())
        assertEquals(listOf<MouseEvent>(MouseEvent.ButtonDown(0)), res.mouse)
        res = route(r, MotionEvent.ACTION_MOVE, 0, Triple(1, lx + 20f, ly + 5f))
        assertEquals(listOf<MouseEvent>(MouseEvent.Move(20f, 5f)), res.mouse)
        assertTrue(res.touches.isEmpty())
        res = route(r, MotionEvent.ACTION_UP, 0, Triple(1, lx + 20f, ly + 5f))
        assertEquals(listOf(MouseEvent.ButtonUp(0), MouseEvent.DragEnd), res.mouse)
    }

    @Test fun plainTapOnLeftIsClickWithoutDragEnd() {
        val r = expandedRouter()
        route(r, MotionEvent.ACTION_DOWN, 0, Triple(1, lx, ly))
        val res = route(r, MotionEvent.ACTION_UP, 0, Triple(1, lx, ly))
        assertEquals(listOf<MouseEvent>(MouseEvent.ButtonUp(0)), res.mouse)
    }

    @Test fun rightButtonDoesNotDragTheMouse() {
        val r = expandedRouter()
        val rx = m.right.left + 10f
        var res = route(r, MotionEvent.ACTION_DOWN, 0, Triple(1, rx, ly))
        assertEquals(listOf<MouseEvent>(MouseEvent.ButtonDown(1)), res.mouse)
        res = route(r, MotionEvent.ACTION_MOVE, 0, Triple(1, rx + 50f, ly))
        assertTrue(res.mouse.isEmpty())
        res = route(r, MotionEvent.ACTION_CANCEL, 0, Triple(1, rx + 50f, ly))
        assertEquals(listOf<MouseEvent>(MouseEvent.ButtonUp(1)), res.mouse)
    }

    @Test fun bodyDragMovesMouseOnly() {
        val r = expandedRouter()
        val bx = m.body.left + 20f
        val by = m.body.top + 20f
        var res = route(r, MotionEvent.ACTION_DOWN, 0, Triple(2, bx, by))
        assertTrue(res.touches.isEmpty() && res.mouse.isEmpty())
        res = route(r, MotionEvent.ACTION_MOVE, 0, Triple(2, bx - 7f, by + 3f))
        assertEquals(listOf<MouseEvent>(MouseEvent.Move(-7f, 3f)), res.mouse)
        res = route(r, MotionEvent.ACTION_UP, 0, Triple(2, bx - 7f, by + 3f))
        assertEquals(listOf<MouseEvent>(MouseEvent.DragEnd), res.mouse)
    }

    @Test fun otherFingersStillGoToTouchWhileMouseHeld() {
        val r = expandedRouter()
        route(r, MotionEvent.ACTION_DOWN, 0, Triple(1, lx, ly))
        var res = route(r, MotionEvent.ACTION_POINTER_DOWN, 1, Triple(1, lx, ly), Triple(2, 5f, 5f))
        assertEquals(listOf(TouchOp(2, TouchPhase.DOWN, 5f, 5f)), res.touches)
        res = route(r, MotionEvent.ACTION_MOVE, 0, Triple(1, lx, ly), Triple(2, 9f, 9f))
        assertEquals(listOf(TouchOp(2, TouchPhase.MOVE, 9f, 9f)), res.moves)
        assertTrue(res.touches.isEmpty())
        assertTrue(res.mouse.isEmpty())
    }

    @Test fun closeTapCollapsesOnlyWhenReleasedOnIt() {
        val r = expandedRouter()
        val cx = m.close.left + 5f
        val cy = m.close.top + 5f
        route(r, MotionEvent.ACTION_DOWN, 0, Triple(1, cx, cy))
        assertEquals(listOf<MouseEvent>(MouseEvent.Collapse), route(r, MotionEvent.ACTION_UP, 0, Triple(1, cx, cy)).mouse)
        route(r, MotionEvent.ACTION_DOWN, 0, Triple(1, cx, cy))
        assertTrue(route(r, MotionEvent.ACTION_UP, 0, Triple(1, 5f, 5f)).mouse.isEmpty())
    }

    @Test fun miniTapExpandsDragMoves() {
        val mini = miniLayout(cw, ch, 1f, 0.5f, sizes)
        val r = TouchRouter(8f).also { it.hitTest = { x, y -> if (mini.contains(x, y)) Zone.Mini else null } }
        val x = mini.left + 10f
        val y = mini.top + 10f
        route(r, MotionEvent.ACTION_DOWN, 0, Triple(1, x, y))
        assertEquals(listOf<MouseEvent>(MouseEvent.Expand), route(r, MotionEvent.ACTION_UP, 0, Triple(1, x + 2f, y)).mouse)
        // 拖动：超过阈值后才移动，位移补上之前走过的
        route(r, MotionEvent.ACTION_DOWN, 0, Triple(1, x, y))
        assertTrue(route(r, MotionEvent.ACTION_MOVE, 0, Triple(1, x - 5f, y)).mouse.isEmpty())
        val mv = route(r, MotionEvent.ACTION_MOVE, 0, Triple(1, x - 30f, y - 10f))
        assertEquals(listOf<MouseEvent>(MouseEvent.MiniMove(-30f, -10f)), mv.mouse)
        val up = route(r, MotionEvent.ACTION_UP, 0, Triple(1, x - 30f, y - 10f))
        assertEquals(listOf<MouseEvent>(MouseEvent.DragEnd), up.mouse)
        assertTrue(up.touches.isEmpty())
    }

    @Test fun miniDoesNotSendInputsAndOutsideIsForwarded() {
        val mini = miniLayout(cw, ch, 1f, 0.5f, sizes)
        val r = TouchRouter(8f).also { it.hitTest = { x, y -> if (mini.contains(x, y)) Zone.Mini else null } }
        val res = route(r, MotionEvent.ACTION_DOWN, 0, Triple(1, 10f, 10f))
        assertEquals(1, res.touches.size)
        assertFalse(res.mouse.isNotEmpty())
    }

    @Test fun withoutHitTestEverythingIsForwarded() {
        val r = TouchRouter()
        val res = route(r, MotionEvent.ACTION_DOWN, 0, Triple(1, lx, ly))
        assertEquals(1, res.touches.size)
        assertTrue(res.mouse.isEmpty())
    }

    @Test fun fingerKeepsRoleAfterLeavingTheKey() {
        val r = expandedRouter()
        route(r, MotionEvent.ACTION_DOWN, 0, Triple(1, lx, ly))
        val res = route(r, MotionEvent.ACTION_MOVE, 0, Triple(1, 5f, 5f))
        assertTrue(res.touches.isEmpty())
    }

    @Test fun expandPlacesBigMouseAtMiniAndStaysOnScreen() {
        val mini = miniLayout(cw, ch, 0.3f, 0.4f, sizes)
        val (fx, fy) = expandedFrom(mini, cw, ch, sizes)
        val big = vmouseLayout(cw, ch, fx, fy, sizes)
        assertEquals(mini.left, big.bounds.left, 0.01f)
        assertEquals(mini.top, big.bounds.top, 0.01f)
    }

    @Test fun expandNearBottomRightIsClampedIntoContainer() {
        val mini = miniLayout(cw, ch, 1f, 1f, sizes)
        val (fx, fy) = expandedFrom(mini, cw, ch, sizes)
        val big = vmouseLayout(cw, ch, fx, fy, sizes)
        assertEquals(cw, big.bounds.left + big.bounds.width, 0.01f)
        assertEquals(ch, big.bounds.top + big.bounds.height, 0.01f)
    }

    @Test fun collapsePlacesMiniAtBigMouseAndStaysOnScreen() {
        val (fx, fy) = collapsedFrom(m.bounds, cw, ch, sizes)
        val mini = miniLayout(cw, ch, fx, fy, sizes)
        assertEquals(m.bounds.left, mini.left, 0.01f)
        assertEquals(m.bounds.top, mini.top, 0.01f)
        val bottom = vmouseLayout(cw, ch, 1f, 1f, sizes)
        val (bx, by) = collapsedFrom(bottom.bounds, cw, ch, sizes)
        val mb = miniLayout(cw, ch, bx, by, sizes)
        assertTrue(mb.left + mb.width <= cw + 0.01f && mb.top + mb.height <= ch + 0.01f)
    }

    @Test fun tipMayBeOutsideThePictureButInsideTheScreen() {
        // 竖屏：画面在中间，大鼠标放在上面的空白处也行；尖只要在屏幕里
        val top = vmouseLayout(cw, ch, 0.5f, 0f, sizes)
        assertEquals(0f, top.tipY, 0.01f)
        assertTrue(top.tipX in 0f..cw)
    }

    @Test fun moveOfAllForwardedFingersComesTogether() {
        val r = TouchRouter().also { it.hitTest = null }
        route(r, MotionEvent.ACTION_DOWN, 0, Triple(1, 10f, 10f))
        route(r, MotionEvent.ACTION_POINTER_DOWN, 1, Triple(1, 10f, 10f), Triple(2, 50f, 50f))
        val res = route(r, MotionEvent.ACTION_MOVE, 0, Triple(1, 12f, 11f), Triple(2, 48f, 52f))
        assertTrue("MOVE 不走逐根的 touch", res.touches.isEmpty())
        assertEquals(listOf(TouchOp(1, TouchPhase.MOVE, 12f, 11f), TouchOp(2, TouchPhase.MOVE, 48f, 52f)), res.moves)
        val up = route(r, MotionEvent.ACTION_POINTER_UP, 1, Triple(1, 12f, 11f), Triple(2, 48f, 52f))
        assertEquals(listOf(TouchOp(2, TouchPhase.UP, 48f, 52f)), up.touches)
        assertTrue(up.moves.isEmpty())
    }

    @Test fun mouseFingerIsLeftOutOfTheMoves() {
        val r = expandedRouter()
        route(r, MotionEvent.ACTION_DOWN, 0, Triple(1, lx, ly))
        route(r, MotionEvent.ACTION_POINTER_DOWN, 1, Triple(1, lx, ly), Triple(2, 5f, 5f))
        val res = route(r, MotionEvent.ACTION_MOVE, 0, Triple(1, lx + 3f, ly), Triple(2, 9f, 9f))
        assertEquals(listOf(TouchOp(2, TouchPhase.MOVE, 9f, 9f)), res.moves)
        assertEquals(listOf<MouseEvent>(MouseEvent.Move(3f, 0f)), res.mouse)
    }
}
