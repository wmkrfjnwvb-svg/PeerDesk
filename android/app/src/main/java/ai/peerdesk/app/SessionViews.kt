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

/**
 * 远程画面的承载：一个裁剪的 FrameLayout，里面放 SurfaceView，靠改它的尺寸和边距来缩放、平移。
 * 不用 scaleX/translation：SurfaceView 的画面不随 View 变换缩放（MediaCodec 直接解到它的 Surface 上，由系统合成）。
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
        addView(surfaceView, LayoutParams(1, 1))
    }

    /** 把 SurfaceView 放到界面坐标的这个矩形（可以超出容器，超出的部分被裁掉）。 */
    fun place(rect: ViewRect) {
        val lp = surfaceView.layoutParams as LayoutParams
        val w = rect.width.toInt().coerceAtLeast(1)
        val h = rect.height.toInt().coerceAtLeast(1)
        val l = rect.left.toInt()
        val t = rect.top.toInt()
        if (lp.width != w || lp.height != h || lp.leftMargin != l || lp.topMargin != t) {
            lp.width = w
            lp.height = h
            lp.leftMargin = l
            lp.topMargin = t
            surfaceView.layoutParams = lp
        }
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
    var onTouches: (List<TouchOp>, Long) -> Unit,
    var onPressed: (Boolean) -> Unit,
    var onMouse: (MouseEvent) -> Unit,
) : View(context) {
    val router = TouchRouter(tapSlopPx)

    override fun onTouchEvent(e: MotionEvent): Boolean {
        val n = e.pointerCount
        val ids = IntArray(n) { e.getPointerId(it) }
        val xs = FloatArray(n) { e.getX(it) }
        val ys = FloatArray(n) { e.getY(it) }
        val r = router.route(e.actionMasked, e.actionIndex, ids, xs, ys)
        r.mouse.forEach(onMouse)
        if (r.touches.isNotEmpty()) onTouches(r.touches, e.eventTime)
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
