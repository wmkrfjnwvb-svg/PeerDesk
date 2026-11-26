//! 真实的系统剪贴板（系统胶水）。读写用 arboard；变化检测用各系统的计数：
//! Windows 剪贴板序号、macOS `changeCount`，Linux（X11）没有便宜的计数，比较内容的哈希。
//!
//! 对方复制的内容不直接写进来，而是放一个“承诺”（[`SystemClipboard::promise`]）：只声明格式，
//! 本机程序来取时才向对方要（Windows 的延迟渲染在 `clipboard_win.rs`，macOS 的数据提供者在
//! `clipboard_mac.rs`，取数和格式转换在 [`crate::promised`]）。Linux 还没有做，放承诺会返回不支持，
//! 这时对方复制的内容只有在远程窗口按粘贴键时才传（控制端 → 被控端），反方向不同步。
//!
//! 测试构建不会创建它（不碰开发者电脑的剪贴板），在各平台实机上验证。
#![cfg_attr(windows, allow(unsafe_code))]

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use peerdesk_core::clipboard::FILES_LIMIT;
use peerdesk_core::clipboard::{Clip, ClipKind};
#[cfg(any(windows, target_os = "macos"))]
use peerdesk_net::clipsync::Promise;
use peerdesk_net::clipsync::{
    ClipNote, ClipboardHandle, Copied, Peeked, SystemClipboard, FETCH_TIMEOUT,
};

use crate::promised::Promised;

use crate::clipboard::{pack_paths, png_to_rgba, rgba_to_png, unpack_to};

/// 剪贴板事件的回调（界面提示用）。
pub type OnNote = Box<dyn Fn(ClipNote) + Send + Sync>;

/// 系统剪贴板。
pub struct SystemClip {
    inner: Mutex<arboard::Clipboard>,
    on_note: crate::promised::Note,
    /// 现在放在系统剪贴板上的承诺（对方的内容）。Linux 上还放不了承诺，用不到。
    #[cfg_attr(not(any(windows, target_os = "macos")), allow(dead_code))]
    promised: Arc<Promised>,
    /// macOS：放上承诺以后的变化计数（计数没变说明剪贴板里还是我们的承诺，不是用户复制的东西）。
    /// Windows 上看剪贴板的所有者是不是我们的窗口，不用它。
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    promised_count: Arc<Mutex<Option<u64>>>,
    /// Windows：延迟渲染用的窗口；建不起来时为 `None`（放承诺返回不支持）。
    #[cfg(windows)]
    delayed: Option<crate::clipboard_win::DelayedClipboard>,
    /// 用户在界面上点了“取消”（界面置位，同步循环取走）。
    cancel: Arc<AtomicBool>,
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
    /// 打开系统剪贴板。`cancel`：界面在用户点“取消传输”时置位。
    pub fn open(on_note: OnNote, cancel: Arc<AtomicBool>) -> Result<Self, String> {
        let inner = arboard::Clipboard::new().map_err(|e| format!("打不开剪贴板：{e}"))?;
        let on_note: crate::promised::Note = Arc::from(on_note);
        let promised = Arc::new(Promised::new(
            Arc::clone(&on_note),
            FETCH_TIMEOUT,
            std::env::temp_dir().join("PeerDesk-clipboard"),
        ));
        Ok(Self {
            inner: Mutex::new(inner),
            on_note,
            #[cfg(windows)]
            delayed: start_delayed(&promised),
            promised,
            promised_count: Arc::default(),
            cancel,
            hashed: Mutex::new((0, 0)),
        })
    }

    fn clipboard(&self) -> std::sync::MutexGuard<'_, arboard::Clipboard> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// 系统剪贴板里现在是不是我们放的承诺（对方的内容）。是的话不能当成本机复制的内容去看、去读：
    /// 读它会触发我们自己的回调，向对方要数据。被别的内容顶掉了就把承诺忘掉。
    fn ours(&self) -> bool {
        #[cfg(windows)]
        {
            // 被顶掉时窗口收到 WM_DESTROYCLIPBOARD，那边会把承诺忘掉
            self.promised.on_board().is_some() && self.delayed.as_ref().is_some_and(|d| d.owns())
        }
        #[cfg(target_os = "macos")]
        {
            let mut at = self
                .promised_count
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            let ours = at.is_some_and(|count| count == self.change_count());
            if !ours && at.take().is_some() {
                self.promised.clear();
            }
            ours
        }
        #[cfg(not(any(windows, target_os = "macos")))]
        {
            false
        }
    }

