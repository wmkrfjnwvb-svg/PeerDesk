//! 文件管理在网络上的两端：被控端回答浏览请求、按请求收发文件；控制端发请求、上传、下载。
//! 协议见 [`peerdesk_core::browse`]。本机一栏也用这里的 [`places`]、[`list`]、[`make`]。

use std::path::{Path, PathBuf};

use peerdesk_core::browse::{sort_entries, Entry, Place, Reply, Request, MAX_ENTRIES, MAX_MSG};
use peerdesk_core::file::FileMeta;
use quinn::{RecvStream, SendStream};

use crate::files::{
    finished, receive_checked, receive_on, send_checked, send_on, FileResult, Progress,
};
use crate::fs_store::scan;
use crate::stream::{write_kind, StreamKind};
use crate::{Link, NetError};

// ───────────────────────────── 本机文件系统 ─────────────────────────────

fn home() -> Option<PathBuf> {
    ["HOME", "USERPROFILE"]
        .iter()
        .filter_map(std::env::var_os)
        .find(|v| !v.is_empty())
        .map(PathBuf::from)
}

fn place(label: &str, path: &Path) -> Place {
    Place {
        label: label.to_owned(),
        path: path.display().to_string(),
    }
}

/// 本机的预置位置：主目录、桌面、下载、文档，以及各个磁盘（Windows 盘符、macOS 的卷、Linux 的挂载点）。
#[must_use]
pub fn places() -> Vec<Place> {
    let mut out = home_places(home().as_deref());
    out.extend(disks());
    out
}

/// 主目录和它下面的桌面、下载、文档（存在的才列）。
fn home_places(home: Option<&Path>) -> Vec<Place> {
    let Some(home) = home else {
        return Vec::new();
    };
    let mut out = vec![place("主目录", home)];
    out.extend(
        [
            ("桌面", "Desktop"),
            ("下载", "Downloads"),
            ("文档", "Documents"),
        ]
        .iter()
        .map(|(label, dir)| (label, home.join(dir)))
        .filter(|(_, p)| p.is_dir())
        .map(|(label, p)| place(label, &p)),
    );
    out
}

/// 磁盘：Windows 的盘符。
#[cfg(windows)]
fn disks() -> Vec<Place> {
    ('A'..='Z')
        .map(|letter| (letter, PathBuf::from(format!("{letter}:\\"))))
        .filter(|(_, root)| root.is_dir())
        .map(|(letter, root)| place(&format!("{letter}:"), &root))
        .collect()
}

/// 磁盘：根目录、macOS 的卷、Linux 的挂载点。
#[cfg(not(windows))]
fn disks() -> Vec<Place> {
    let mut out = vec![place("根目录 /", Path::new("/"))];
    for parent in ["/Volumes", "/media", "/mnt"] {
        out.extend(
            std::fs::read_dir(parent)
                .into_iter()
                .flatten()
                .flatten()
                .map(|entry| (entry.file_name(), entry.path()))
                .filter(|(_, path)| path.is_dir())
                .map(|(name, path)| place(&name.to_string_lossy(), &path)),
        );
    }
    out
}

fn reason(e: &std::io::Error) -> String {
    use std::io::ErrorKind::{AlreadyExists, NotFound, PermissionDenied};
    let kind = e.kind();
    if kind == NotFound {
        "不存在".into()
    } else if kind == PermissionDenied {
        "没有权限".into()
    } else if kind == AlreadyExists {
        "已经存在".into()
    } else {
        e.to_string()
    }
}

/// 列目录（文件夹在前）。超过 [`MAX_ENTRIES`] 截断。读不了的条目跳过。
#[must_use]
pub fn list(path: &str) -> Reply {
    list_at_most(path, MAX_ENTRIES)
}

/// 目录里的一项；读不了时为 `None`。跟随符号链接（链接到文件夹的显示成文件夹），断掉的链接按它本身算。
fn entry_of(item: &std::fs::DirEntry) -> Option<Entry> {
    let meta = std::fs::metadata(item.path())
        .or_else(|_| item.metadata())
        .ok()?;
    let modified = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_secs());
    Some(Entry {
        name: item.file_name().to_string_lossy().into_owned(),
        dir: meta.is_dir(),
        size: if meta.is_dir() { 0 } else { meta.len() },
        modified,
    })
}

