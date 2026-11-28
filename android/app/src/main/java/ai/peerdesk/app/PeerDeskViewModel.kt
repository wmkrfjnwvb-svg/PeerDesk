package ai.peerdesk.app

import android.app.Application
import android.content.Context
import android.os.SystemClock
import android.view.Surface
import androidx.lifecycle.AndroidViewModel
import androidx.lifecycle.viewModelScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.isActive
import kotlinx.coroutines.launch

enum class Phase { Connect, Session }

/** 回到连接页时要显示的原因。文字由界面按语言取，核心给的原因文字原样带上。 */
sealed interface EndNotice {
    /** 还没连上就结束了。 */
    data class ConnectFailed(val text: String) : EndNotice
    /** 连上之后结束了。 */
    data class Ended(val text: String) : EndNotice
    data object Background : EndNotice
    data object NoSurface : EndNotice
}

data class UiState(
    val phase: Phase = Phase.Connect,
    val server: String = "",
    val code: String = "",
    val password: String = "",
    val notice: EndNotice? = null,

    // 会话
    val connected: Boolean = false,
    val route: Route = Route.Unknown,
    val peer: CoreEvent.Peer? = null,
    val quality: CoreEvent.Quality? = null,
    val reconnecting: CoreEvent.Reconnecting? = null,
    val coreNotice: String? = null,
    val trackpad: Boolean = false,
    val toolbarVisible: Boolean = true,
    val locked: Set<ModKey> = emptySet(),

    // 画面布局
    val containerW: Float = 0f,
    val containerH: Float = 0f,
    val view: ViewState = ViewState(),
) {
    val peerOs: PeerOs get() = peer?.os ?: PeerOs.Unknown
    val aspect: Float get() = aspectOf(quality?.width ?: 0, quality?.height ?: 0)
    val rect: ViewRect get() = rectOf(view, containerW, containerH, aspect)
}

/**
 * 界面状态和会话生命周期。
 *
 * 流程：连接页点 Connect → phase=Session，会话页的 SurfaceView 创建好后调 [startSession]；
 * 后台线程循环取事件；收到 `ended`（或用户断开、切后台）后 `free` 句柄并回到连接页。
 */
class PeerDeskViewModel(app: Application) : AndroidViewModel(app) {
    private val prefs = app.getSharedPreferences("peerdesk", Context.MODE_PRIVATE)
    private val _ui = MutableStateFlow(UiState(server = prefs.getString(KEY_SERVER, "") ?: ""))
    val ui: StateFlow<UiState> = _ui.asStateFlow()

    @Volatile private var session: NativeSession? = null
    private val bar = ShortcutBarState()

    @Volatile private var userDisconnect = false
    @Volatile private var backgroundEnd = false
    @Volatile private var stopRequestedAt = 0L

    // —— 连接页 ——

    fun setServer(v: String) = _ui.update { it.copy(server = v) }
    fun setCode(v: String) = _ui.update { it.copy(code = sanitizeCode(v)) }
    fun setPassword(v: String) = _ui.update { it.copy(password = v) }

    /** 点 Connect：记住服务器地址，进会话页（真正连接在 SurfaceView 创建后）。 */
    fun connect() {
        val s = _ui.value
        prefs.edit().putString(KEY_SERVER, s.server.trim()).apply()
        _ui.update {
            it.copy(
                phase = Phase.Session, notice = null, connected = false, route = Route.Unknown,
                peer = null, quality = null, reconnecting = null, coreNotice = null,
                trackpad = false, view = ViewState(), locked = emptySet(),
            )
        }
    }

    // —— 会话生命周期 ——

    /** SurfaceView 创建好后调用。已经有会话在跑就忽略。 */
    fun startSession(surface: Surface, slopPx: Float, notchPx: Float) {
        if (session != null || _ui.value.phase != Phase.Session) return
        val s = _ui.value
        userDisconnect = false
        backgroundEnd = false
        stopRequestedAt = 0L
        bar.reset()
        val dataDir = getApplication<Application>().filesDir.absolutePath
        val h = Native.start(dataDir, s.server.trim(), s.code, s.password, surface, slopPx, notchPx)
        if (h == 0L) {
            _ui.update { it.copy(phase = Phase.Connect, notice = EndNotice.NoSurface) }
            return
        }
        val ns = NativeSession(h)
        session = ns
        pushView()
        viewModelScope.launch(Dispatchers.IO) { eventLoop(ns) }
    }

    private fun eventLoop(ns: NativeSession) {
        var end: CoreEvent.Ended? = null
        while (viewModelScope.isActive) {
            val ev = parseEvent(ns.nextEvent(200))
            if (ev is CoreEvent.Ended) {
                end = ev
                break
            }
            if (ev != null) handleEvent(ev)
            // 已经叫核心停了却迟迟没有结束事件：不无限等
            val req = stopRequestedAt
            if (req != 0L && SystemClock.uptimeMillis() - req > STOP_GRACE_MS) break
        }
        ns.free()
        session = null
        val notice: EndNotice? = when {
            backgroundEnd -> EndNotice.Background
            userDisconnect -> null
            end == null -> EndNotice.Ended("")
            end.text.isBlank() && end.ok -> null
            _ui.value.connected -> EndNotice.Ended(end.text)
            else -> EndNotice.ConnectFailed(end.text)
        }
        _ui.update {
            it.copy(
                phase = Phase.Connect, notice = notice, connected = false, quality = null,
                peer = null, reconnecting = null, coreNotice = null, password = "",
            )
        }
    }

