package ai.peerdesk.app

import kotlin.math.hypot

/** 容器像素坐标里的矩形。 */
data class PxBox(val left: Float, val top: Float, val width: Float, val height: Float) {
    fun contains(x: Float, y: Float) = x >= left && x < left + width && y >= top && y < top + height
}

/** 大鼠标上能点到的地方。 */
enum class Zone { Left, Right, Body, Close, Mini }

/** 尺寸（像素）。界面按 dp 换算后传进来。 */
data class MouseSizes(
    val width: Float,
    /** 鼠标本体高度（不含上面放箭头的一条）。 */
    val height: Float,
    /** 鼠标上方放箭头光标和关闭按钮的一条的高度。 */
    val arrowH: Float,
    val closeSize: Float,
    val miniSize: Float,
    /** 左右键占本体高度的比例（上半部分）。 */
    val buttonFrac: Float = 0.45f,
)

/** 默认位置（比例）：大鼠标在屏幕中间偏下；小鼠标在右下角。 */
const val MOUSE_DEFAULT_FX = 0.5f
const val MOUSE_DEFAULT_FY = 0.7f
const val MINI_DEFAULT_FX = 1f
const val MINI_DEFAULT_FY = 0.85f

/**
 * 展开的大鼠标：最上面一条放箭头光标（箭头尖在这一条的最上沿、在左键的正上方）和右上角的关闭按钮；
 * 下面是鼠标本体，上半是左键、右键，下半是身体（拖把）。
 */
data class VMouse(
    val bounds: PxBox,
    val left: PxBox,
    val right: PxBox,
    val body: PxBox,
    val close: PxBox,
    /** 箭头尖的屏幕位置 = 遥控端光标的位置。尖在左键正上方（左键水平中心），纵向在最上沿。 */
    val tipX: Float,
    val tipY: Float,
) {
    fun hit(x: Float, y: Float): Zone? = when {
        close.contains(x, y) -> Zone.Close
        left.contains(x, y) -> Zone.Left
        right.contains(x, y) -> Zone.Right
        body.contains(x, y) -> Zone.Body
        else -> null
    }
}

/**
 * 位置用比例（0..1）：0 贴左/上，1 贴右/下，按“容器减去鼠标”的可移动范围算，
 * 所以整只鼠标永远在容器里，屏幕尺寸变了也不会跑出去。
 */
fun vmouseLayout(containerW: Float, containerH: Float, fx: Float, fy: Float, s: MouseSizes): VMouse {
    val gh = s.arrowH + s.height
    val l = fx.coerceIn(0f, 1f) * (containerW - s.width).coerceAtLeast(0f)
    val t = fy.coerceIn(0f, 1f) * (containerH - gh).coerceAtLeast(0f)
    val btnH = s.height * s.buttonFrac
    val bodyTop = t + s.arrowH + btnH
    return VMouse(
        bounds = PxBox(l, t, s.width, gh),
        left = PxBox(l, t + s.arrowH, s.width / 2, btnH),
        right = PxBox(l + s.width / 2, t + s.arrowH, s.width / 2, btnH),
        body = PxBox(l, bodyTop, s.width, t + gh - bodyTop),
        close = PxBox(l + s.width - s.closeSize, t, s.closeSize, minOf(s.closeSize, s.arrowH)),
        tipX = l + s.width / 4,
        tipY = t,
    )
}

/** 收起后的小鼠标图标。 */
fun miniLayout(containerW: Float, containerH: Float, fx: Float, fy: Float, s: MouseSizes): PxBox {
    val l = fx.coerceIn(0f, 1f) * (containerW - s.miniSize).coerceAtLeast(0f)
    val t = fy.coerceIn(0f, 1f) * (containerH - s.miniSize).coerceAtLeast(0f)
    return PxBox(l, t, s.miniSize, s.miniSize)
}

