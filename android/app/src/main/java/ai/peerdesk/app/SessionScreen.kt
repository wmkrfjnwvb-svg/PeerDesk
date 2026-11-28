package ai.peerdesk.app

import android.app.Activity
import android.content.Context
import android.view.KeyEvent
import android.os.SystemClock
import android.widget.Toast
import android.view.HapticFeedbackConstants
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
import androidx.compose.foundation.layout.offset
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.ime
import androidx.compose.foundation.layout.WindowInsetsSides
import androidx.compose.foundation.layout.imePadding
import androidx.compose.foundation.layout.only
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.displayCutout
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.windowInsetsPadding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.material3.Button
import androidx.compose.material3.ButtonDefaults
import androidx.compose.material3.FilterChip
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.mutableFloatStateOf
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.withFrameMillis
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clipToBounds
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.platform.LocalView
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.unit.dp
import androidx.compose.ui.layout.boundsInRoot
import androidx.compose.ui.layout.onGloballyPositioned
import androidx.compose.ui.layout.onSizeChanged
import androidx.compose.ui.platform.LocalLayoutDirection
import androidx.compose.ui.viewinterop.AndroidView
import androidx.compose.foundation.Canvas
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.geometry.CornerRadius
import androidx.compose.ui.geometry.RoundRect
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.ui.graphics.Path
import androidx.compose.ui.graphics.drawscope.Stroke
import androidx.compose.ui.graphics.drawscope.scale
import androidx.compose.ui.graphics.drawscope.translate
import androidx.compose.ui.res.painterResource
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.unit.IntOffset
import kotlin.math.roundToInt
import androidx.core.view.WindowCompat
import androidx.core.view.WindowInsetsCompat
import androidx.core.view.WindowInsetsControllerCompat
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext

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
    val imeBottom = WindowInsets.ime.getBottom(density)
    val imeVisible = imeBottom > 0
    val hostView = LocalView.current
    // 快捷键栏在键盘上方，可见区域的下沿是它的顶
    var barH by remember { mutableIntStateOf(0) }
    LaunchedEffect(density) { vm.density = density.density }
    LaunchedEffect(imeBottom, barH, ui.containerH) {
        vm.onImeChanged(if (imeBottom > 0 && ui.containerH > 0f) ui.containerH - imeBottom - barH else null)
    }
    // 大鼠标的尺寸：宽 96 dp、本体高 150 dp，上面一条 30 dp 放箭头和关闭按钮，小图标 48 dp
    LaunchedEffect(density) {
        with(density) {
            vm.setMouseSizes(
                MouseSizes(
                    width = 96.dp.toPx(), height = 150.dp.toPx(), arrowH = 30.dp.toPx(),
                    closeSize = 30.dp.toPx(), miniSize = 48.dp.toPx(),
                ),
            )
        }
    }
    val vmouse = ui.vmouse
    val miniBox = ui.miniBox
    // 长按到点了：震一下（HapticFeedbackConstants.LONG_PRESS 不需要 VIBRATE 权限）
    LaunchedEffect(vm) {
        vm.haptics.collect { hostView.performHapticFeedback(HapticFeedbackConstants.LONG_PRESS) }
    }

    // 沉浸式：会话中隐藏状态栏和导航栏（从边缘滑一下临时露出），离开会话页或 Activity 销毁时恢复
    DisposableEffect(hostView) {
        val window = (ctx as? Activity)?.window
        val controller = window?.let { WindowCompat.getInsetsController(it, hostView) }
        controller?.systemBarsBehavior = WindowInsetsControllerCompat.BEHAVIOR_SHOW_TRANSIENT_BARS_BY_SWIPE
        controller?.hide(WindowInsetsCompat.Type.systemBars())
        onDispose { controller?.show(WindowInsetsCompat.Type.systemBars()) }
    }

    // 刘海和工具条下沿：画面平移要避开它们（上边距 = 工具条下沿和刘海上沿里大的那个）
    val layoutDirection = LocalLayoutDirection.current
    val cutoutInsets = WindowInsets.displayCutout
    val cutout = Edges(
        cutoutInsets.getLeft(density, layoutDirection).toFloat(),
        cutoutInsets.getTop(density).toFloat(),
        cutoutInsets.getRight(density, layoutDirection).toFloat(),
        cutoutInsets.getBottom(density).toFloat(),
    )
    var toolbarBottom by remember { mutableFloatStateOf(0f) }
    LaunchedEffect(cutout, toolbarBottom) { vm.onSafeInsets(toolbarBottom, cutout) }

    // 返回键：第一次显示工具条并提示，2 秒内再按一次才断开（边缘滑动手势操作时容易误触）
    val backHint = stringResource(R.string.back_again_to_disconnect)
    BackHandler {
        if (vm.onBackPressed() == BackAction.ShowHint) Toast.makeText(ctx, backHint, Toast.LENGTH_SHORT).show()
    }

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
                update = {
                    // 布局尺寸固定为缩放 1 倍的完整放下大小，缩放平移（和键盘顶起）靠 View 变换
                    val (bw, bh) = fitSize(ui.containerW, ui.containerH, ui.aspect)
                    it.place(ui.shownRect, bw, bh)
                },
            )
            // 触摸层盖在整个会话页上
            AndroidView(
                modifier = Modifier.fillMaxSize(),
                factory = { c ->
                    TouchLayerView(
                        c,
                        tapSlopPx = slopPx,
                        onTouches = { ops, mv, t -> vm.forwardTouch(ops, mv, t) },
                        onRaw = vm::recordMotion,
                        onPressed = { pressed = it },
                        onMouse = vm::onMouseEvent,
                    )
                },
                update = {
                    it.onTouches = { ops, mv, t -> vm.forwardTouch(ops, mv, t) }
                    it.onRaw = vm::recordMotion
                    it.onPressed = { p -> pressed = p }
                    it.onMouse = vm::onMouseEvent
                    // 大鼠标（或小鼠标）上的手指不转发给 touch，其余手指都转发
                    it.router.hitTest = when {
                        ui.mouseExpanded && vmouse != null -> { x, y -> vmouse.hit(x, y) }
                        !ui.mouseExpanded && miniBox != null -> { x, y -> if (miniBox.contains(x, y)) Zone.Mini else null }
                        else -> null
                    }
                },
            )
            if (ui.mouseExpanded && vmouse != null) VirtualMouse(vmouse, ui.buttonsDown)
            else if (!ui.mouseExpanded && miniBox != null) MiniMouse(miniBox)
            // 点击水波盖在最上面，不接收触摸
            if (ui.ripples.isNotEmpty()) RippleLayer(ui.ripples, onIdle = vm::pruneRipples)
        }

        if (!ui.connected && ui.quality == null) {
            Text(
                stringResource(R.string.session_connecting),
                color = Color.White,
                modifier = Modifier.align(Alignment.Center),
            )
        }

        Toolbar(ui, vm, imeVisible, onBottom = { toolbarBottom = it }, onKeyboard = {
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
                Modifier.align(Alignment.BottomCenter).imePadding().onSizeChanged { barH = it.height },
            )
        }
    }
}

