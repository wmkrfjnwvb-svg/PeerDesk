//! 文件系统上的 [`Store`] 实现，以及发送前的目录扫描。
//!
//! 接收中的文件写在 `<名字>.part`，校验通过后改名为正式文件名。
//! 所有路径都限制在根目录之内。

use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use peerdesk_core::file::{check_path, FileMeta, Store, MAX_FILES};
use peerdesk_core::Error;

/// 未完成文件的后缀。
pub const PARTIAL_SUFFIX: &str = ".part";

/// 以某个目录为根的存储。
#[derive(Debug, Clone)]
pub struct FsStore {
    root: PathBuf,
}

fn storage(what: &str, path: &Path, e: &std::io::Error) -> Error {
    Error::Storage(format!("{what} {}：{e}", path.display()))
}

fn read_at(path: &Path, offset: u64, len: usize) -> Result<Vec<u8>, Error> {
    let mut file = File::open(path).map_err(|e| storage("打开", path, &e))?;
    file.seek(SeekFrom::Start(offset))
        .map_err(|e| storage("定位", path, &e))?;
    let mut out = Vec::with_capacity(len);
    file.take(u64::try_from(len).unwrap_or(u64::MAX))
        .read_to_end(&mut out)
        .map_err(|e| storage("读取", path, &e))?;
    Ok(out)
}

impl FsStore {
    /// 创建。
    #[must_use]
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    fn final_path(&self, rel: &str) -> Result<PathBuf, Error> {
        check_path(rel)?;
        Ok(rel
            .split('/')
            .fold(self.root.clone(), |p, part| p.join(part)))
    }

    fn partial_path(&self, rel: &str) -> Result<PathBuf, Error> {
        let mut path = self.final_path(rel)?.into_os_string();
        path.push(PARTIAL_SUFFIX);
        Ok(PathBuf::from(path))
    }
}

impl Store for FsStore {
    fn read(&mut self, path: &str, offset: u64, len: usize) -> Result<Vec<u8>, Error> {
        read_at(&self.final_path(path)?, offset, len)
    }

    fn partial_len(&mut self, path: &str) -> Result<u64, Error> {
        let partial = self.partial_path(path)?;
        match fs::metadata(&partial) {
            Ok(meta) if meta.is_file() => Ok(meta.len()),
            Ok(_) => Err(Error::Storage(format!(
                "不是普通文件：{}",
                partial.display()
            ))),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(0),
            Err(e) => Err(storage("查看", &partial, &e)),
        }
    }

    fn read_partial(&mut self, path: &str, offset: u64, len: usize) -> Result<Vec<u8>, Error> {
        read_at(&self.partial_path(path)?, offset, len)
    }

    fn write(&mut self, path: &str, offset: u64, data: &[u8]) -> Result<(), Error> {
        let partial = self.partial_path(path)?;
        let mut file = OpenOptions::new()
            .write(true)
            .open(&partial)
            .map_err(|e| storage("打开", &partial, &e))?;
        file.seek(SeekFrom::Start(offset))
            .map_err(|e| storage("定位", &partial, &e))?;
        file.write_all(data)
            .map_err(|e| storage("写入", &partial, &e))
    }

    fn truncate(&mut self, path: &str, len: u64) -> Result<(), Error> {
        let partial = self.partial_path(path)?;
        if let Some(parent) = partial.parent() {
            fs::create_dir_all(parent).map_err(|e| storage("创建目录", parent, &e))?;
        }
        let file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .open(&partial)
            .map_err(|e| storage("创建", &partial, &e))?;
        file.set_len(len).map_err(|e| storage("截断", &partial, &e))
    }

    fn finish(&mut self, path: &str, mtime: u64) -> Result<(), Error> {
        let (partial, target) = (self.partial_path(path)?, self.final_path(path)?);
        let file = OpenOptions::new()
            .write(true)
            .open(&partial)
            .map_err(|e| storage("打开", &partial, &e))?;
        file.sync_all().map_err(|e| storage("落盘", &partial, &e))?;
        // 修改时间设置失败不影响文件内容，忽略。
        let _ = file.set_modified(UNIX_EPOCH + Duration::from_secs(mtime));
        drop(file);
        // 同名文件已存在时覆盖。
        fs::rename(&partial, &target).map_err(|e| storage("改名", &target, &e))
    }
}