fn list_at_most(path: &str, max: usize) -> Reply {
    let dir = match std::fs::read_dir(path) {
        Ok(dir) => dir,
        Err(e) => return Reply::Failed(format!("打不开 {path}：{}", reason(&e))),
    };
    let mut entries: Vec<Entry> = dir
        .flatten()
        .filter_map(|i| entry_of(&i))
        .take(max + 1)
        .collect();
    let truncated = entries.len() > max;
    entries.truncate(max);
    sort_entries(&mut entries);
    Reply::List { entries, truncated }
}

/// 新建文件夹或空文件（已存在时失败，不覆盖）。
#[must_use]
pub fn make(dir: &str, name: &str, folder: bool) -> Reply {
    let path = Path::new(dir).join(name);
    let made = if folder {
        std::fs::create_dir(&path)
    } else {
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .map(drop)
    };
    match made {
        Ok(()) => Reply::Done,
        Err(e) => Reply::Failed(format!("新建 {name} 失败：{}", reason(&e))),
    }
}

/// `dir` 下这些条目（文件或文件夹）的文件清单，路径相对 `dir`。
///
/// # Errors
/// 条目不存在、读不了。
pub fn manifest(dir: &Path, names: &[String]) -> Result<Vec<FileMeta>, NetError> {
    let mut files = Vec::new();
    for name in names {
        files.extend(scan(dir, name)?);
    }
    Ok(files)
}

// ───────────────────────────── 流上的消息 ─────────────────────────────

async fn write_msg(send: &mut SendStream, framed: &[u8]) -> Result<(), NetError> {
    send.write_all(framed)
        .await
        .map_err(|e| NetError::Closed(e.to_string()))
}

/// 读一条消息体：先 4 字节长度，再正好这么多字节（不多读：后面可能紧跟文件传输的消息）。
async fn read_body(recv: &mut RecvStream) -> Result<Vec<u8>, NetError> {
    let mut len = [0u8; 4];
    recv.read_exact(&mut len)
        .await
        .map_err(|e| NetError::Closed(e.to_string()))?;
    let len = usize::try_from(u32::from_be_bytes(len)).unwrap_or(usize::MAX);
    if len > MAX_MSG {
        return Err(NetError::Protocol("文件浏览消息太长"));
    }
    let mut body = vec![0u8; len];
    recv.read_exact(&mut body)
        .await
        .map_err(|e| NetError::Closed(e.to_string()))?;
    Ok(body)
}

// ───────────────────────────── 被控端 ─────────────────────────────

/// 回答一个浏览请求（流头已经读过）。`allowed` 为假时一律拒绝。返回传输的文件结果（浏览类请求为空）。
pub async fn serve(
    mut send: SendStream,
    mut recv: RecvStream,
    allowed: bool,
    progress: &Progress,
) -> Result<Vec<FileResult>, NetError> {
    let request = Request::decode(&read_body(&mut recv).await?)?;
    let reply = |r: Reply| r.frame().map_err(NetError::from);
    if !allowed {
        write_msg(
            &mut send,
            &reply(Reply::Failed("对方没有允许文件管理".into()))?,
        )
        .await?;
        let _ = send.finish();
        return Ok(Vec::new());
    }
    crate::note!(info, "文件管理：对方请求 {request:?}");
    let answer = match &request {
        Request::Places => Reply::Places(places()),
        Request::List { path } => list(path),
        Request::MakeDir { dir, name } => make(dir, name, true),
        Request::MakeFile { dir, name } => make(dir, name, false),
        Request::Put { dir } => {
            if Path::new(dir).is_dir() {
                write_msg(&mut send, &reply(Reply::Ready)?).await?;
                return receive_on(send, recv, Path::new(dir), progress).await;
            }
            Reply::Failed(format!("目标文件夹不存在：{dir}"))
        }
        Request::Get { dir, names } => match manifest(Path::new(dir), names) {
            Ok(files) => {
                write_msg(&mut send, &reply(Reply::Ready)?).await?;
                return send_on(send, recv, Path::new(dir), files, progress, Ok(()))
                    .await
                    .map(|(results, _)| results);
            }
            Err(e) => Reply::Failed(e.to_string()),
        },
    };
    write_msg(&mut send, &reply(answer)?).await?;
    let _ = send.finish();
    Ok(Vec::new())
}

