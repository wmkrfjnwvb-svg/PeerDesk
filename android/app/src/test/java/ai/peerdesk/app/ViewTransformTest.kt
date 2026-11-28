package ai.peerdesk.app

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class ViewTransformTest {
    private val w = 1000f
    private val h = 500f // 容器 2:1
    private val a = 16f / 9f
    private val eps = 0.01f

    @Test fun fitLetterboxes() {
        // 容器比画面宽：高度铺满，左右留黑边
        val (fw, fh) = fitSize(w, h, a)
        assertEquals(500f, fh, eps)
        assertEquals(500f * a, fw, eps)
        // 容器比画面窄：宽度铺满
        val (nw, nh) = fitSize(300f, 600f, a)
        assertEquals(300f, nw, eps)
        assertEquals(300f / a, nh, eps)
    }

    @Test fun aspectFallsBackTo16by9() {
        assertEquals(DEFAULT_ASPECT, aspectOf(0, 0), 1e-6f)
        assertEquals(2f, aspectOf(1000, 500), 1e-6f)
    }

    @Test fun unzoomedRectIsCentered() {
        val r = rectOf(ViewState(), w, h, a)
        assertEquals((w - r.width) / 2f, r.left, eps)
        assertEquals(0f, r.top, eps)
    }

    @Test fun zoomClampedTo1to5() {
        var s = applyZoom(ViewState(), 100f, 500f, 250f, w, h, a)
        assertEquals(5f, s.zoom, 1e-6f)
        s = applyZoom(s, 0.0001f, 500f, 250f, w, h, a)
        assertEquals(1f, s.zoom, 1e-6f)
        assertEquals(0f, s.panX, eps)
        assertEquals(0f, s.panY, eps)
    }

    @Test fun zoomKeepsFocusPointFixed() {
        val cx = 600f
        val cy = 100f
        val before = rectOf(ViewState(), w, h, a)
        val fx = (cx - before.left) / before.width
        val fy = (cy - before.top) / before.height
        val s = applyZoom(ViewState(), 2f, cx, cy, w, h, a)
        val after = rectOf(s, w, h, a)
        // 焦点下面的画面内容位置不变（没有被边缘夹取时）
        assertEquals(cx, after.left + fx * after.width, 0.5f)
        assertEquals(cy, after.top + fy * after.height, 0.5f)
    }

    @Test fun panCannotLeaveTheEdges() {
        var s = applyZoom(ViewState(), 3f, 500f, 250f, w, h, a)
        s = applyPan(s, 100000f, 100000f, w, h, a)
        var r = rectOf(s, w, h, a)
        assertEquals(0f, r.left, eps)   // 画面左边缘贴着容器左边缘
        assertEquals(0f, r.top, eps)
        s = applyPan(s, -200000f, -200000f, w, h, a)
        r = rectOf(s, w, h, a)
        assertEquals(w, r.left + r.width, eps)
        assertEquals(h, r.top + r.height, eps)
    }

    @Test fun noPanWhenPictureSmallerThanContainer() {
        // 缩放 1 倍：容器宽于画面，水平不能平移；垂直刚好铺满，也不能平移
        val s = applyPan(ViewState(), 50f, 50f, w, h, a)
        assertEquals(0f, s.panX, eps)
        assertEquals(0f, s.panY, eps)
    }

    @Test fun horizontalPanAllowedOnlyOnceWiderThanContainer() {
        // 画面 889 宽（500*16/9）；放大 1.2 倍后 1067 > 1000，才有 33 的余量
        val s = applyZoom(ViewState(), 1.2f, 500f, 250f, w, h, a)
        val moved = applyPan(s, 1000f, 0f, w, h, a)
        val r = rectOf(moved, w, h, a)
        assertEquals(0f, r.left, eps)
    }

    @Test fun opsApplyInOrder() {
        val s = applyOps(ViewState(), listOf(ViewOp.Zoom(2f, 500f, 250f), ViewOp.Pan(-30f, 0f)), w, h, a)
        assertEquals(2f, s.zoom, 1e-6f)
        assertTrue(s.panX < 0f)
    }

    @Test fun reclampWhenAspectChanges() {
        var s = applyZoom(ViewState(), 5f, 500f, 250f, w, h, a)
        s = applyPan(s, -100000f, 0f, w, h, a)
        // 远程画面变成竖屏比例：画面变窄，原来的平移要重新夹住
        val narrow = clampState(s, w, h, 0.5f)
        val r = rectOf(narrow, w, h, 0.5f)
        assertTrue(r.left <= 0.01f && r.left + r.width >= w - 0.01f)
    }

    @Test fun invalidFactorIgnored() {
        val s = ViewState(2f, 10f, 5f)
        assertEquals(s, applyZoom(s, Float.NaN, 0f, 0f, w, h, a))
        assertEquals(s, applyZoom(s, -1f, 0f, 0f, w, h, a))
    }

    @Test fun zoomedFlag() {
        assertFalse(isZoomed(ViewState()))
        assertTrue(isZoomed(ViewState(1.5f)))
    }

    // ---- 边距（工具条、刘海）----

    private val toolbar = Edges(0f, 60f, 0f, 0f)

    @Test fun zeroInsetsBehaveLikeBefore() {
        val s = ViewState(2f, 123f, -45f)
        assertEquals(clampState(s, w, h, a), clampState(s, w, h, a, Edges.ZERO))
    }

    @Test fun landscapeZoom1CanBePulledDownByTheToolbarHeight() {
        // 画面铺满高度；工具条 60 px：可以往下拉 60，让画面最上一行露出来；不能往上拉过头
        var s = applyPan(ViewState(), 0f, 100000f, w, h, a, toolbar)
        assertEquals(60f, rectOf(s, w, h, a).top, eps)
        s = applyPan(s, 0f, -100000f, w, h, a, toolbar)
        assertEquals(0f, rectOf(s, w, h, a).top, eps)
        assertEquals(h, rectOf(s, w, h, a).top + rectOf(s, w, h, a).height, eps)
    }

    @Test fun zoomedInTopAndBottomEdges() {
        var s = applyZoom(ViewState(), 3f, 500f, 250f, w, h, a, toolbar)
        s = applyPan(s, 0f, 100000f, w, h, a, toolbar)
        var r = rectOf(s, w, h, a)
        assertEquals("画面上沿最多拉到工具条下沿", 60f, r.top, eps)
        s = applyPan(s, 0f, -100000f, w, h, a, toolbar)
        r = rectOf(s, w, h, a)
        assertEquals("画面下沿最少停在容器底边", h, r.top + r.height, eps)
    }

    @Test fun pictureSmallerThanSafeSpanIsCenteredInIt() {
        val ins = Edges(0f, 100f, 0f, 50f)
        val s = clampState(ViewState(), 500f, 1000f, a, ins)
        val r = rectOf(s, 500f, 1000f, a)
        assertEquals(100f + (850f - r.height) / 2f, r.top, eps)
        // 平移不起作用
        val p = applyPan(s, 0f, 300f, 500f, 1000f, a, ins)
        assertEquals(r.top, rectOf(p, 500f, 1000f, a).top, eps)
    }

    @Test fun toolbarShownAndHiddenSwitchReclamps() {
        val cutout = Edges.ZERO
        val shown = safeInsets(true, 60f, cutout)
        val hidden = safeInsets(false, 60f, cutout)
        assertEquals(60f, shown.top, eps)
        assertEquals(0f, hidden.top, eps)
        // 工具条显示时拉下 60；收起后重新夹回 0
        val pulled = applyPan(ViewState(), 0f, 100000f, w, h, a, shown)
        assertEquals(60f, pulled.panY, eps)
        assertEquals(0f, clampState(pulled, w, h, a, hidden).panY, eps)
    }

    @Test fun cutoutBeatsSmallerToolbarAndCutoutSidesApply() {
        assertEquals(100f, safeInsets(true, 60f, Edges(0f, 100f, 0f, 0f)).top, eps)
        assertEquals(60f, safeInsets(true, 60f, Edges(0f, 20f, 0f, 0f)).top, eps)
        val ins = safeInsets(false, 0f, Edges(60f, 0f, 0f, 0f)) // 刘海在左边
        assertEquals(60f, ins.left, eps)
        // zoom 1：画面 889 宽 < 1000-60，居中在安全区里
        val s = clampState(ViewState(), w, h, a, ins)
        val r = rectOf(s, w, h, a)
        assertEquals(60f + (940f - r.width) / 2f, r.left, eps)
        // 放大后左边最多拉到刘海右沿，右边最少停在容器右边
        var z = applyZoom(ViewState(), 3f, 500f, 250f, w, h, a, ins)
        z = applyPan(z, 100000f, 0f, w, h, a, ins)
        assertEquals(60f, rectOf(z, w, h, a).left, eps)
        z = applyPan(z, -100000f, 0f, w, h, a, ins)
        assertEquals(w, rectOf(z, w, h, a).left + rectOf(z, w, h, a).width, eps)
        // 刘海在右边
        val right = Edges(0f, 0f, 60f, 0f)
        var q = applyZoom(ViewState(), 3f, 500f, 250f, w, h, a, right)
        q = applyPan(q, -100000f, 0f, w, h, a, right)
        assertEquals(w - 60f, rectOf(q, w, h, a).left + rectOf(q, w, h, a).width, eps)
    }

    @Test fun opsUseInsets() {
        val s = applyOps(ViewState(), listOf(ViewOp.Pan(0f, 100000f)), w, h, a, 0f, toolbar)
        assertEquals(60f, s.panY, eps)
    }
}
