package ai.peerdesk.app

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.imePadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.safeDrawingPadding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.Button
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalClipboardManager
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.res.painterResource
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.LiveRegionMode
import androidx.compose.ui.semantics.liveRegion
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.AnnotatedString
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.text.input.OffsetMapping
import androidx.compose.ui.text.input.PasswordVisualTransformation
import androidx.compose.ui.text.input.TransformedText
import androidx.compose.ui.text.input.VisualTransformation
import androidx.compose.ui.unit.dp
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext

/** 设备码输入框的显示：纯数字显示成 3-3-3。 */
private val CodeTransformation = VisualTransformation { text ->
    TransformedText(
        AnnotatedString(formatCode(text.text)),
        object : OffsetMapping {
            override fun originalToTransformed(offset: Int) = codeDisplayOffset(offset)
            override fun transformedToOriginal(offset: Int) = codeOriginalOffset(offset).coerceAtMost(text.length)
        },
    )
}

@Composable
fun ConnectScreen(ui: UiState, vm: PeerDeskViewModel) {
    // 服务器默认官方的：只填设备码就能连
    val canConnect = ui.code.length == 9
    // 诊断记录（核心日志加手势记录）在会话结束后导出：会话中弹出系统分享会把应用切到后台，会话就断了
    val ctx = LocalContext.current
    val hasTrace = vm.hasTrace()
    val available = remember(ui.phase, ui.notice, hasTrace) { diagAvailable(logNames(ctx), hasTrace) }
    val scope = rememberCoroutineScope()
    val shareTitle = stringResource(R.string.diag_share_title)
    val exportDiag: () -> Unit = {
        val text = if (hasTrace) vm.traceText() else null
        scope.launch {
            // 打 zip 在后台线程做
            val uri = withContext(Dispatchers.IO) { writeDiagZipFile(ctx, text) }
            ctx.startActivity(shareDiagIntent(uri, shareTitle))
        }
    }
    Column(
        Modifier
            .fillMaxSize()
            .safeDrawingPadding()
            .imePadding()
            .verticalScroll(rememberScrollState())
            .padding(horizontal = 24.dp, vertical = 16.dp),
        verticalArrangement = Arrangement.spacedBy(16.dp, Alignment.CenterVertically),
    ) {
        Text(stringResource(R.string.connect_title), style = MaterialTheme.typography.headlineMedium)

        // 失败原因直接写在页面上，不弹窗
        ui.notice?.let { n ->
            NoticeBlock(
                n,
                canRetry = canConnect,
                canExport = ui.phase == Phase.Connect && available,
                onRetry = vm::connect,
                onServerSettings = vm::openAdvanced,
                onExport = exportDiag,
            )
        }

        OutlinedTextField(
            value = ui.code,
            onValueChange = vm::setCode,
            label = { Text(stringResource(R.string.connect_code)) },
            singleLine = true,
            visualTransformation = CodeTransformation,
            keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.NumberPassword),
            isError = ui.code.isNotEmpty() && ui.code.length < 9,
            supportingText = {
                if (ui.code.isNotEmpty() && ui.code.length < 9) Text(stringResource(R.string.connect_bad_code))
            },
            modifier = Modifier.fillMaxWidth(),
        )
        var showPassword by rememberSaveable { mutableStateOf(false) }
        OutlinedTextField(
            value = ui.password,
            onValueChange = vm::setPassword,
            label = { Text(stringResource(R.string.connect_password)) },
            singleLine = true,
            visualTransformation = if (showPassword) VisualTransformation.None else PasswordVisualTransformation(),
            trailingIcon = {
                IconButton(onClick = { showPassword = !showPassword }) {
                    Icon(
                        painter = painterResource(if (showPassword) R.drawable.ic_eye_off else R.drawable.ic_eye),
                        contentDescription = stringResource(
                            if (showPassword) R.string.connect_hide_password else R.string.connect_show_password,
                        ),
                    )
                }
            },
            keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Password),
            modifier = Modifier.fillMaxWidth(),
        )
        Button(
            onClick = vm::connect,
            enabled = canConnect,
            modifier = Modifier.fillMaxWidth(),
        ) { Text(stringResource(R.string.connect_button)) }

        ServerBlock(ui, vm)

        // 失败提示里已经有“导出诊断记录”时不再重复
        val inNotice = (ui.notice as? EndNotice.ConnectFailed)?.reason?.actions?.contains(ReasonAction.Diagnostics) == true ||
            (ui.notice as? EndNotice.Ended)?.reason?.actions?.contains(ReasonAction.Diagnostics) == true
        if (ui.phase == Phase.Connect && available && !inNotice) {
            TextButton(onClick = exportDiag) { Text(stringResource(R.string.export_diagnostics)) }
        }

        Text(
            stringResource(R.string.connect_background_note) + "\n" + stringResource(R.string.connect_gestures),
            style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
    }
}

