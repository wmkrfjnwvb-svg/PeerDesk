//! 给 Kotlin 调的 JNI 函数（类 `ai.peerdesk.app.Native`）。只做类型转换和接线，逻辑都在平台无关的部分。
//!
//! 句柄：`start` 返回一个 `long`（`Box<Handle>` 的地址），之后每个调用都带着它，`free` 之后不能再用。
//! Kotlin 那边保证 `free` 只调一次、之后不再用这个句柄（`Native.kt` 里包成一个类，`close()` 里调）。
#![allow(unsafe_code)]

use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use jni::objects::{JClass, JFloatArray, JIntArray, JObject, JString};
use jni::sys::{jboolean, jfloat, jint, jlong, jstring, JNI_TRUE};
use jni::JNIEnv;
use ndk::native_window::NativeWindow;
use peerdesk_core::frame::Picture;
use peerdesk_core::input::InputEvent;
use peerdesk_core::video::VideoDecoder;
use peerdesk_net::NetError;

use super::codec::SurfaceDecoder;
use crate::gesture::{Gestures, Phase, Touch, View, RIGHT};
use crate::{
    addr_from_handle, apply, device_target, handle_from_addr, keymap, DecoderFactory, ScreenSink,
    Session, SinkFactory,
};

/// 一次访问在 Kotlin 那边的句柄背后的东西。
struct Handle {
    session: Session,
    gestures: Mutex<Gestures>,
}

/// 画面由解码器直接画到 Surface 上，这里什么也不用做。
struct NoSink;

impl ScreenSink for NoSink {
    fn present(&mut self, _picture: &Picture) {}
}

fn string(env: &mut JNIEnv<'_>, s: &JString<'_>) -> String {
    env.get_string(s).map(Into::into).unwrap_or_default()
}

fn new_string(env: &mut JNIEnv<'_>, s: Option<String>) -> jstring {
    s.and_then(|s| env.new_string(s).ok())
        .map_or(std::ptr::null_mut(), JString::into_raw)
}

/// 句柄换回引用。0 或不对齐的地址是 `None`。
fn handle<'a>(h: jlong) -> Option<&'a Handle> {
    let addr = addr_from_handle(h)?;
    // SAFETY: 非零的句柄只来自 `start` 里的 `Box::into_raw`，Kotlin 保证 `free` 之后不再传它（见文件开头）；
    // `Handle` 里的东西都能跨线程用（会话的事件队列在锁里、手势在锁里）。
    unsafe { (addr as *const Handle).as_ref() }
}

/// 开始一次访问：按服务器地址和设备码连接，画面画到 `surface`。`slop` 是防抖距离、`notch` 是滚一格的距离（像素）。
/// 返回句柄；Surface 不可用时返回 0（界面提示“画面初始化失败”）。服务器、设备码填错时照样返回句柄，
/// 第一个事件就是带原因的结束。
#[no_mangle]
pub extern "system" fn Java_ai_peerdesk_app_Native_start<'local>(
    mut env: JNIEnv<'local>,
    _class: JClass<'local>,
    data_dir: JString<'local>,
    server: JString<'local>,
    code: JString<'local>,
    password: JString<'local>,
    surface: JObject<'local>,
    slop: jfloat,
    notch: jfloat,
) -> jlong {
    let (data_dir, server, code, password) = (
        string(&mut env, &data_dir),
        string(&mut env, &server),
        string(&mut env, &code),
        string(&mut env, &password),
    );
    // SAFETY: `env` 是这次 JNI 调用的有效环境，`surface` 是 Kotlin 传来的 android.view.Surface（不是 null 时）；
    // `from_surface` 增加了引用计数，之后和 Java 对象的生命周期无关。
    let Some(window) = (unsafe { NativeWindow::from_surface(env.get_raw(), surface.as_raw()) })
    else {
        return 0;
    };
    let decoder = DecoderFactory(Arc::new(
        move || -> Result<Box<dyn VideoDecoder>, NetError> {
            Ok(Box::new(SurfaceDecoder::new(window.clone())))
        },
    ));
    let sink: SinkFactory = Arc::new(|| -> Box<dyn ScreenSink> { Box::new(NoSink) });
    // 查服务器的 DNS 在会话线程里做（start_with）：这个函数在界面线程上调，要马上返回
    let session = Session::start_with(
        PathBuf::from(data_dir),
        move || device_target(&server, &code),
        &password,
        sink,
        Some(decoder),
    );
    let view = View {
        left: 0.0,
        top: 0.0,
        width: 1.0,
        height: 1.0,
    };
    let handle = Box::new(Handle {
        session,
        gestures: Mutex::new(Gestures::new(view, slop, notch)),
    });
    handle_from_addr(Box::into_raw(handle) as usize)
}

