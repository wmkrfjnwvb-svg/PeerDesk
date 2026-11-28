package ai.peerdesk.app

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

class NetworkDiagnosisTest {
    private fun q(rtt: Int = 20, lossPermille: Int = 0, router: Int? = 3) = CoreEvent.Quality(
        rttMs = rtt, lossPermille = lossPermille, queueMs = 0, fps = 30, recvKbps = 2000, decodeMs = 4,
        width = 1280, height = 720, routerMs = router, decoder = "mc", fec = "RS",
    )

    @Test fun healthyIsNull() {
        assertNull(diagnoseNetwork(q()))
        assertNull("正好 2% 和 150 ms 不提示", diagnoseNetwork(q(rtt = 150, lossPermille = 20)))
    }

    @Test fun highLossWithSlowRouterIsLocal() {
        assertEquals(NetworkProblem.LocalRouter(60), diagnoseNetwork(q(rtt = 90, lossPermille = 50, router = 60)))
    }

    @Test fun highRttWithFastRouterIsInternet() {
        assertEquals(NetworkProblem.Internet, diagnoseNetwork(q(rtt = 220, router = 4)))
        assertEquals(NetworkProblem.Internet, diagnoseNetwork(q(rtt = 40, lossPermille = 30, router = 3)))
    }

    @Test fun unmeasuredRouter() {
        assertEquals(NetworkProblem.Unmeasured, diagnoseNetwork(q(rtt = 300, router = null)))
    }

    @Test fun routerShareOfRtt() {
        // 往返 40 ms，其中一半（20 ms）在路由器这一段，又在丢包：算本机到路由器的问题
        assertEquals(NetworkProblem.LocalRouter(20), diagnoseNetwork(q(rtt = 40, lossPermille = 30, router = 20)))
        assertEquals(NetworkProblem.LocalRouter(80), diagnoseNetwork(q(rtt = 160, router = 80)))
    }

    @Test fun formatting() {
        assertEquals("0.3%", formatLoss(3))
        assertEquals("12.5%", formatLoss(125))
        assertEquals("0.0%", formatLoss(0))
        assertEquals(2.0 to true, splitBitrate(2000))
        assertEquals(900.0 to false, splitBitrate(900))
    }
}