private fun fractionOf(
    left: Float, top: Float, containerW: Float, containerH: Float, w: Float, h: Float,
): Pair<Float, Float> {
    val rx = containerW - w
    val ry = containerH - h
    return (if (rx > 0f) left / rx else 0f).coerceIn(0f, 1f) to (if (ry > 0f) top / ry else 0f).coerceIn(0f, 1f)
}

/** 点小鼠标展开：大鼠标放在小鼠标所在的位置（左上角对齐），不超出容器。返回大鼠标的比例位置。 */
fun expandedFrom(mini: PxBox, containerW: Float, containerH: Float, s: MouseSizes): Pair<Float, Float> =
    fractionOf(mini.left, mini.top, containerW, containerH, s.width, s.arrowH + s.height)

/** 点关闭收起：小鼠标放在大鼠标所在的位置（左上角对齐），不超出容器。返回小鼠标的比例位置。 */
fun collapsedFrom(big: PxBox, containerW: Float, containerH: Float, s: MouseSizes): Pair<Float, Float> =
    fractionOf(big.left, big.top, containerW, containerH, s.miniSize, s.miniSize)

/** 拖动 (dx, dy) 像素后的新比例位置，夹在容器内。 */
fun dragMouseGroup(
    fx: Float, fy: Float, dx: Float, dy: Float,
    containerW: Float, containerH: Float, groupW: Float, groupH: Float,
): Pair<Float, Float> {
    val rangeX = containerW - groupW
    val rangeY = containerH - groupH
    val nx = if (rangeX > 0f) fx + dx / rangeX else fx
    val ny = if (rangeY > 0f) fy + dy / rangeY else fy
    return nx.coerceIn(0f, 1f) to ny.coerceIn(0f, 1f)
}

/** 触摸层认出的大鼠标动作，由 ViewModel 执行。 */
sealed interface MouseEvent {
    /** 左键(0)/右键(1)按下：先 pointAt 尖的位置，再按键。 */
    data class ButtonDown(val index: Int) : MouseEvent
    data class ButtonUp(val index: Int) : MouseEvent
    /** 整只鼠标跟着手指走（左键按住拖动，或拖身体）：移动后 pointAt。 */
    data class Move(val dx: Float, val dy: Float) : MouseEvent
    /** 小图标被拖动（不发任何输入）。 */
    data class MiniMove(val dx: Float, val dy: Float) : MouseEvent
    /** 拖动结束，该保存位置。 */
    data object DragEnd : MouseEvent
    /** 点了关闭：缩成小图标。 */
    data object Collapse : MouseEvent
    /** 点了小图标：恢复成完整鼠标。 */
    data object Expand : MouseEvent
}

private sealed interface Role {
    data object Forward : Role
    /** 左右键。左键按住可以拖整只鼠标，右键不拖。 */
    class Key(val index: Int, var lastX: Float, var lastY: Float, var moved: Boolean = false) : Role
    class Body(var lastX: Float, var lastY: Float) : Role
    data object Close : Role
    class Mini(val downX: Float, val downY: Float, var lastX: Float, var lastY: Float, var moved: Boolean = false) : Role
}

data class RouteResult(
    /** 按下、抬起、取消：逐根转发给 `Native.touch`。 */
    val touches: List<TouchOp>,
    /** 同一个 ACTION_MOVE 里所有转发的手指的新位置：一起交给 `Native.moves`（逐根交会让捏合抖）。 */
    val moves: List<TouchOp>,
    val mouse: List<MouseEvent>,
)

/**
 * 触摸层的分发：同一个 MotionEvent 里每根手指按“按下时落在哪”决定去向，之后一直保持。
 *
 * - 落在大鼠标（左键、右键、身体、关闭）或小图标上的手指：只产生 [MouseEvent]，不转发给 touch。
 * - 其他手指（包括鼠标上有手指按着时另外的手指）：照常转发给 touch。
 *
 * 为什么放在触摸层里按坐标判断，而不是让鼠标自己是一个 View：Compose 里两个 AndroidView 叠在一起时，
 * 第一根手指落在鼠标上之后，第二根手指会不会再分发给下面的触摸层取决于 Compose 的指针分发细节，没法在没有真机时确认；
 * 一个 View 自己拿到完整的多点 MotionEvent、按坐标分流，行为是确定的。鼠标只是画在上面的、不接收触摸的 Compose 元素。
 */
