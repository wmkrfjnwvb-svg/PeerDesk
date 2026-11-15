//! 访问授权：用密码证明“我有权控制这台设备”。
//!
//! 使用 SPAKE2（一种口令认证密钥交换）：密码本身和它的任何可离线破解的衍生物都不会出现在线路上，
//! 窃听者或冒充对端的人每次连接只能猜一个密码。交换出的密钥再与“通道绑定”一起做
//! HMAC-SHA256 确认，通道绑定来自底层加密连接，所以中间人无法把一条连接上的证明转用到另一条。
//!
//! [`Limiter`] 限制连续失败的次数，防止在线逐个尝试。

use hmac::{Hmac, Mac};
use sha2::Sha256;
use spake2::{Ed25519Group, Identity, Password, Spake2};

use crate::{Error, Time};

/// 确认码的长度。
pub const PROOF_LEN: usize = 32;
/// SPAKE2 消息的最大长度。
pub const MAX_PAKE_MSG: usize = 64;

/// 本端的授权配置。
#[derive(Clone, PartialEq, Eq)]
pub enum Auth {
    /// 被控端：不要求密码。控制端：没有密码可提供。
    Open,
    /// 被控端：要求这个密码。控制端：被问到时提供这个密码。
    Password(String),
}

impl std::fmt::Debug for Auth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // 不输出密码。
        f.write_str(match self {
            Auth::Open => "Open",
            Auth::Password(_) => "Password(***)",
        })
    }
}

/// 交换中的一方。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    /// 控制端。
    Controller,
    /// 被控端。
    Host,
}

const ID_CONTROLLER: &[u8] = b"peerdesk-controller";
const ID_HOST: &[u8] = b"peerdesk-host";

/// 进行中的密码交换。
pub struct Pake {
    side: Side,
    state: Spake2<Ed25519Group>,
}

impl std::fmt::Debug for Pake {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Pake")
            .field("side", &self.side)
            .finish_non_exhaustive()
    }
}

/// 交换完成后双方各自要出示和要核对的确认码。
#[derive(Clone, PartialEq, Eq)]
pub struct Proofs {
    /// 本端发给对端的确认码。
    pub mine: [u8; PROOF_LEN],
    key: Vec<u8>,
    binding: Vec<u8>,
    side: Side,
}

impl std::fmt::Debug for Proofs {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Proofs")
            .field("side", &self.side)
            .finish_non_exhaustive()
    }
}

fn mac(key: &[u8], side: Side, binding: &[u8]) -> Hmac<Sha256> {
    // HMAC 接受任意长度的密钥，new_from_slice 不会失败；万一失败退回到空密钥的实例，
    // 结果只是确认码对不上、认证失败，不会放行。
    let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(key)
        .unwrap_or_else(|_| <Hmac<Sha256> as hmac::digest::KeyInit>::new(&Default::default()));
    mac.update(match side {
        Side::Controller => b"C:",
        Side::Host => b"H:",
    });
    mac.update(binding);
    mac
}

impl Pake {
    /// 开始交换，返回状态和要发给对端的消息。
    #[must_use]
    pub fn start(side: Side, password: &str) -> (Self, Vec<u8>) {
        let password = Password::new(password.as_bytes());
        let (a, b) = (Identity::new(ID_CONTROLLER), Identity::new(ID_HOST));
        let (state, msg) = match side {
            Side::Controller => Spake2::<Ed25519Group>::start_a(&password, &a, &b),
            Side::Host => Spake2::<Ed25519Group>::start_b(&password, &a, &b),
        };
        (Self { side, state }, msg)
    }

    /// 收到对端的消息后完成交换。`binding` 是底层连接的通道绑定，两端必须相同。
    pub fn finish(self, peer_msg: &[u8], binding: &[u8]) -> Result<Proofs, Error> {
        let key = self
            .state
            .finish(peer_msg)
            .map_err(|_| Error::Inconsistent("密码交换消息无效"))?;
        let mine = mac(&key, self.side, binding).finalize().into_bytes().into();
        Ok(Proofs {
            mine,
            key,
            binding: binding.to_vec(),
            side: self.side,
        })
    }
}

impl Proofs {
    /// 核对对端出示的确认码。比较是常数时间的。
    #[must_use]
    pub fn verify_peer(&self, proof: &[u8]) -> bool {
        let peer = match self.side {
            Side::Controller => Side::Host,
            Side::Host => Side::Controller,
        };
        mac(&self.key, peer, &self.binding)
            .verify_slice(proof)
            .is_ok()
    }
}

/// 限制连续的认证失败：达到阈值后锁定，锁定时间逐次翻倍。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Limiter {
    max_failures: u32,
    base_lock_us: u64,
    max_lock_us: u64,
    failures: u32,
    lockouts: u32,
    locked_until: Time,
}

impl Default for Limiter {
    /// 连续 5 次失败后锁定 30 秒，之后每次锁定时间翻倍，最长 1 小时。
    fn default() -> Self {
        Self::new(5, 30_000_000, 3_600_000_000)
    }
}

impl Limiter {
    /// 创建。`max_failures` 至少为 1。
    #[must_use]
    pub const fn new(max_failures: u32, base_lock_us: u64, max_lock_us: u64) -> Self {
        Self {
            max_failures: if max_failures == 0 { 1 } else { max_failures },
            base_lock_us,
            max_lock_us,
            failures: 0,
            lockouts: 0,
            locked_until: Time::ZERO,
        }
    }

