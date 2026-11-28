package ai.peerdesk.app

import android.annotation.SuppressLint
import android.content.Context
import android.text.InputType
import android.view.KeyEvent
import android.view.MotionEvent
import android.view.SurfaceHolder
import android.view.SurfaceView
import android.view.View
import android.view.inputmethod.BaseInputConnection
import android.view.inputmethod.EditorInfo
import android.view.inputmethod.InputConnection
import android.widget.FrameLayout
import kotlin.math.roundToInt

/**
 * 远程画面的承载：一个裁剪的 FrameLayout，里面放 SurfaceView。
 *
 * 缩放和平移用 View 变换：SurfaceView 的布局尺寸固定在“缩放 1 倍时完整放下”的大小（取整），
 * 只在容器尺寸或画面宽高比变化时才改；每次画面矩形变化只设 pivot=(0,0)、scaleX/scaleY、translationX/Y。
 * 依据：Android 官方文档说 N（API 24）起 SurfaceView 的窗口位置和其他 View 的渲染同步更新，
 * 平移、缩放 SurfaceView 不会产生渲染瑕疵；minSdk 是 29，所以都适用。
 * （旧做法每一帧改布局宽高，left 和 width 各自取整，会有 1 像素抖动，捏合时很明显。）
 * 父容器的裁剪保留：放大后超出容器的部分被裁掉。
 */
class RemoteSurfaceLayout(
    context: Context,
    onCreated: (android.view.Surface) -> Unit,
    onDestroyed: () -> Unit,
) : FrameLayout(context) {
    private val surfaceView = SurfaceView(context)

    init {
        clipChildren = true
        clipToPadding = true
        surfaceView.holder.addCallback(object : SurfaceHolder.Callback {
            override fun surfaceCreated(holder: SurfaceHolder) = onCreated(holder.surface)
            override fun surfaceChanged(holder: SurfaceHolder, format: Int, width: Int, height: Int) = Unit
            override fun surfaceDestroyed(holder: SurfaceHolder) = onDestroyed()
        })
        surfaceView.pivotX = 0f
        surfaceView.pivotY = 0f
        addView(surfaceView, LayoutParams(1, 1))
    }

    /**
     * 画面矩形（界面坐标，已算上缩放、平移、键盘顶起）变了就调。
     * [baseW]、[baseH] 是缩放 1 倍时的完整放下尺寸（浮点），取整后作为布局尺寸；缩放倍数按取整后的宽度算。
     */
    fun place(rect: ViewRect, baseW: Float, baseH: Float) {
        val w = baseW.roundToInt().coerceAtLeast(1)
        val h = baseH.roundToInt().coerceAtLeast(1)
        val lp = surfaceView.layoutParams as LayoutParams
        if (lp.width != w || lp.height != h || lp.leftMargin != 0 || lp.topMargin != 0) {
            lp.width = w
            lp.height = h
            lp.leftMargin = 0
            lp.topMargin = 0
            surfaceView.layoutParams = lp
        }
        val sx = (rect.width / w).coerceAtLeast(0.001f)
        val sy = (rect.height / h).coerceAtLeast(0.001f)
        if (surfaceView.scaleX != sx) surfaceView.scaleX = sx
        if (surfaceView.scaleY != sy) surfaceView.scaleY = sy
        if (surfaceView.translationX != rect.left) surfaceView.translationX = rect.left
        if (surfaceView.translationY != rect.top) surfaceView.translationY = rect.top
    }
}

/**
 * 盖在整个会话页上的触摸层：拿到完整的多点 MotionEvent，按 [TouchRouter] 把每根手指分流：
 * 普通手指转发给 touch，落在虚拟鼠标上的手指只产生鼠标动作。
 */
