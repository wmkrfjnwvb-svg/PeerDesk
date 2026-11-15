//! 文件传输测试：完整流程、断点续传、损坏、非法与不安全输入。
#![allow(clippy::unwrap_used, clippy::indexing_slicing, clippy::panic)]

use peerdesk_core::file::{
    check_path, Decision, Deframer, FileMeta, FileMsg, FileReceiver, FileSender, MemStore, Outcome,
    Store, MAX_CHUNK, MAX_FILES, MAX_FRAME,
};
use peerdesk_core::sim::Rng;
use peerdesk_core::Error;

fn content(seed: u64, len: usize) -> Vec<u8> {
    let mut rng = Rng::new(seed);
    (0..len)
        .map(|_| u8::try_from(rng.below(256)).unwrap())
        .collect()
}

fn meta(path: &str, data: &[u8]) -> FileMeta {
    FileMeta {
        path: path.into(),
        size: data.len() as u64,
        mtime: 1_700_000_000,
    }
}

/// 把发送方和接收方接起来跑到结束。消息经过编码、按随机长度切碎、再重组，模拟真实的流。
fn pump(
    sender: &mut FileSender,
    src: &mut MemStore,
    receiver: &mut FileReceiver,
    dst: &mut MemStore,
) {
    let mut rng = Rng::new(42);
    let mut to_receiver = Deframer::new();
    let mut to_sender = Deframer::new();
    let mut deliver = |msg: FileMsg,
                       sender: &mut FileSender,
                       src: &mut MemStore,
                       receiver: &mut FileReceiver,
                       dst: &mut MemStore| {
        let bytes = msg.frame().unwrap();
        let mut rest = bytes.as_slice();
        while !rest.is_empty() {
            let n = usize::try_from(rng.below(9000))
                .unwrap()
                .clamp(1, rest.len());
            let (head, tail) = rest.split_at(n);
            rest = tail;
            for m in to_receiver.push(head).unwrap() {
                for reply in receiver.on_msg(m, dst).unwrap() {
                    for r in to_sender.push(&reply.frame().unwrap()).unwrap() {
                        sender.on_msg(&r, src).unwrap();
                    }
                }
            }
        }
    };
    deliver(sender.offer().unwrap(), sender, src, receiver, dst);
    while let Some(msg) = sender.poll(src).unwrap() {
        deliver(msg, sender, src, receiver, dst);
    }
}

#[test]
fn several_files_including_empty_and_multi_chunk() {
    let files = [
        ("docs/readme.txt", content(1, 10)),
        ("empty.bin", Vec::new()),
        ("big/video.bin", content(2, MAX_CHUNK * 3 + 123)),
        ("exact.bin", content(3, MAX_CHUNK)),
    ];
    let mut src = MemStore::default();
    for (path, data) in &files {
        src.sources.insert((*path).into(), data.clone());
    }
    let metas = files.iter().map(|(p, d)| meta(p, d)).collect();
    let mut sender = FileSender::new(metas).unwrap();
    let mut receiver = FileReceiver::new();
    let mut dst = MemStore::default();
    assert!(!receiver.is_finished());
    pump(&mut sender, &mut src, &mut receiver, &mut dst);

    assert!(sender.is_finished() && receiver.is_finished());
    assert!(!sender.is_aborted());
    assert_eq!(sender.outcomes(), &[Some(Outcome::Done); 4]);
    assert_eq!(receiver.outcomes(), &[Some(Outcome::Done); 4]);
    for (path, data) in &files {
        assert_eq!(dst.finished[*path], (data.clone(), 1_700_000_000), "{path}");
    }
    assert!(dst.partials.is_empty());
    let total: usize = files.iter().map(|(_, d)| d.len()).sum();
    assert_eq!(sender.bytes_sent(), total as u64);
    assert_eq!(receiver.bytes_received(), total as u64);
    assert_eq!(receiver.aborted(), None);
    // 传完后再 poll 没有更多消息
    assert_eq!(sender.poll(&mut src).unwrap(), None);
}