    /// 现在是否允许尝试。
    #[must_use]
    pub fn allowed(&self, now: Time) -> bool {
        now >= self.locked_until
    }

    /// 还要锁定多久（微秒）。
    #[must_use]
    pub const fn remaining_us(&self, now: Time) -> u64 {
        self.locked_until.since(now)
    }

    /// 记录一次失败。
    pub fn failure(&mut self, now: Time) {
        self.failures = self.failures.saturating_add(1);
        if self.failures >= self.max_failures {
            self.failures = 0;
            let factor = 1u64.checked_shl(self.lockouts).unwrap_or(u64::MAX);
            let lock = self
                .base_lock_us
                .saturating_mul(factor)
                .min(self.max_lock_us);
            self.locked_until = now.plus(lock);
            self.lockouts = self.lockouts.saturating_add(1);
        }
    }

    /// 记录一次成功：清零失败计数和锁定级别。
    pub fn success(&mut self) {
        self.failures = 0;
        self.lockouts = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn exchange(pw_c: &str, pw_h: &str, bind_c: &[u8], bind_h: &[u8]) -> (Proofs, Proofs) {
        let (c, msg_c) = Pake::start(Side::Controller, pw_c);
        let (h, msg_h) = Pake::start(Side::Host, pw_h);
        assert!(msg_c.len() <= MAX_PAKE_MSG && msg_h.len() <= MAX_PAKE_MSG);
        (
            c.finish(&msg_h, bind_c).unwrap(),
            h.finish(&msg_c, bind_h).unwrap(),
        )
    }

    #[test]
    fn same_password_and_binding_verifies_both_ways() {
        let (c, h) = exchange("hunter2", "hunter2", b"chan", b"chan");
        assert!(h.verify_peer(&c.mine));
        assert!(c.verify_peer(&h.mine));
        // 自己的确认码不能当作对端的用（防反射）
        assert!(!c.verify_peer(&c.mine));
        assert!(!h.verify_peer(&h.mine));
        assert!(!h.verify_peer(&c.mine[..31]));
        assert!(!h.verify_peer(&[]));
    }

    #[test]
    fn wrong_password_or_binding_fails() {
        let (c, h) = exchange("hunter2", "hunter3", b"chan", b"chan");
        assert!(!h.verify_peer(&c.mine));
        assert!(!c.verify_peer(&h.mine));
        // 密码对，但两端不在同一条加密连接上（中间人转发）
        let (c, h) = exchange("hunter2", "hunter2", b"chan-1", b"chan-2");
        assert!(!h.verify_peer(&c.mine));
        assert!(!c.verify_peer(&h.mine));
    }

    #[test]
    fn messages_differ_every_time_and_garbage_is_rejected() {
        let (_, m1) = Pake::start(Side::Controller, "pw");
        let (p, m2) = Pake::start(Side::Controller, "pw");
        assert_ne!(m1, m2, "每次交换的消息都不同，窃听者无法重放");
        assert!(p.finish(b"garbage", b"x").is_err());
        // 把控制端的消息喂给控制端（角色不对）
        let (p, _) = Pake::start(Side::Controller, "pw");
        assert!(p.finish(&m1, b"x").is_err());
    }

    #[test]
    fn nothing_secret_in_debug_output() {
        let auth = Auth::Password("s3cret".into());
        assert_eq!(format!("{auth:?}"), "Password(***)");
        assert_eq!(format!("{:?}", Auth::Open), "Open");
        let (p, _) = Pake::start(Side::Host, "s3cret");
        assert!(!format!("{p:?}").contains("s3cret"));
        let (c, _) = exchange("s3cret", "s3cret", b"b", b"b");
        assert_eq!(format!("{c:?}"), "Proofs { side: Controller, .. }");
    }

    #[test]
    fn limiter_locks_with_exponential_backoff() {
        let mut l = Limiter::new(2, 1000, 3000);
        assert!(l.allowed(Time(0)));
        l.failure(Time(0));
        assert!(l.allowed(Time(0)), "还没到阈值");
        l.failure(Time(10));
        assert!(!l.allowed(Time(10)));
        assert_eq!(l.remaining_us(Time(10)), 1000);
        assert!(!l.allowed(Time(1009)));
        assert!(l.allowed(Time(1010)));
        assert_eq!(l.remaining_us(Time(5000)), 0);
        // 第二轮：锁定时间翻倍
        l.failure(Time(2000));
        l.failure(Time(2000));
        assert_eq!(l.remaining_us(Time(2000)), 2000);
        // 第三轮：封顶
        l.failure(Time(9000));
        l.failure(Time(9000));
        assert_eq!(l.remaining_us(Time(9000)), 3000);
        // 成功后回到初始级别
        l.success();
        l.failure(Time(20_000));
        l.failure(Time(20_000));
        assert_eq!(l.remaining_us(Time(20_000)), 1000);
        // 阈值为 0 时按 1 处理；锁定级别极大时不溢出
        let mut l = Limiter::new(0, u64::MAX, u64::MAX);
        for _ in 0..100 {
            l.failure(Time(0));
        }
        assert!(!l.allowed(Time(1)));
        assert_eq!(
            Limiter::default(),
            Limiter::new(5, 30_000_000, 3_600_000_000)
        );
    }
}
