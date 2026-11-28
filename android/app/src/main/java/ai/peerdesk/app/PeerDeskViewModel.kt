package ai.peerdesk.app

import android.app.Application
import android.content.Context
import android.os.SystemClock
import android.view.Surface
import androidx.lifecycle.AndroidViewModel
import androidx.lifecycle.viewModelScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.MutableSharedFlow
import kotlinx.coroutines.flow.SharedFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asSharedFlow
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
    /** 屏幕上的鼠标是展开的大鼠标，还是收成了小鼠标（默认小鼠标）。 */
    val mouseExpanded: Boolean = false,
    val buttonsDown: Set<Int> = emptySet(),
    val mouseFx: Float = MOUSE_DEFAULT_FX,
    val mouseFy: Float = MOUSE_DEFAULT_FY,
    val miniFx: Float = MINI_DEFAULT_FX,
    val miniFy: Float = MINI_DEFAULT_FY,
    /** 大小鼠标的像素尺寸，界面按 dp 换算后设置；没设置前不画。 */
    val mouseSizes: MouseSizes? = null,
    /** 点击水波（界面画，过期的会被清掉）。 */
    val ripples: List<Ripple> = emptyList(),
    /** 输入位置 = 最后一次左键点击在画面矩形里的比例位置（跟着缩放平移走）。 */
    val inputPoint: Pair<Float, Float>? = null,
    /** 软键盘弹出时可见区域的下沿（键盘顶，或快捷键栏顶）；键盘没弹出为 null。 */
    val imeVisibleBottom: Float? = null,
    /** 画面被软键盘向上顶起的距离（像素，>= 0）。 */
    val imeShift: Float = 0f,
    val toolbarVisible: Boolean = true,
    /** 工具条下沿在容器坐标里的位置（像素），界面测量后报告。 */
    val toolbarBottom: Float = 0f,
    /** 刘海（display cutout）四边的宽度（像素）。 */
    val cutout: Edges = Edges.ZERO,
    val locked: Set<ModKey> = emptySet(),

    // 画面布局
    val containerW: Float = 0f,
    val containerH: Float = 0f,
    val view: ViewState = ViewState(),
) {
    /** 画面平移时要避开的边距：工具条（显示时）和刘海。键盘由 imeShift 处理，不在这里。 */
    val insets: Edges get() = safeInsets(toolbarVisible, toolbarBottom, cutout)
    val peerOs: PeerOs get() = peer?.os ?: PeerOs.Unknown
    val aspect: Float get() = aspectOf(quality?.width ?: 0, quality?.height ?: 0)
    /** 没有被键盘顶起时画面的矩形（缩放、平移的计算都以它为准）。 */
    val rect: ViewRect get() = rectOf(view, containerW, containerH, aspect)
    /** 实际显示的矩形：向上顶起 [imeShift]。SurfaceView 的位置和 setView 都用它。 */
    val shownRect: ViewRect get() = rect.copy(top = rect.top - imeShift)
    val vmouse: VMouse?
        get() = mouseSizes?.let { vmouseLayout(containerW, containerH, mouseFx, mouseFy, it) }
    val miniBox: PxBox?
        get() = mouseSizes?.let { miniLayout(containerW, containerH, miniFx, miniFy, it) }
}

/**
 * 界面状态和会话生命周期。
 *
 * 流程：连接页点 Connect → phase=Session，会话页的 SurfaceView 创建好后调 [startSession]；
 * 后台线程循环取事件；收到 `ended`（或用户断开、切后台）后 `free` 句柄并回到连接页。
 */