#[test]
fn resume_sends_only_the_missing_tail() {
    let data = content(7, MAX_CHUNK * 4);
    let mut src = MemStore::default();
    src.sources.insert("a.bin".into(), data.clone());
    let mut dst = MemStore::default();
    // 上次传到一半断了
    let have = MAX_CHUNK * 2 + 1000;
    dst.partials.insert("a.bin".into(), data[..have].to_vec());

    let mut sender = FileSender::new(vec![meta("a.bin", &data)]).unwrap();
    let mut receiver = FileReceiver::new();
    pump(&mut sender, &mut src, &mut receiver, &mut dst);
    assert_eq!(sender.outcomes(), &[Some(Outcome::Done)]);
    assert_eq!(dst.finished["a.bin"].0, data);
    assert_eq!(sender.bytes_sent(), (data.len() - have) as u64);
}

#[test]
fn resume_restarts_when_existing_part_differs() {
    let data = content(8, MAX_CHUNK * 2);
    let mut src = MemStore::default();
    src.sources.insert("a.bin".into(), data.clone());
    for stale in [
        content(99, MAX_CHUNK),         // 内容不同
        content(99, MAX_CHUNK * 2 + 5), // 比源文件还长
    ] {
        let mut dst = MemStore::default();
        dst.partials.insert("a.bin".into(), stale);
        let mut sender = FileSender::new(vec![meta("a.bin", &data)]).unwrap();
        let mut receiver = FileReceiver::new();
        pump(&mut sender, &mut src, &mut receiver, &mut dst);
        assert_eq!(dst.finished["a.bin"].0, data);
        assert_eq!(sender.bytes_sent(), data.len() as u64, "从头传");
    }
}

#[test]
fn complete_partial_is_verified_without_resending() {
    let data = content(9, 5000);
    let mut src = MemStore::default();
    src.sources.insert("a.bin".into(), data.clone());
    let mut dst = MemStore::default();
    dst.partials.insert("a.bin".into(), data.clone());
    let mut sender = FileSender::new(vec![meta("a.bin", &data)]).unwrap();
    let mut receiver = FileReceiver::new();
    pump(&mut sender, &mut src, &mut receiver, &mut dst);
    assert_eq!(sender.bytes_sent(), 0);
    assert_eq!(dst.finished["a.bin"].0, data);
}

fn start_transfer(data: &[u8]) -> (FileSender, MemStore, FileReceiver, MemStore) {
    let mut src = MemStore::default();
    src.sources.insert("a.bin".into(), data.to_vec());
    let mut sender = FileSender::new(vec![meta("a.bin", data)]).unwrap();
    let mut receiver = FileReceiver::new();
    let mut dst = MemStore::default();
    let offer = sender.offer().unwrap();
    for reply in receiver.on_msg(offer, &mut dst).unwrap() {
        sender.on_msg(&reply, &mut src).unwrap();
    }
    (sender, src, receiver, dst)
}

#[test]
fn corrupted_content_fails_verification_and_leaves_no_partial() {
    let data = content(10, 3000);
    let (mut sender, mut src, mut receiver, mut dst) = start_transfer(&data);
    let mut results = Vec::new();
    while let Some(mut msg) = sender.poll(&mut src).unwrap() {
        if let FileMsg::Data { data, .. } = &mut msg {
            data[0] ^= 1; // 途中被改
        }
        results.extend(receiver.on_msg(msg, &mut dst).unwrap());
    }
    assert_eq!(
        results,
        vec![FileMsg::Result {
            index: 0,
            ok: false
        }]
    );
    sender.on_msg(&results[0], &mut src).unwrap();
    assert_eq!(sender.outcomes(), &[Some(Outcome::Failed)]);
    assert_eq!(receiver.outcomes(), &[Some(Outcome::Failed)]);
    assert!(dst.finished.is_empty());
    assert_eq!(dst.partials["a.bin"].len(), 0);
}

#[test]
fn short_file_fails_verification() {
    let data = content(11, 3000);
    let (mut sender, mut src, mut receiver, mut dst) = start_transfer(&data);
    let start = sender.poll(&mut src).unwrap().unwrap();
    receiver.on_msg(start, &mut dst).unwrap();
    // 不发数据直接结束
    let reply = receiver
        .on_msg(
            FileMsg::End {
                index: 0,
                hash: [0; 32],
            },
            &mut dst,
        )
        .unwrap();
    assert_eq!(
        reply,
        vec![FileMsg::Result {
            index: 0,
            ok: false
        }]
    );
}

