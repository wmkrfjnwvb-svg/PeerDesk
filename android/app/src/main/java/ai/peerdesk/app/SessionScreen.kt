package ai.peerdesk.app

import android.app.Activity
import android.content.Context
import android.view.KeyEvent
import android.view.View
import android.view.ViewConfiguration
import androidx.activity.compose.BackHandler
import androidx.compose.foundation.background
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.WindowInsets
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.ime
import androidx.compose.foundation.layout.WindowInsetsSides
import androidx.compose.foundation.layout.imePadding
import androidx.compose.foundation.layout.only
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.safeDrawing
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.windowInsetsPadding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.material3.Button
import androidx.compose.material3.ButtonDefaults
import androidx.compose.material3.FilterChip
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clipToBounds
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.unit.dp
import androidx.compose.ui.layout.onSizeChanged
import androidx.compose.ui.viewinterop.AndroidView
import androidx.core.view.WindowCompat
import androidx.core.view.WindowInsetsCompat
import kotlinx.coroutines.delay

/** 手指按着时每隔多久调一次 tick（长按拖拽靠它）。 */
private const val TICK_MS = 50L

@Composable
fun SessionScreen(ui: UiState, vm: PeerDeskViewModel) {
    val ctx = LocalContext.current
    val density = LocalDensity.current
    val slopPx = remember(ctx) { ViewConfiguration.get(ctx).scaledTouchSlop.toFloat() }
    val notchPx = with(density) { 40.dp.toPx() }
    var pressed by remember { mutableStateOf(false) }
    var keyCatcher by remember { mutableStateOf<KeyCatcherView?>(null) }
    val imeVisible = WindowInsets.ime.getBottom(density) > 0

    BackHandler { vm.disconnect() }

    // 有手指按着时每 50 ms 调一次 tick，手势识别靠它判断长按
    LaunchedEffect(pressed) {
        while (pressed) {
            delay(TICK_MS)
            vm.tick()
        }
    }

    Box(Modifier.fillMaxSize().background(Color.Black)) {
        // 裁剪容器：画面放大后超出的部分被裁掉
        Box(
            Modifier
                .fillMaxSize()
                .clipToBounds()
                .onSizeChanged { vm.onContainerSize(it.width.toFloat(), it.height.toFloat()) },
        ) {
            AndroidView(
                modifier = Modifier.fillMaxSize(),
                factory = { c ->
                    RemoteSurfaceLayout(
                        c,
                        onCreated = { vm.startSession(it, slopPx, notchPx) },
                        // Surface 没了（例如被系统回收）就不能继续显示，结束会话
                        onDestroyed = { vm.onBackground() },
                    )
                },
                update = { it.place(ui.rect) },
            )
            // 触摸层盖在整个会话页上
            AndroidView(
                modifier = Modifier.fillMaxSize(),
                factory = { c ->
                    TouchLayerView(
                        c,
                        onOps = { ops, t -> vm.forwardTouch(ops, t) },
                        onPressed = { pressed = it },
                    )
                },
                update = {
                    it.onOps = { ops, t -> vm.forwardTouch(ops, t) }
                    it.onPressed = { p -> pressed = p }
                },
            )
        }

        if (!ui.connected && ui.quality == null) {
            Text(
                stringResource(R.string.session_connecting),
                color = Color.White,
                modifier = Modifier.align(Alignment.Center),
            )
        }

        Toolbar(ui, vm, imeVisible, onKeyboard = {
            val v = keyCatcher
            val window = (ctx as? Activity)?.window
            if (v != null && window != null) {
                val controller = WindowCompat.getInsetsController(window, v)
                if (imeVisible) {
                    controller.hide(WindowInsetsCompat.Type.ime())
                } else {
                    v.requestFocus()
                    controller.show(WindowInsetsCompat.Type.ime())
                }
            }
        })

        // 隐藏的输入接收器（1 dp，不可见）
        AndroidView(
            modifier = Modifier.size(1.dp).align(Alignment.BottomStart),
            factory = { c ->
                KeyCatcherView(c, onText = vm::typeText, onKey = vm::softKey).also { keyCatcher = it }
            },
            update = {
                it.onText = vm::typeText
                it.onKey = vm::softKey
            },
        )

        if (imeVisible) {
            ShortcutBar(
                ui, vm,
                Modifier.align(Alignment.BottomCenter).imePadding(),
            )
        }
    }
}

