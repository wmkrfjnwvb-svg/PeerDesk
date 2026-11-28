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
}