class TouchRouter(private val tapSlopPx: Float = 8f) {
    private val roles = HashMap<Int, Role>()

    /** 给出屏幕坐标落在哪个区域；鼠标模式关着时为 null，所有手指都转发。 */
    var hitTest: ((Float, Float) -> Zone?)? = null

    fun route(
        action: Int, actionIndex: Int, ids: IntArray, xs: FloatArray, ys: FloatArray,
    ): RouteResult {
        val ops = touchOps(action, actionIndex, ids, xs, ys)
        val touches = ArrayList<TouchOp>()
        val moves = ArrayList<TouchOp>()
        val mouse = ArrayList<MouseEvent>()
        for (op in ops) {
            if (op.phase == TouchPhase.DOWN) {
                val role: Role = when (hitTest?.invoke(op.x, op.y)) {
                    Zone.Left -> Role.Key(0, op.x, op.y).also { mouse.add(MouseEvent.ButtonDown(0)) }
                    Zone.Right -> Role.Key(1, op.x, op.y).also { mouse.add(MouseEvent.ButtonDown(1)) }
                    Zone.Body -> Role.Body(op.x, op.y)
                    Zone.Close -> Role.Close
                    Zone.Mini -> Role.Mini(op.x, op.y, op.x, op.y)
                    null -> Role.Forward.also { touches.add(op) }
                }
                roles[op.id] = role
                continue
            }
            val role = roles[op.id] ?: Role.Forward
            val move = op.phase == TouchPhase.MOVE
            val up = op.phase == TouchPhase.UP
            val ending = up || op.phase == TouchPhase.CANCEL
            when (role) {
                Role.Forward -> if (move) moves.add(op) else touches.add(op)
                is Role.Key -> {
                    // 只有左键按住拖动才带着整只鼠标走
                    if (move && role.index == 0 && (op.x != role.lastX || op.y != role.lastY)) {
                        mouse.add(MouseEvent.Move(op.x - role.lastX, op.y - role.lastY))
                        role.lastX = op.x
                        role.lastY = op.y
                        role.moved = true
                    }
                    if (ending) {
                        mouse.add(MouseEvent.ButtonUp(role.index))
                        if (role.moved) mouse.add(MouseEvent.DragEnd)
                    }
                }
                is Role.Body -> {
                    if (move && (op.x != role.lastX || op.y != role.lastY)) {
                        mouse.add(MouseEvent.Move(op.x - role.lastX, op.y - role.lastY))
                        role.lastX = op.x
                        role.lastY = op.y
                    }
                    if (ending) mouse.add(MouseEvent.DragEnd)
                }
                Role.Close -> if (up && hitTest?.invoke(op.x, op.y) == Zone.Close) mouse.add(MouseEvent.Collapse)
                is Role.Mini -> {
                    if (move) {
                        if (!role.moved && hypot(op.x - role.downX, op.y - role.downY) > tapSlopPx) role.moved = true
                        if (role.moved) {
                            // 第一次超过阈值时把之前走过的那段也补上，小图标不会落后手指
                            mouse.add(MouseEvent.MiniMove(op.x - role.lastX, op.y - role.lastY))
                            role.lastX = op.x
                            role.lastY = op.y
                        }
                    }
                    if (up && !role.moved) mouse.add(MouseEvent.Expand)
                    else if (ending && role.moved) mouse.add(MouseEvent.DragEnd)
                }
            }
            if (ending) roles.remove(op.id)
        }
        return RouteResult(touches, moves, mouse)
    }
}
