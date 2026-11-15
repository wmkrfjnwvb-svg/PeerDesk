//! 非法输入测试：任何字节序列都只能得到 Ok 或 Err，绝不 panic。
#![allow(clippy::unwrap_used, clippy::indexing_slicing, clippy::panic)]

use peerdesk_core::cluster::ClusterMsg;
use peerdesk_core::crypto::Suite;
use peerdesk_core::proto::{
    decode_datagram, encode_datagram, Caps, Channel, Control, Header, VideoHeader, MAGIC, VERSION,
};
use peerdesk_core::sim::Rng;
use peerdesk_core::wire::{Reader, Writer};
use peerdesk_core::{Error, Time};

fn valid_datagram() -> Vec<u8> {
    let header = Header {
        channel: Channel::Control,
        flags: 0,
        conn_id: 1,
        seq: 2,
        send_time: Time(3),
    };
    encode_datagram(&header, &Control::Bye.encode().unwrap()).unwrap()
}

fn valid_hello() -> Vec<u8> {
    Control::Hello {
        version: VERSION,
        device_id: 1,
        caps: Caps::H264,
        region: "us".into(),
        suites: vec![Suite::Aes256Gcm, Suite::None],
    }
    .encode()
    .unwrap()
}

#[test]
fn header_field_errors() {
    let good = valid_datagram();
    assert_eq!(good[0], MAGIC);

    let mut bad = good.clone();
    bad[0] = 0;
    assert_eq!(decode_datagram(&bad).map(|_| ()), Err(Error::BadMagic(0)));

    let mut bad = good.clone();
    bad[1] = VERSION + 1;
    assert_eq!(
        decode_datagram(&bad).map(|_| ()),
        Err(Error::UnsupportedVersion(VERSION + 1))
    );

    let mut bad = good.clone();
    bad[2] = 200;
    assert_eq!(
        decode_datagram(&bad).map(|_| ()),
        Err(Error::UnknownChannel(200))
    );
}

#[test]
fn every_truncation_of_a_valid_datagram_fails_cleanly() {
    let good = valid_datagram();
    for len in 0..20 {
        assert!(
            matches!(decode_datagram(&good[..len]), Err(Error::Truncated { .. })),
            "len={len}"
        );
    }
}

#[test]
fn every_truncation_of_a_valid_hello_fails_cleanly() {
    let good = valid_hello();
    for len in 0..good.len() {
        assert!(Control::decode(&good[..len]).is_err(), "len={len}");
    }
    assert!(Control::decode(&good).is_ok());
}

#[test]
fn control_field_errors() {
    assert_eq!(Control::decode(&[0]), Err(Error::UnknownMessage(0)));
    assert_eq!(Control::decode(&[99]), Err(Error::UnknownMessage(99)));
    assert_eq!(Control::decode(&[3, 0]), Err(Error::UnknownReason(0)));
    assert_eq!(
        Control::decode(&[2, 9, 0, 0, 0, 1, 0, 0, 0, 0]),
        Err(Error::UnknownSuite(9))
    );
    assert_eq!(Control::decode(&[6, 0]), Err(Error::TrailingBytes(1)));

    // Hello 里的套件编号非法
    let mut bad = valid_hello();
    *bad.last_mut().unwrap() = 77;
    assert_eq!(Control::decode(&bad), Err(Error::UnknownSuite(77)));

    // Hello 声称的套件数超过上限
    let mut w = Writer::new();
    w.u8(1).u8(VERSION).u64(1).u32(0);
    w.string("us", 32).unwrap();
    w.u8(17);
    assert_eq!(
        Control::decode(&w.into_vec()),
        Err(Error::TooManyItems { len: 17, max: 16 })
    );

    // 区域名不是 UTF-8
    let mut w = Writer::new();
    w.u8(1)
        .u8(VERSION)
        .u64(1)
        .u32(0)
        .u8(2)
        .bytes(&[0xff, 0xff])
        .u8(0);
    assert_eq!(Control::decode(&w.into_vec()), Err(Error::BadUtf8));

    // 区域名长度前缀超过上限
    let mut w = Writer::new();
    w.u8(1)
        .u8(VERSION)
        .u64(1)
        .u32(0)
        .u8(33)
        .bytes(&[b'a'; 33])
        .u8(0);
    assert_eq!(
        Control::decode(&w.into_vec()),
        Err(Error::StringTooLong { len: 33, max: 32 })
    );
}

#[test]
fn video_header_errors() {
    let encode = |index: u16, count: u16| {
        let mut w = Writer::new();
        w.u32(1).u16(index).u16(count).u32(0);
        w.into_vec()
    };
    assert_eq!(
        VideoHeader::decode(&mut Reader::new(&encode(0, 0))),
        Err(Error::InvalidFragment { index: 0, count: 0 })
    );
    assert_eq!(
        VideoHeader::decode(&mut Reader::new(&encode(3, 3))),
        Err(Error::InvalidFragment { index: 3, count: 3 })
    );
    assert!(VideoHeader::decode(&mut Reader::new(&encode(u16::MAX - 1, u16::MAX))).is_ok());
    assert!(matches!(
        VideoHeader::decode(&mut Reader::new(&[0; 11])),
        Err(Error::Truncated { .. })
    ));
}

/// 用固定种子生成大量随机字节，喂给所有解码入口。
#[test]
fn random_bytes_never_panic() {
    let mut rng = Rng::new(0xbad_c0de);
    for _ in 0..20_000 {
        let len = usize::try_from(rng.below(64)).unwrap();
        let buf: Vec<u8> = (0..len)
            .map(|_| u8::try_from(rng.below(256)).unwrap())
            .collect();
        let _ = decode_datagram(&buf);
        let _ = Control::decode(&buf);
        let _ = ClusterMsg::decode(&buf);
        let _ = VideoHeader::decode(&mut Reader::new(&buf));
    }
}

/// 在合法消息上随机翻转一个比特：要么解出一条合法消息，要么报错。
#[test]
fn bit_flips_never_panic() {
    let mut rng = Rng::new(7);
    let samples = [valid_datagram(), valid_hello()];
    for sample in &samples {
        for _ in 0..5_000 {
            let mut buf = sample.clone();
            let byte = usize::try_from(rng.below(u64::try_from(buf.len()).unwrap())).unwrap();
            buf[byte] ^= 1 << rng.below(8);
            let _ = decode_datagram(&buf);
            let _ = Control::decode(&buf);
        }
    }
}