// ───────────────────────────── 控制端 ─────────────────────────────

async fn open(link: &Link, request: &Request) -> Result<(SendStream, RecvStream, Reply), NetError> {
    let (mut send, mut recv) = link.open_stream().await?;
    write_kind(&mut send, StreamKind::Browse).await?;
    write_msg(&mut send, &request.frame()?).await?;
    let reply = Reply::decode(&read_body(&mut recv).await?)?;
    Ok((send, recv, reply))
}

/// 发一个浏览类请求（位置、列目录、新建），返回答复。
///
/// # Errors
/// 连接问题、对方回的消息不对。对方拒绝时返回 `Ok(Reply::Failed(..))`。
pub async fn request(link: &Link, request: &Request) -> Result<Reply, NetError> {
    let (_, _, reply) = open(link, request).await?;
    Ok(reply)
}

fn refused(reply: Reply) -> NetError {
    match reply {
        Reply::Failed(why) => NetError::Connect(why),
        Reply::Places(_) | Reply::List { .. } | Reply::Done | Reply::Ready => {
            NetError::Protocol("文件管理：对方的答复不对")
        }
    }
}

/// 把本机 `local_dir` 下的 `names`（文件或文件夹）上传到对方的 `remote_dir`。
///
/// # Errors
/// 本机文件读不了、对方拒绝、连接中断。
pub async fn upload(
    link: &Link,
    local_dir: &Path,
    names: &[String],
    remote_dir: &str,
    progress: &Progress,
) -> Result<Vec<FileResult>, NetError> {
    let files = manifest(local_dir, names)?;
    let request = Request::Put {
        dir: remote_dir.to_owned(),
    };
    let (send, recv, reply) = open(link, &request).await?;
    if reply != Reply::Ready {
        return Err(refused(reply));
    }
    let (results, _, aborted) =
        send_checked(send, recv, local_dir, files, progress, Ok(())).await?;
    finished(results, aborted)
}

/// 把对方 `remote_dir` 下的 `names` 下载到本机的 `local_dir`。
///
/// # Errors
/// 对方拒绝、本机写不了、连接中断。
pub async fn download(
    link: &Link,
    remote_dir: &str,
    names: &[String],
    local_dir: &Path,
    progress: &Progress,
) -> Result<Vec<FileResult>, NetError> {
    let request = Request::Get {
        dir: remote_dir.to_owned(),
        names: names.to_vec(),
    };
    let (send, recv, reply) = open(link, &request).await?;
    if reply != Reply::Ready {
        return Err(refused(reply));
    }
    let (results, aborted) = receive_checked(send, recv, local_dir, progress).await?;
    finished(results, aborted)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn reasons_and_odd_replies() {
        use std::io::{Error, ErrorKind};
        assert_eq!(
            reason(&Error::from(ErrorKind::PermissionDenied)),
            "没有权限"
        );
        assert!(!reason(&Error::other("磁盘坏了")).is_empty());
        assert!(matches!(refused(Reply::Done), NetError::Protocol(_)));
        assert!(matches!(
            refused(Reply::Failed("x".into())),
            NetError::Connect(_)
        ));
    }

    #[test]
    fn home_and_truncation() {
        let home = std::env::temp_dir().join(format!("peerdesk-home-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(home.join("Desktop")).unwrap();
        let found = home_places(Some(&home));
        assert_eq!(found.len(), 2, "主目录和桌面（下载、文档不存在）");
        assert!(home_places(None).is_empty());
        for name in ["a", "b", "c"] {
            std::fs::write(home.join(name), b"").unwrap();
        }
        let listed = list_at_most(&home.display().to_string(), 2);
        assert!(
            matches!(listed, Reply::List { ref entries, truncated: true } if entries.len() == 2)
        );
    }
}
