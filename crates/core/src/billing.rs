//! 中继计费（不做 IO）。
//!
//! - **账户**：计费挂在账户上，不挂在设备指纹上（设备指纹随时可以重新生成）。账户令牌由服务器
//!   用计费密钥签发：`pdk1-<账户号 16 位十六进制>-<每月中继额度 GB，0 表示不限>-<签名 24 位十六进制>`。
//!   集群里所有节点配置同一个计费密钥，任何节点都能验证令牌，不需要共享账户数据库。
//! - **谁付钱**：发起连接的一端（控制端）。被控端不需要账户。
//! - **账本**：每个中继会话每隔一段时间记一笔增量（崩溃最多丢最近一段），按月汇总；
//!   集群里每个节点把自己的本月汇总通过心跳告诉其他节点，额度按全集群的合计检查。

use std::collections::BTreeMap;

use hmac::{Hmac, Mac};
use sha2::Sha256;

use crate::Error;

/// 账户号。
pub type AccountId = [u8; 8];

/// 没有账户时（服务器不要求账户）中继流量记到这个公共账户上。
pub const PUBLIC_ACCOUNT: AccountId = [0; 8];

/// 1 GB（十进制，和云厂商的流量计价单位一致）。
pub const GB: u64 = 1_000_000_000;

/// 令牌前缀。
const PREFIX: &str = "pdk1";
/// 签名长度（字节）。
const SIG_LEN: usize = 12;

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn unhex<const N: usize>(text: &str) -> Option<[u8; N]> {
    if text.len() != N * 2 || !text.is_ascii() {
        return None;
    }
    let mut out = [0u8; N];
    for (i, byte) in out.iter_mut().enumerate() {
        *byte = u8::from_str_radix(text.get(i * 2..i * 2 + 2)?, 16).ok()?;
    }
    Some(out)
}

fn signature(secret: &[u8], body: &str) -> [u8; SIG_LEN] {
    let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(secret)
        .unwrap_or_else(|_| <Hmac<Sha256> as hmac::digest::KeyInit>::new(&Default::default()));
    mac.update(body.as_bytes());
    let full = mac.finalize().into_bytes();
    let mut sig = [0u8; SIG_LEN];
    for (dst, src) in sig.iter_mut().zip(full.iter()) {
        *dst = *src;
    }
    sig
}

/// 一个验证过的账户。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Account {
    /// 账户号。
    pub id: AccountId,
    /// 每月中继额度（字节）；0 表示不限。
    pub quota_bytes: u64,
}

impl Account {
    /// 账户号的十六进制写法。
    #[must_use]
    pub fn id_hex(&self) -> String {
        hex(&self.id)
    }
}

/// 用计费密钥签发一个账户令牌。`quota_gb` 是每月中继额度（GB），0 表示不限。
#[must_use]
pub fn issue(secret: &[u8], id: AccountId, quota_gb: u64) -> String {
    let body = format!("{PREFIX}-{}-{quota_gb}", hex(&id));
    format!("{body}-{}", hex(&signature(secret, &body)))
}

/// 验证账户令牌。
pub fn verify(secret: &[u8], token: &str) -> Result<Account, Error> {
    let bad = |why| Err(Error::Inconsistent(why));
    let token = token.trim();
    let Some((body, sig)) = token.rsplit_once('-') else {
        return bad("账户令牌格式不对");
    };
    let mut parts = body.split('-');
    let (Some(PREFIX), Some(id), Some(quota), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return bad("账户令牌格式不对");
    };
    let (Some(id), Ok(quota_gb), Some(sig)) =
        (unhex::<8>(id), quota.parse::<u64>(), unhex::<SIG_LEN>(sig))
    else {
        return bad("账户令牌格式不对");
    };
    // 逐字节比较全部签名，不在第一个不同处提前返回。
    let expected = signature(secret, body);
    let diff = expected
        .iter()
        .zip(sig.iter())
        .fold(0u8, |acc, (a, b)| acc | (a ^ b));
    if diff != 0 {
        return bad("账户令牌不是这个服务器签发的");
    }
    Ok(Account {
        id,
        quota_bytes: quota_gb.saturating_mul(GB),
    })
}