@Composable
private fun Toolbar(ui: UiState, vm: PeerDeskViewModel, imeVisible: Boolean, onKeyboard: () -> Unit) {
    Box(Modifier.fillMaxSize().windowInsetsPadding(WindowInsets.safeDrawing.only(WindowInsetsSides.Top + WindowInsetsSides.Horizontal))) {
        if (!ui.toolbarVisible) {
            TextButton(
                onClick = vm::toggleToolbar,
                modifier = Modifier.align(Alignment.TopEnd),
            ) { Text(stringResource(R.string.toolbar_show), color = Color.White.copy(alpha = 0.7f)) }
            return@Box
        }
        Column(
            Modifier
                .fillMaxWidth()
                .background(Color.Black.copy(alpha = 0.6f))
                .padding(horizontal = 8.dp, vertical = 4.dp),
        ) {
            Row(
                Modifier.horizontalScroll(rememberScrollState()),
                horizontalArrangement = Arrangement.spacedBy(8.dp),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                Button(
                    onClick = vm::disconnect,
                    colors = ButtonDefaults.buttonColors(containerColor = MaterialTheme.colorScheme.error),
                ) { Text(stringResource(R.string.toolbar_disconnect)) }
                FilterChip(
                    selected = !ui.trackpad,
                    onClick = { vm.setTrackpad(false) },
                    label = { Text(stringResource(R.string.toolbar_mode_direct)) },
                )
                FilterChip(
                    selected = ui.trackpad,
                    onClick = { vm.setTrackpad(true) },
                    label = { Text(stringResource(R.string.toolbar_mode_trackpad)) },
                )
                FilterChip(
                    selected = imeVisible,
                    onClick = onKeyboard,
                    label = { Text(stringResource(R.string.toolbar_keyboard)) },
                )
                TextButton(onClick = vm::toggleToolbar) {
                    Text(stringResource(R.string.toolbar_hide), color = Color.White)
                }
            }
            StatusLine(ui)
        }
    }
}

/** 一行状态；网络变差时再加一行原因和建议。不弹窗。 */
@Composable
private fun StatusLine(ui: UiState) {
    val r = ui.reconnecting
    val q = ui.quality
    val line: String = when {
        r != null -> stringResource(R.string.session_reconnecting, r.attempt, r.why)
        q == null -> stringResource(R.string.status_waiting)
        else -> {
            val (v, mbps) = splitBitrate(q.recvKbps)
            val rate = if (mbps) stringResource(R.string.status_mbps, v) else stringResource(R.string.status_kbps, q.recvKbps)
            val route = when (ui.route) {
                Route.Direct -> stringResource(R.string.route_direct) + " · "
                Route.Relay -> stringResource(R.string.route_relay) + " · "
                Route.Unknown -> ""
            }
            route + stringResource(R.string.status_line, q.rttMs, q.fps, rate, formatLoss(q.lossPermille))
        }
    }
    Text(line, color = Color.White, style = MaterialTheme.typography.bodySmall)
    val problem = q?.let(::diagnoseNetwork)
    if (r == null && problem != null) {
        val why = when (problem) {
            is NetworkProblem.LocalRouter -> stringResource(R.string.diag_router, problem.routerMs)
            NetworkProblem.Internet -> stringResource(R.string.diag_internet)
            NetworkProblem.Unmeasured -> stringResource(R.string.diag_unmeasured)
        }
        Text(why, color = Color(0xFFFFC857), style = MaterialTheme.typography.bodySmall)
    }
    ui.coreNotice?.let { Text(it, color = Color.White.copy(alpha = 0.8f), style = MaterialTheme.typography.bodySmall) }
}

@Composable
private fun ShortcutBar(ui: UiState, vm: PeerDeskViewModel, modifier: Modifier) {
    val cmd = metaLabelIsCmd(ui.peerOs)
    val metaLabel = stringResource(if (cmd) R.string.key_cmd else R.string.key_win)
    Row(
        modifier
            .fillMaxWidth()
            .background(MaterialTheme.colorScheme.surfaceContainerHigh)
            .horizontalScroll(rememberScrollState())
            .padding(horizontal = 6.dp, vertical = 4.dp),
        horizontalArrangement = Arrangement.spacedBy(6.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        KeyButton(stringResource(R.string.key_esc)) { vm.pressKey(KeyEvent.KEYCODE_ESCAPE) }
        KeyButton(stringResource(R.string.key_tab)) { vm.pressKey(KeyEvent.KEYCODE_TAB) }
        ModButton(stringResource(R.string.key_ctrl), ModKey.Ctrl in ui.locked) { vm.toggleModifier(ModKey.Ctrl) }
        ModButton(stringResource(R.string.key_alt), ModKey.Alt in ui.locked) { vm.toggleModifier(ModKey.Alt) }
        ModButton(metaLabel, ModKey.Meta in ui.locked) { vm.toggleModifier(ModKey.Meta) }
        KeyButton("←") { vm.pressKey(KeyEvent.KEYCODE_DPAD_LEFT) }
        KeyButton("↑") { vm.pressKey(KeyEvent.KEYCODE_DPAD_UP) }
        KeyButton("↓") { vm.pressKey(KeyEvent.KEYCODE_DPAD_DOWN) }
        KeyButton("→") { vm.pressKey(KeyEvent.KEYCODE_DPAD_RIGHT) }
        KeyButton((if (cmd) "⌘" else "Ctrl") + "+C") { vm.shortcut(KeyEvent.KEYCODE_C) }
        KeyButton((if (cmd) "⌘" else "Ctrl") + "+V") { vm.shortcut(KeyEvent.KEYCODE_V) }
    }
}

@Composable
private fun KeyButton(label: String, onClick: () -> Unit) {
    FilterChip(selected = false, onClick = onClick, label = { Text(label) })
}

@Composable
private fun ModButton(label: String, locked: Boolean, onClick: () -> Unit) {
    FilterChip(selected = locked, onClick = onClick, label = { Text(label) })
}