    /// 剪贴板里有没有图片。只看有没有这种格式，不把像素取出来（一张 4K 截图有 30 多 MB）。
    fn has_image(&self) -> bool {
        #[cfg(windows)]
        {
            use windows::Win32::System::DataExchange::IsClipboardFormatAvailable;
            // CF_BITMAP = 2，CF_DIB = 8，CF_DIBV5 = 17（有其中一种时系统会按需合成另外两种）
            // SAFETY: 只读剪贴板状态，参数是常数。
            unsafe {
                [2u32, 8, 17]
                    .into_iter()
                    .any(|f| IsClipboardFormatAvailable(f).is_ok())
            }
        }
        #[cfg(target_os = "macos")]
        {
            objc2_app_kit::NSPasteboard::generalPasteboard()
                .types()
                .is_some_and(|types| {
                    types.iter().any(|t| {
                        let t = t.to_string();
                        t == "public.png" || t == "public.tiff"
                    })
                })
        }
        #[cfg(not(any(windows, target_os = "macos")))]
        {
            // X11 没有便宜的办法只问格式（arboard 不提供），只能取一次；Linux 做控制端时每次复制图片多这一步
            self.clipboard().get_image().is_ok()
        }
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
            // 复制的文件也算进去：有的文件管理器复制文件时不带文本，只看文本和图片就发现不了
            if let Ok(paths) = self.clipboard().get().file_list() {
                paths.hash(&mut h);
            }
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
        if self.ours() {
            return None;
        }
        if concealed() {
            peerdesk_net::note!(info, "剪贴板：内容被标为敏感（密码管理器），不同步");
            return None;
        }
        let mut board = self.clipboard();
        // 复制了文件：先看文件（同时还会有文件名文本、图标图片，不用它们）
        if let Ok(paths) = board.get().file_list() {
            if !paths.is_empty() {
                drop(board);
                peerdesk_net::note!(
                    info,
                    "剪贴板：本机复制了 {} 个文件或文件夹，第一个是 {:?}",
                    paths.len(),
                    paths.first()
                );
                let packed = match pack_paths(&paths, FILES_LIMIT) {
                    Ok(packed) => packed,
                    Err(size) => {
                        return Some(Copied::TooLarge {
                            kind: ClipKind::Files,
                            size,
                        })
                    }
                };
                // 读不了的、名字对方不能用的：写进日志，并告诉用户（不能悄悄少了文件）
                for (path, why) in &packed.skipped {
                    peerdesk_net::note!(warn, "剪贴板：{path:?} 没有同步：{why}");
                }
                if !packed.skipped.is_empty() {
                    (self.on_note)(ClipNote::FilesSkipped {
                        skipped: u32::try_from(packed.skipped.len()).unwrap_or(u32::MAX),
                        sent: packed.sent,
                    });
                }
                // 一个都没读出来：没有可同步的（发一份空的过去，对方的剪贴板只会被清掉）
                if packed.sent == 0 {
                    return None;
                }
                return Some(Copied::Clip(Clip {
                    kind: ClipKind::Files,
                    data: packed.data,
                }));
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

    fn peek(&self) -> Option<Peeked> {
        if concealed() || self.ours() {
            return None;
        }
        // 顺序和 read 一样：文件、文字、图片。文件只看有没有，不读盘；图片只看格式，不取像素
        let mut board = self.clipboard();
        if let Ok(paths) = board.get().file_list() {
            if !paths.is_empty() {
                let items = u16::try_from(paths.len()).unwrap_or(u16::MAX);
                return Some(Peeked::Later(ClipKind::Files, items));
            }
        }
        if let Ok(text) = board.get_text() {
            if !text.is_empty() {
                return Some(Peeked::Text(Clip::text(&text)));
            }
        }
        drop(board);
        self.has_image().then_some(Peeked::Later(ClipKind::Png, 1))
    }

    fn take_cancel(&self) -> bool {
        self.cancel.swap(false, Ordering::Relaxed)
    }

    fn promise_in_place(&self) -> bool {
        self.ours()
    }

    #[cfg(windows)]
    fn promise(&self, promise: Promise) -> Result<(), String> {
        use crate::clipboard_win::{png_format, CF_DIBV5, CF_HDROP, CF_UNICODETEXT};
        let delayed = self.delayed.as_ref().ok_or("延迟渲染用的窗口没有建起来")?;
        let formats = match promise.kind {
            ClipKind::Text => vec![CF_UNICODETEXT],
            // PNG 在前：认它的程序能拿到带透明度的原图
            ClipKind::Png => vec![png_format(), CF_DIBV5],
            ClipKind::Files => vec![CF_HDROP],
        };
        // 先声明、再记下承诺：声明时系统会先通知“上一份被清掉了”（把旧承诺忘掉）
        delayed.offer(&formats)?;
        self.promised.set(promise);
        Ok(())
    }

    #[cfg(target_os = "macos")]
    fn promise(&self, promise: Promise) -> Result<(), String> {
        use crate::clipboard_mac::{offer, Provide, Provided, Settled, FILE_URL, PNG, TEXT};
        let (kind, items) = match promise.kind {
            ClipKind::Text => (TEXT, 1),
            ClipKind::Png => (PNG, 1),
            ClipKind::Files => (FILE_URL, usize::from(promise.items)),
        };
        let promised = Arc::clone(&self.promised);
        let provide: Provide = Arc::new(move |index, kind| match kind {
            TEXT => promised.text().map(Provided::Text),
            PNG => promised.png().map(Provided::Data),
            FILE_URL => promised
                .files()
                .and_then(|paths| paths.get(index).map(|p| crate::promised::file_url(p)))
                .map(Provided::Text),
            _ => None,
        });
        // 数据交给系统以后变化计数可能会变（文档没有说）：重新记，免得把自己的承诺当成用户复制的东西
        let count = Arc::clone(&self.promised_count);
        let settled: Settled = Arc::new(move || {
            let now = objc2_app_kit::NSPasteboard::generalPasteboard().changeCount();
            let mut at = count.lock().unwrap_or_else(PoisonError::into_inner);
            if at.is_some() {
                *at = u64::try_from(now).ok();
            }
        });
        self.promised.set(promise);
        offer(items, kind, &provide, &settled).inspect_err(|_| self.promised.clear())?;
        self.promised_placed();
        Ok(())
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
                // 对方是老版本时才走这里（整份推过来）；新版本放承诺，被取时解到同一个临时目录。
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
                peerdesk_net::note!(
                    info,
                    "剪贴板：收到的 {} 个文件或文件夹解到 {dir:?}，放进剪贴板",
                    top.len()
                );
                // 先清空再放：arboard 3.7 在 Windows 上放文件列表时不清空剪贴板（放文字、图片时会清），
                // 之前复制的文字会留在里面，和文件并存，有的程序粘贴出来的是旧文字。
                board.clear().map_err(|e| format!("清空剪贴板失败：{e}"))?;
                board.set().file_list(&top).map_err(|e| {
                    format!("把文件放进剪贴板失败（{} 个，在 {dir:?}）：{e}", top.len())
                })
            }
        }
    }

    fn note(&self, note: ClipNote) {
        (self.on_note)(note);
    }
}

impl SystemClip {
    /// 承诺刚放上去：记下现在的变化计数。
    #[cfg(target_os = "macos")]
    fn promised_placed(&self) {
        let count = self.change_count();
        *self
            .promised_count
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(count);
    }
}

/// Windows：起延迟渲染的窗口。来取某个格式时从承诺里取数、转成这个格式的字节。
#[cfg(windows)]
fn start_delayed(promised: &Arc<Promised>) -> Option<crate::clipboard_win::DelayedClipboard> {
    use crate::clipboard_win::{png_format, DelayedClipboard, CF_DIBV5, CF_HDROP, CF_UNICODETEXT};
    use crate::promised::{dibv5, dropfiles, utf16_nul};
    let png = png_format();
    let render = Arc::clone(promised);
    let lost = Arc::clone(promised);
    let started = DelayedClipboard::start(
        Arc::new(move |format| match format {
            CF_UNICODETEXT => render.text().map(|text| utf16_nul(&text)),
            CF_DIBV5 => render.rgba().map(|(w, h, rgba)| dibv5(w, h, &rgba)),
            CF_HDROP => render.files().map(|paths| dropfiles(&paths)),
            other if other == png && png != 0 => render.png(),
            _ => None,
        }),
        Arc::new(move || lost.clear()),
    );
    match started {
        Ok(delayed) => Some(delayed),
        Err(e) => {
            peerdesk_net::note!(warn, "剪贴板：{e}，对方复制的内容不能按需取");
            None
        }
    }
}

/// 打开系统剪贴板，放进会话选项；打不开时记日志并返回 `None`（会话照常，只是不同步剪贴板）。
#[must_use]
pub fn open_clipboard(on_note: OnNote, cancel: Arc<AtomicBool>) -> Option<ClipboardHandle> {
    match SystemClip::open(on_note, cancel) {
        Ok(clip) => Some(ClipboardHandle(Arc::new(clip))),
        Err(e) => {
            peerdesk_net::note!(warn, "剪贴板不可用，不同步：{e}");
            None
        }
    }
}