/// 公历月份（例如 202611），用 Unix 毫秒时间算（UTC）。
#[must_use]
pub fn month_of(unix_ms: u64) -> u32 {
    let days = unix_ms / 86_400_000;
    // Howard Hinnant 的 civil_from_days。
    let z = days + 719_468;
    let era = z / 146_097;
    let doe = z % 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + u64::from(m <= 2);
    u32::try_from(y * 100 + m).unwrap_or(u32::MAX)
}

/// 账本里的一行：某个中继会话在某个时刻新增的用量。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Entry {
    /// 记账时间（Unix 毫秒）。
    pub unix_ms: u64,
    /// 记账的节点。
    pub node: u64,
    /// 账户。
    pub account: AccountId,
    /// 中继会话号。
    pub session: [u8; 16],
    /// 新增字节数。
    pub bytes: u64,
    /// 新增包数。
    pub packets: u64,
    /// 会话是否已结束（这是它的最后一笔）。
    pub last: bool,
}

impl Entry {
    /// 写成一行文本（以空格分隔，没有换行）。
    #[must_use]
    pub fn to_line(&self) -> String {
        format!(
            "{} {} {} {} {} {} {}",
            self.unix_ms,
            self.node,
            hex(&self.account),
            hex(&self.session),
            self.bytes,
            self.packets,
            u8::from(self.last)
        )
    }

    /// 从一行文本读回。读不懂时返回 `None`（例如崩溃时写了一半的最后一行）。
    #[must_use]
    pub fn from_line(line: &str) -> Option<Self> {
        let mut p = line.split_whitespace();
        let entry = Self {
            unix_ms: p.next()?.parse().ok()?,
            node: p.next()?.parse().ok()?,
            account: unhex::<8>(p.next()?)?,
            session: unhex::<16>(p.next()?)?,
            bytes: p.next()?.parse().ok()?,
            packets: p.next()?.parse().ok()?,
            last: match p.next()? {
                "0" => false,
                "1" => true,
                _ => return None,
            },
        };
        p.next().is_none().then_some(entry)
    }
}

/// 某个账户某个月的用量。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Totals {
    /// 字节数。
    pub bytes: u64,
    /// 包数。
    pub packets: u64,
    /// 结束的会话数。
    pub sessions: u64,
}

/// 按月、按账户汇总的账本。本节点的数据来自自己的记账；其他节点的数据来自它们的心跳
/// （每个节点只报告自己的数，取最新的一份，合计时相加，不会重复计算）。
#[derive(Debug, Clone, Default)]
pub struct Ledger {
    node: u64,
    own: BTreeMap<(u32, AccountId), Totals>,
    others: BTreeMap<u64, (u32, BTreeMap<AccountId, u64>)>,
}

impl Ledger {
    /// 节点 `node` 的账本。
    #[must_use]
    pub const fn new(node: u64) -> Self {
        Self {
            node,
            own: BTreeMap::new(),
            others: BTreeMap::new(),
        }
    }

    /// 记一笔（本节点的；从文件恢复时也走这里）。别的节点记的行忽略。
    pub fn add(&mut self, entry: &Entry) {
        if entry.node != self.node {
            return;
        }
        let t = self
            .own
            .entry((month_of(entry.unix_ms), entry.account))
            .or_default();
        t.bytes = t.bytes.saturating_add(entry.bytes);
        t.packets = t.packets.saturating_add(entry.packets);
        t.sessions = t.sessions.saturating_add(u64::from(entry.last));
    }

    /// 本节点某月某账户的用量。
    #[must_use]
    pub fn own(&self, month: u32, account: &AccountId) -> Totals {
        self.own
            .get(&(month, *account))
            .copied()
            .unwrap_or_default()
    }