#[test]
fn source_modified_during_transfer_aborts_sender() {
    let data = content(12, MAX_CHUNK * 2);
    let (mut sender, mut src, _, _) = start_transfer(&data);
    sender.poll(&mut src).unwrap(); // Start
    sender.poll(&mut src).unwrap(); // 第一块
    src.sources
        .get_mut("a.bin")
        .unwrap()
        .truncate(MAX_CHUNK + 10);
    assert_eq!(
        sender.poll(&mut src),
        Err(Error::Inconsistent("源文件在传输过程中被修改"))
    );
    assert!(sender.is_aborted());
    assert_eq!(sender.poll(&mut src).unwrap(), None);
    // 源文件消失
    let (mut sender, mut src, _, _) = start_transfer(&data);
    sender.poll(&mut src).unwrap();
    src.sources.clear();
    assert!(matches!(sender.poll(&mut src), Err(Error::Storage(_))));
}

#[test]
fn skipped_files_are_not_sent() {
    let a = content(13, 100);
    let b = content(14, 100);
    let mut src = MemStore::default();
    src.sources.insert("a".into(), a.clone());
    src.sources.insert("b".into(), b.clone());
    let mut sender = FileSender::new(vec![meta("a", &a), meta("b", &b)]).unwrap();
    sender.offer().unwrap();
    let empty_hash = *blake3_empty();
    let answer = FileMsg::Answer {
        decisions: vec![
            Decision::Skip,
            Decision::Accept {
                offset: 0,
                prefix_hash: empty_hash,
            },
        ],
    };
    sender.on_msg(&answer, &mut src).unwrap();
    assert_eq!(sender.outcomes(), &[Some(Outcome::Skipped), None]);
    assert_eq!(
        sender.poll(&mut src).unwrap(),
        Some(FileMsg::Start {
            index: 1,
            offset: 0
        })
    );
}

fn blake3_empty() -> &'static [u8; 32] {
    // 空输入的 BLAKE3 哈希（公开的测试向量）。
    &[
        0xaf, 0x13, 0x49, 0xb9, 0xf5, 0xf9, 0xa1, 0xa6, 0xa0, 0x40, 0x4d, 0xea, 0x36, 0xdc, 0xc9,
        0x49, 0x9b, 0xcb, 0x25, 0xc9, 0xad, 0xc1, 0x12, 0xb7, 0xcc, 0x9a, 0x93, 0xca, 0xe4, 0x1f,
        0x32, 0x62,
    ]
}

#[test]
fn receiver_answers_with_the_standard_empty_hash() {
    let mut receiver = FileReceiver::new();
    let mut dst = MemStore::default();
    let reply = receiver
        .on_msg(
            FileMsg::Offer {
                files: vec![meta("a", b"xyz")],
            },
            &mut dst,
        )
        .unwrap();
    assert_eq!(
        reply,
        vec![FileMsg::Answer {
            decisions: vec![Decision::Accept {
                offset: 0,
                prefix_hash: *blake3_empty()
            }]
        }]
    );
}

#[test]
fn unsafe_paths_are_rejected_by_both_sides() {
    let bad = [
        "",
        "/etc/passwd",
        "../up",
        "a/../b",
        "a/./b",
        "a//b",
        "a/",
        "C:\\x",
        "a\\b",
        "c:evil",
        "a\0b",
        "a\nb",
        "trailing.",
        "trailing ",
        "dir./x",
        "CON",
        "con.txt",
        "sub/NUL",
        "COM1",
        "lpt9.log",
        "aux",
    ];
    for path in bad {
        assert!(
            matches!(check_path(path), Err(Error::UnsafePath(_))),
            "{path:?}"
        );
        assert!(FileSender::new(vec![meta(path, b"")]).is_err(), "{path:?}");
        let mut receiver = FileReceiver::new();
        let offer = FileMsg::Offer {
            files: vec![meta(path, b"")],
        };
        assert!(
            receiver.on_msg(offer, &mut MemStore::default()).is_err(),
            "{path:?}"
        );
    }
    assert!(check_path(&"x".repeat(1025)).is_err());
    for ok in [
        "a",
        "a/b/c.txt",
        "中文/文件 名.txt",
        ".hidden",
        "COM0",
        "COM10",
        "console",
        "a.b.c",
        "null",
    ] {
        check_path(ok).unwrap_or_else(|e| panic!("{ok}: {e}"));
    }
}

