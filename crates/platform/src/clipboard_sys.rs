//! 真实的系统剪贴板（系统胶水）。读写用 arboard；变化检测用各系统的计数：
//! Windows 剪贴板序号、macOS `changeCount`，Linux（X11）没有便宜的计数，比较内容的哈希。
//!
//! 测试构建不会创建它（不碰开发者电脑的剪贴板），在各平台实机上验证。
#![cfg_attr(windows, allow(unsafe_code))]

use std::sync::{Mutex, PoisonError};

use peerdesk_core::clipboard::FILES_LIMIT;
use peerdesk_core::clipboard::{Clip, ClipKind};
use peerdesk_net::clipsync::{ClipNote, ClipboardHandle, Copied, SystemClipboard};

use crate::clipboard::{pack_paths, png_to_rgba, rgba_to_png, unpack_to};

/// 剪贴板事件的回调（界面提示用）。
pub type OnNote = Box<dyn Fn(ClipNote) + Send + Sync>;

/// 系统剪贴板。
pub struct SystemClip {
    inner: Mutex<arboard::Clipboard>,
    on_note: OnNote,
    /// Linux 上用内容哈希当计数：（上次的哈希，计数）。
    #[cfg_attr(any(windows, target_os = "macos"), allow(dead_code))]
    hashed: Mutex<(u64, u64)>,
}

impl std::fmt::Debug for SystemClip {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SystemClip")
    }
}

impl SystemClip {
    /// 打开系统剪贴板。
    pub fn open(on_note: OnNote) -> Result<Self, String> {
        let inner = arboard::Clipboard::new().map_err(|e| format!("打不开剪贴板：{e}"))?;
        Ok(Self {
            inner: Mutex::new(inner),
            on_note,
            hashed: Mutex::new((0, 0)),
        })
    }

    fn clipboard(&self) -> std::sync::MutexGuard<'_, arboard::Clipboard> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// 密码管理器把内容标成敏感时不同步。
#[cfg(windows)]
fn concealed() -> bool {
    use windows::core::w;
    use windows::Win32::System::DataExchange::{
        IsClipboardFormatAvailable, RegisterClipboardFormatW,
    };
    // SAFETY: 两个函数都只读剪贴板状态，参数是静态的宽字符串。
    unsafe {
        let format = RegisterClipboardFormatW(w!("ExcludeClipboardContentFromMonitorProcessing"));
        format != 0 && IsClipboardFormatAvailable(format).is_ok()
    }
}

#[cfg(target_os = "macos")]
fn concealed() -> bool {
    use objc2_app_kit::NSPasteboard;
    let board = NSPasteboard::generalPasteboard();
    board.types().is_some_and(|types| {
        types
            .iter()
            .any(|t| t.to_string() == "org.nspasteboard.ConcealedType")
    })
}

#[cfg(not(any(windows, target_os = "macos")))]
const fn concealed() -> bool {
    false
}

impl SystemClipboard for SystemClip {
    fn change_count(&self) -> u64 {
        #[cfg(windows)]
        {
            // SAFETY: 只读一个计数，没有参数。
            u64::from(unsafe { windows::Win32::System::DataExchange::GetClipboardSequenceNumber() })
        }
        #[cfg(target_os = "macos")]
        {
            let count = objc2_app_kit::NSPasteboard::generalPasteboard().changeCount();
            u64::try_from(count).unwrap_or(0)
        }
        #[cfg(not(any(windows, target_os = "macos")))]
        {
            use std::hash::{Hash, Hasher};
            let mut h = std::collections::hash_map::DefaultHasher::new();
            match self.clipboard().get_text() {
                Ok(text) => text.hash(&mut h),
                // 没有文本时看图片的尺寸和开头的像素（读整张图太贵）
                Err(_) => {
                    if let Ok(img) = self.clipboard().get_image() {
                        (img.width, img.height).hash(&mut h);
                        img.bytes.get(..4096).hash(&mut h);
                    }
                }
            }
            let hash = h.finish();
            let mut state = self.hashed.lock().unwrap_or_else(PoisonError::into_inner);
            if state.0 != hash {
                *state = (hash, state.1.wrapping_add(1));
            }
            state.1
        }
    }

    fn read(&self) -> Option<Copied> {
        if concealed() {
            peerdesk_net::note!(info, "剪贴板：内容被标为敏感（密码管理器），不同步");
            return None;
        }
        let mut board = self.clipboard();
        // 复制了文件：先看文件（同时还会有文件名文本、图标图片，不用它们）
        if let Ok(paths) = board.get().file_list() {
            if !paths.is_empty() {
                drop(board);
                return Some(match pack_paths(&paths, FILES_LIMIT) {
                    Ok(data) => Copied::Clip(Clip {
                        kind: ClipKind::Files,
                        data,
                    }),
                    Err(size) => Copied::TooLarge {
                        kind: ClipKind::Files,
                        size,
                    },
                });
            }
        }
        // 同时有文本和图片时（例如从网页复制）用文本
        if let Ok(text) = board.get_text() {
            if !text.is_empty() {
                return Some(Copied::Clip(Clip::text(&text)));
            }
        }
        let img = board.get_image().ok()?;
        let width = u32::try_from(img.width).ok()?;
        let height = u32::try_from(img.height).ok()?;
        match rgba_to_png(width, height, &img.bytes) {
            Ok(data) => Some(Copied::Clip(Clip {
                kind: ClipKind::Png,
                data,
            })),
            Err(e) => {
                peerdesk_net::note!(warn, "剪贴板：图片转 PNG 失败：{e}");
                None
            }
        }
    }

    fn write(&self, clip: &Clip) -> Result<(), String> {
        let mut board = self.clipboard();
        match clip.kind {
            ClipKind::Text => {
                let text = std::str::from_utf8(&clip.data).map_err(|e| e.to_string())?;
                board.set_text(text).map_err(|e| e.to_string())
            }
            ClipKind::Png => {
                let (width, height, rgba) = png_to_rgba(&clip.data)?;
                let image = arboard::ImageData {
                    width: usize::try_from(width).map_err(|e| e.to_string())?,
                    height: usize::try_from(height).map_err(|e| e.to_string())?,
                    bytes: rgba.into(),
                };
                board.set_image(image).map_err(|e| e.to_string())
            }
            ClipKind::Files => {
                // 解到临时目录里新建的子目录（每次一个，不覆盖之前粘贴的），剪贴板里放这些文件
                let dir = std::env::temp_dir()
                    .join("PeerDesk-clipboard")
                    .join(format!(
                        "{}",
                        std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .map_or(0, |d| d.as_millis())
                    ));
                let top = unpack_to(&clip.data, &dir)?;
                board.set().file_list(&top).map_err(|e| e.to_string())
            }
        }
    }

    fn note(&self, note: ClipNote) {
        (self.on_note)(note);
    }
}

/// 打开系统剪贴板，放进会话选项；打不开时记日志并返回 `None`（会话照常，只是不同步剪贴板）。
#[must_use]
pub fn open_clipboard(on_note: OnNote) -> Option<ClipboardHandle> {
    match SystemClip::open(on_note) {
        Ok(clip) => Some(ClipboardHandle(std::sync::Arc::new(clip))),
        Err(e) => {
            peerdesk_net::note!(warn, "剪贴板不可用，不同步：{e}");
            None
        }
    }
}