/// 取下一个事件（一行 JSON），最多等 `timeout_ms`；没有就是 null。
#[no_mangle]
pub extern "system" fn Java_ai_peerdesk_app_Native_nextEvent<'local>(
    mut env: JNIEnv<'local>,
    _class: JClass<'local>,
    h: jlong,
    timeout_ms: jint,
) -> jstring {
    let timeout = Duration::from_millis(u64::try_from(timeout_ms).unwrap_or(0));
    let json = handle(h)
        .and_then(|h| h.session.next_event(timeout))
        .map(|e| e.to_json());
    new_string(&mut env, json)
}

/// 新手势第一根手指（`ACTION_DOWN`）的阶段码：先清掉上一个手势留下的状态，再当按下处理。
const FIRST_DOWN: jint = 4;

/// 一个触点。`phase`：0 按下、1 移动、2 抬起、3 取消、4 新手势的第一根手指按下。返回要界面做的缩放、平移（JSON 数组）或 null。
#[no_mangle]
pub extern "system" fn Java_ai_peerdesk_app_Native_touch<'local>(
    mut env: JNIEnv<'local>,
    _class: JClass<'local>,
    h: jlong,
    id: jint,
    phase: jint,
    x: jfloat,
    y: jfloat,
    t_ms: jlong,
) -> jstring {
    let out = handle(h).and_then(|h| {
        let first = phase == FIRST_DOWN;
        let phase = if first {
            Phase::Down
        } else {
            Phase::from_code(phase)?
        };
        let touch = Touch {
            id: u32::try_from(id).unwrap_or(0),
            phase,
            x,
            y,
            t_ms: u64::try_from(t_ms).unwrap_or(0),
        };
        let mut gestures = h.gestures.lock().unwrap_or_else(PoisonError::into_inner);
        let mut actions = if first { gestures.reset() } else { Vec::new() };
        actions.extend(gestures.touch(&touch));
        drop(gestures);
        apply(&h.session, actions)
    });
    new_string(&mut env, out)
}

/// 一个触摸事件里所有手指的新位置（Android 的 `ACTION_MOVE`）：`ids`、`xs`、`ys` 一一对应，一起交给手势识别，
/// 两根手指不会一根一根地更新（那样缩放会抖，见 `Gestures::moves`）。返回值同 `touch`。
#[no_mangle]
pub extern "system" fn Java_ai_peerdesk_app_Native_moves<'local>(
    mut env: JNIEnv<'local>,
    _class: JClass<'local>,
    h: jlong,
    ids: JIntArray<'local>,
    xs: JFloatArray<'local>,
    ys: JFloatArray<'local>,
    t_ms: jlong,
) -> jstring {
    let touches = read_moves(&mut env, &ids, &xs, &ys, u64::try_from(t_ms).unwrap_or(0));
    let out = handle(h).and_then(|h| {
        let actions = h
            .gestures
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .moves(&touches);
        apply(&h.session, actions)
    });
    new_string(&mut env, out)
}

/// 最多看几根手指（系统一般最多报 10 根）。
const MAX_POINTERS: usize = 10;

/// 把三个数组读成移动的触点；长度不一样时按最短的，读不出来就是空。
fn read_moves(
    env: &mut JNIEnv<'_>,
    ids: &JIntArray<'_>,
    xs: &JFloatArray<'_>,
    ys: &JFloatArray<'_>,
    t_ms: u64,
) -> Vec<Touch> {
    let len = |n: jni::errors::Result<i32>| usize::try_from(n.unwrap_or(0)).unwrap_or(0);
    let n = len(env.get_array_length(ids))
        .min(len(env.get_array_length(xs)))
        .min(len(env.get_array_length(ys)))
        .min(MAX_POINTERS);
    let mut id = [0; MAX_POINTERS];
    let mut x = [0.0; MAX_POINTERS];
    let mut y = [0.0; MAX_POINTERS];
    let (Some(id), Some(x), Some(y)) = (id.get_mut(..n), x.get_mut(..n), y.get_mut(..n)) else {
        return Vec::new();
    };
    if env.get_int_array_region(ids, 0, id).is_err()
        || env.get_float_array_region(xs, 0, x).is_err()
        || env.get_float_array_region(ys, 0, y).is_err()
    {
        return Vec::new();
    }
    id.iter()
        .zip(x.iter())
        .zip(y.iter())
        .map(|((id, x), y)| Touch {
            id: u32::try_from(*id).unwrap_or(0),
            phase: Phase::Move,
            x: *x,
            y: *y,
            t_ms,
        })
        .collect()
}