#[test]
fn receiver_rejects_out_of_order_and_inconsistent_messages() {
    let data = content(15, 100);
    let fresh = || {
        let mut r = FileReceiver::new();
        let mut d = MemStore::default();
        r.on_msg(
            FileMsg::Offer {
                files: vec![meta("a", &data), meta("b", &data)],
            },
            &mut d,
        )
        .unwrap();
        (r, d)
    };
    let err = |r: Result<Vec<FileMsg>, Error>| assert!(r.is_err(), "{r:?}");

    let (mut r, mut d) = fresh();
    err(r.on_msg(
        FileMsg::Offer {
            files: vec![meta("c", &data)],
        },
        &mut d,
    )); // 重复 Offer
    err(r.on_msg(
        FileMsg::Data {
            index: 0,
            data: vec![1],
        },
        &mut d,
    )); // 还没 Start
    err(r.on_msg(
        FileMsg::End {
            index: 0,
            hash: [0; 32],
        },
        &mut d,
    ));
    err(r.on_msg(
        FileMsg::Start {
            index: 9,
            offset: 0,
        },
        &mut d,
    )); // 序号越界
    err(r.on_msg(
        FileMsg::Start {
            index: 0,
            offset: 5,
        },
        &mut d,
    )); // 偏移与答复不符
    err(r.on_msg(FileMsg::Answer { decisions: vec![] }, &mut d)); // 方向不对
    err(r.on_msg(FileMsg::Result { index: 0, ok: true }, &mut d));

    let (mut r, mut d) = fresh();
    r.on_msg(
        FileMsg::Start {
            index: 0,
            offset: 0,
        },
        &mut d,
    )
    .unwrap();
    err(r.on_msg(
        FileMsg::Start {
            index: 1,
            offset: 0,
        },
        &mut d,
    )); // 上一个没结束
    err(r.on_msg(
        FileMsg::Data {
            index: 1,
            data: vec![1],
        },
        &mut d,
    )); // 不是当前文件
    err(r.on_msg(
        FileMsg::Data {
            index: 0,
            data: vec![0; 101],
        },
        &mut d,
    )); // 超过声明大小
    r.on_msg(
        FileMsg::End {
            index: 0,
            hash: [0; 32],
        },
        &mut d,
    )
    .unwrap();
    err(r.on_msg(
        FileMsg::Start {
            index: 0,
            offset: 0,
        },
        &mut d,
    )); // 同一个文件不能开始两次

    let mut r = FileReceiver::new();
    err(r.on_msg(FileMsg::Offer { files: vec![] }, &mut MemStore::default())); // 空 Offer

    // 中止之后一切消息都被拒绝
    let (mut r, mut d) = fresh();
    assert!(r
        .on_msg(
            FileMsg::Abort {
                reason: "用户取消".into()
            },
            &mut d
        )
        .unwrap()
        .is_empty());
    assert_eq!(r.aborted(), Some("用户取消"));
    err(r.on_msg(
        FileMsg::Start {
            index: 0,
            offset: 0,
        },
        &mut d,
    ));
}

