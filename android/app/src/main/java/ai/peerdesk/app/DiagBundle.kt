package ai.peerdesk.app

import java.io.InputStream
import java.io.OutputStream
import java.time.Instant
import java.time.ZoneOffset
import java.time.format.DateTimeFormatter
import java.util.zip.ZipEntry
import java.util.zip.ZipOutputStream

/*
 * “导出诊断记录”：把 Rust 核心写的日志（和桌面端同一个日志器、同一种行格式）和手势记录打成一个 zip 分享出去。
 * 这个文件只放纯逻辑（要打哪些文件、起什么名、清理旧包、写 zip），好在 JVM 上测；读写 Android 目录的部分在 TraceExport.kt。
 */

/** Rust 核心的日志文件名（数据目录下 `logs/`，见 crates/net 的 logfile）。 */
const val LOG_FILE = "peerdesk.log"
const val LOG_OLD_FILE = "peerdesk.1.log"

/** zip 里手势记录的文件名。 */
const val TRACE_ENTRY = "gesture-trace.jsonl"

/** 包里只留最近这几个，别越攒越多。 */
const val DIAG_KEEP = 4

/** 一个要放进 zip 的文件：zip 里的名字和读内容的方法。 */
class ZipSource(val name: String, val open: () -> InputStream)

/**
 * 要打包的文件名（zip 里的名字），顺序固定：`logs/peerdesk.log`、`logs/peerdesk.1.log`（有才放）、手势记录（有才放）。
 * `logNames` 是日志目录里现有的文件名。
 */
fun diagEntryNames(logNames: Set<String>, hasTrace: Boolean): List<String> = buildList {
    if (LOG_FILE in logNames) add("logs/$LOG_FILE")
    if (LOG_OLD_FILE in logNames) add("logs/$LOG_OLD_FILE")
    if (hasTrace) add(TRACE_ENTRY)
}

/** 有日志或手势记录才显示导出按钮。 */
fun diagAvailable(logNames: Set<String>, hasTrace: Boolean): Boolean = diagEntryNames(logNames, hasTrace).isNotEmpty()

private val STAMP = DateTimeFormatter.ofPattern("yyyyMMdd-HHmmss").withZone(ZoneOffset.UTC)

/** 包名：`peerdesk-diag-20261128-140205.zip`（UTC，跨时区不乱）。 */
fun diagZipName(timeMs: Long): String = "peerdesk-diag-${STAMP.format(Instant.ofEpochMilli(timeMs))}.zip"

/** 是不是我们自己打的包（清理时只动这些，不碰目录里别的东西）。 */
fun isDiagZip(name: String): Boolean = name.startsWith("peerdesk-diag-") && name.endsWith(".zip")

/** 目录里现有的 (文件名, 修改时间)：返回要删的——只看我们的包，按修改时间留最新的 [keep] 个。 */
fun diagZipsToDelete(files: List<Pair<String, Long>>, keep: Int = DIAG_KEEP): List<String> =
    files.filter { isDiagZip(it.first) }
        .sortedWith(compareByDescending<Pair<String, Long>> { it.second }.thenByDescending { it.first })
        .drop(keep.coerceAtLeast(0))
        .map { it.first }

/** 把这些文件写成 zip。读不出来的文件（打包时被删了）跳过，不让整个导出失败。返回实际放进去的名字。 */
fun writeDiagZip(out: OutputStream, sources: List<ZipSource>): List<String> {
    val written = ArrayList<String>()
    ZipOutputStream(out).use { zip ->
        for (s in sources) {
            val input = try {
                s.open()
            } catch (_: java.io.IOException) {
                continue
            }
            input.use {
                zip.putNextEntry(ZipEntry(s.name))
                it.copyTo(zip)
                zip.closeEntry()
            }
            written.add(s.name)
        }
    }
    return written
}
