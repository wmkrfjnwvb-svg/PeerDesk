//! Reed-Solomon 纠删码（GF(2^8) 上的系统 Cauchy 码）：k 个数据分片加 m 个校验分片，
//! 收到其中任意 k 个就能还原全部数据分片。
//!
//! 为什么要它：异或纠错每组只能补一个丢包，10% 以上丢包时一组里常丢两个以上，补不回来，
//! 只能等重传或重发关键帧（仿真里 10% 丢包冻结 9 次、关键帧 17 个，见 `docs/research/README.md` 3.7.5）。
//!
//! 做法（经典教科书构造，自己实现，不依赖外部库）：
//! - 域：GF(256)，本原多项式 x^8 + x^4 + x^3 + x^2 + 1（0x11d），对数表做乘除；
//! - 校验矩阵 C[j][i] = 1 / (x_j + y_i)，x_j = j（j < m），y_i = m + i（i < k）。所有 x、y 互不相同，
//!   所以 C 的任意方子阵都可逆（Cauchy 矩阵的性质），丢了任意 ≤ m 个数据分片都解得出来；
//! - 要求 k + m ≤ 256。
//!
//! 分片长度可以不同：编码时按最长的补零，解出来后由调用方截回原长。

use crate::error::Error;

/// GF(256) 的指数表和对数表。
struct Tables {
    exp: [u8; 512],
    log: [u8; 256],
}

impl Tables {
    fn exp(&self, i: usize) -> u8 {
        self.exp.get(i).copied().unwrap_or(0)
    }

    fn log(&self, a: u8) -> usize {
        usize::from(self.log.get(usize::from(a)).copied().unwrap_or(0))
    }
}

fn tables() -> &'static Tables {
    static T: std::sync::OnceLock<Tables> = std::sync::OnceLock::new();
    T.get_or_init(|| {
        let mut exp = [0u8; 512];
        let mut log = [0u8; 256];
        let mut x: u16 = 1;
        for i in 0..255usize {
            if let Some(e) = exp.get_mut(i) {
                *e = u8::try_from(x).unwrap_or(0);
            }
            if let Some(l) = log.get_mut(usize::from(x)) {
                *l = u8::try_from(i).unwrap_or(0);
            }
            x <<= 1;
            if x & 0x100 != 0 {
                x ^= 0x11d;
            }
        }
        // 复制一份，乘法时下标相加不用取模
        for i in 255..512usize {
            let v = exp.get(i - 255).copied().unwrap_or(0);
            if let Some(e) = exp.get_mut(i) {
                *e = v;
            }
        }
        Tables { exp, log }
    })
}

/// 乘法。
fn mul(a: u8, b: u8) -> u8 {
    if a == 0 || b == 0 {
        0
    } else {
        let t = tables();
        t.exp(t.log(a) + t.log(b))
    }
}

/// 求逆（a ≠ 0）。
fn inv(a: u8) -> u8 {
    if a == 0 {
        0
    } else {
        let t = tables();
        t.exp(255 - t.log(a))
    }
}

/// 校验矩阵的元素：第 j 个校验分片里第 i 个数据分片的系数。
fn coef(j: usize, i: usize, m: usize) -> u8 {
    let x = u8::try_from(j).unwrap_or(0);
    let y = u8::try_from(m + i).unwrap_or(0);
    inv(x ^ y)
}

/// `acc += c * src`（逐字节，GF(256)）。
fn mul_add(acc: &mut [u8], src: &[u8], c: u8) {
    if c == 0 {
        return;
    }
    let t = tables();
    let lc = t.log(c);
    for (a, &s) in acc.iter_mut().zip(src) {
        if s != 0 {
            *a ^= t.exp(lc + t.log(s));
        }
    }
}

