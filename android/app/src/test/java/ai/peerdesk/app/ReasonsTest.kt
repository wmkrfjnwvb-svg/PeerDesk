package ai.peerdesk.app

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class ReasonsTest {
    @Test fun codesRoundTripAndAreUnique() {
        for (r in Reason.entries) assertEquals(r, Reason.of(r.code))
        assertEquals(Reason.entries.size, Reason.entries.map { it.code }.toSet().size)
        assertEquals(Reason.entries.size, Reason.entries.map { it.cause }.toSet().size)
    }

    @Test fun unknownOrMissingCodesAreOther() {
        assertEquals(Reason.Other, Reason.of("from_the_future"))
        assertEquals(Reason.Other, Reason.of(""))
        assertEquals(Reason.Other, Reason.of(null))
    }

    @Test fun atMostTwoButtons() {
        for (r in Reason.entries) assertTrue(r.name, r.actions.size <= 2)
        // 连不上服务器：可以去看服务器设置
        assertTrue(ReasonAction.ServerSettings in Reason.ServerUnreachable.actions)
    }

    @Test fun calmReasonsAreNotErrors() {
        assertTrue(Reason.PeerClosed.calm && Reason.PeerQuit.calm && Reason.Cancelled.calm)
        assertFalse(Reason.WrongPassword.calm || Reason.NetworkLost.calm || Reason.Other.calm)
    }
}