@Composable
private fun Toolbar(ui: UiState, vm: PeerDeskViewModel, imeVisible: Boolean, onBottom: (Float) -> Unit, onKeyboard: () -> Unit) {
    Box(Modifier.fillMaxSize().windowInsetsPadding(WindowInsets.displayCutout.only(WindowInsetsSides.Top + WindowInsetsSides.Horizontal))) {
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
                // 工具条的下沿（容器坐标）：显示时画面不能被它盖住的那一段
                .onGloballyPositioned { onBottom(it.boundsInRoot().bottom) }
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

/** 箭头光标的外形（相对箭头尖的坐标，单位 dp）。 */
private fun arrowPath(): Path = Path().apply {
    moveTo(0f, 0f)
    lineTo(0f, 17f)
    lineTo(4.2f, 13f)
    lineTo(7.4f, 20f)
    lineTo(10f, 19f)
    lineTo(6.9f, 12.2f)
    lineTo(12f, 12.2f)
    close()
}

/**
 * 展开的大鼠标：顶上一个箭头（尖 = 远程光标位置）和右上角的关闭按钮，下面是鼠标：
 * 上半左右两个键，下半是身体（拖把）。只负责画，触摸由触摸层按坐标分流（见 [TouchRouter]）。
 */
@Composable
private fun VirtualMouse(m: VMouse, down: Set<Int>) {
    val d = LocalDensity.current
    val primary = MaterialTheme.colorScheme.primary
    val leftDesc = stringResource(R.string.mouse_left_desc)
    val rightDesc = stringResource(R.string.mouse_right_desc)
    val bodyDesc = stringResource(R.string.mouse_handle_desc)
    val closeDesc = stringResource(R.string.mouse_close_desc)
    Canvas(Modifier.fillMaxSize()) {
        val u = d.density
        val shell = Color.Black.copy(alpha = 0.5f)
        val line = Color.White.copy(alpha = 0.85f)
        // 鼠标外形：上圆下圆的长圆角矩形，上半分成左右两键
        val top = m.left.top
        val l = m.left.left
        val w = m.left.width * 2
        val h = (m.body.top + m.body.height) - top
        val r = CornerRadius(w / 2, w / 2)
        val shellPath = Path().apply { addRoundRect(RoundRect(l, top, l + w, top + h, r)) }
        drawPath(shellPath, shell)
        if (0 in down) {
            val p = Path().apply {
                addRoundRect(RoundRect(m.left.left, m.left.top, m.left.left + m.left.width, m.left.top + m.left.height, topLeftCornerRadius = r))
            }
            drawPath(p, primary.copy(alpha = 0.85f))
        }
        if (1 in down) {
            val p = Path().apply {
                addRoundRect(RoundRect(m.right.left, m.right.top, m.right.left + m.right.width, m.right.top + m.right.height, topRightCornerRadius = r))
            }
            drawPath(p, primary.copy(alpha = 0.85f))
        }
        drawPath(shellPath, line, style = Stroke(width = 1.6f * u))
        drawLine(line, Offset(l + w / 2, top), Offset(l + w / 2, m.left.top + m.left.height), 1.4f * u)
        drawLine(line, Offset(l, m.body.top), Offset(l + w, m.body.top), 1.4f * u)
        // 身体上的“抓握”提示：三道短横线
        for (i in -1..1) {
            val y = m.body.top + m.body.height / 2 + i * 6 * u
            drawLine(line.copy(alpha = 0.5f), Offset(l + w / 2 - 12 * u, y), Offset(l + w / 2 + 12 * u, y), 1.6f * u)
        }
        // 关闭按钮：圆底加叉
        val cx = m.close.left + m.close.width / 2
        val cy = m.close.top + m.close.height / 2
        drawCircle(Color.Black.copy(alpha = 0.6f), radius = 10 * u, center = Offset(cx, cy))
        drawLine(Color.White, Offset(cx - 4 * u, cy - 4 * u), Offset(cx + 4 * u, cy + 4 * u), 1.8f * u)
        drawLine(Color.White, Offset(cx - 4 * u, cy + 4 * u), Offset(cx + 4 * u, cy - 4 * u), 1.8f * u)
        // 箭头光标，尖对准 (tipX, tipY)
        translate(m.tipX, m.tipY) {
            // 按住任意一个键时箭头缩到 0.8 倍（围着尖缩），像点击时光标变小
            val k = if (down.isNotEmpty()) ARROW_PRESSED_SCALE else 1f
            scale(u * 1.2f * k, u * 1.2f * k, pivot = Offset.Zero) {
                val p = arrowPath()
                drawPath(p, Color.White)
                drawPath(p, Color.Black, style = Stroke(width = 1.2f))
            }
        }
    }
    // 给无障碍用的透明块，不接收触摸
    Box(Modifier.fillMaxSize()) {
        fun Modifier.at(b: PxBox) = this
            .offset { IntOffset(b.left.roundToInt(), b.top.roundToInt()) }
            .size(with(d) { b.width.toDp() }, with(d) { b.height.toDp() })
        Box(Modifier.at(m.left).semantics { contentDescription = leftDesc })
        Box(Modifier.at(m.right).semantics { contentDescription = rightDesc })
        Box(Modifier.at(m.body).semantics { contentDescription = bodyDesc })
        Box(Modifier.at(m.close).semantics { contentDescription = closeDesc })
    }
}

/** 点击水波：左键一圈实心加描边，右键双圈。不接收触摸。 */
@Composable
private fun RippleLayer(ripples: List<Ripple>, onIdle: () -> Unit) {
    var now by remember { mutableStateOf(SystemClock.uptimeMillis()) }
    LaunchedEffect(ripples) {
        while (true) {
            withFrameMillis { now = SystemClock.uptimeMillis() }
            if (ripples.all { isExpired(it, now) }) {
                onIdle()
                break
            }
        }
    }
    val d = LocalDensity.current.density
    Canvas(Modifier.fillMaxSize()) {
        for (r in ripples) {
            if (isExpired(r, now)) continue
            val p = rippleProgress(r, now)
            val radius = rippleRadiusDp(p) * d
            val a = rippleAlpha(p)
            val c = Offset(r.x, r.y)
            if (r.button == 1) {
                val color = Color(0xFFFFB74D)
                drawCircle(color.copy(alpha = (a * 2f).coerceAtMost(1f)), radius, c, style = Stroke(2f * d))
                drawCircle(color.copy(alpha = (a * 2f).coerceAtMost(1f)), radius * 0.6f, c, style = Stroke(2f * d))
            } else {
                val color = Color(0xFF4DA3FF)
                drawCircle(color.copy(alpha = a), radius, c)
                drawCircle(color.copy(alpha = (a * 1.5f).coerceAtMost(1f)), radius, c, style = Stroke(1.5f * d))
            }
        }
    }
}

/** 收起后的小鼠标。 */
@Composable
private fun MiniMouse(box: PxBox) {
    val d = LocalDensity.current
    val desc = stringResource(R.string.mouse_mini_desc)
    Box(
        Modifier
            .offset { IntOffset(box.left.roundToInt(), box.top.roundToInt()) }
            .size(with(d) { box.width.toDp() }, with(d) { box.height.toDp() })
            .background(Color.Black.copy(alpha = 0.55f), CircleShape)
            .semantics { contentDescription = desc },
        contentAlignment = Alignment.Center,
    ) {
        Icon(painterResource(R.drawable.ic_mouse), contentDescription = null, tint = Color.White, modifier = Modifier.size(26.dp))
    }
}
