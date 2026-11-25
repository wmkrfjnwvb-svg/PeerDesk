//! 远程画面的显卡显示：NV12 交给 [`peerdesk_media::gl::Nv12Renderer`]，在 egui 的绘制回调里
//! （GL 线程上）上传、转颜色、画出来，CPU 不逐像素转换。
//!
//! 着色器打不开（OpenGL 太老）时 [`YuvView::failed`] 为真，查看窗口改在 CPU 上转颜色，并写日志说明原因。

use std::sync::{Arc, Mutex, PoisonError};

use eframe::{egui, egui_glow, glow};
use peerdesk_core::frame::Nv12;
use peerdesk_media::gl::Nv12Renderer;

/// 关掉的窗口留下的渲染器：没有 GL 上下文时删不了，下次绘制回调时删。
static TRASH: Mutex<Vec<Nv12Renderer>> = Mutex::new(Vec::new());

#[derive(Debug, Default)]
struct Shared {
    /// 还没上传的最新一帧。
    pending: Option<Arc<Nv12>>,
    renderer: Option<Nv12Renderer>,
    /// 打不开着色器的原因。
    failed: Option<String>,
}

/// 一个查看窗口的显卡画面。
#[derive(Debug, Default)]
pub struct YuvView {
    shared: Arc<Mutex<Shared>>,
}

impl YuvView {
    /// 换上新的一帧（下次绘制时上传）。
    pub fn set(&self, frame: Arc<Nv12>) {
        self.lock().pending = Some(frame);
    }

    /// 着色器打不开的原因（之后改在 CPU 上转颜色）。
    pub fn failed(&self) -> Option<String> {
        self.lock().failed.clone()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Shared> {
        self.shared.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// 在 `rect` 里画最新的画面。
    pub fn paint(&self, painter: &egui::Painter, rect: egui::Rect) {
        let shared = Arc::clone(&self.shared);
        let callback = egui_glow::CallbackFn::new(move |_info, glow_painter| {
            let gl: &glow::Context = glow_painter.gl();
            for old in TRASH
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .drain(..)
            {
                old.destroy(gl);
            }
            let mut s = shared.lock().unwrap_or_else(PoisonError::into_inner);
            if s.renderer.is_none() && s.failed.is_none() {
                let version = egui_glow::ShaderVersion::get(gl);
                match Nv12Renderer::new(
                    gl,
                    version.version_declaration(),
                    version.is_new_shader_interface(),
                ) {
                    Ok(r) => s.renderer = Some(r),
                    Err(why) => {
                        log::warn!("远程画面改在 CPU 上转颜色：{why}");
                        s.failed = Some(why);
                    }
                }
            }
            let pending = s.pending.take();
            if let Some(r) = s.renderer.as_mut() {
                if let Some(frame) = pending {
                    r.upload(gl, &frame);
                }
                r.paint(gl);
            }
        });
        painter.add(egui::PaintCallback {
            rect,
            callback: Arc::new(callback),
        });
    }
}

impl Drop for YuvView {
    fn drop(&mut self) {
        if let Some(r) = self.lock().renderer.take() {
            TRASH.lock().unwrap_or_else(PoisonError::into_inner).push(r);
        }
    }
}
