package ai.peerdesk.app

import android.app.Application
import android.content.Context
import android.graphics.Canvas
import android.graphics.ColorFilter
import android.graphics.LinearGradient
import android.graphics.Paint
import android.graphics.PixelFormat
import android.graphics.RectF
import android.graphics.Shader
import android.graphics.Typeface
import android.graphics.drawable.Drawable
import android.os.SystemClock
import android.view.SurfaceView
import android.view.View
import android.view.ViewGroup
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.darkColorScheme
import androidx.compose.material3.lightColorScheme
import androidx.compose.runtime.Composable
import androidx.compose.runtime.SideEffect
import androidx.compose.runtime.currentComposer
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.LocalView
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.unit.dp
import app.cash.paparazzi.DeviceConfig
import app.cash.paparazzi.Paparazzi
import com.android.resources.Density
import com.android.resources.NightMode
import com.android.resources.ScreenOrientation
import org.junit.Rule
import org.junit.Test

/**
 * 界面截图（Paparazzi，JVM 上渲染，不要模拟器）。每个页面的每个状态用假的 [UiState] 渲染成 PNG，
 * 不连任何真实的东西：两种语言、浅色深色、两种手机尺寸，会话页另有横屏。
 *
 * 远程画面是 SurfaceView，截图里画不出来：这里在 SurfaceView 上贴一个“桌面截图占位”（标注清楚不是真画面），
 * 它跟着 SurfaceView 的缩放平移走，所以能看出画面和工具条、鼠标的相对位置。
 * 系统 Toast（“再按一次返回键断开”）不在视图树里，用同样的文字画一个仿制的，图上标了。
 * 主题用 Material3 的静态配色（真机 Android 12+ 用壁纸动态取色，颜色会不同）。
 */
class ScreenshotsTest {
    @get:Rule val paparazzi = Paparazzi(deviceConfig = cfg(PHONE_BIG, landscape = false, dark = false, locale = "en"))

    private class FakeApp(base: Context) : Application() {
        init {
            attachBaseContext(base)
        }
    }

    private data class Phone(val tag: String, val w: Int, val h: Int, val density: Density, val dpi: Int)

    private companion object {
        /** 产品负责人的手机：1080×2400，3 倍。 */
        val PHONE_BIG = Phone("p1080", 1080, 2400, Density.XXHIGH, 480)
        /** 小屏：720×1600，2 倍。 */
        val PHONE_SMALL = Phone("p720", 720, 1600, Density.XHIGH, 320)

        fun cfg(p: Phone, landscape: Boolean, dark: Boolean, locale: String) = DeviceConfig.PIXEL_5.copy(
            screenWidth = if (landscape) p.h else p.w,
            screenHeight = if (landscape) p.w else p.h,
            xdpi = p.dpi,
            ydpi = p.dpi,
            density = p.density,
            orientation = if (landscape) ScreenOrientation.LANDSCAPE else ScreenOrientation.PORTRAIT,
            nightMode = if (dark) NightMode.NIGHT else NightMode.NOTNIGHT,
            locale = locale,
        )
    }

    /** 一个要截的状态。`build` 拿到当前屏幕的像素尺寸和密度，造出界面状态。 */
    private class Shot(val name: String, val page: String, val content: @Composable (Env) -> Unit)

    private class Env(val vm: PeerDeskViewModel, val wPx: Int, val hPx: Int, val density: Float) {
        fun dp(v: Float) = v * density
        val sizes = MouseSizes(
            width = dp(96f), height = dp(150f), arrowH = dp(30f), closeSize = dp(30f), miniSize = dp(48f),
        )
    }

    // ---- 假状态 ----

    private fun quality(
        rtt: Int = 28, lossPermille: Int = 1, fps: Int = 60, kbps: Int = 3200, router: Int? = 3,
    ) = CoreEvent.Quality(
        rttMs = rtt, lossPermille = lossPermille, queueMs = 4, fps = fps, recvKbps = kbps, decodeMs = 4,
        width = 1920, height = 1080, routerMs = router, decoder = "c2.qti.avc.decoder", fec = "RS",
    )

