//! 边界测试：长度、数量、数值刚好在上限和刚好越过上限时的行为。
#![allow(clippy::unwrap_used, clippy::indexing_slicing, clippy::panic)]

use peerdesk_core::crypto::Suite;
use peerdesk_core::proto::{
    decode_datagram, encode_datagram, Caps, Channel, Control, Header, HEADER_LEN, MAX_DATAGRAM,
    MAX_REGION_LEN, MAX_SUITES, VERSION,
};
use peerdesk_core::{Error, Time};

fn header() -> Header {
    Header {
        channel: Channel::Video,
        flags: 0xff,
        conn_id: u32::MAX,
        seq: u32::MAX,
        send_time: Time(u64::MAX),
    }
}

fn hello(region: String, suites: Vec<Suite>) -> Control {
    Control::Hello {
        version: VERSION,
        device_id: u64::MAX,
        caps: Caps(u32::MAX),
        region,
        suites,
    }
}

#[test]
fn datagram_exactly_at_limit_is_accepted() {
    let payload = vec![7u8; MAX_DATAGRAM - HEADER_LEN];
    let bytes = encode_datagram(&header(), &payload).unwrap();
    assert_eq!(bytes.len(), MAX_DATAGRAM);
    let (h, p) = decode_datagram(&bytes).unwrap();
    assert_eq!(h, header());
    assert_eq!(p, payload.as_slice());
}

#[test]
fn datagram_one_byte_over_limit_is_rejected_both_ways() {
    let payload = vec![7u8; MAX_DATAGRAM - HEADER_LEN + 1];
    assert_eq!(
        encode_datagram(&header(), &payload),
        Err(Error::PayloadTooLarge {
            len: MAX_DATAGRAM + 1,
            max: MAX_DATAGRAM
        })
    );
    assert_eq!(
        decode_datagram(&vec![0u8; MAX_DATAGRAM + 1]).map(|_| ()),
        Err(Error::PayloadTooLarge {
            len: MAX_DATAGRAM + 1,
            max: MAX_DATAGRAM
        })
    );
}

#[test]
fn empty_payload_datagram() {
    let bytes = encode_datagram(&header(), &[]).unwrap();
    assert_eq!(bytes.len(), HEADER_LEN);
    assert!(decode_datagram(&bytes).unwrap().1.is_empty());
}

#[test]
fn region_length_limits() {
    let ok = hello("r".repeat(MAX_REGION_LEN), vec![Suite::None]);
    assert_eq!(Control::decode(&ok.encode().unwrap()).unwrap(), ok);
    let empty = hello(String::new(), vec![]);
    assert_eq!(Control::decode(&empty.encode().unwrap()).unwrap(), empty);
    assert_eq!(
        hello("r".repeat(MAX_REGION_LEN + 1), vec![]).encode(),
        Err(Error::StringTooLong {
            len: MAX_REGION_LEN + 1,
            max: MAX_REGION_LEN
        })
    );
}

#[test]
fn suite_count_limits() {
    let ok = hello("x".into(), vec![Suite::Aes256Gcm; MAX_SUITES]);
    assert_eq!(Control::decode(&ok.encode().unwrap()).unwrap(), ok);
    assert_eq!(
        hello("x".into(), vec![Suite::Aes256Gcm; MAX_SUITES + 1]).encode(),
        Err(Error::TooManyItems {
            len: MAX_SUITES + 1,
            max: MAX_SUITES
        })
    );
    assert_eq!(
        hello("x".into(), vec![Suite::Aes256Gcm; 300]).encode(),
        Err(Error::TooManyItems {
            len: 300,
            max: MAX_SUITES
        })
    );
}

#[test]
fn extreme_numeric_values_survive_roundtrip() {
    for msg in [
        Control::Ping { nonce: 0 },
        Control::Ping { nonce: u64::MAX },
        Control::Accept {
            suite: Suite::None,
            session_id: 0,
            caps: Caps(0),
            name: String::new(),
        },
        Control::Accept {
            suite: Suite::ChaCha20Poly1305,
            session_id: u32::MAX,
            caps: Caps(u32::MAX),
            name: String::new(),
        },
    ] {
        assert_eq!(Control::decode(&msg.encode().unwrap()).unwrap(), msg);
    }
}
