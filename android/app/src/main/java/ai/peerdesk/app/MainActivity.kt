package ai.peerdesk.app

import android.os.Bundle
import android.view.KeyEvent
import android.view.WindowManager
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.activity.enableEdgeToEdge
import androidx.activity.viewModels
import androidx.compose.foundation.isSystemInDarkTheme
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.material3.darkColorScheme
import androidx.compose.material3.dynamicDarkColorScheme
import androidx.compose.material3.dynamicLightColorScheme
import androidx.compose.material3.lightColorScheme
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.getValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.foundation.layout.fillMaxSize
import androidx.lifecycle.compose.collectAsStateWithLifecycle

class MainActivity : ComponentActivity() {
    private val vm: PeerDeskViewModel by viewModels()

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        enableEdgeToEdge()
        setContent {
            val dark = isSystemInDarkTheme()
            val ctx = LocalContext.current
            val scheme = when {
                android.os.Build.VERSION.SDK_INT >= 31 ->
                    if (dark) dynamicDarkColorScheme(ctx) else dynamicLightColorScheme(ctx)
                dark -> darkColorScheme()
                else -> lightColorScheme()
            }
            MaterialTheme(colorScheme = scheme) {
                Surface(Modifier.fillMaxSize()) {
                    val ui by vm.ui.collectAsStateWithLifecycle()
                    // 会话期间保持亮屏
                    DisposableEffect(ui.phase) {
                        if (ui.phase == Phase.Session) {
                            window.addFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON)
                        }
                        onDispose { window.clearFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON) }
                    }
                    when (ui.phase) {
                        Phase.Connect -> ConnectScreen(ui, vm)
                        Phase.Session -> SessionScreen(ui, vm)
                    }
                }
            }
        }
    }

    /** 第一版：离开应用就断开，回来重新连（连接页会写明）。 */
    override fun onStop() {
        vm.onBackground()
        super.onStop()
    }

    /** 外接键盘：直接转给核心；系统键（返回、音量等）和核心不认识的键走系统默认。 */
    override fun dispatchKeyEvent(event: KeyEvent): Boolean {
        if (vm.sessionActive && event.keyCode !in SYSTEM_KEYS &&
            (event.action == KeyEvent.ACTION_DOWN || event.action == KeyEvent.ACTION_UP)
        ) {
            if (vm.hardwareKey(event.keyCode, event.action == KeyEvent.ACTION_DOWN)) return true
        }
        return super.dispatchKeyEvent(event)
    }

    private companion object {
        val SYSTEM_KEYS = setOf(
            KeyEvent.KEYCODE_BACK, KeyEvent.KEYCODE_VOLUME_UP, KeyEvent.KEYCODE_VOLUME_DOWN,
            KeyEvent.KEYCODE_VOLUME_MUTE, KeyEvent.KEYCODE_POWER, KeyEvent.KEYCODE_HOME,
            KeyEvent.KEYCODE_APP_SWITCH, KeyEvent.KEYCODE_SLEEP, KeyEvent.KEYCODE_WAKEUP,
        )
    }
}