    /// 本节点某月所有账户的字节数（发给其他节点）。
    #[must_use]
    pub fn own_month(&self, month: u32) -> BTreeMap<AccountId, u64> {
        self.own
            .iter()
            .filter(|((m, _), _)| *m == month)
            .map(|((_, a), t)| (*a, t.bytes))
            .collect()
    }

    /// 收到另一个节点报告的它本月各账户的字节数（取代它之前的报告）。
    pub fn merge(&mut self, node: u64, month: u32, totals: BTreeMap<AccountId, u64>) {
        if node != self.node {
            self.others.insert(node, (month, totals));
        }
    }

    /// 全集群某月某账户的字节数。
    #[must_use]
    pub fn cluster_bytes(&self, month: u32, account: &AccountId) -> u64 {
        let others: u64 = self
            .others
            .values()
            .filter(|(m, _)| *m == month)
            .filter_map(|(_, t)| t.get(account))
            .sum();
        self.own(month, account).bytes.saturating_add(others)
    }

    /// 账户本月还能不能用中继：额度为 0 表示不限。
    #[must_use]
    pub fn allows(&self, month: u32, account: &Account) -> bool {
        account.quota_bytes == 0 || self.cluster_bytes(month, &account.id) < account.quota_bytes
    }

    /// 本节点某月的所有账户和用量（管理接口用）。
    #[must_use]
    pub fn month(&self, month: u32) -> Vec<(AccountId, Totals)> {
        self.own
            .iter()
            .filter(|((m, _), _)| *m == month)
            .map(|((_, a), t)| (*a, *t))
            .collect()
    }
}

/// 一笔中继流量的费用（分），`price_cents_per_gb` 是每 GB 的价格（分）。向上取整到分。
#[must_use]
pub fn charge_cents(bytes: u64, price_cents_per_gb: u64) -> u64 {
    let cents = u128::from(bytes) * u128::from(price_cents_per_gb);
    u64::try_from(cents.div_ceil(u128::from(GB))).unwrap_or(u64::MAX)
}

/// 编码其他节点要的本月汇总：`月份(4) 条数(2) [账户(8) 字节数(8)]…`。
#[must_use]
pub fn encode_totals(month: u32, totals: &BTreeMap<AccountId, u64>) -> Vec<u8> {
    let n = totals.len().min(usize::from(u16::MAX));
    let mut out = Vec::with_capacity(6 + n * 16);
    out.extend_from_slice(&month.to_be_bytes());
    out.extend_from_slice(&u16::try_from(n).unwrap_or(u16::MAX).to_be_bytes());
    for (account, bytes) in totals.iter().take(n) {
        out.extend_from_slice(account);
        out.extend_from_slice(&bytes.to_be_bytes());
    }
    out
}