/** 服务器：一行说现在用的是哪个；自己的服务器收在“高级”里（自建用户才需要）。 */
@Composable
private fun ServerBlock(ui: UiState, vm: PeerDeskViewModel) {
    Column(verticalArrangement = Arrangement.spacedBy(4.dp)) {
        val custom = ui.server.trim()
        Text(
            if (custom.isEmpty()) stringResource(R.string.connect_server_official)
            else stringResource(R.string.connect_server_custom, custom),
            style = MaterialTheme.typography.bodyMedium,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
        TextButton(onClick = vm::toggleAdvanced, contentPadding = androidx.compose.foundation.layout.PaddingValues(0.dp)) {
            Text(stringResource(if (ui.advancedOpen) R.string.connect_advanced_hide else R.string.connect_advanced))
        }
        if (ui.advancedOpen) {
            OutlinedTextField(
                value = ui.server,
                onValueChange = vm::setServer,
                label = { Text(stringResource(R.string.connect_server)) },
                placeholder = { Text(stringResource(R.string.connect_server_hint)) },
                supportingText = { Text(stringResource(R.string.connect_server_note)) },
                singleLine = true,
                keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Uri),
                modifier = Modifier.fillMaxWidth(),
            )
        }
    }
}

/**
 * 上次为什么结束：一句原因、一句办法、最多两个按钮；核心的原文收在“详情”里，可以复制。
 * 切后台、显示面不可用这两种是界面自己的原因，只有一句话。
 */
@Composable
private fun NoticeBlock(
    n: EndNotice,
    canRetry: Boolean,
    canExport: Boolean,
    onRetry: () -> Unit,
    onServerSettings: () -> Unit,
    onExport: () -> Unit,
) {
    val muted = MaterialTheme.colorScheme.onSurfaceVariant
    val live = Modifier.semantics { liveRegion = LiveRegionMode.Polite }
    val (reason, details) = when (n) {
        is EndNotice.ConnectFailed -> n.reason to n.details
        is EndNotice.Ended -> n.reason to n.details
        EndNotice.Background -> {
            Text(stringResource(R.string.connect_ended_background), color = muted, style = MaterialTheme.typography.bodyMedium, modifier = live)
            return
        }
        EndNotice.NoSurface -> {
            Text(stringResource(R.string.connect_no_surface), color = MaterialTheme.colorScheme.error, style = MaterialTheme.typography.bodyMedium, modifier = live)
            return
        }
    }
    Column(verticalArrangement = Arrangement.spacedBy(6.dp), modifier = live) {
        Text(
            stringResource(reason.cause),
            color = if (reason.calm) MaterialTheme.colorScheme.onSurface else MaterialTheme.colorScheme.error,
            style = MaterialTheme.typography.titleSmall,
        )
        Text(stringResource(reason.fix), color = muted, style = MaterialTheme.typography.bodyMedium)
        val actions = reason.actions.filter {
            when (it) {
                ReasonAction.Retry -> canRetry
                ReasonAction.ServerSettings -> true
                ReasonAction.Diagnostics -> canExport
            }
        }
        var open by rememberSaveable(n) { mutableStateOf(false) }
        Row(horizontalArrangement = Arrangement.spacedBy(8.dp), verticalAlignment = Alignment.CenterVertically) {
            actions.forEachIndexed { i, a ->
                val label = stringResource(
                    when (a) {
                        ReasonAction.Retry -> R.string.action_retry
                        ReasonAction.ServerSettings -> R.string.action_server_settings
                        ReasonAction.Diagnostics -> R.string.export_diagnostics
                    },
                )
                val click = when (a) {
                    ReasonAction.Retry -> onRetry
                    ReasonAction.ServerSettings -> onServerSettings
                    ReasonAction.Diagnostics -> onExport
                }
                // 第一个是主要的
                if (i == 0) OutlinedButton(onClick = click) { Text(label) } else TextButton(onClick = click) { Text(label) }
            }
            if (details.isNotBlank()) {
                TextButton(onClick = { open = !open }) {
                    Text(stringResource(if (open) R.string.details_hide else R.string.details_show))
                }
            }
        }
        if (open && details.isNotBlank()) {
            val clipboard = LocalClipboardManager.current
            Text(details, color = muted, style = MaterialTheme.typography.bodySmall.copy(fontFamily = FontFamily.Monospace))
            TextButton(onClick = { clipboard.setText(AnnotatedString(details)) }) {
                Text(stringResource(R.string.details_copy))
            }
        }
    }
}