fn scan_into(root: &Path, rel: &str, out: &mut Vec<FileMeta>) -> Result<(), Error> {
    let path = rel
        .split('/')
        .fold(root.to_path_buf(), |p, part| p.join(part));
    // 不跟随符号链接：避免把根目录之外的内容发出去。
    let meta = fs::symlink_metadata(&path).map_err(|e| storage("查看", &path, &e))?;
    if meta.is_file() {
        check_path(rel)?;
        if out.len() >= MAX_FILES {
            return Err(Error::TooManyItems {
                len: out.len().saturating_add(1),
                max: MAX_FILES,
            });
        }
        let mtime = meta
            .modified()
            .ok()
            .and_then(|t: SystemTime| t.duration_since(UNIX_EPOCH).ok())
            .map_or(0, |d| d.as_secs());
        out.push(FileMeta {
            path: rel.to_owned(),
            size: meta.len(),
            mtime,
        });
    } else if meta.is_dir() {
        let mut names = Vec::new();
        for entry in fs::read_dir(&path).map_err(|e| storage("列出", &path, &e))? {
            let entry = entry.map_err(|e| storage("列出", &path, &e))?;
            match entry.file_name().into_string() {
                Ok(name) => names.push(name),
                Err(_) => return Err(Error::UnsafePath(path.display().to_string())),
            }
        }
        names.sort();
        for name in names {
            scan_into(root, &format!("{rel}/{name}"), out)?;
        }
    }
    Ok(())
}