/// 有手指按着时每隔几十毫秒调一次（长按）。返回值同 `touch`。
#[no_mangle]
pub extern "system" fn Java_ai_peerdesk_app_Native_tick<'local>(
    mut env: JNIEnv<'local>,
    _class: JClass<'local>,
    h: jlong,
    t_ms: jlong,
) -> jstring {
    let out = handle(h).and_then(|h| {
        let now = u64::try_from(t_ms).unwrap_or(0);
        let actions = h
            .gestures
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .tick(now);
        apply(&h.session, actions)
    });
    new_string(&mut env, out)
}

/// 画面在界面上的位置变了（缩放、平移、横竖屏）；`view_w`、`view_h` 是画面画出来的像素，被控端按它编码。
#[no_mangle]
#[allow(clippy::too_many_arguments)]
pub extern "system" fn Java_ai_peerdesk_app_Native_setView<'local>(
    _env: JNIEnv<'local>,
    _class: JClass<'local>,
    h: jlong,
    left: jfloat,
    top: jfloat,
    width: jfloat,
    height: jfloat,
    view_w: jint,
    view_h: jint,
) {
    if let Some(h) = handle(h) {
        h.gestures
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .set_view(View {
                left,
                top,
                width,
                height,
            });
        h.session.set_view(
            u16::try_from(view_w).unwrap_or(0),
            u16::try_from(view_h).unwrap_or(0),
        );
    }
}

/// 大鼠标的箭头尖到了界面上的 (`x`, `y`)（像素，和触点同一个坐标系）：把被控端的指针移过去。
#[no_mangle]
pub extern "system" fn Java_ai_peerdesk_app_Native_pointAt<'local>(
    _env: JNIEnv<'local>,
    _class: JClass<'local>,
    h: jlong,
    x: jfloat,
    y: jfloat,
) {
    if let Some(h) = handle(h) {
        let actions = h
            .gestures
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .point(x, y);
        let _ = apply(&h.session, actions);
    }
}

/// 大鼠标的左键、右键：`button` 0 左键、1 右键，`down` 按下或抬起。按在指针现在的位置（界面先调 `pointAt`）。
#[no_mangle]
pub extern "system" fn Java_ai_peerdesk_app_Native_button<'local>(
    _env: JNIEnv<'local>,
    _class: JClass<'local>,
    h: jlong,
    button: jint,
    down: jboolean,
) {
    if let Some(h) = handle(h) {
        let Some(button) = u8::try_from(button).ok().filter(|b| *b <= RIGHT) else {
            return;
        };
        let actions = h
            .gestures
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .button(button, down == JNI_TRUE);
        let _ = apply(&h.session, actions);
    }
}

/// 一个按键（Android 的按键码）。不认识的键不发，返回 false。
#[no_mangle]
pub extern "system" fn Java_ai_peerdesk_app_Native_key<'local>(
    _env: JNIEnv<'local>,
    _class: JClass<'local>,
    h: jlong,
    keycode: jint,
    down: jboolean,
) -> jboolean {
    let sent = handle(h)
        .zip(keymap::hid(keycode))
        .is_some_and(|(h, code)| {
            h.session.send(InputEvent::Key {
                code,
                down: down == JNI_TRUE,
            })
        });
    jboolean::from(sent)
}

/// 软键盘打的字。
#[no_mangle]
pub extern "system" fn Java_ai_peerdesk_app_Native_text<'local>(
    mut env: JNIEnv<'local>,
    _class: JClass<'local>,
    h: jlong,
    text: JString<'local>,
) {
    let text = string(&mut env, &text);
    if let Some(h) = handle(h).filter(|_| !text.is_empty()) {
        let _ = h.session.send(InputEvent::Text(text));
    }
}

/// 松开所有按着的键和鼠标键（切到后台、断开前）。
#[no_mangle]
pub extern "system" fn Java_ai_peerdesk_app_Native_releaseAll<'local>(
    _env: JNIEnv<'local>,
    _class: JClass<'local>,
    h: jlong,
) {
    if let Some(h) = handle(h) {
        let _ = h.session.send(InputEvent::ReleaseAll);
    }
}

/// 断开。之后还能取事件（会收到结束），直到 `free`。
#[no_mangle]
pub extern "system" fn Java_ai_peerdesk_app_Native_stop<'local>(
    _env: JNIEnv<'local>,
    _class: JClass<'local>,
    h: jlong,
) {
    if let Some(h) = handle(h) {
        h.session.stop();
    }
}

/// 放掉句柄（会断开）。之后不能再用这个句柄。
#[no_mangle]
pub extern "system" fn Java_ai_peerdesk_app_Native_free<'local>(
    _env: JNIEnv<'local>,
    _class: JClass<'local>,
    h: jlong,
) {
    let Some(addr) = addr_from_handle(h) else {
        return;
    };
    // SAFETY: 同 `handle`：地址来自 `start` 的 `Box::into_raw`，Kotlin 保证只放一次、放了不再用。
    drop(unsafe { Box::from_raw(addr as *mut Handle) });
}