    private fun connect(base: UiState = UiState()) = base.copy(phase = Phase.Connect)

    private fun session(e: Env, mod: UiState.() -> UiState = { this }): UiState {
        val landscape = e.wPx > e.hPx
        var s = UiState(
            phase = Phase.Session, connected = true, route = Route.Direct,
            peer = CoreEvent.Peer(PeerOs.Windows, "Office-PC"),
            quality = quality(),
            containerW = e.wPx.toFloat(), containerH = e.hPx.toFloat(),
            mouseSizes = e.sizes,
            // 工具条下沿：大约两行（按钮行加状态行）
            toolbarBottom = e.dp(if (landscape) 96f else 104f),
        ).mod()
        // 和真实流程一样：按边距夹住平移
        s = s.copy(view = clampState(s.view, s.containerW, s.containerH, s.aspect, s.insets))
        return s
    }

    private fun shots(): List<Shot> = buildList {
        // ---- 连接页 ----
        add(Shot("empty", "connect") { e -> ConnectScreen(connect(), e.vm) })
        add(Shot("filled", "connect") { e ->
            ConnectScreen(connect(UiState(server = "203.0.113.10:4433", code = "123456789", password = "hunter2")), e.vm)
        })
        add(Shot("invalid-code", "connect") { e ->
            ConnectScreen(connect(UiState(server = "203.0.113.10:4433", code = "1234", password = "hunter2")), e.vm)
        })
        val filled = UiState(server = "203.0.113.10:4433", code = "123456789", password = "hunter2")
        add(Shot("ended-connect-failed", "connect") { e ->
            ConnectScreen(connect(filled.copy(notice = EndNotice.ConnectFailed("connection timed out after 10 s: no route to the device"))), e.vm)
        })
        add(Shot("ended-disconnected-by-peer", "connect") { e ->
            ConnectScreen(connect(filled.copy(notice = EndNotice.Ended("the other side closed the session"))), e.vm)
        })
        add(Shot("ended-background", "connect") { e ->
            ConnectScreen(connect(filled.copy(notice = EndNotice.Background)), e.vm)
        })
        add(Shot("ended-no-surface", "connect") { e ->
            ConnectScreen(connect(filled.copy(notice = EndNotice.NoSurface)), e.vm)
        })
        add(Shot("with-export-button", "connect") { e ->
            // 导出按钮在有手势记录（或日志）时显示：往 vm 的记录里放一条
            e.vm.trace.addMotion(1L, 0, 0, intArrayOf(0), floatArrayOf(1f), floatArrayOf(1f))
            ConnectScreen(connect(filled.copy(notice = EndNotice.Ended("the other side closed the session"))), e.vm)
        })

        // ---- 会话页 ----
        add(Shot("toolbar-normal", "session") { e -> Remote(e, session(e)) })
        add(Shot("toolbar-network-router", "session") { e ->
            Remote(e, session(e) { copy(quality = quality(rtt = 220, lossPermille = 55, kbps = 900, router = 80)) })
        })
        add(Shot("toolbar-network-internet", "session") { e ->
            Remote(e, session(e) { copy(quality = quality(rtt = 320, lossPermille = 30, kbps = 1500, router = 4), route = Route.Relay) })
        })
        add(Shot("toolbar-network-unmeasured", "session") { e ->
            Remote(e, session(e) { copy(quality = quality(rtt = 260, lossPermille = 40, kbps = 1200, router = null)) })
        })
        add(Shot("toolbar-hidden", "session") { e -> Remote(e, session(e) { copy(toolbarVisible = false) }) })
        add(Shot("reconnecting", "session") { e ->
            Remote(e, session(e) { copy(reconnecting = CoreEvent.Reconnecting(2, "network changed (Wi-Fi to mobile data)")) })
        })
        add(Shot("connecting-no-picture", "session") { e ->
            Remote(e, session(e) { copy(connected = false, quality = null, peer = null, route = Route.Unknown) }, placeholder = false)
        })
        add(Shot("keyboard-shortcut-bar", "session") { e ->
            val kb = if (e.wPx > e.hPx) 180f else 300f // 横屏的键盘矮一些
            val base = session(e)
            val barH = e.dp(48f)
            // 输入位置在画面下部（比如点了下面的输入框）；键盘弹出后画面被顶起，让它露在键盘上方
            val point = 0.5f to 0.85f
            val bottom = e.hPx - e.dp(kb) - barH
            val shift = imeShiftOnOpen(inputPointY(base.rect, point.second), bottom, e.dp(48f))
            Remote(e, base.copy(inputPoint = point, imeVisibleBottom = bottom, imeShift = shift), imeHeightDp = kb)
        })
        add(Shot("mouse-small", "session") { e -> Remote(e, session(e) { copy(mouseExpanded = false) }) })
        add(Shot("mouse-big-left-pressed", "session") { e ->
            Remote(e, session(e) { copy(mouseExpanded = true, buttonsDown = setOf(0)) })
        })
        add(Shot("tap-ripples", "session") { e ->
            val now = SystemClock.uptimeMillis()
            Remote(e, session(e) {
                copy(
                    ripples = listOf(
                        Ripple(1, e.wPx * 0.35f, e.hPx * 0.40f, 0, now - 120),
                        Ripple(2, e.wPx * 0.65f, e.hPx * 0.55f, 1, now - 120),
                    ),
                )
            })
        })
        add(Shot("back-hint", "session") { e -> Remote(e, session(e), backToast = true) })
    }

