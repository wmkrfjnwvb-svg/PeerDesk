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
    external fun tick(h: Long, tMs: Long): String?
    external fun setView(
        h: Long, left: Float, top: Float, width: Float, height: Float,
        zoomed: Boolean, viewW: Int, viewH: Int,
    )
    external fun setMode(h: Long, mode: Int)
    external fun key(h: Long, keycode: Int, down: Boolean): Boolean
    external fun text(h: Long, text: String)
    external fun releaseAll(h: Long)
    external fun stop(h: Long)
    external fun free(h: Long)
}
