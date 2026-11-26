//! 放在系统剪贴板上的“承诺”：记着现在承诺的是什么，系统来取时向对方要数据，
//! 再变成各系统要的格式（Windows 的 UTF-16、`CF_DIBV5`、`DROPFILES`，macOS 的文件 URL）。
//!
//! 这里没有系统调用，可以单独测试；调系统接口的胶水在 `clipboard_win.rs`、`clipboard_mac.rs`
//! 和 `clipboard_sys.rs`。

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use peerdesk_core::clipboard::ClipKind;
use peerdesk_net::clipsync::{ClipNote, Promise};

use crate::clipboard::{png_to_rgba, unpack_to};

/// 剪贴板事件的回调（界面提示用），可以在系统回调的线程上调用。
pub type Note = Arc<dyn Fn(ClipNote) + Send + Sync>;

#[derive(Default)]
struct State {
    promise: Option<Promise>,
    /// 承诺的文件已经解到了哪里（同一份承诺只解一次；系统会为每个格式、每个文件各来取一次）。
    unpacked: Option<Vec<PathBuf>>,
}

/// 现在放在系统剪贴板上的承诺。
pub struct Promised {
    state: Mutex<State>,
    note: Note,
    /// 系统回调里最多等多久。
    timeout: Duration,
    /// 文件解到这个目录下（每份承诺一个子目录）。
    temp: PathBuf,
}

impl std::fmt::Debug for Promised {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Promised({:?})", self.on_board())
    }
}

