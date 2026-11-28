package ai.peerdesk.app

import android.content.Context
import android.content.Intent
import android.net.Uri
import androidx.core.content.FileProvider
import java.io.File

/** 把手势记录写到 cache/traces/ 下，返回通过 FileProvider 给出的 Uri。写文件要在后台线程做。 */
fun writeTraceFile(ctx: Context, text: String): Uri {
    val dir = File(ctx.cacheDir, "traces").apply { mkdirs() }
    // 只留最近几份，别越攒越多
    dir.listFiles()?.sortedByDescending { it.lastModified() }?.drop(4)?.forEach { it.delete() }
    val file = File(dir, "gesture-trace-${System.currentTimeMillis()}.jsonl")
    file.writeText(text)
    return FileProvider.getUriForFile(ctx, "${ctx.packageName}.fileprovider", file)
}

/** 分享这份记录（ACTION_SEND，text/plain）。 */
fun shareTraceIntent(uri: Uri, title: String): Intent =
    Intent.createChooser(
        Intent(Intent.ACTION_SEND).apply {
            type = "text/plain"
            putExtra(Intent.EXTRA_STREAM, uri)
            putExtra(Intent.EXTRA_SUBJECT, title)
            addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION)
        },
        title,
    )
