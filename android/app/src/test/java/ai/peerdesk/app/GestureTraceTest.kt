package ai.peerdesk.app

import org.json.JSONObject
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

class GestureTraceTest {
    @Test fun headerAndLines() {
        val t = GestureTrace()
        t.addContainer(900, 1080f, 2200f)
        t.addMotion(1000, 0, 0, intArrayOf(0), floatArrayOf(10.5f), floatArrayOf(20f))
        t.addMotion(1016, 5, 1, intArrayOf(0, 1), floatArrayOf(10.5f, 300.25f), floatArrayOf(20f, 400f))
        t.addRect(1020, ViewRect(0f, 100f, 1080f, 607.5f))
        val lines = t.toJsonLines(2.75f, 1080f, 2200f).trimEnd().split("\n")
        assertEquals(5, lines.size)
        val h = JSONObject(lines[0])
        assertEquals(1, h.getInt("v"))
        assertEquals(2.75, h.getDouble("density"), 1e-9)
        assertEquals(1080.0, h.getJSONArray("container").getDouble(0), 1e-9)
        assertEquals(2200.0, h.getJSONArray("container").getDouble(1), 1e-9)
        val c = JSONObject(lines[1])
        assertEquals(900, c.getLong("t"))
        assertEquals(2200.0, c.getJSONArray("container").getDouble(1), 1e-9)
        val m = JSONObject(lines[3])
        assertEquals(1016, m.getLong("t"))
        assertEquals(5, m.getInt("a"))
        assertEquals(1, m.getInt("i"))
        val p = m.getJSONArray("p")
        assertEquals(2, p.length())
        assertEquals(1, p.getJSONArray(1).getInt(0))
        assertEquals(300.25, p.getJSONArray(1).getDouble(1), 1e-9)
        val r = JSONObject(lines[4])
        assertEquals(1020, r.getLong("t"))
        assertEquals(607.5, r.getJSONArray("rect").getDouble(3), 1e-9)
    }

    @Test fun numbersAreCompactAndFinite() {
        val t = GestureTrace()
        t.addMotion(1, 2, 0, intArrayOf(3), floatArrayOf(Float.NaN), floatArrayOf(1.0f / 3f))
        val line = t.toJsonLines(1f, 10f, 10f).trimEnd().split("\n")[1]
        assertEquals("""{"t":1,"a":2,"i":0,"p":[[3,0,0.333]]}""", line)
    }

    @Test fun keepsOnlyTheLast120Seconds() {
        val t = GestureTrace()
        for (s in 0..300) t.addMotion(s * 1000L, 2, 0, intArrayOf(0), floatArrayOf(0f), floatArrayOf(0f))
        // 最新 300 s，窗口 120 s：保留 180..300 共 121 条
        assertEquals(121, t.size())
        val first = JSONObject(t.toJsonLines(1f, 1f, 1f).split("\n")[1])
        assertEquals(180_000L, first.getLong("t"))
        t.clear()
        assertEquals(0, t.size())
        assertTrue(t.toJsonLines(1f, 1f, 1f).trimEnd().lines().size == 1)
    }

    @Test fun mismatchedArrayLengthsUseTheShortest() {
        val t = GestureTrace()
        t.addMotion(1, 2, 0, intArrayOf(0, 1), floatArrayOf(1f), floatArrayOf(1f, 2f))
        val m = JSONObject(t.toJsonLines(1f, 1f, 1f).split("\n")[1])
        assertEquals(1, m.getJSONArray("p").length())
    }
}