/// 列出 `root` 下名为 `name` 的文件，或名为 `name` 的目录里的所有文件（递归，按名字排序）。
pub fn scan(root: &Path, name: &str) -> Result<Vec<FileMeta>, Error> {
    check_path(name)?;
    let mut out = Vec::new();
    scan_into(root, name, &mut out)?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("peerdesk-fs-{}-{tag}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn partial_then_finish() {
        let root = temp_dir("store");
        let mut store = FsStore::new(&root);
        assert_eq!(store.partial_len("sub/a.bin").unwrap(), 0);
        store.truncate("sub/a.bin", 0).unwrap();
        store.write("sub/a.bin", 0, b"hello ").unwrap();
        store.write("sub/a.bin", 6, b"world").unwrap();
        assert_eq!(store.partial_len("sub/a.bin").unwrap(), 11);
        assert_eq!(store.read_partial("sub/a.bin", 6, 100).unwrap(), b"world");
        assert!(store.read_partial("sub/a.bin", 99, 5).unwrap().is_empty());
        store.truncate("sub/a.bin", 5).unwrap();
        assert_eq!(store.partial_len("sub/a.bin").unwrap(), 5);
        store.finish("sub/a.bin", 1_600_000_000).unwrap();
        assert_eq!(fs::read(root.join("sub/a.bin")).unwrap(), b"hello");
        assert!(!root.join("sub/a.bin.part").exists());
        let mtime = fs::metadata(root.join("sub/a.bin"))
            .unwrap()
            .modified()
            .unwrap();
        assert_eq!(
            mtime.duration_since(UNIX_EPOCH).unwrap().as_secs(),
            1_600_000_000
        );
        assert_eq!(store.read("sub/a.bin", 1, 3).unwrap(), b"ell");
        // 再传一次同名文件：覆盖
        store.truncate("sub/a.bin", 0).unwrap();
        store.write("sub/a.bin", 0, b"new").unwrap();
        store.finish("sub/a.bin", 0).unwrap();
        assert_eq!(fs::read(root.join("sub/a.bin")).unwrap(), b"new");
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn errors_are_reported() {
        let root = temp_dir("errors");
        let mut store = FsStore::new(&root);
        assert!(matches!(
            store.read("missing", 0, 1),
            Err(Error::Storage(_))
        ));
        assert!(matches!(
            store.read_partial("missing", 0, 1),
            Err(Error::Storage(_))
        ));
        assert!(matches!(
            store.write("missing", 0, b"x"),
            Err(Error::Storage(_))
        ));
        assert!(matches!(store.finish("missing", 0), Err(Error::Storage(_))));
        for bad in ["../escape", "/abs", "a\\b"] {
            assert!(matches!(store.read(bad, 0, 1), Err(Error::UnsafePath(_))));
            assert!(matches!(store.partial_len(bad), Err(Error::UnsafePath(_))));
            assert!(matches!(
                store.read_partial(bad, 0, 1),
                Err(Error::UnsafePath(_))
            ));
            assert!(matches!(
                store.write(bad, 0, b""),
                Err(Error::UnsafePath(_))
            ));
            assert!(matches!(store.truncate(bad, 0), Err(Error::UnsafePath(_))));
            assert!(matches!(store.finish(bad, 0), Err(Error::UnsafePath(_))));
        }
        // .part 位置上是个目录
        fs::create_dir_all(root.join("d.part")).unwrap();
        assert!(matches!(store.partial_len("d"), Err(Error::Storage(_))));
        assert!(matches!(store.truncate("d", 0), Err(Error::Storage(_))));
        // 父目录位置上是个文件
        fs::write(root.join("file"), b"x").unwrap();
        assert!(matches!(
            store.truncate("file/child", 0),
            Err(Error::Storage(_))
        ));
        assert!(matches!(
            store.partial_len("file/child"),
            Err(Error::Storage(_))
        ));
        // 目标位置是个非空目录：改名失败
        fs::create_dir_all(root.join("taken/inner")).unwrap();
        store.truncate("taken", 0).unwrap();
        assert!(matches!(store.finish("taken", 0), Err(Error::Storage(_))));
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn scan_lists_files_recursively_in_order() {
        let root = temp_dir("scan");
        fs::create_dir_all(root.join("proj/src")).unwrap();
        fs::write(root.join("proj/b.txt"), b"bb").unwrap();
        fs::write(root.join("proj/a.txt"), b"a").unwrap();
        fs::write(root.join("proj/src/main.rs"), b"fn main() {}").unwrap();
        fs::write(root.join("single.bin"), b"12345").unwrap();

        let files = scan(&root, "proj").unwrap();
        let listed: Vec<(&str, u64)> = files.iter().map(|f| (f.path.as_str(), f.size)).collect();
        assert_eq!(
            listed,
            vec![
                ("proj/a.txt", 1),
                ("proj/b.txt", 2),
                ("proj/src/main.rs", 12)
            ]
        );
        assert!(files.iter().all(|f| f.mtime > 1_600_000_000));
        assert_eq!(scan(&root, "single.bin").unwrap().len(), 1);
        assert!(matches!(scan(&root, "missing"), Err(Error::Storage(_))));
        assert!(matches!(scan(&root, "../x"), Err(Error::UnsafePath(_))));

        // 名字不安全的文件让整个扫描失败，而不是被悄悄跳过。
        // （带冒号的文件名在 Windows 上建不出来，所以只在 Unix 上测。）
        #[cfg(unix)]
        {
            fs::write(root.join("proj/bad:name"), b"").unwrap();
            assert!(matches!(scan(&root, "proj"), Err(Error::UnsafePath(_))));
            fs::remove_file(root.join("proj/bad:name")).unwrap();
        }

        #[cfg(unix)]
        {
            // 符号链接不跟随
            std::os::unix::fs::symlink("/etc", root.join("proj/link")).unwrap();
            assert_eq!(scan(&root, "proj").unwrap().len(), 3);
        }
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn scan_refuses_too_many_files() {
        let root = temp_dir("many");
        fs::create_dir_all(root.join("d")).unwrap();
        for i in 0..=MAX_FILES {
            fs::write(root.join(format!("d/{i:05}")), b"").unwrap();
        }
        assert!(matches!(scan(&root, "d"), Err(Error::TooManyItems { .. })));
        fs::remove_dir_all(&root).unwrap();
    }
}