class PeerDeskViewModel(app: Application) : AndroidViewModel(app) {
    private val prefs = app.getSharedPreferences("peerdesk", Context.MODE_PRIVATE)
    private val _ui = MutableStateFlow(
        UiState(
            server = prefs.getString(KEY_SERVER, "") ?: "",
            code = prefs.getString(KEY_CODE, "") ?: "",
            // 记住的密码是加密存的（见 SecretStore），解不开就当没记住
            password = prefs.getString(KEY_PASSWORD, null)?.let(SecretStore::decrypt) ?: "",
            mouseExpanded = prefs.getBoolean(KEY_MOUSE_EXPANDED, false),
            mouseFx = prefs.getFloat(KEY_MOUSE_FX, MOUSE_DEFAULT_FX),
            mouseFy = prefs.getFloat(KEY_MOUSE_FY, MOUSE_DEFAULT_FY),
            miniFx = prefs.getFloat(KEY_MINI_FX, MINI_DEFAULT_FX),
            miniFy = prefs.getFloat(KEY_MINI_FY, MINI_DEFAULT_FY),
        ),
    )
    val ui: StateFlow<UiState> = _ui.asStateFlow()

    /** 手势识别说“长按到点了”：界面收到后震一下。 */
    private val _haptics = MutableSharedFlow<Unit>(extraBufferCapacity = 4)
    val haptics: SharedFlow<Unit> = _haptics.asSharedFlow()

    /** 屏幕密度（界面设置），换算 dp 的边距用。 */
    @Volatile var density: Float = 1f

    /** 最近 120 秒的原始触摸记录，导出给开发者看真机上的问题。 */
    val trace = GestureTrace()

