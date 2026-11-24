//! H.264 码流两种封装之间的转换（与系统无关，有单元测试）。
//!
//! - **Annex B**：每个 NAL 前面是起始码 `00 00 01` 或 `00 00 00 01`。OpenH264、我们的线上格式用它。
//! - **AVCC**：每个 NAL 前面是 4 字节大端长度；SPS、PPS 不在码流里，放在格式描述里。
//!   macOS 的 VideoToolbox 用它。
//!
//! Mac 硬件编码器的输出要转成 Annex B（关键帧前补上 SPS、PPS，接收端的 OpenH264 才解得了）；
//! Mac 硬件解码器的输入要从 Annex B 转成 AVCC，并取出 SPS、PPS 建格式描述。

/// NAL 单元类型：SPS。
pub const NAL_SPS: u8 = 7;
/// NAL 单元类型：PPS。
pub const NAL_PPS: u8 = 8;
/// NAL 单元类型：访问单元分隔符。
pub const NAL_AUD: u8 = 9;
/// NAL 单元类型：IDR（关键帧）片。
pub const NAL_IDR: u8 = 5;

/// NAL 单元的类型（头一个字节的低 5 位）。
#[must_use]
pub fn nal_type(nal: &[u8]) -> u8 {
    nal.first().map_or(0, |b| b & 0x1f)
}

/// 把 Annex B 码流切成 NAL 单元（不含起始码）。
#[must_use]
pub fn nal_units(data: &[u8]) -> Vec<&[u8]> {
    // 找出每个起始码之后的位置
    let mut starts = Vec::new();
    let mut i = 0usize;
    while i + 3 <= data.len() {
        if data.get(i..i + 3) == Some(&[0, 0, 1]) {
            starts.push(i + 3);
            i += 3;
        } else {
            i += 1;
        }
    }
    let mut out = Vec::with_capacity(starts.len());
    for (n, &start) in starts.iter().enumerate() {
        // 到下一个起始码为止；四字节起始码多出来的那个 0 不属于前一个 NAL
        let mut end = starts.get(n + 1).map_or(data.len(), |next| next - 3);
        while end > start && data.get(end - 1) == Some(&0) && starts.get(n + 1).is_some() {
            end -= 1;
        }
        if let Some(nal) = data.get(start..end) {
            if !nal.is_empty() {
                out.push(nal);
            }
        }
    }
    out
}

/// Annex B → AVCC：去掉 SPS、PPS、分隔符，其余 NAL 加 4 字节长度。
/// 同时返回码流里的 SPS、PPS（关键帧才有）。
#[must_use]
pub fn to_avcc(data: &[u8]) -> (Vec<u8>, Option<Vec<u8>>, Option<Vec<u8>>) {
    let (mut out, mut sps, mut pps) = (Vec::with_capacity(data.len()), None, None);
    for nal in nal_units(data) {
        match nal_type(nal) {
            NAL_SPS => sps = Some(nal.to_vec()),
            NAL_PPS => pps = Some(nal.to_vec()),
            NAL_AUD => {}
            _ => {
                let len = u32::try_from(nal.len()).unwrap_or(u32::MAX);
                out.extend_from_slice(&len.to_be_bytes());
                out.extend_from_slice(nal);
            }
        }
    }
    (out, sps, pps)
}

/// AVCC → Annex B。`params` 是要放在前面的 SPS、PPS（关键帧时给）。长度字段不合法时返回 `None`。
#[must_use]
pub fn from_avcc(avcc: &[u8], params: &[&[u8]]) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(avcc.len() + 64);
    for p in params {
        out.extend_from_slice(&[0, 0, 0, 1]);
        out.extend_from_slice(p);
    }
    let mut rest = avcc;
    while !rest.is_empty() {
        let len_bytes: [u8; 4] = rest.get(..4)?.try_into().ok()?;
        let len = usize::try_from(u32::from_be_bytes(len_bytes)).ok()?;
        let nal = rest.get(4..4 + len)?;
        out.extend_from_slice(&[0, 0, 0, 1]);
        out.extend_from_slice(nal);
        rest = rest.get(4 + len..)?;
    }
    Some(out)
}

/// 码流里有没有 IDR（关键帧）片。
#[must_use]
pub fn has_idr(data: &[u8]) -> bool {
    nal_units(data).iter().any(|n| nal_type(n) == NAL_IDR)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SPS: [u8; 4] = [0x67, 0x42, 0x00, 0x1f];
    const PPS: [u8; 3] = [0x68, 0xce, 0x3c];
    const IDR: [u8; 5] = [0x65, 0x88, 0x84, 0x00, 0x10];
    const P: [u8; 3] = [0x41, 0x9a, 0x02];

    fn annexb(nals: &[&[u8]], long: bool) -> Vec<u8> {
        let mut out = Vec::new();
        for n in nals {
            out.extend_from_slice(if long {
                &[0, 0, 0, 1][..]
            } else {
                &[0, 0, 1][..]
            });
            out.extend_from_slice(n);
        }
        out
    }

    #[test]
    fn splits_both_start_code_lengths() {
        for long in [true, false] {
            let data = annexb(&[&SPS, &PPS, &IDR], long);
            assert_eq!(nal_units(&data), vec![&SPS[..], &PPS[..], &IDR[..]]);
        }
        assert!(nal_units(&[]).is_empty());
        assert!(nal_units(&[1, 2, 3]).is_empty());
        assert_eq!(nal_type(&[]), 0);
    }

    #[test]
    fn roundtrip_between_annexb_and_avcc() {
        let key = annexb(&[&[0x09, 0xf0], &SPS, &PPS, &IDR], true);
        assert!(has_idr(&key));
        let (avcc, sps, pps) = to_avcc(&key);
        assert_eq!(
            (sps.as_deref(), pps.as_deref()),
            (Some(&SPS[..]), Some(&PPS[..]))
        );
        assert_eq!(avcc, [&[0, 0, 0, 5][..], &IDR[..]].concat());
        // 转回来：关键帧前补上参数集
        let back = from_avcc(&avcc, &[&SPS, &PPS]).unwrap();
        assert_eq!(back, annexb(&[&SPS, &PPS, &IDR], true));
        // 普通帧
        let delta = annexb(&[&P], false);
        assert!(!has_idr(&delta));
        let (avcc, sps, _) = to_avcc(&delta);
        assert!(sps.is_none());
        assert_eq!(from_avcc(&avcc, &[]).unwrap(), annexb(&[&P], true));
        // 长度字段不合法
        assert!(from_avcc(&[0, 0, 0, 9, 1], &[]).is_none());
        assert!(from_avcc(&[0, 0], &[]).is_none());
    }
}
