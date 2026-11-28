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
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
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
        OutlinedTextField(
            value = ui.password,
            onValueChange = vm::setPassword,
            label = { Text(stringResource(R.string.connect_password)) },
            singleLine = true,
            visualTransformation = PasswordVisualTransformation(),
            keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Password),
            modifier = Modifier.fillMaxWidth(),
        )
        Button(
            onClick = vm::connect,
            enabled = canConnect,
            modifier = Modifier.fillMaxWidth(),
        ) { Text(stringResource(R.string.connect_button)) }

        Text(
            stringResource(R.string.connect_background_note),
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
