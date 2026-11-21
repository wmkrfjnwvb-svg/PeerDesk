//! 线上格式快照：冻结当前版本发出的字节。改动导致老版本读不懂时这里失败。
//! 兼容规则见 `docs/compat.md`：只能在消息末尾追加字段、用能力位协商新功能，不能改已有字段。
#![allow(clippy::unwrap_used)]

use peerdesk_core::crypto::Suite;
use peerdesk_core::paths::{Candidate, PathKind};
use peerdesk_core::proto::{
    decode_datagram, encode_datagram, Caps, Channel, Control, Header, VERSION,
};
use peerdesk_core::signal::{Nat, PunchInfo, SignalMsg};
use peerdesk_core::Time;

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn hello() -> Control {
    Control::Hello {
        version: VERSION,
        device_id: 0x0102_0304_0506_0708,
        caps: Caps::H264,
        region: "us".into(),
        suites: vec![Suite::Aes256Gcm],
    }
}

fn register() -> SignalMsg {
    SignalMsg::Register {
        locals: vec![Candidate {
            addr: "192.168.1.2:4433".parse().unwrap(),
            kind: PathKind::Lan,
        }],
    }
}

fn connect_code() -> SignalMsg {
    SignalMsg::ConnectCode {
        code: 123_456_789,
        locals: vec![],
        account: String::new(),
    }
}

fn accept() -> Control {
    Control::Accept {
        suite: Suite::Aes256Gcm,
        session_id: 7,
        caps: Caps::H264,
    }
}

fn registered() -> SignalMsg {
    SignalMsg::Registered {
        observed: "203.0.113.5:40000".parse().unwrap(),
        code: 123_456_789,
        probe: None,
    }
}

fn peer() -> SignalMsg {
    SignalMsg::Peer {
        device: [3; 32],
        addr: "203.0.113.9:5000".parse().unwrap(),
        locals: vec![],
        you: "203.0.113.5:40000".parse().unwrap(),
        punch: PunchInfo {
            nonce: [4; 16],
            me: Nat::Easy,
            peer: Nat::Hard,
        },
        relay: None,
        note: "free".into(),
    }
}

fn header() -> Vec<u8> {
    encode_datagram(
        &Header {
            channel: Channel::Video,
            flags: 0,
            conn_id: 9,
            seq: 10,
            send_time: Time(11),
        },
        b"x",
    )
    .unwrap()
}

fn unhex(s: &str) -> Vec<u8> {
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(s.get(i..i + 2).unwrap(), 16).unwrap())
        .collect()
}

/// 冻结的字节（2026-11，协议版本 1）。**不要改这些常量**：改了就意味着老版本读不懂新版本。
const HELLO: &str = "01010102030405060708000000010275730102";
const ACCEPT: &str = "02020000000700000001";
const REGISTER: &str = "01010004c0a801021151";
const CONNECT_CODE: &str = "07075bcd15000000";
const REGISTERED: &str = "0204cb0071059c40075bcd1500";
const PEER: &str = "04030303030303030303030303030303030303030303030303030303030303030304cb00710913880004cb0071059c4004040404040404040404040404040404010200000466726565";
const HEADER: &str = "50010300000000090000000a000000000000000b78";

/// 新版本可能在末尾追加的扩展字段。
const EXTENSION: [u8; 3] = [0xee, 0x01, 0x02];

#[test]
fn session_handshake_bytes_are_frozen() {
    assert_eq!(hex(&hello().encode().unwrap()), HELLO);
    assert_eq!(hex(&accept().encode().unwrap()), ACCEPT);
    for (bytes, msg) in [(HELLO, hello()), (ACCEPT, accept())] {
        let mut b = unhex(bytes);
        assert_eq!(Control::decode(&b).unwrap(), msg);
        // 新版本追加的扩展字段被忽略
        b.extend_from_slice(&EXTENSION);
        assert_eq!(Control::decode(&b).unwrap(), msg);
    }
}

#[test]
fn signal_bytes_are_frozen() {
    let cases = [
        (REGISTER, register()),
        (CONNECT_CODE, connect_code()),
        (REGISTERED, registered()),
        (PEER, peer()),
    ];
    for (bytes, msg) in cases {
        assert_eq!(hex(&msg.encode()), bytes, "{msg:?}");
        let mut b = unhex(bytes);
        assert_eq!(SignalMsg::decode(&b).unwrap(), msg);
        b.extend_from_slice(&EXTENSION);
        assert_eq!(SignalMsg::decode(&b).unwrap(), msg, "追加字段后仍能读");
    }
}

#[test]
fn datagram_header_is_frozen() {
    assert_eq!(hex(&header()), HEADER);
    let b = unhex(HEADER);
    let (h, payload) = decode_datagram(&b).unwrap();
    assert_eq!((h.channel, h.seq, payload), (Channel::Video, 10, &b"x"[..]));
}

#[test]
fn feature_bits_are_stable_and_negotiated_away_for_old_peers() {
    // 能力位的位置一旦发布就不能变
    assert_eq!(Caps::H264.0, 1);
    assert_eq!(Caps::CLIPBOARD.0, 1 << 16);
    assert_eq!(Caps::AUDIO.0, 1 << 17);
    // 老版本只发 H264：交集里没有新功能
    let old = Caps::H264;
    assert_eq!(Caps::ALL.common(old), Caps::H264);
    assert!(!Caps::ALL.common(old).has(Caps::CLIPBOARD));
}