#[test]
fn sender_rejects_out_of_order_and_inconsistent_messages() {
    let data = content(16, 100);
    let mut src = MemStore::default();
    src.sources.insert("a".into(), data.clone());
    let mut s = FileSender::new(vec![meta("a", &data)]).unwrap();
    let answer = FileMsg::Answer {
        decisions: vec![Decision::Accept {
            offset: 0,
            prefix_hash: [0; 32],
        }],
    };
    // 还没 Offer
    assert!(s.on_msg(&answer, &mut src).is_err());
    assert_eq!(s.poll(&mut src).unwrap(), None);
    s.offer().unwrap();
    assert!(s.offer().is_err(), "Offer 只能发一次");
    assert!(s
        .on_msg(&FileMsg::Result { index: 0, ok: true }, &mut src)
        .is_err());
    assert!(
        s.on_msg(&FileMsg::Answer { decisions: vec![] }, &mut src)
            .is_err(),
        "数量不一致"
    );
    s.on_msg(&answer, &mut src).unwrap();
    assert!(s.on_msg(&answer, &mut src).is_err(), "重复的 Answer");
    assert!(
        s.on_msg(&FileMsg::Result { index: 5, ok: true }, &mut src)
            .is_err(),
        "序号越界"
    );
    for wrong in [
        FileMsg::Offer { files: vec![] },
        FileMsg::Start {
            index: 0,
            offset: 0,
        },
        FileMsg::Data {
            index: 0,
            data: vec![],
        },
        FileMsg::End {
            index: 0,
            hash: [0; 32],
        },
    ] {
        assert!(s.on_msg(&wrong, &mut src).is_err(), "{}", wrong.name());
    }
    s.on_msg(&FileMsg::Abort { reason: "x".into() }, &mut src)
        .unwrap();
    assert!(s.is_aborted());
    assert!(s.offer().is_err());
    // 文件数超过上限
    let many = (0..=MAX_FILES)
        .map(|i| meta(&format!("f{i}"), b""))
        .collect();
    assert!(matches!(
        FileSender::new(many),
        Err(Error::TooManyItems { .. })
    ));
}

#[test]
fn message_encoding_limits_and_errors() {
    let names: Vec<&str> = [
        FileMsg::Offer { files: vec![] },
        FileMsg::Answer {
            decisions: vec![Decision::Skip],
        },
        FileMsg::Start {
            index: 1,
            offset: 2,
        },
        FileMsg::Data {
            index: 1,
            data: vec![1, 2, 3],
        },
        FileMsg::Data {
            index: 1,
            data: vec![],
        },
        FileMsg::End {
            index: 1,
            hash: [7; 32],
        },
        FileMsg::Result { index: 1, ok: true },
        FileMsg::Result {
            index: 1,
            ok: false,
        },
        FileMsg::Abort {
            reason: "磁盘已满".into(),
        },
    ]
    .into_iter()
    .map(|msg| {
        assert_eq!(FileMsg::decode(&msg.encode().unwrap()).unwrap(), msg);
        msg.name()
    })
    .collect();
    assert_eq!(
        names,
        ["Offer", "Answer", "Start", "Data", "Data", "End", "Result", "Result", "Abort"]
    );

    // 上限
    let full = FileMsg::Data {
        index: 0,
        data: vec![0; MAX_CHUNK],
    };
    assert_eq!(FileMsg::decode(&full.encode().unwrap()).unwrap(), full);
    assert!(matches!(
        FileMsg::Data {
            index: 0,
            data: vec![0; MAX_CHUNK + 1]
        }
        .encode(),
        Err(Error::PayloadTooLarge { .. })
    ));
    let mut oversized = vec![4, 0, 0, 0, 0];
    oversized.extend(vec![0; MAX_CHUNK + 1]);
    assert!(matches!(
        FileMsg::decode(&oversized),
        Err(Error::PayloadTooLarge { .. })
    ));
    let too_many = FileMsg::Answer {
        decisions: vec![Decision::Skip; MAX_FILES + 1],
    };
    assert!(matches!(too_many.encode(), Err(Error::TooManyItems { .. })));
    assert!(matches!(
        FileMsg::decode(&[1, 0xff, 0xff]),
        Err(Error::TooManyItems { .. })
    ));
    assert!(matches!(
        FileMsg::Abort {
            reason: "x".repeat(201)
        }
        .encode(),
        Err(Error::StringTooLong { .. })
    ));
    assert!(matches!(
        FileMsg::Offer {
            files: vec![meta(&"p".repeat(1025), b"")]
        }
        .encode(),
        Err(Error::StringTooLong { .. })
    ));

    // 非法字节
    assert_eq!(FileMsg::decode(&[0]), Err(Error::UnknownMessage(0)));
    assert_eq!(
        FileMsg::decode(&[2, 0, 1, 9]),
        Err(Error::UnknownMessage(9))
    );
    assert_eq!(
        FileMsg::decode(&[6, 0, 0, 0, 0, 2]),
        Err(Error::Inconsistent("结果标志只能是 0 或 1"))
    );
    assert_eq!(
        FileMsg::decode(&[3, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 9]),
        Err(Error::TrailingBytes(1))
    );
    let mut rng = Rng::new(5);
    for _ in 0..20_000 {
        let len = usize::try_from(rng.below(80)).unwrap();
        let buf: Vec<u8> = (0..len)
            .map(|_| u8::try_from(rng.below(256)).unwrap())
            .collect();
        let _ = FileMsg::decode(&buf);
        let _ = Deframer::new().push(&buf);
    }
}

