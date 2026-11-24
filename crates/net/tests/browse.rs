//! 文件管理：真实 QUIC 连接上浏览、新建、上传文件夹、下载回来、核对内容；不允许时拒绝。

#![allow(clippy::unwrap_used, clippy::indexing_slicing, clippy::panic)]

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use peerdesk_core::browse::{Reply, Request};
use peerdesk_core::crypto::Policy;
use peerdesk_core::file::Outcome;
use peerdesk_net::browse;
use peerdesk_net::files::Progress;
use peerdesk_net::stream::{read_kind, StreamKind};
use peerdesk_net::{Identity, Link, Node};

fn any() -> SocketAddr {
    ([127, 0, 0, 1], 0).into()
}

fn temp(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("peerdesk-browse-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// 一对连好的端点，被控端在后台回答所有浏览流（`allowed` 是被控端的权限开关）。
async fn connected(allowed: bool) -> (Link, Node, Node) {
    let policy = Policy::recommended();
    let (hid, cid) = (Identity::generate().unwrap(), Identity::generate().unwrap());
    let host = Node::bind(any(), &hid, &policy).unwrap();
    let ctrl = Node::bind(any(), &cid, &policy).unwrap();
    let addr = host.local_addr().unwrap();
    let pin = hid.fingerprint();
    let (c, h) = tokio::join!(ctrl.connect(addr, Some(&pin)), host.accept());
    let host_link = Arc::new(h.unwrap().unwrap());
    tokio::spawn(async move {
        while let Ok((send, mut recv)) = host_link.accept_stream().await {
            assert_eq!(read_kind(&mut recv).await.unwrap(), StreamKind::Browse);
            let _ = browse::serve(send, recv, allowed, &Progress::default()).await;
        }
    });
    (c.unwrap(), host, ctrl)
}

fn write(path: &Path, bytes: &[u8]) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, bytes).unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn browse_make_upload_and_download() {
    let (link, _h, _c) = connected(true).await;
    let remote = temp("remote");
    let local = temp("local");
    let back = temp("back");

    // 位置：至少有主目录（CI 上有 HOME）
    let Reply::Places(places) = browse::request(&link, &Request::Places).await.unwrap() else {
        panic!("应当是位置列表");
    };
    assert!(!places.is_empty());

    // 新建文件夹和文件；重名失败并说明
    let dir = remote.display().to_string();
    let mk = |name: &str, folder| {
        if folder {
            Request::MakeDir {
                dir: dir.clone(),
                name: name.into(),
            }
        } else {
            Request::MakeFile {
                dir: dir.clone(),
                name: name.into(),
            }
        }
    };
    assert_eq!(
        browse::request(&link, &mk("新建文件夹", true))
            .await
            .unwrap(),
        Reply::Done
    );
    assert_eq!(
        browse::request(&link, &mk("空.txt", false)).await.unwrap(),
        Reply::Done
    );
    let Reply::Failed(why) = browse::request(&link, &mk("空.txt", false)).await.unwrap() else {
        panic!("重名应当失败");
    };
    assert!(why.contains("已经存在"), "{why}");

    // 上传一个文件夹（含子文件夹）和一个大一点的文件
    write(&local.join("资料/a.txt"), b"hello");
    write(&local.join("资料/子/b.bin"), &vec![7u8; 300_000]);
    write(
        &local.join("big.bin"),
        &(0..3_000_000u32)
            .map(|i| (i % 251) as u8)
            .collect::<Vec<_>>(),
    );
    let progress = Progress::default();
    let names = vec!["资料".to_owned(), "big.bin".to_owned()];
    let results = browse::upload(&link, &local, &names, &dir, &progress)
        .await
        .unwrap();
    assert_eq!(results.len(), 3);
    assert!(
        results.iter().all(|r| r.outcome == Some(Outcome::Done)),
        "{results:?}"
    );
    let (done, total) = progress.get();
    assert_eq!((done, total), (3_300_005, 3_300_005));

    // 列目录：文件夹在前，大小对
    let Reply::List { entries, truncated } =
        browse::request(&link, &Request::List { path: dir.clone() })
            .await
            .unwrap()
    else {
        panic!("应当是列表");
    };
    assert!(!truncated);
    let names_seen: Vec<(&str, bool)> = entries.iter().map(|e| (e.name.as_str(), e.dir)).collect();
    assert!(names_seen[0].1, "{names_seen:?}");
    let big = entries.iter().find(|e| e.name == "big.bin").unwrap();
    assert_eq!(big.size, 3_000_000);
    assert!(big.modified > 0);

    // 下载回来，内容一样
    let progress = Progress::default();
    let results = browse::download(&link, &dir, &names, &back, &progress)
        .await
        .unwrap();
    assert!(
        results.iter().all(|r| r.outcome == Some(Outcome::Done)),
        "{results:?}"
    );
    assert_eq!(
        std::fs::read(back.join("资料/子/b.bin")).unwrap(),
        vec![7u8; 300_000]
    );
    assert_eq!(
        std::fs::read(back.join("big.bin")).unwrap(),
        std::fs::read(local.join("big.bin")).unwrap()
    );
    assert_eq!(progress.get().1, 3_300_005);

    // 再下载一次：已有的文件跳过，进度照样到 100%
    let progress = Progress::default();
    let _ = browse::download(&link, &dir, &names, &back, &progress)
        .await
        .unwrap();
    let (done, total) = progress.get();
    assert!(done <= total);

    // 出错的情况：不存在的目录、不存在的条目、上传到不存在的目录
    let missing = remote.join("没有这个").display().to_string();
    let Reply::Failed(why) = browse::request(
        &link,
        &Request::List {
            path: missing.clone(),
        },
    )
    .await
    .unwrap() else {
        panic!("应当失败");
    };
    assert!(why.contains("不存在"), "{why}");
    assert!(browse::download(
        &link,
        &dir,
        &["没有.txt".to_owned()],
        &back,
        &Progress::default()
    )
    .await
    .is_err());
    let err = browse::upload(
        &link,
        &local,
        &["big.bin".to_owned()],
        &missing,
        &Progress::default(),
    )
    .await
    .unwrap_err();
    assert!(err.to_string().contains("不存在"), "{err}");
}

#[tokio::test(flavor = "multi_thread")]
async fn refused_when_not_allowed() {
    let (link, _h, _c) = connected(false).await;
    let Reply::Failed(why) = browse::request(&link, &Request::Places).await.unwrap() else {
        panic!("应当拒绝");
    };
    assert!(why.contains("没有允许"), "{why}");
    let local = temp("refused");
    write(&local.join("x.txt"), b"x");
    let err = browse::upload(
        &link,
        &local,
        &["x.txt".to_owned()],
        "/tmp",
        &Progress::default(),
    )
    .await
    .unwrap_err();
    assert!(err.to_string().contains("没有允许"), "{err}");
}

#[test]
fn local_filesystem_helpers() {
    let dir = temp("local-helpers");
    let d = dir.display().to_string();
    assert_eq!(browse::make(&d, "f", true), Reply::Done);
    assert!(matches!(browse::make(&d, "f", true), Reply::Failed(_)));
    assert!(matches!(
        browse::make(&dir.join("nope").display().to_string(), "x", false),
        Reply::Failed(_)
    ));
    assert!(!browse::places().is_empty());
    assert!(browse::manifest(&dir, &["没有".to_owned()]).is_err());
}

#[tokio::test(flavor = "multi_thread")]
async fn oversized_request_is_refused() {
    use peerdesk_net::stream::write_kind;
    let (link, _h, _c) = connected(true).await;
    let (mut send, mut recv) = link.open_stream().await.unwrap();
    write_kind(&mut send, StreamKind::Browse).await.unwrap();
    // 声称有 4 GB：被控端不分配，直接关流
    send.write_all(&u32::MAX.to_be_bytes()).await.unwrap();
    let _ = send.finish();
    let mut buf = [0u8; 16];
    assert!(matches!(recv.read(&mut buf).await, Ok(None) | Err(_)));
    // 之后的正常请求不受影响
    assert!(matches!(
        browse::request(&link, &Request::Places).await.unwrap(),
        Reply::Places(_)
    ));
}