impl Promised {
    /// 创建。`timeout`：系统回调里等对方数据的上限；`temp`：收到的文件解到哪里。
    #[must_use]
    pub fn new(note: Note, timeout: Duration, temp: PathBuf) -> Self {
        Self {
            state: Mutex::new(State::default()),
            note,
            timeout,
            temp,
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// 换成新的承诺（刚放到系统剪贴板上）。
    pub fn set(&self, promise: Promise) {
        *self.lock() = State {
            promise: Some(promise),
            unpacked: None,
        };
    }

    /// 承诺没了（系统剪贴板被别的内容顶掉）。
    pub fn clear(&self) {
        *self.lock() = State::default();
    }

    /// 现在承诺的是什么、有几项。
    #[must_use]
    pub fn on_board(&self) -> Option<(ClipKind, u16)> {
        self.lock().promise.as_ref().map(|p| (p.kind, p.items))
    }

    /// 向对方要承诺的内容（要等，不能拿着锁）；种类不对、取不到时为 `None`（原因已经告诉用户）。
    fn fetch(&self, kind: ClipKind) -> Option<Vec<u8>> {
        let promise = self.lock().promise.clone().filter(|p| p.kind == kind)?;
        let clip = promise.get(self.timeout, &|note| (self.note)(note))?;
        (clip.kind == kind).then_some(clip.data)
    }

    /// 文字。
    #[must_use]
    pub fn text(&self) -> Option<String> {
        String::from_utf8(self.fetch(ClipKind::Text)?).ok()
    }

    /// 图片（PNG）。
    #[must_use]
    pub fn png(&self) -> Option<Vec<u8>> {
        self.fetch(ClipKind::Png)
    }

    /// 图片解成 RGBA：（宽，高，像素）。
    #[must_use]
    pub fn rgba(&self) -> Option<(u32, u32, Vec<u8>)> {
        match png_to_rgba(&self.png()?) {
            Ok(image) => Some(image),
            Err(e) => {
                peerdesk_net::note!(warn, "剪贴板：对方的图片解不开：{e}");
                None
            }
        }
    }

    /// 文件：取到以后解到临时目录，返回最上层的路径。同一份承诺只解一次。
    #[must_use]
    pub fn files(&self) -> Option<Vec<PathBuf>> {
        if let Some(paths) = self.lock().unpacked.clone() {
            return Some(paths);
        }
        let data = self.fetch(ClipKind::Files)?;
        let mut state = self.lock();
        // 等数据的时候别的线程可能已经解好了（系统同时来取几个格式）
        if let Some(paths) = state.unpacked.clone() {
            return Some(paths);
        }
        let dir = self.temp.join(format!(
            "{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_millis())
        ));
        match unpack_to(&data, &dir) {
            Ok(paths) => {
                peerdesk_net::note!(
                    info,
                    "剪贴板：对方的 {} 个文件或文件夹解到 {dir:?}",
                    paths.len()
                );
                state.unpacked = Some(paths.clone());
                Some(paths)
            }
            Err(e) => {
                peerdesk_net::note!(warn, "剪贴板：对方的文件解不到 {dir:?}：{e}");
                None
            }
        }
    }
}

/// Windows `CF_UNICODETEXT`：UTF-16（小端），以 0 结尾。
#[must_use]
pub fn utf16_nul(text: &str) -> Vec<u8> {
    text.encode_utf16()
        .chain(std::iter::once(0))
        .flat_map(u16::to_le_bytes)
        .collect()
}

/// Windows `CF_DIBV5`：`BITMAPV5HEADER`（124 字节）后面跟像素。和 arboard 3.7 写图片时一样：
/// 32 位、`BI_BITFIELDS`、BGRA、行从下到上（高度写正数：Word 不认负的高度）、sRGB。
#[must_use]
pub fn dibv5(width: u32, height: u32, rgba: &[u8]) -> Vec<u8> {
    const HEADER: u32 = 124;
    const BI_BITFIELDS: u32 = 3;
    const LCS_SRGB: u32 = 0x7352_4742;
    const LCS_GM_IMAGES: u32 = 4;
    let size = u32::try_from(rgba.len()).unwrap_or(u32::MAX);
    let mut out = Vec::with_capacity(rgba.len().saturating_add(124));
    let mut put = |v: u32| out.extend_from_slice(&v.to_le_bytes());
    put(HEADER);
    put(width);
    put(height);
    put(1 | (32 << 16)); // 位面数 1（16 位）和每像素位数 32（16 位）
    put(BI_BITFIELDS);
    put(size);
    put(0); // 横向分辨率
    put(0); // 纵向分辨率
    put(0); // 调色板
    put(0);
    put(0x00ff_0000); // 红
    put(0x0000_ff00); // 绿
    put(0x0000_00ff); // 蓝
    put(0xff00_0000); // 透明度
    put(LCS_SRGB);
    for _ in 0..9 {
        put(0); // 色彩空间端点（sRGB 时不用）
    }
    put(0); // 三个伽马
    put(0);
    put(0);
    put(LCS_GM_IMAGES);
    put(0); // 配置文件
    put(0);
    put(0); // 保留
    let stride = usize::try_from(width)
        .unwrap_or(usize::MAX)
        .saturating_mul(4);
    if stride > 0 {
        for row in rgba.chunks_exact(stride).rev() {
            for px in row.chunks_exact(4) {
                if let [r, g, b, a] = px {
                    out.extend_from_slice(&[*b, *g, *r, *a]);
                }
            }
        }
    }
    out
}

/// Windows `CF_HDROP`：`DROPFILES`（20 字节，宽字符）后面跟每个路径（以 0 结尾），最后再一个 0。
#[must_use]
pub fn dropfiles(paths: &[PathBuf]) -> Vec<u8> {
    let mut out = Vec::new();
    for v in [20u32, 0, 0, 0, 1] {
        // 路径表的偏移、落点 x、y、是否非客户区、是否宽字符
        out.extend_from_slice(&v.to_le_bytes());
    }
    for path in paths {
        out.extend(utf16_nul(&path.to_string_lossy()));
    }
    out.extend_from_slice(&[0, 0]);
    out
}

/// 文件路径变成 `file://` 网址（macOS `public.file-url`）：路径分隔符和不用转义的字符保留，其余按字节转义。
#[must_use]
pub fn file_url(path: &Path) -> String {
    let mut out = String::from("file://");
    for byte in path.to_string_lossy().bytes() {
        if byte.is_ascii_alphanumeric() || b"-._~/".contains(&byte) {
            out.push(char::from(byte));
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::indexing_slicing)]
    use super::*;
    use peerdesk_core::clipboard::{pack_files, Clip, PackedFile};
    use peerdesk_net::clipsync::FetchError;
    use std::sync::atomic::{AtomicU32, Ordering};

    type Notes = Arc<Mutex<Vec<ClipNote>>>;

    fn promised(tag: &str) -> (Promised, Notes, PathBuf) {
        let notes = Notes::default();
        let sink = Arc::clone(&notes);
        let temp =
            std::env::temp_dir().join(format!("peerdesk-promised-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&temp);
        let p = Promised::new(
            Arc::new(move |note| sink.lock().unwrap().push(note)),
            Duration::from_millis(10),
            temp.clone(),
        );
        (p, notes, temp)
    }

    /// 一个取数会成功（并数次数）或者失败的承诺。
    fn promise(result: Result<Clip, FetchError>, kind: ClipKind) -> (Promise, Arc<AtomicU32>) {
        let fetched = Arc::new(AtomicU32::new(0));
        let count = Arc::clone(&fetched);
        let promise = Promise {
            kind,
            items: 1,
            fetch: Arc::new(move |_| {
                count.fetch_add(1, Ordering::Relaxed);
                result.clone()
            }),
        };
        (promise, fetched)
    }

    #[test]
    fn text_and_images_are_fetched_when_the_system_asks() {
        let (p, notes, _) = promised("text");
        // 没有承诺：什么都没有
        assert_eq!((p.on_board(), p.text(), p.png()), (None, None, None));
        assert!(p.files().is_none() && p.rgba().is_none());
        assert_eq!(format!("{p:?}"), "Promised(None)");
        let (text, fetched) = promise(Ok(Clip::text("对方的文字")), ClipKind::Text);
        p.set(text);
        assert_eq!(p.on_board(), Some((ClipKind::Text, 1)));
        assert_eq!(fetched.load(Ordering::Relaxed), 0, "放承诺时不取");
        assert_eq!(p.text().as_deref(), Some("对方的文字"));
        // 承诺的是文字，系统来要别的格式：没有，也不去取
        assert!(p.png().is_none() && p.files().is_none());
        assert_eq!(fetched.load(Ordering::Relaxed), 1);
        // 图片
        let png = crate::clipboard::rgba_to_png(2, 1, &[1, 2, 3, 4, 5, 6, 7, 8]).unwrap();
        let clip = Clip {
            kind: ClipKind::Png,
            data: png.clone(),
        };
        p.set(promise(Ok(clip), ClipKind::Png).0);
        assert_eq!(p.png(), Some(png));
        assert_eq!(p.rgba(), Some((2, 1, vec![1, 2, 3, 4, 5, 6, 7, 8])));
        // 对方给的不是图片、图片是坏的：空结果
        p.set(promise(Ok(Clip::text("不是图片")), ClipKind::Png).0);
        assert!(p.png().is_none());
        let bad = Clip {
            kind: ClipKind::Png,
            data: vec![1, 2, 3],
        };
        p.set(promise(Ok(bad), ClipKind::Png).0);
        assert!(p.rgba().is_none());
        let not_utf8 = Clip {
            kind: ClipKind::Text,
            data: vec![0xff],
        };
        p.set(promise(Ok(not_utf8), ClipKind::Text).0);
        assert!(p.text().is_none());
        // 取不到：空结果，原因告诉用户
        assert!(notes.lock().unwrap().is_empty());
        p.set(promise(Err(FetchError::Timeout), ClipKind::Text).0);
        assert!(p.text().is_none());
        assert_eq!(
            *notes.lock().unwrap(),
            vec![ClipNote::FetchFailed(FetchError::Timeout)]
        );
        // 被顶掉以后什么都没有
        p.clear();
        assert_eq!(p.on_board(), None);
    }

    #[test]
    fn files_are_unpacked_once_per_promise() {
        let (p, _, temp) = promised("files");
        let packed = pack_files(&[
            PackedFile {
                path: "夹/a.txt".into(),
                data: b"hello".to_vec(),
            },
            PackedFile {
                path: "b.bin".into(),
                data: vec![7; 9],
            },
        ])
        .unwrap();
        let clip = Clip {
            kind: ClipKind::Files,
            data: packed,
        };
        let (files, fetched) = promise(Ok(clip.clone()), ClipKind::Files);
        p.set(files);
        let paths = p.files().unwrap();
        assert_eq!(paths.len(), 2);
        assert!(paths.iter().all(|path| path.starts_with(&temp)));
        assert_eq!(std::fs::read(paths[0].join("a.txt")).unwrap(), b"hello");
        assert_eq!(std::fs::read(&paths[1]).unwrap(), vec![7; 9]);
        // 系统再来取（每个文件、每个格式一次）：不再取、不再解
        assert_eq!(p.files().unwrap(), paths);
        assert_eq!(fetched.load(Ordering::Relaxed), 1);
        // 新的承诺：重新取、解到新的目录
        std::thread::sleep(Duration::from_millis(2));
        p.set(promise(Ok(clip), ClipKind::Files).0);
        assert_ne!(p.files().unwrap(), paths);
        // 包是坏的、临时目录建不了：空结果
        let bad = Clip {
            kind: ClipKind::Files,
            data: vec![0, 9],
        };
        p.set(promise(Ok(bad), ClipKind::Files).0);
        assert!(p.files().is_none());
        let _ = std::fs::remove_dir_all(temp);
    }

    #[test]
    fn windows_formats() {
        assert_eq!(utf16_nul("a字"), vec![0x61, 0, 0x57, 0x5b, 0, 0]);
        // 2×2 的图：头 124 字节，像素 BGRA、行从下到上
        let rgba = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16];
        let dib = dibv5(2, 2, &rgba);
        assert_eq!(dib.len(), 124 + 16);
        assert_eq!(dib[0..4], 124u32.to_le_bytes());
        assert_eq!(dib[4..8], 2u32.to_le_bytes());
        assert_eq!(dib[8..12], 2u32.to_le_bytes());
        assert_eq!(dib[12..16], [1, 0, 32, 0]);
        assert_eq!(dib[16..20], 3u32.to_le_bytes());
        assert_eq!(dib[20..24], 16u32.to_le_bytes());
        assert_eq!(dib[40..44], 0x00ff_0000u32.to_le_bytes());
        assert_eq!(dib[52..56], 0xff00_0000u32.to_le_bytes());
        assert_eq!(dib[56..60], *b"BGRs");
        assert_eq!(dib[108..112], 4u32.to_le_bytes());
        assert_eq!(
            dib[124..],
            [11, 10, 9, 12, 15, 14, 13, 16, 3, 2, 1, 4, 7, 6, 5, 8]
        );
        assert_eq!(dibv5(0, 0, &[]).len(), 124);
        // 文件列表：头 20 字节，路径以 0 结尾，最后多一个 0
        let drop = dropfiles(&[PathBuf::from("C:\\a"), PathBuf::from("b")]);
        assert_eq!(drop[0..4], 20u32.to_le_bytes());
        assert_eq!(drop[16..20], 1u32.to_le_bytes());
        let wide: Vec<u16> = drop[20..]
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect();
        assert_eq!(String::from_utf16(&wide).unwrap(), "C:\\a\0b\0\0");
    }

    #[test]
    fn file_urls_are_escaped() {
        assert_eq!(
            file_url(Path::new("/tmp/报告 1/a-b_c.txt")),
            "file:///tmp/%E6%8A%A5%E5%91%8A%201/a-b_c.txt"
        );
    }
}
