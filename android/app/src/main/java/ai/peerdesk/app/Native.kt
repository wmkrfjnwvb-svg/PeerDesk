package ai.peerdesk.app

import android.view.Surface

/** Rust 核心（libpeerdesk_mobile.so）的 JNI 入口。类名和函数签名必须和 crates/mobile/src/android/bridge.rs 一致。 */
object Native {
    init {
        System.loadLibrary("peerdesk_mobile")
    }

    external fun start(
        dataDir: String, server: String, code: String, password: String,
        surface: Surface, slopPx: Float, notchPx: Float,
    ): Long

    external fun nextEvent(h: Long, timeoutMs: Int): String?
    external fun touch(h: Long, id: Int, phase: Int, x: Float, y: Float, tMs: Long): String?
    /** 一个 ACTION_MOVE 里所有手指的新位置，一起交（逐根交会让捏合抖）。返回同 touch。 */
    external fun moves(h: Long, ids: IntArray, xs: FloatArray, ys: FloatArray, tMs: Long): String?
    external fun tick(h: Long, tMs: Long): String?
    external fun setView(
        h: Long, left: Float, top: Float, width: Float, height: Float,
        viewW: Int, viewH: Int,
    )
    /** 屏幕上大鼠标的左右键：0 左键、1 右键。 */
    external fun button(h: Long, button: Int, down: Boolean)
    /** 把远程光标移到屏幕坐标 (x, y)（和 touch 同一个坐标系，触摸层像素）；Rust 按画面矩形换算。 */
    external fun pointAt(h: Long, x: Float, y: Float)
    external fun key(h: Long, keycode: Int, down: Boolean): Boolean
    external fun text(h: Long, text: String)
    external fun releaseAll(h: Long)
    external fun stop(h: Long)
    external fun free(h: Long)
}