    // ---- 渲染 ----

    @Test fun render() {
        val vm = PeerDeskViewModel(FakeApp(paparazzi.context))
        val phones = listOf(PHONE_BIG, PHONE_SMALL)
        for (shot in shots()) {
            for (p in phones) {
                // 会话页另外截横屏；连接页只截竖屏
                val orientations = if (shot.page == "session") listOf(false, true) else listOf(false)
                for (landscape in orientations) for (locale in listOf("en", "zh-rCN")) for (dark in listOf(false, true)) {
                    paparazzi.unsafeUpdateConfig(cfg(p, landscape, dark, locale))
                    val density = p.dpi / 160f
                    val w = if (landscape) p.h else p.w
                    val h = if (landscape) p.w else p.h
                    val env = Env(vm, w, h, density)
                    val name = "${shot.page}-${shot.name}__${p.tag}${if (landscape) "-land" else ""}__${locale.take(2)}__${if (dark) "dark" else "light"}"
                    paparazzi.snapshot(name) {
                        MaterialTheme(colorScheme = if (dark) darkColorScheme() else lightColorScheme()) {
                            Surface(Modifier.fillMaxSize()) { shot.content(env) }
                        }
                    }
                }
            }
        }
    }

    // ---- 会话页外壳：占位画面、键盘、Toast ----

    @Composable
    private fun Remote(
        e: Env, ui: UiState, placeholder: Boolean = true, imeHeightDp: Float = 0f, backToast: Boolean = false,
    ) {
        val root = LocalView.current
        SideEffect {
            if (placeholder) root.findSurfaceView()?.background = DesktopPlaceholder()
        }
        Box(Modifier.fillMaxSize()) {
            SessionScreen(ui, e.vm)
            if (imeHeightDp > 0f) {
                // 键盘：Paparazzi 里没有真的输入法，也没法给界面派发 WindowInsets.ime（试过，界面读不到）。
                // 所以画一块仿制的键盘，快捷键栏用反射借 SessionScreen.kt 里私有的那个 ShortcutBar 画出来、放在键盘上方；
                // 画面被顶起的量用同一个纯函数算好放进假状态。键盘按钮的“选中”状态因此不会亮。
                Box(Modifier.align(Alignment.BottomCenter).fillMaxWidth().height(imeHeightDp.dp).background(Color(0xFF3A3A40))) {
                    Text("SOFT KEYBOARD (mock)", color = Color.White, modifier = Modifier.align(Alignment.Center))
                }
                BorrowedShortcutBar(ui, e.vm, Modifier.align(Alignment.BottomCenter).padding(bottom = imeHeightDp.dp))
            }
            if (backToast) {
                // 仿制的系统 Toast（真的 Toast 不在视图树里，截图拍不到）
                Box(Modifier.align(Alignment.BottomCenter).padding(bottom = 96.dp)) {
                    Text(
                        stringResource(R.string.back_again_to_disconnect) + "  (toast mock)",
                        color = Color.White,
                        modifier = Modifier.background(Color(0xE6303030), androidx.compose.foundation.shape.RoundedCornerShape(24.dp))
                            .padding(horizontal = 20.dp, vertical = 12.dp),
                    )
                }
            }
        }
    }
}

