#!/bin/bash
# 把 Compose 界面的每个页面、每个状态渲染成 PNG（Paparazzi，JVM 上跑，不要模拟器和真机）。
# 输出：android/screenshots/*.png 和 android/screenshots/INDEX.txt（文件名 → 哪个状态）。
# 只在加 -Pscreenshots 时才启用截图测试，平时的 ./gradlew testDebugUnitTest 不受影响。
# 用法：android/screenshots.sh
set -euo pipefail
cd "$(dirname "$0")"
fail() { echo "错误：$*" >&2; exit 1; }

[ -f local.properties ] || fail "没有 local.properties。先跑一次 scripts/android-build.sh（它会写好 sdk.dir）。"

rm -rf app/src/test/snapshots/images screenshots
./gradlew --no-daemon recordPaparazziDebug -Pscreenshots || fail "截图测试失败（见上面的输出）。"

src=app/src/test/snapshots/images
[ -d "$src" ] || fail "没有生成图片目录 $src。"
mkdir -p screenshots
for f in "$src"/ai.peerdesk.app_ScreenshotsTest_render_*.png; do
  cp "$f" "screenshots/$(basename "$f" | sed 's/^ai.peerdesk.app_ScreenshotsTest_render_//')"
done

python3 - <<'PY'
import glob, os
states = {
  "connect-empty": "连接页：第一次打开（全空）",
  "connect-filled": "连接页：填好服务器、设备码、密码",
  "connect-invalid-code": "连接页：设备码不足 9 位（提示错误）",
  "connect-ended-connect-failed": "连接页：上次连接失败，页面上显示原因",
  "connect-ended-disconnected-by-peer": "连接页：上次会话被对方断开，显示原因",
  "connect-ended-background": "连接页：上次因为应用切到后台而断开",
  "connect-ended-no-surface": "连接页：画面初始化失败（Surface 不可用）",
  "connect-with-export-button": "连接页：有手势记录，显示“导出诊断记录”按钮",
  "session-toolbar-normal": "会话页：工具条显示，网络正常（延迟、帧率、码率、丢包）",
  "session-toolbar-network-router": "会话页：网络差，原因在本机到路由器",
  "session-toolbar-network-internet": "会话页：网络差，原因在公网（中继）",
  "session-toolbar-network-unmeasured": "会话页：网络差，没测到路由器",
  "session-toolbar-hidden": "会话页：工具条收起",
  "session-reconnecting": "会话页：重连中",
  "session-connecting-no-picture": "会话页：正在连接，还没有画面",
  "session-keyboard-shortcut-bar": "会话页：软键盘加快捷键栏（键盘是仿制块，画面已顶起）",
  "session-mouse-small": "会话页：小鼠标",
  "session-mouse-big-left-pressed": "会话页：大鼠标，左键按下",
  "session-tap-ripples": "会话页：点击水波（左键一圈、右键双圈）",
  "session-back-hint": "会话页：“再按一次返回键断开”提示（Toast 是仿制的）",
}
devices = {"p1080": "1080x2400 @3x 竖屏", "p1080-land": "1080x2400 @3x 横屏",
           "p720": "720x1600 @2x 竖屏", "p720-land": "720x1600 @2x 横屏"}
langs = {"en": "English", "zh": "简体中文"}
themes = {"light": "浅色", "dark": "深色"}
rows = []
for f in sorted(glob.glob("screenshots/*.png")):
    name = os.path.basename(f)[:-4]
    state, dev, lang, theme = name.split("__")
    rows.append(f"{os.path.basename(f)}\t{states.get(state, state)} | {devices.get(dev, dev)} | {langs.get(lang, lang)} | {themes.get(theme, theme)}")
open("screenshots/INDEX.txt", "w", encoding="utf-8").write("\n".join(rows) + "\n")
print(f"{len(rows)} 张图，索引：android/screenshots/INDEX.txt")
PY