    private var nextRippleId = 0L

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
        // 记住服务器、设备码和密码（密码加密后再存），下次不用重输
        val sealed = s.password.takeIf { it.isNotEmpty() }?.let(SecretStore::encrypt)
        prefs.edit()
            .putString(KEY_SERVER, s.server.trim())
            .putString(KEY_CODE, s.code)
            .apply { if (sealed != null) putString(KEY_PASSWORD, sealed) else remove(KEY_PASSWORD) }
            .apply()
        _ui.update {
            it.copy(
                phase = Phase.Session, notice = null, connected = false, route = Route.Unknown,
                peer = null, quality = null, reconnecting = null, coreNotice = null,
                buttonsDown = emptySet(), view = ViewState(), locked = emptySet(),
            )
        }
    }

    // —— 会话生命周期 ——

    /** SurfaceView 创建好后调用。已经有会话在跑就忽略。 */
    fun startSession(surface: Surface, slopPx: Float, notchPx: Float) {
        if (session != null || _ui.value.phase != Phase.Session) return
        val s = _ui.value
        synchronized(heldButtons) { heldButtons.clear() }
        userDisconnect = false
        backgroundEnd = false
        stopRequestedAt = 0L
        bar.reset()
        backTracker.reset()
        val dataDir = getApplication<Application>().filesDir.absolutePath
        val h = Native.start(dataDir, s.server.trim(), s.code, s.password, surface, slopPx, notchPx)
        if (h == 0L) {
            _ui.update { it.copy(phase = Phase.Connect, notice = EndNotice.NoSurface) }
            return
        }
        val ns = NativeSession(h)
        pushedRect = null
        pushedHint = null
        hint.reset()
        trace.clear()
        trace.addContainer(SystemClock.uptimeMillis(), s.containerW, s.containerH)
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
                peer = null, reconnecting = null, coreNotice = null,
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
                        next = next.copy(view = clampState(next.view, next.containerW, next.containerH, next.aspect, next.insets))
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
        if (w != _ui.value.containerW || h != _ui.value.containerH) trace.addContainer(SystemClock.uptimeMillis(), w, h)
        _ui.update { it.copy(containerW = w, containerH = h, view = clampState(it.view, w, h, it.aspect, it.insets)) }
        pushView()
    }

    /** 触摸层交来的：按下/抬起/取消逐根走 touch，同一个 ACTION_MOVE 里所有手指的移动一起走 moves。 */
    fun forwardTouch(touches: List<TouchOp>, moves: List<TouchOp>, tMs: Long) {
        val ns = session ?: return
        val ops = ArrayList<ViewOp>()
        touches.forEach { ops += parseViewOps(ns.touch(it.id, it.phase, it.x, it.y, tMs)) }
        if (moves.isNotEmpty()) {
            ops += parseViewOps(
                ns.moves(
                    IntArray(moves.size) { moves[it].id },
                    FloatArray(moves.size) { moves[it].x },
                    FloatArray(moves.size) { moves[it].y },
                    tMs,
                ),
            )
        }
        applyViewOps(ops)
    }

    /** 记录一个原始 MotionEvent（导出手势记录用）。 */
    fun recordMotion(t: Long, action: Int, index: Int, ids: IntArray, xs: FloatArray, ys: FloatArray) =
        trace.addMotion(t, action, index, ids, xs, ys)

    /** 手势记录的 JSON Lines 文本（头部带密度和容器尺寸）。 */
    /** 上一次会话有没有手势记录（连接页据此显示“导出上次的手势记录”）。 */
    fun hasTrace(): Boolean = !trace.isEmpty()

    fun traceText(): String {
        val s = _ui.value
        return trace.toJsonLines(density, s.containerW, s.containerH)
    }

    fun tick() {
        val ns = session ?: return
        applyViewOps(parseViewOps(ns.tick(SystemClock.uptimeMillis())))
    }

    private fun applyViewOps(ops: List<ViewOp>) {
        if (ops.isEmpty()) return
        if (ops.any { it == ViewOp.Haptic }) _haptics.tryEmit(Unit)
        val taps = ops.filterIsInstance<ViewOp.Tap>()
        // 点击坐标是动作发生时的屏幕坐标，输入位置按变换之前显示的矩形换算
        val before = _ui.value.shownRect
        _ui.update { it.copy(view = applyOps(it.view, ops, it.containerW, it.containerH, it.aspect, it.imeShift, it.insets)) }
        taps.forEach { addRippleAt(it.x, it.y, it.button); if (it.button == 0) setInputPoint(before, it.x, it.y) }
        pushView()
    }

    // —— 点击水波 ——

    private fun addRippleAt(x: Float, y: Float, button: Int) {
        val now = SystemClock.uptimeMillis()
        val r = Ripple(nextRippleId++, x, y, button, now)
        _ui.update { it.copy(ripples = addRipple(it.ripples, r, now)) }
    }

    /** 界面画完最后一个水波后清理。 */
    fun pruneRipples() {
        val now = SystemClock.uptimeMillis()
        _ui.update { if (it.ripples.isEmpty()) it else it.copy(ripples = pruneRipples(it.ripples, now)) }
    }

    // —— 软键盘和输入位置 ——

    /** 记住输入位置（最后一次左键点击）。键盘在时按“新输入位置”的规则重算顶起量。 */
    private fun setInputPoint(shown: ViewRect, x: Float, y: Float) {
        val fr = toFraction(shown, x, y) ?: return
        _ui.update { s ->
            val bottom = s.imeVisibleBottom
            val shift = if (bottom == null) s.imeShift else {
                imeShiftOnNewPoint(s.imeShift, inputPointY(s.rect, fr.second), bottom, 48f * density, 16f * density)
            }
            s.copy(inputPoint = fr, imeShift = shift)
        }
    }

    /**
     * 界面报告键盘状态：visibleBottom 是可见区域的下沿（键盘顶，有快捷键栏时是它的顶），键盘没弹出传 null。
     * 弹出时顶起量按 [imeShiftOnOpen]，之后（键盘高度变化）按 [imeShiftOnNewPoint]，关闭回 0。
     */
    fun onImeChanged(visibleBottom: Float?) {
        _ui.update { s ->
            if (visibleBottom == null) return@update if (s.imeVisibleBottom == null && s.imeShift == 0f) s
            else s.copy(imeVisibleBottom = null, imeShift = 0f)
            val p = s.inputPoint
            val shift = if (p == null) 0f else {
                val y = inputPointY(s.rect, p.second)
                if (s.imeVisibleBottom == null) imeShiftOnOpen(y, visibleBottom, 48f * density)
                else imeShiftOnNewPoint(s.imeShift, y, visibleBottom, 48f * density, 16f * density)
            }
            s.copy(imeVisibleBottom = visibleBottom, imeShift = shift)
        }
        pushView()
    }

    /** 上次告诉核心的画面矩形和尺寸提示；没变就不再发（每个质量事件、长按震动都会走到 [pushView]）。 */
    private var pushedRect: ViewRect? = null
    private var pushedHint: Pair<Int, Int>? = null
    private var hintJob: Job? = null
    private val hint = SizeHintDebouncer(HINT_DELAY_MS) { SystemClock.uptimeMillis() }

    /**
     * 把画面矩形告诉核心（布局、缩放、平移、键盘顶起变了都要调）。
     * 矩形每次变化都发（触点换算要用）；编码尺寸提示（viewW/viewH）防抖，最后一次变化后 300 ms 才更新，
     * 之前一直用上一次的值（见 [SizeHintDebouncer]）。矩形和提示都没变什么也不做。
     */
    @Synchronized
    private fun pushView() {
        val ns = session ?: return
        val s = _ui.value
        if (s.containerW <= 0f || s.containerH <= 0f) return
        val rect = s.shownRect
        // 被控端按“画面画出来有多大”编码：放大时画面变大，字也要跟着清楚（被控端按自己的原始分辨率封顶）
        val size = hint.submit(rect.width.toInt(), rect.height.toInt())
        val rectChanged = rect != pushedRect
        if (!rectChanged && size == pushedHint) return
        pushedRect = rect
        pushedHint = size
        if (rectChanged) trace.addRect(SystemClock.uptimeMillis(), rect)
        ns.setView(rect, size.first, size.second)
        // 画面缩放、平移、布局变了，同一个箭头尖对应的远程位置也变了：大鼠标展开着就重新对一次。
        // 只在矩形变了时做：否则触屏拖拽（按着左键）中途来一个质量事件，选区会跳到箭头尖。
        if (rectChanged) pointAtTip(ns, s)
        if (hint.hasPending()) {
            hintJob?.cancel()
            hintJob = viewModelScope.launch {
                delay(HINT_DELAY_MS + 20)
                pushView()
            }
        }
    }

    private fun pointAtTip(ns: NativeSession? = session, s: UiState = _ui.value) {
        if (ns == null || !s.mouseExpanded) return
        s.vmouse?.let { ns.pointAt(it.tipX, it.tipY) }
    }

    fun setMouseSizes(sizes: MouseSizes) {
        if (_ui.value.mouseSizes != sizes) _ui.update { it.copy(mouseSizes = sizes) }
    }

    /** 触摸层认出的鼠标动作。 */
    fun onMouseEvent(e: MouseEvent) {
        when (e) {
            is MouseEvent.ButtonDown -> {
                pointAtTip() // 先把光标放到箭头尖，再按键
                mouseButton(e.index, true)
                // 按下时尖上冒一个水波；左键按下的位置也是输入位置
                _ui.value.vmouse?.let { m ->
                    addRippleAt(m.tipX, m.tipY, e.index)
                    if (e.index == 0) setInputPoint(_ui.value.shownRect, m.tipX, m.tipY)
                }
            }
            is MouseEvent.ButtonUp -> mouseButton(e.index, false)
            is MouseEvent.Move -> {
                _ui.update { s ->
                    val z = s.mouseSizes ?: return@update s
                    val (fx, fy) = dragMouseGroup(
                        s.mouseFx, s.mouseFy, e.dx, e.dy, s.containerW, s.containerH, z.width, z.arrowH + z.height,
                    )
                    s.copy(mouseFx = fx, mouseFy = fy)
                }
                pointAtTip()
            }
            is MouseEvent.MiniMove -> _ui.update { s ->
                val z = s.mouseSizes ?: return@update s
                val (fx, fy) = dragMouseGroup(s.miniFx, s.miniFy, e.dx, e.dy, s.containerW, s.containerH, z.miniSize, z.miniSize)
                s.copy(miniFx = fx, miniFy = fy)
            }
            MouseEvent.DragEnd -> saveMouse()
            // 收起不发任何输入；已按住的键由触摸层在抬起时松开
            MouseEvent.Collapse -> {
                _ui.update { s ->
                    val z = s.mouseSizes
                    val big = s.vmouse
                    if (z == null || big == null) return@update s.copy(mouseExpanded = false)
                    val (fx, fy) = collapsedFrom(big.bounds, s.containerW, s.containerH, z)
                    s.copy(mouseExpanded = false, miniFx = fx, miniFy = fy)
                }
                saveMouse()
            }
            MouseEvent.Expand -> {
                _ui.update { s ->
                    val z = s.mouseSizes
                    val mini = s.miniBox
                    if (z == null || mini == null) return@update s.copy(mouseExpanded = true)
                    val (fx, fy) = expandedFrom(mini, s.containerW, s.containerH, z)
                    s.copy(mouseExpanded = true, mouseFx = fx, mouseFy = fy)
                }
                saveMouse()
                pointAtTip()
            }
        }
    }

    private val heldButtons = HashSet<Int>()

    /** 屏幕上的左键（0）、右键（1）。重复的按下/抬起不转发。 */
    fun mouseButton(index: Int, down: Boolean) {
        val ns = session ?: return
        val changed = synchronized(heldButtons) { if (down) heldButtons.add(index) else heldButtons.remove(index) }
        if (!changed) return
        _ui.update { it.copy(buttonsDown = if (down) it.buttonsDown + index else it.buttonsDown - index) }
        ns.button(index, down)
    }

    /** 记住大小鼠标的位置。 */
    private fun saveMouse() {
        val s = _ui.value
        prefs.edit()
            .putFloat(KEY_MOUSE_FX, s.mouseFx).putFloat(KEY_MOUSE_FY, s.mouseFy)
            .putFloat(KEY_MINI_FX, s.miniFx).putFloat(KEY_MINI_FY, s.miniFy)
            .putBoolean(KEY_MOUSE_EXPANDED, s.mouseExpanded)
            .apply()
    }

    /** 显示/收起工具条：上边距变了，重新夹住平移。 */
    fun toggleToolbar() = setToolbarVisible(!_ui.value.toolbarVisible)

    private fun setToolbarVisible(visible: Boolean) {
        _ui.update { reclamp(it.copy(toolbarVisible = visible)) }
        pushView()
    }

    private fun reclamp(s: UiState): UiState =
        s.copy(view = clampState(s.view, s.containerW, s.containerH, s.aspect, s.insets))

    /** 界面报告工具条下沿和刘海：变了就重新夹住平移并告诉核心。 */
    fun onSafeInsets(toolbarBottom: Float, cutout: Edges) {
        if (toolbarBottom == _ui.value.toolbarBottom && cutout == _ui.value.cutout) return
        _ui.update { reclamp(it.copy(toolbarBottom = toolbarBottom, cutout = cutout)) }
        pushView()
    }

    // —— 返回键 ——

    private val backTracker = BackPressTracker(BACK_WINDOW_MS) { SystemClock.uptimeMillis() }

    /**
     * 会话中按返回：第一次显示工具条并让界面提示，2 秒内再按一次才断开。
     * 返回要界面做的事（提示文字界面自己取）。
     */
    fun onBackPressed(): BackAction {
        val a = backTracker.onBack()
        when (a) {
            BackAction.ShowHint -> if (!_ui.value.toolbarVisible) setToolbarVisible(true)
            BackAction.Disconnect -> disconnect()
        }
        return a
    }

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
        private const val KEY_MOUSE_FX = "mouse_fx"
        private const val KEY_MOUSE_FY = "mouse_fy"
        private const val KEY_MINI_FX = "mini_fx"
        private const val KEY_MINI_FY = "mini_fy"
        private const val KEY_MOUSE_EXPANDED = "mouse_expanded"
        private const val KEY_CODE = "code"
        private const val KEY_PASSWORD = "password-sealed"
        private const val STOP_GRACE_MS = 3000L
        /** 编码尺寸提示在最后一次变化后等这么久才更新。 */
        const val HINT_DELAY_MS = 300L
        /** 会话中第二次返回要在这么久之内才断开。 */
        const val BACK_WINDOW_MS = 2000L
    }
}
