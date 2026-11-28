package ai.peerdesk.app

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class EventsTest {
    @Test fun connected() {
        val e = parseEvent("""{"type":"connected","route":"Relay","detail":"x"}""")
        assertEquals(CoreEvent.Connected(Route.Relay, "x"), e)
    }

    @Test fun peerMac() {
        val e = parseEvent("""{"type":"peer","os":"MacOs","name":"mini"}""")
        assertEquals(CoreEvent.Peer(PeerOs.MacOs, "mini"), e)
    }

    @Test fun peerUnknownOs() {
        assertEquals(PeerOs.Unknown, (parseEvent("""{"type":"peer","os":"Plan9","name":"a"}""") as CoreEvent.Peer).os)
    }

    @Test fun qualityWithRouter() {
        val e = parseEvent(
            """{"type":"quality","rtt_ms":12,"loss_permille":3,"queue_ms":5,"fps":30,"recv_kbps":2000,""" +
                """"decode_ms":4,"width":1280,"height":720,"router_ms":3,"decoder":"mc","fec":"RS"}""",
        ) as CoreEvent.Quality
        assertEquals(12, e.rttMs)
        assertEquals(3, e.lossPermille)
        assertEquals(2000, e.recvKbps)
        assertEquals(1280, e.width)
        assertEquals(3, e.routerMs)
        assertEquals("RS", e.fec)
    }

    @Test fun qualityRouterNull() {
        val e = parseEvent(
            """{"type":"quality","rtt_ms":12,"loss_permille":3,"queue_ms":5,"fps":30,"recv_kbps":2000,""" +
                """"decode_ms":4,"width":1280,"height":720,"router_ms":null,"decoder":"mc","fec":"RS"}""",
        ) as CoreEvent.Quality
        assertNull(e.routerMs)
    }

    @Test fun reconnectingNoticeEnded() {
        assertEquals(CoreEvent.Reconnecting(2, "wifi"), parseEvent("""{"type":"reconnecting","attempt":2,"why":"wifi"}"""))
        assertEquals(CoreEvent.Notice("hi"), parseEvent("""{"type":"notice","text":"hi"}"""))
        assertEquals(CoreEvent.Ended(false, "bad password"), parseEvent("""{"type":"ended","ok":false,"text":"bad password"}"""))
        assertEquals(CoreEvent.Ended(true, ""), parseEvent("""{"type":"ended","ok":true,"text":""}"""))
    }

    @Test fun badInputIsNull() {
        assertNull(parseEvent(null))
        assertNull(parseEvent(""))
        assertNull(parseEvent("not json"))
        assertNull(parseEvent("""{"type":"future_thing"}"""))
    }

    @Test fun viewOps() {
        val ops = parseViewOps("""[{"type":"zoom","factor":1.2,"cx":100,"cy":200},{"type":"pan","dx":-5.5,"dy":3},{"type":"x"}]""")
        assertEquals(listOf(ViewOp.Zoom(1.2f, 100f, 200f), ViewOp.Pan(-5.5f, 3f)), ops)
        assertTrue(parseViewOps(null).isEmpty())
        assertTrue(parseViewOps("[]").isEmpty())
        assertTrue(parseViewOps("oops").isEmpty())
    }

    @Test fun hapticOpAndUnknownCursorIgnored() {
        val ops = parseViewOps("""[{"type":"haptic"},{"type":"cursor","x":0.25,"y":0.75},{"type":"pan","dx":1,"dy":2}]""")
        assertEquals(listOf(ViewOp.Haptic, ViewOp.Pan(1f, 2f)), ops)
    }

    @Test fun tapOps() {
        val ops = parseViewOps("""[{"type":"tap","x":120.5,"y":300,"button":1},{"type":"tap","x":1,"y":2,"button":0},{"type":"zoom","factor":1.1,"cx":1,"cy":2},{"type":"pan","dx":3,"dy":4}]""")
        assertEquals(
            listOf(ViewOp.Tap(120.5f, 300f, 1), ViewOp.Tap(1f, 2f, 0), ViewOp.Zoom(1.1f, 1f, 2f), ViewOp.Pan(3f, 4f)),
            ops,
        )
    }
}