#[test]
fn deframer_handles_split_and_oversized_frames() {
    let a = FileMsg::Start {
        index: 1,
        offset: 2,
    }
    .frame()
    .unwrap();
    let b = FileMsg::Result { index: 1, ok: true }.frame().unwrap();
    let mut joined = a.clone();
    joined.extend(&b);
    // 一次给全
    assert_eq!(Deframer::new().push(&joined).unwrap().len(), 2);
    // 一个字节一个字节地给
    let mut d = Deframer::new();
    let mut got = Vec::new();
    for byte in &joined {
        got.extend(d.push(&[*byte]).unwrap());
    }
    assert_eq!(got.len(), 2);
    // 声称的长度超过上限：立刻拒绝，不会去等那么多数据
    let huge = u32::try_from(MAX_FRAME + 1).unwrap().to_be_bytes();
    assert!(matches!(
        Deframer::new().push(&huge),
        Err(Error::PayloadTooLarge { .. })
    ));
    // 帧内容非法
    assert_eq!(
        Deframer::new().push(&[0, 0, 0, 1, 99]),
        Err(Error::UnknownMessage(99))
    );
}

#[test]
fn mem_store_edge_cases() {
    let mut s = MemStore::default();
    assert!(matches!(s.read("nope", 0, 1), Err(Error::Storage(_))));
    assert_eq!(s.partial_len("nope").unwrap(), 0);
    assert!(s.read_partial("nope", 0, 10).unwrap().is_empty());
    s.write("f", 0, b"abc").unwrap();
    assert!(
        matches!(s.write("f", 9, b"x"), Err(Error::Storage(_))),
        "不连续的写入"
    );
    assert_eq!(s.read_partial("f", 1, 100).unwrap(), b"bc");
    assert!(s.read_partial("f", 100, 1).unwrap().is_empty());
    s.truncate("f", 1).unwrap();
    assert_eq!(s.partial_len("f").unwrap(), 1);
    s.finish("f", 5).unwrap();
    assert_eq!(s.finished["f"], (b"a".to_vec(), 5));
    s.finish("never-written", 0).unwrap();
    assert!(s.finished["never-written"].0.is_empty());
}

#[test]
fn resume_restarts_when_source_is_shorter_than_declared() {
    // 清单里写的是 100 字节，但源文件实际只有 10 字节（发出清单后被截短）。
    // 对方声称已有 50 字节：发送方读不到那么多，不能续传，只能从头来，随后因大小不符而中止。
    let mut src = MemStore::default();
    src.sources.insert("a".into(), vec![1; 10]);
    let declared = FileMeta {
        path: "a".into(),
        size: 100,
        mtime: 0,
    };
    let mut sender = FileSender::new(vec![declared]).unwrap();
    sender.offer().unwrap();
    let answer = FileMsg::Answer {
        decisions: vec![Decision::Accept {
            offset: 50,
            prefix_hash: [0; 32],
        }],
    };
    sender.on_msg(&answer, &mut src).unwrap();
    assert_eq!(
        sender.poll(&mut src).unwrap(),
        Some(FileMsg::Start {
            index: 0,
            offset: 0
        })
    );
    assert_eq!(
        sender.poll(&mut src),
        Err(Error::Inconsistent("源文件在传输过程中被修改"))
    );
}
