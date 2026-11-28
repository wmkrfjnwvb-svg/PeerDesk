package ai.peerdesk.app

import android.content.Context
import android.content.Intent
import android.net.Uri
import androidx.core.content.FileProvider
import java.io.ByteArrayInputStream
import java.io.File

/** Rust 核心的日志目录：数据目录（`filesDir`，也是传给 `Native.start` 的 dataDir）下的 `logs/`。 */
private fun logDir(ctx: Context) = File(ctx.filesDir, "logs")

/** 日志目录里现有的文件名。 */
fun logNames(ctx: Context): Set<String> = logDir(ctx).list()?.toSet() ?: emptySet()

/**
 * 把日志和手势记录打成 zip，放到 cache/diag/，返回通过 FileProvider 给出的 Uri。要在后台线程调。
 * `traceText` 为 null 表示没有手势记录。
 */
fun writeDiagZipFile(ctx: Context, traceText: String?): Uri {
    val dir = File(ctx.cacheDir, "diag").apply { mkdirs() }
    val now = System.currentTimeMillis()
    val file = File(dir, diagZipName(now))
    val sources = buildList {
        for (name in diagEntryNames(logNames(ctx), traceText != null)) {
            if (name == TRACE_ENTRY) {
                val bytes = (traceText ?: "").toByteArray(Charsets.UTF_8)
                add(ZipSource(name) { ByteArrayInputStream(bytes) })
            } else {
                add(ZipSource(name) { File(logDir(ctx), name.removePrefix("logs/")).inputStream() })
            }
        }
    }
    file.outputStream().use { writeDiagZip(it, sources) }
    // 只留最近几个包
    val existing = dir.listFiles()?.map { it.name to it.lastModified() } ?: emptyList()
    diagZipsToDelete(existing).forEach { File(dir, it).delete() }
    return FileProvider.getUriForFile(ctx, "${ctx.packageName}.fileprovider", file)
}

/** 分享这个包（ACTION_SEND，application/zip）。 */
fun shareDiagIntent(uri: Uri, title: String): Intent =
    Intent.createChooser(
        Intent(Intent.ACTION_SEND).apply {
            type = "application/zip"
            putExtra(Intent.EXTRA_STREAM, uri)
            putExtra(Intent.EXTRA_SUBJECT, title)
            addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION)
        },
        title,
    )