/** 借 SessionScreen.kt 里私有的 ShortcutBar（编译后是私有静态方法，反射调用；只在截图测试里这么做）。 */
@Composable
private fun BorrowedShortcutBar(ui: UiState, vm: PeerDeskViewModel, modifier: Modifier) {
    val m = Class.forName("ai.peerdesk.app.SessionScreenKt").declaredMethods.first { it.name == "ShortcutBar" }
    m.isAccessible = true
    m.invoke(null, ui, vm, modifier, currentComposer, 0)
}

private fun View.findSurfaceView(): SurfaceView? {
    if (this is SurfaceView) return this
    if (this is ViewGroup) for (i in 0 until childCount) getChildAt(i).findSurfaceView()?.let { return it }
    return null
}

/** 假的桌面：渐变壁纸、几个窗口、任务栏，中间写明“截图占位”。画在 SurfaceView 的背景上，跟着它的缩放平移走。 */
private class DesktopPlaceholder : Drawable() {
    private val p = Paint(Paint.ANTI_ALIAS_FLAG)

    override fun draw(c: Canvas) {
        val w = bounds.width().toFloat()
        val h = bounds.height().toFloat()
        p.shader = LinearGradient(0f, 0f, w, h, 0xFF2B5C9E.toInt(), 0xFF6C3FA0.toInt(), Shader.TileMode.CLAMP)
        c.drawRect(0f, 0f, w, h, p)
        p.shader = null
        // 窗口
        fun window(l: Float, t: Float, r: Float, b: Float, title: Int) {
            p.color = 0xFFF2F2F2.toInt(); c.drawRoundRect(RectF(l * w, t * h, r * w, b * h), 8f, 8f, p)
            p.color = title; c.drawRect(l * w, t * h, r * w, t * h + 0.05f * h, p)
            p.color = 0xFFB8B8B8.toInt()
            var y = t * h + 0.09f * h
            while (y < b * h - 0.03f * h) { c.drawRect(l * w + 0.02f * w, y, r * w - 0.04f * w, y + 0.012f * h, p); y += 0.035f * h }
        }
        window(0.06f, 0.10f, 0.58f, 0.70f, 0xFF3C7DD9.toInt())
        window(0.40f, 0.30f, 0.94f, 0.82f, 0xFF2E9E6B.toInt())
        // 任务栏
        p.color = 0xFF1B1B1F.toInt(); c.drawRect(0f, h * 0.93f, w, h, p)
        p.color = 0xFF3C7DD9.toInt(); c.drawRect(w * 0.01f, h * 0.945f, w * 0.04f, h * 0.99f, p)
        // 标注
        p.color = Color.White.value.toLong().let { 0xFFFFFFFF.toInt() }
        p.typeface = Typeface.DEFAULT_BOLD
        p.textSize = h * 0.028f
        p.textAlign = Paint.Align.RIGHT
        c.drawText("SCREENSHOT PLACEHOLDER (not a real remote picture)", w * 0.99f, h * 0.975f, p)
    }

    override fun setAlpha(alpha: Int) = Unit
    override fun setColorFilter(colorFilter: ColorFilter?) = Unit
    @Deprecated("Deprecated in Java")
    override fun getOpacity() = PixelFormat.OPAQUE
}
