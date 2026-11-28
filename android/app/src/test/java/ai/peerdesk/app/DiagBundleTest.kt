package ai.peerdesk.app

import java.io.ByteArrayInputStream
import java.io.ByteArrayOutputStream
import java.io.IOException
import java.util.zip.ZipInputStream
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class DiagBundleTest {
    @Test fun entryNamesAndOrder() {
        assertEquals(
            listOf("logs/peerdesk.log", "logs/peerdesk.1.log", "gesture-trace.jsonl"),
            diagEntryNames(setOf("peerdesk.1.log", "peerdesk.log", "other.txt"), true),
        )
        assertEquals(listOf("logs/peerdesk.log"), diagEntryNames(setOf("peerdesk.log"), false))
        // 只有轮换出去的旧日志也要放；无关文件不放
        assertEquals(listOf("logs/peerdesk.1.log"), diagEntryNames(setOf("peerdesk.1.log", "x"), false))
        assertEquals(listOf("gesture-trace.jsonl"), diagEntryNames(emptySet(), true))
    }

    @Test fun availableWhenEitherExists() {
        assertFalse(diagAvailable(emptySet(), false))
        assertFalse(diagAvailable(setOf("random"), false))
        assertTrue(diagAvailable(setOf("peerdesk.log"), false))
        assertTrue(diagAvailable(emptySet(), true))
    }

    @Test fun zipNameIsUtcTimestamp() {
        // 2026-11-28 14:02:05 UTC
        assertEquals("peerdesk-diag-20261128-140205.zip", diagZipName(1_795_874_525_000L))
        assertTrue(isDiagZip(diagZipName(0)))
        assertFalse(isDiagZip("gesture.jsonl"))
        assertFalse(isDiagZip("peerdesk-diag-1.txt"))
    }

    @Test fun cleanupKeepsNewestFewAndOnlyTouchesOurZips() {
        val files = listOf(
            "peerdesk-diag-a.zip" to 10L,
            "peerdesk-diag-b.zip" to 50L,
            "peerdesk-diag-c.zip" to 30L,
            "peerdesk-diag-d.zip" to 40L,
            "peerdesk-diag-e.zip" to 20L,
            "notes.txt" to 1L, // 不是我们的，不动
        )
        assertEquals(listOf("peerdesk-diag-e.zip", "peerdesk-diag-a.zip"), diagZipsToDelete(files, keep = 3))
        assertTrue(diagZipsToDelete(files, keep = 10).isEmpty())
        assertEquals(5, diagZipsToDelete(files, keep = 0).size)
        assertEquals(1, diagZipsToDelete(files, keep = DIAG_KEEP).size)
    }

    private fun unzip(bytes: ByteArray): Map<String, String> {
        val out = linkedMapOf<String, String>()
        ZipInputStream(ByteArrayInputStream(bytes)).use { z ->
            while (true) {
                val e = z.nextEntry ?: break
                out[e.name] = z.readBytes().toString(Charsets.UTF_8)
            }
        }
        return out
    }

    @Test fun zipHasTheFilesAndSkipsUnreadableOnes() {
        val buf = ByteArrayOutputStream()
        val written = writeDiagZip(
            buf,
            listOf(
                ZipSource("logs/peerdesk.log") { ByteArrayInputStream("日志行\n".toByteArray()) },
                ZipSource("logs/peerdesk.1.log") { throw IOException("gone") },
                ZipSource("gesture-trace.jsonl") { ByteArrayInputStream("{\"v\":1}\n".toByteArray()) },
            ),
        )
        assertEquals(listOf("logs/peerdesk.log", "gesture-trace.jsonl"), written)
        val files = unzip(buf.toByteArray())
        assertEquals(listOf("logs/peerdesk.log", "gesture-trace.jsonl"), files.keys.toList())
        assertEquals("日志行\n", files["logs/peerdesk.log"])
        assertEquals("{\"v\":1}\n", files["gesture-trace.jsonl"])
    }
}