/// 生成 `m` 个校验分片，每个长度等于最长的数据分片。
///
/// # Errors
/// 没有数据分片，或 k + m 超过 256。
pub fn encode(data: &[&[u8]], m: usize) -> Result<Vec<Vec<u8>>, Error> {
    let k = data.len();
    if k == 0 || k + m > 256 {
        return Err(Error::InvalidArgument(
            "RS：分片数不合法（要求 1 ≤ k，k + m ≤ 256）",
        ));
    }
    let len = data.iter().map(|d| d.len()).max().unwrap_or(0);
    Ok((0..m)
        .map(|j| {
            let mut p = vec![0u8; len];
            for (i, d) in data.iter().enumerate() {
                mul_add(&mut p, d, coef(j, i, m));
            }
            p
        })
        .collect())
}

/// 用收到的数据分片和校验分片还原缺失的数据分片（补零到同一长度，由调用方截回原长）。
///
/// `data[i]` 为 `None` 表示第 i 个数据分片丢了；`parity[j]` 同理。`m` 是这一帧校验分片的总数。
/// 返回还原出来的分片（序号, 内容）；丢的比收到的校验分片多时返回 `None`。
#[must_use]
pub fn reconstruct(
    data: &[Option<&[u8]>],
    parity: &[Option<&[u8]>],
    m: usize,
) -> Option<Vec<(usize, Vec<u8>)>> {
    let k = data.len();
    let missing: Vec<usize> = (0..k)
        .filter(|i| data.get(*i).is_some_and(Option::is_none))
        .collect();
    if missing.is_empty() {
        return Some(Vec::new());
    }
    let rows: Vec<(usize, &[u8])> = parity
        .iter()
        .enumerate()
        .filter_map(|(j, p)| p.map(|p| (j, p)))
        .take(missing.len())
        .collect();
    if rows.len() < missing.len() || k + m > 256 {
        return None;
    }
    let len = rows.iter().map(|(_, p)| p.len()).max().unwrap_or(0);
    // 方程：A · x = s。s_j = p_j − Σ_{已收到 i} C[j][i]·d_i（GF(256) 里减就是异或）。
    let n = missing.len();
    let mut a: Vec<Vec<u8>> = rows
        .iter()
        .map(|(j, _)| missing.iter().map(|&i| coef(*j, i, m)).collect())
        .collect();
    let mut s: Vec<Vec<u8>> = rows
        .iter()
        .map(|(j, p)| {
            let mut acc = p.to_vec();
            acc.resize(len, 0);
            for (i, d) in data.iter().enumerate() {
                if let Some(d) = d {
                    mul_add(&mut acc, d, coef(*j, i, m));
                }
            }
            acc
        })
        .collect();
    // 高斯消元（Cauchy 子阵一定可逆，主元总能找到）
    for col in 0..n {
        let pivot = (col..n).find(|&r| {
            a.get(r)
                .and_then(|row| row.get(col))
                .is_some_and(|v| *v != 0)
        })?;
        a.swap(col, pivot);
        s.swap(col, pivot);
        let scale = inv(a.get(col).and_then(|r| r.get(col)).copied().unwrap_or(0));
        if let (Some(row), Some(rhs)) = (a.get_mut(col), s.get_mut(col)) {
            for v in row.iter_mut() {
                *v = mul(*v, scale);
            }
            for v in rhs.iter_mut() {
                *v = mul(*v, scale);
            }
        }
        let pivot_row = a.get(col).cloned().unwrap_or_default();
        let pivot_rhs = s.get(col).cloned().unwrap_or_default();
        for r in 0..n {
            if r == col {
                continue;
            }
            let factor = a.get(r).and_then(|row| row.get(col)).copied().unwrap_or(0);
            if factor == 0 {
                continue;
            }
            if let (Some(row), Some(rhs)) = (a.get_mut(r), s.get_mut(r)) {
                for (v, p) in row.iter_mut().zip(&pivot_row) {
                    *v ^= mul(factor, *p);
                }
                mul_add(rhs, &pivot_rhs, factor);
            }
        }
    }
    Some(missing.into_iter().zip(s).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frames(k: usize, len: usize) -> Vec<Vec<u8>> {
        (0..k)
            .map(|i| {
                (0..len - i % 3)
                    .map(|b| u8::try_from((i * 31 + b * 7) % 251).unwrap())
                    .collect()
            })
            .collect()
    }

    #[test]
    fn field_arithmetic() {
        for a in 1..=255u8 {
            assert_eq!(mul(a, inv(a)), 1, "{a}");
            assert_eq!(mul(a, 1), a);
            assert_eq!(mul(a, 0), 0);
        }
        assert_eq!(inv(0), 0);
        // 交换律、结合律抽查
        assert_eq!(mul(7, 200), mul(200, 7));
        assert_eq!(mul(mul(3, 9), 77), mul(3, mul(9, 77)));
    }

    #[test]
    fn any_k_of_k_plus_m_rebuild_the_data() {
        let (k, m) = (10, 4);
        let data = frames(k, 50);
        let refs: Vec<&[u8]> = data.iter().map(Vec::as_slice).collect();
        let parity = encode(&refs, m).unwrap();
        assert_eq!(parity.len(), m);
        // 丢掉不同组合的数据分片（最多 m 个），用不同的校验分片还原
        for lost in [vec![0], vec![3, 7], vec![0, 1, 2, 9], vec![5, 6, 8]] {
            for parity_lost in [vec![], vec![0], vec![1, 3]] {
                if lost.len() + parity_lost.len() > m {
                    continue;
                }
                let have: Vec<Option<&[u8]>> = (0..k)
                    .map(|i| (!lost.contains(&i)).then(|| refs[i]))
                    .collect();
                let ps: Vec<Option<&[u8]>> = (0..m)
                    .map(|j| (!parity_lost.contains(&j)).then(|| parity[j].as_slice()))
                    .collect();
                let back = reconstruct(&have, &ps, m).unwrap();
                assert_eq!(back.len(), lost.len());
                for (i, bytes) in back {
                    assert_eq!(&bytes[..data[i].len()], data[i].as_slice(), "分片 {i}");
                    assert!(bytes[data[i].len()..].iter().all(|b| *b == 0));
                }
            }
        }
        // 丢得比校验分片多：还原不了
        let have: Vec<Option<&[u8]>> = (0..k).map(|i| (i >= 5).then(|| refs[i])).collect();
        let ps: Vec<Option<&[u8]>> = parity.iter().map(|p| Some(p.as_slice())).collect();
        assert!(reconstruct(&have, &ps, m).is_none());
        // 什么都没丢：不用还原
        let all: Vec<Option<&[u8]>> = refs.iter().map(|r| Some(*r)).collect();
        assert_eq!(reconstruct(&all, &[], m), Some(Vec::new()));
    }

    #[test]
    fn zero_coefficient_adds_nothing() {
        let mut acc = vec![5u8, 6];
        mul_add(&mut acc, &[7, 8], 0);
        assert_eq!(acc, [5, 6]);
    }

    #[test]
    fn limits() {
        assert!(encode(&[], 2).is_err());
        let one = [&[1u8, 2][..]];
        assert!(encode(&one, 256).is_err());
        assert_eq!(encode(&one, 0).unwrap().len(), 0);
        // 大帧：200 个数据分片、56 个校验分片
        let data = frames(200, 30);
        let refs: Vec<&[u8]> = data.iter().map(Vec::as_slice).collect();
        let parity = encode(&refs, 56).unwrap();
        let have: Vec<Option<&[u8]>> = (0..200).map(|i| (i % 4 != 0).then(|| refs[i])).collect();
        let ps: Vec<Option<&[u8]>> = parity.iter().map(|p| Some(p.as_slice())).collect();
        let back = reconstruct(&have, &ps, 56).unwrap();
        assert_eq!(back.len(), 50);
        for (i, bytes) in back {
            assert_eq!(&bytes[..data[i].len()], data[i].as_slice());
        }
    }
}