/// 解码 [`encode_totals`] 的结果。
pub fn decode_totals(buf: &[u8]) -> Result<(u32, BTreeMap<AccountId, u64>), Error> {
    let mut r = crate::wire::Reader::new(buf);
    let month = r.u32()?;
    let n = r.u16()?;
    let mut totals = BTreeMap::new();
    for _ in 0..n {
        let mut account = [0u8; 8];
        account.copy_from_slice(r.take(8)?);
        totals.insert(account, r.u64()?);
    }
    r.finish()?;
    Ok((month, totals))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    const KEY: &[u8] = b"billing-secret-for-tests";

    #[test]
    fn tokens_verify_only_with_the_right_key() {
        let token = issue(KEY, [0xab; 8], 50);
        assert!(token.starts_with("pdk1-abababababababab-50-"), "{token}");
        let account = verify(KEY, &format!("  {token}\n")).unwrap();
        assert_eq!(account.id, [0xab; 8]);
        assert_eq!(account.quota_bytes, 50 * GB);
        assert_eq!(account.id_hex(), "abababababababab");
        assert!(verify(b"other-key", &token).is_err());
        let tampered = token.replace("-50-", "-500-");
        assert!(verify(KEY, &tampered).is_err(), "改额度会让签名失效");
        for bad in [
            "",
            "pdk1",
            "pdk2-ab-1-cd",
            "pdk1-zz-1-00",
            "pdk1-abababababababab-x-00",
            "pdk1-a-b-c-d-e",
        ] {
            assert!(verify(KEY, bad).is_err(), "{bad}");
        }
        assert_eq!(verify(KEY, &issue(KEY, [1; 8], 0)).unwrap().quota_bytes, 0);
    }

    #[test]
    fn months_follow_the_calendar() {
        assert_eq!(month_of(0), 197_001);
        assert_eq!(month_of(1_795_249_805_123), 202_611);
        // 2024-02-29 23:59:59.999 UTC 和 2024-03-01 00:00 UTC
        assert_eq!(month_of(1_709_251_199_999), 202_402);
        assert_eq!(month_of(1_709_251_200_000), 202_403);
    }

    #[test]
    fn entries_round_trip_and_torn_lines_are_skipped() {
        let e = Entry {
            unix_ms: 1_795_249_805_123,
            node: 2,
            account: [7; 8],
            session: [9; 16],
            bytes: 12_345,
            packets: 10,
            last: true,
        };
        assert_eq!(Entry::from_line(&e.to_line()), Some(e));
        let line = e.to_line();
        assert_eq!(
            Entry::from_line(&line[..line.len() - 3]),
            None,
            "写了一半的行"
        );
        assert_eq!(Entry::from_line(&format!("{line} extra")), None);
        assert_eq!(Entry::from_line(&line.replace(" 1", " 2")), None);
    }

    #[test]
    fn ledger_sums_own_entries_and_other_nodes_reports() {
        let mut l = Ledger::new(1);
        let entry = |node, bytes, last| Entry {
            unix_ms: 1_795_249_805_123,
            node,
            account: [7; 8],
            session: [1; 16],
            bytes,
            packets: 1,
            last,
        };
        l.add(&entry(1, 600, false));
        l.add(&entry(1, 400, true));
        l.add(&entry(2, 999, true)); // 别的节点的行：忽略
        let m = 202_611;
        assert_eq!(
            l.own(m, &[7; 8]),
            Totals {
                bytes: 1000,
                packets: 2,
                sessions: 1
            }
        );
        assert_eq!(l.month(m).len(), 1);
        assert_eq!(l.own_month(m).get(&[7; 8]), Some(&1000));
        // 节点 2 报告了 500；之后又报告 700（取代，不累加）；节点 1 自己的报告忽略
        l.merge(2, m, BTreeMap::from([([7; 8], 500)]));
        l.merge(2, m, BTreeMap::from([([7; 8], 700)]));
        l.merge(1, m, BTreeMap::from([([7; 8], 99_999)]));
        l.merge(3, 202_610, BTreeMap::from([([7; 8], 5_000)])); // 上个月的：不算
        assert_eq!(l.cluster_bytes(m, &[7; 8]), 1700);
        let limited = Account {
            id: [7; 8],
            quota_bytes: 1700,
        };
        assert!(!l.allows(m, &limited), "用满额度");
        assert!(l.allows(
            m,
            &Account {
                quota_bytes: 1701,
                ..limited
            }
        ));
        assert!(
            l.allows(
                m,
                &Account {
                    quota_bytes: 0,
                    ..limited
                }
            ),
            "0 表示不限"
        );
    }

    #[test]
    fn charges_round_up_and_totals_encode() {
        assert_eq!(charge_cents(0, 80), 0);
        assert_eq!(charge_cents(GB, 80), 80);
        assert_eq!(charge_cents(1, 80), 1, "不足 1 分按 1 分");
        let totals = BTreeMap::from([([1; 8], 10), ([2; 8], 20)]);
        let bytes = encode_totals(202_611, &totals);
        assert_eq!(decode_totals(&bytes).unwrap(), (202_611, totals));
        assert!(decode_totals(&bytes[..bytes.len() - 1]).is_err());
    }
}