    private fun handleEvent(ev: CoreEvent) {
        when (ev) {
            is CoreEvent.Connected ->
                _ui.update { it.copy(connected = true, route = ev.route, reconnecting = null) }
            is CoreEvent.Peer -> _ui.update { it.copy(peer = ev) }
            is CoreEvent.Quality -> {
                _ui.update { s ->
                    val sizeChanged = ev.width != s.quality?.width || ev.height != s.quality.height
                    var next = s.copy(quality = ev, reconnecting = null)
                    if (sizeChanged) {
                        // 画面宽高比变了：按新比例重新夹住平移
                        next = next.copy(view = clampState(next.view, next.containerW, next.containerH, next.aspect))
                    }
                    next
                }
                pushView()
            }
            is CoreEvent.Reconnecting -> _ui.update { it.copy(reconnecting = ev) }
            is CoreEvent.Notice -> _ui.update { it.copy(coreNotice = ev.text) }
            is CoreEvent.Ended -> Unit
        }
    }

    /** 用户点 Disconnect。 */
    fun disconnect() {
        userDisconnect = true
        stopSession()
    }

    /** Activity 进入后台（onStop）：释放按住的键和手指，断开。第一版回到前台需要重新连。 */
    fun onBackground() {
        if (session != null) backgroundEnd = true
        stopSession()
    }

    private fun stopSession() {
        val ns = session ?: return
        ns.releaseAll()
        ns.stop()
        if (stopRequestedAt == 0L) stopRequestedAt = SystemClock.uptimeMillis()
    }

    override fun onCleared() {
        session?.let {
            it.releaseAll()
            it.stop()
            it.free()
        }
        session = null
    }

    // —— 触控和画面 ——

    fun onContainerSize(w: Float, h: Float) {
        _ui.update { it.copy(containerW = w, containerH = h, view = clampState(it.view, w, h, it.aspect)) }
        pushView()
    }

    fun forwardTouch(ops: List<TouchOp>, tMs: Long) {
        val ns = session ?: return
        val viewOps = ops.flatMap { parseViewOps(ns.touch(it.id, it.phase, it.x, it.y, tMs)) }
        applyViewOps(viewOps)
    }

    fun tick() {
        val ns = session ?: return
        applyViewOps(parseViewOps(ns.tick(SystemClock.uptimeMillis())))
    }

    private fun applyViewOps(ops: List<ViewOp>) {
        if (ops.isEmpty()) return
        _ui.update { it.copy(view = applyOps(it.view, ops, it.containerW, it.containerH, it.aspect)) }
        pushView()
    }

    /** 把画面矩形告诉核心（布局、缩放、平移变了都要调）。 */
    private fun pushView() {
        val ns = session ?: return
        val s = _ui.value
        if (s.containerW <= 0f || s.containerH <= 0f) return
        // 被控端按“画面画出来有多大”编码：放大时画面变大，字也要跟着清楚（被控端按自己的原始分辨率封顶）
        ns.setView(s.rect, isZoomed(s.view), s.rect.width.toInt(), s.rect.height.toInt())
    }

    fun setTrackpad(on: Boolean) {
        _ui.update { it.copy(trackpad = on) }
        session?.setMode(if (on) 1 else 0)
    }

    fun toggleToolbar() = _ui.update { it.copy(toolbarVisible = !it.toolbarVisible) }

    // —— 键盘 ——

    private fun send(actions: List<KeyAction>) {
        val ns = session ?: return
        actions.forEach { ns.key(it.keycode, it.down) }
        _ui.update { it.copy(locked = ModKey.entries.filter(bar::isLocked).toSet()) }
    }

    fun toggleModifier(m: ModKey) {
        bar.toggle(m)
        send(emptyList())
    }

    /** 快捷键栏上的普通键。 */
    fun pressKey(keycode: Int) = send(bar.press(keycode))

    fun shortcut(keycode: Int) = send(ShortcutBarState.shortcut(_ui.value.peerOs, keycode))

    /** 软键盘提交的文字。 */
    fun typeText(text: String) {
        val ns = session ?: return
        if (text.isEmpty()) return
        val keys = bar.typed(text)
        if (keys == null) ns.text(text) else keys.forEach { ns.key(it.keycode, it.down) }
        _ui.update { it.copy(locked = ModKey.entries.filter(bar::isLocked).toSet()) }
    }

    /** 软键盘上的退格、回车等（带上锁住的修饰键）。 */
    fun softKey(keycode: Int) = pressKey(keycode)

    /** 外接键盘：原样转发，返回核心认不认识。 */
    fun hardwareKey(keycode: Int, down: Boolean): Boolean = session?.key(keycode, down) ?: false

    val sessionActive: Boolean get() = session != null

    companion object {
        private const val KEY_SERVER = "server"
        private const val STOP_GRACE_MS = 3000L
    }
}
