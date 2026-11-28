package ai.peerdesk.app

import org.junit.Assert.assertEquals
import org.junit.Test

class BackPressTest {
    private var now = 10_000L
    private val t = BackPressTracker(2000) { now }

    @Test fun firstBackShowsHintSecondWithinWindowDisconnects() {
        assertEquals(BackAction.ShowHint, t.onBack())
        now += 1500
        assertEquals(BackAction.Disconnect, t.onBack())
    }

    @Test fun secondBackAfterWindowIsAHintAgain() {
        assertEquals(BackAction.ShowHint, t.onBack())
        now += 2001
        assertEquals(BackAction.ShowHint, t.onBack())
        now += 100
        assertEquals(BackAction.Disconnect, t.onBack())
    }

    @Test fun exactlyAtTheWindowEdgeStillDisconnects() {
        t.onBack()
        now += 2000
        assertEquals(BackAction.Disconnect, t.onBack())
    }

    @Test fun afterDisconnectTheCycleStartsOver() {
        t.onBack()
        t.onBack()
        now += 10
        assertEquals(BackAction.ShowHint, t.onBack())
    }

    @Test fun resetForgetsTheFirstPress() {
        t.onBack()
        t.reset()
        now += 100
        assertEquals(BackAction.ShowHint, t.onBack())
    }
}
