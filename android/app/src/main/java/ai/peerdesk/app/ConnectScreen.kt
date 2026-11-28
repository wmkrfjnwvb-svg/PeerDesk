package ai.peerdesk.app

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.imePadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.safeDrawingPadding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.Button
import androidx.compose.material3.TextButton
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.ui.platform.LocalContext
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.res.painterResource
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.semantics.LiveRegionMode
import androidx.compose.ui.semantics.liveRegion
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.text.input.OffsetMapping
import androidx.compose.ui.text.input.PasswordVisualTransformation
import androidx.compose.ui.text.input.TransformedText
import androidx.compose.ui.text.input.VisualTransformation
import androidx.compose.ui.text.AnnotatedString
import androidx.compose.ui.unit.dp

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
    val canConnect = ui.server.isNotBlank() && ui.code.length == 9
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

        ui.notice?.let { n ->
            // 失败原因直接写在页面上，不弹窗
            Text(
                noticeText(n),
                color = if (n is EndNotice.Background) MaterialTheme.colorScheme.onSurfaceVariant
                else MaterialTheme.colorScheme.error,
                style = MaterialTheme.typography.bodyMedium,
                modifier = Modifier.semantics { liveRegion = LiveRegionMode.Polite },
            )
        }

        OutlinedTextField(
            value = ui.server,
            onValueChange = vm::setServer,
            label = { Text(stringResource(R.string.connect_server)) },
            placeholder = { Text(stringResource(R.string.connect_server_hint)) },
            singleLine = true,
            keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Uri),
            modifier = Modifier.fillMaxWidth(),
        )
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

        // 手势记录在会话结束后导出：会话中弹出系统分享会把应用切到后台，会话就断了
        if (ui.phase == Phase.Connect && vm.hasTrace()) {
            val ctx = LocalContext.current
            val scope = rememberCoroutineScope()
            val title = stringResource(R.string.trace_share_title)
            TextButton(onClick = {
                val text = vm.traceText()
                scope.launch {
                    val uri = withContext(Dispatchers.IO) { writeTraceFile(ctx, text) }
                    ctx.startActivity(shareTraceIntent(uri, title))
                }
            }) { Text(stringResource(R.string.toolbar_export_trace)) }
        }

        Text(
            stringResource(R.string.connect_background_note) + "\n" + stringResource(R.string.connect_gestures),
            style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
    }
}

@Composable
private fun noticeText(n: EndNotice): String = when (n) {
    is EndNotice.ConnectFailed -> stringResource(R.string.connect_failed, n.text)
    is EndNotice.Ended ->
        if (n.text.isBlank()) stringResource(R.string.connect_ended, "-") else stringResource(R.string.connect_ended, n.text)
    EndNotice.Background -> stringResource(R.string.connect_ended_background)
    EndNotice.NoSurface -> stringResource(R.string.connect_no_surface)
}