@SuppressLint("ViewConstructor", "ClickableViewAccessibility")
class TouchLayerView(
    context: Context,
    tapSlopPx: Float,
    var onTouches: (List<TouchOp>, List<TouchOp>, Long) -> Unit,
    /** 原始 MotionEvent（手势记录用）：时间、actionMasked、actionIndex、所有手指。 */
    var onRaw: (Long, Int, Int, IntArray, FloatArray, FloatArray) -> Unit,
    var onPressed: (Boolean) -> Unit,
    var onMouse: (MouseEvent) -> Unit,
) : View(context) {
    val router = TouchRouter(tapSlopPx)

    override fun onTouchEvent(e: MotionEvent): Boolean {
        val n = e.pointerCount
        val ids = IntArray(n) { e.getPointerId(it) }
        val xs = FloatArray(n) { e.getX(it) }
        val ys = FloatArray(n) { e.getY(it) }
        onRaw(e.eventTime, e.actionMasked, e.actionIndex, ids, xs, ys)
        val r = router.route(e.actionMasked, e.actionIndex, ids, xs, ys)
        r.mouse.forEach(onMouse)
        if (r.touches.isNotEmpty() || r.moves.isNotEmpty()) onTouches(r.touches, r.moves, e.eventTime)
        when (e.actionMasked) {
            MotionEvent.ACTION_DOWN -> onPressed(true)
            MotionEvent.ACTION_UP, MotionEvent.ACTION_CANCEL -> onPressed(false)
        }
        return true
    }
}

/**
 * 隐藏的输入接收器：软键盘的文字、退格、回车都从它的 InputConnection 进来。
 * 用 VISIBLE_PASSWORD 类型：多数键盘因此不做联想、不攒组合文字，字符直接提交。
 */
class KeyCatcherView(
    context: Context,
    var onText: (String) -> Unit,
    var onKey: (Int) -> Unit,
) : View(context) {
    init {
        isFocusable = true
        isFocusableInTouchMode = true
    }

    override fun onCheckIsTextEditor() = true

    override fun onCreateInputConnection(outAttrs: EditorInfo): InputConnection {
        outAttrs.inputType = InputType.TYPE_CLASS_TEXT or
            InputType.TYPE_TEXT_FLAG_NO_SUGGESTIONS or
            InputType.TYPE_TEXT_VARIATION_VISIBLE_PASSWORD
        outAttrs.imeOptions = EditorInfo.IME_FLAG_NO_EXTRACT_UI or
            EditorInfo.IME_FLAG_NO_FULLSCREEN or EditorInfo.IME_ACTION_NONE
        return object : BaseInputConnection(this, false) {
            override fun commitText(text: CharSequence?, newCursorPosition: Int): Boolean {
                val s = text?.toString().orEmpty()
                // 换行当回车键发，其余当文字
                val parts = s.split('\n')
                parts.forEachIndexed { i, part ->
                    if (part.isNotEmpty()) onText(part)
                    if (i < parts.lastIndex) onKey(KeyEvent.KEYCODE_ENTER)
                }
                return true
            }

            // 组合中的文字（中文拼音等）等输入法自己提交，我们不显示它
            override fun setComposingText(text: CharSequence?, newCursorPosition: Int) = true
            override fun finishComposingText() = true

            override fun deleteSurroundingText(beforeLength: Int, afterLength: Int): Boolean {
                repeat(beforeLength.coerceIn(0, MAX_DELETE)) { onKey(KeyEvent.KEYCODE_DEL) }
                repeat(afterLength.coerceIn(0, MAX_DELETE)) { onKey(KeyEvent.KEYCODE_FORWARD_DEL) }
                return true
            }

            override fun sendKeyEvent(event: KeyEvent): Boolean {
                // 按下时发整个“按下+抬起”，抬起忽略
                if (event.action == KeyEvent.ACTION_DOWN) onKey(event.keyCode)
                return true
            }

            override fun performEditorAction(editorAction: Int): Boolean {
                onKey(KeyEvent.KEYCODE_ENTER)
                return true
            }
        }
    }

    private companion object {
        const val MAX_DELETE = 64
    }
}
