//! 会话握手状态机。不做 IO：输入是收到的控制消息，输出是要发出的控制消息。
//!
//! 不需要密码时：控制端发 Hello，被控端回 Accept 或 Reject。
//!
//! 需要密码时，中间多一轮密码交换（见 [`crate::auth`]）：
//!
//! ```text
//! 控制端                         被控端
//!   Hello            ──────────▶
//!                    ◀──────────  AuthStart（需要密码）
//!   AuthStart + AuthProof ─────▶
//!                    ◀──────────  AuthProof + Accept，或 Reject(Unauthorized)
//! ```
//!
//! 双方都要出示确认码：被控端确认控制端知道密码，控制端也确认对面真的是那台设备。
//! Hello 可以重发，被控端对同一设备的重复 Hello 幂等地回同样的应答。

use crate::auth::{Auth, Pake, Proofs, Side};
use crate::crypto::{negotiate, Policy, Suite};
use crate::proto::{Caps, Control, PeerOs, RejectReason, MIN_VERSION, VERSION};
use crate::trace::{Stage, TraceId, Tracer};
use crate::{Error, Time};

/// 会话一端的配置。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionConfig {
    /// 本端设备标识。
    pub device_id: u64,
    /// 本端所在区域。
    pub region: String,
    /// 本端编解码能力。
    pub caps: Caps,
    /// 本端加密策略。
    pub policy: Policy,
    /// 授权：被控端要求的密码，或控制端持有的密码。
    pub auth: Auth,
    /// 通道绑定：来自底层加密连接、两端相同的一段字节。仿真里用固定值。
    pub binding: Vec<u8>,
}

/// 角色。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    /// 控制端：发起连接、观看画面。
    Controller,
    /// 被控端：接受连接、提供画面。
    Host,
}

/// 会话状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    /// 尚未开始。
    Idle,
    /// 控制端已发 Hello，等待应答。
    HelloSent,
    /// 密码交换进行中。
    AwaitAuth,
    /// 已建立。
    Established {
        /// 选中的加密套件。
        suite: Suite,
        /// 会话标识。
        session_id: u32,
        /// 对端设备标识（控制端一侧在握手阶段不知道对端标识，记为 0）。
        peer: u64,
        /// 两端共同的编解码能力。
        caps: Caps,
    },
    /// 被拒绝。
    Rejected(RejectReason),
    /// 已关闭。
    Closed,
}

impl State {
    /// 状态名，用于错误信息。
    #[must_use]
    pub const fn name(&self) -> &'static str {
        match self {
            State::Idle => "Idle",
            State::HelloSent => "HelloSent",
            State::AwaitAuth => "AwaitAuth",
            State::Established { .. } => "Established",
            State::Rejected(_) => "Rejected",
            State::Closed => "Closed",
        }
    }
}

/// 被控端在密码交换期间记住的、建立会话所需的信息。
#[derive(Debug, Clone, Copy)]
struct Pending {
    device_id: u64,
    caps: Caps,
    suite: Suite,
}

/// 会话的一端。
#[derive(Debug)]
pub struct Session {
    role: Role,
    cfg: SessionConfig,
    state: State,
    trace: TraceId,
    pake: Option<Pake>,
    proofs: Option<Proofs>,
    pending: Option<Pending>,
    /// 被控端发出的 AuthStart，重复的 Hello 原样重发。
    auth_start: Option<Vec<u8>>,
    /// 控制端是否已经核对过被控端的确认码。
    host_verified: bool,
}

impl Session {
    const fn new(role: Role, cfg: SessionConfig, trace: TraceId) -> Self {
        Self {
            role,
            cfg,
            state: State::Idle,
            trace,
            pake: None,
            proofs: None,
            pending: None,
            auth_start: None,
            host_verified: false,
        }
    }

    /// 创建控制端。
    #[must_use]
    pub const fn controller(cfg: SessionConfig, trace: TraceId) -> Self {
        Self::new(Role::Controller, cfg, trace)
    }

    /// 创建被控端。
    #[must_use]
    pub const fn host(cfg: SessionConfig, trace: TraceId) -> Self {
        Self::new(Role::Host, cfg, trace)
    }

    /// 角色。
    #[must_use]
    pub const fn role(&self) -> Role {
        self.role
    }

    /// 当前状态。
    #[must_use]
    pub const fn state(&self) -> &State {
        &self.state
    }

    /// 已建立时返回选中的套件。
    #[must_use]
    pub const fn suite(&self) -> Option<Suite> {
        match self.state {
            State::Established { suite, .. } => Some(suite),
            State::Idle
            | State::HelloSent
            | State::AwaitAuth
            | State::Rejected(_)
            | State::Closed => None,
        }
    }

    const fn unexpected(&self, msg: &Control) -> Error {
        Error::UnexpectedMessage {
            state: self.state.name(),
            msg: msg.name(),
        }
    }

    fn reject(&mut self, now: Time, reason: RejectReason, tracer: &mut Tracer) -> Control {
        self.state = State::Rejected(reason);
        self.pake = None;
        self.proofs = None;
        tracer.record(now, self.trace, Stage::Rejected, u64::from(reason.id()));
        Control::Reject { reason }
    }

    /// 控制端生成 Hello。等待应答期间可以反复调用来重发。
    pub fn hello(&mut self, now: Time, tracer: &mut Tracer) -> Result<Control, Error> {
        match (self.role, self.state) {
            (Role::Controller, State::Idle | State::HelloSent) => {
                self.state = State::HelloSent;
                tracer.record(now, self.trace, Stage::HelloSent, 0);
                Ok(Control::Hello {
                    version: VERSION,
                    device_id: self.cfg.device_id,
                    caps: self.cfg.caps,
                    region: self.cfg.region.clone(),
                    suites: self.cfg.policy.suites().to_vec(),
                })
            }
            (
                Role::Controller,
                State::AwaitAuth | State::Established { .. } | State::Rejected(_) | State::Closed,
            )
            | (Role::Host, _) => Err(Error::UnexpectedMessage {
                state: self.state.name(),
                msg: "发起 Hello",
            }),
        }
    }

    /// 处理收到的控制消息，返回需要按顺序回复的消息。
    pub fn on_control(
        &mut self,
        now: Time,
        msg: &Control,
        tracer: &mut Tracer,
    ) -> Result<Vec<Control>, Error> {
        match msg {
            Control::Hello {
                version,
                device_id,
                caps,
                suites,
                region: _,
            } => self.on_hello(now, msg, *version, *device_id, *caps, suites, tracer),
            Control::AuthStart { msg: pake_msg } => self.on_auth_start(now, msg, pake_msg, tracer),
            Control::AuthProof { proof } => self.on_auth_proof(now, msg, proof, tracer),
            Control::Accept {
                suite,
                session_id,
                caps,
            } => self.on_accept(now, msg, *suite, *session_id, *caps, tracer),
            Control::Reject { reason } => self.on_reject(now, msg, *reason, tracer),
            Control::Ping { nonce } => match self.state {
                State::Established { .. } => Ok(vec![Control::Pong { nonce: *nonce }]),
                State::Idle
                | State::HelloSent
                | State::AwaitAuth
                | State::Rejected(_)
                | State::Closed => Err(self.unexpected(msg)),
            },
            Control::Pong { .. } => match self.state {
                State::Established { .. } => Ok(Vec::new()),
                State::Idle
                | State::HelloSent
                | State::AwaitAuth
                | State::Rejected(_)
                | State::Closed => Err(self.unexpected(msg)),
            },
            Control::Bye => {
                self.mark_closed(now, tracer);
                Ok(Vec::new())
            }
        }
    }

    fn establish(&mut self, now: Time, p: Pending, tracer: &mut Tracer) -> Control {
        // 占位的会话号：保证非零。
        let mixed = p.device_id ^ self.cfg.device_id ^ now.micros();
        let session_id = u32::from_be_bytes({
            let b = mixed.to_be_bytes();
            [b[4] ^ b[0], b[5] ^ b[1], b[6] ^ b[2], b[7] ^ b[3]]
        }) | 1;
        self.state = State::Established {
            suite: p.suite,
            session_id,
            peer: p.device_id,
            caps: p.caps,
        };
        tracer.record(now, self.trace, Stage::AcceptSent, u64::from(session_id));
        tracer.record(now, self.trace, Stage::Established, u64::from(p.suite.id()));
        Control::Accept {
            suite: p.suite,
            session_id,
            caps: p.caps.with_os(PeerOs::current()),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn on_hello(
        &mut self,
        now: Time,
        msg: &Control,
        version: u8,
        device_id: u64,
        caps: Caps,
        suites: &[Suite],
        tracer: &mut Tracer,
    ) -> Result<Vec<Control>, Error> {
        match self.role {
            Role::Controller => return Err(self.unexpected(msg)),
            Role::Host => {}
        }
        match self.state {
            State::Idle => {}
            // 同一设备重发的 Hello：幂等地重发上一次的应答。
            State::Established {
                suite,
                session_id,
                peer,
                caps: common,
            } if peer == device_id => {
                return Ok(vec![Control::Accept {
                    suite,
                    session_id,
                    caps: common.with_os(PeerOs::current()),
                }]);
            }
            State::AwaitAuth if self.pending.is_some_and(|p| p.device_id == device_id) => {
                return Ok(self
                    .auth_start
                    .clone()
                    .map(|msg| Control::AuthStart { msg })
                    .into_iter()
                    .collect());
            }
            State::Established { .. } | State::AwaitAuth => {
                return Ok(vec![Control::Reject {
                    reason: RejectReason::Busy,
                }]);
            }
            State::HelloSent | State::Rejected(_) | State::Closed => {
                return Err(self.unexpected(msg));
            }
        }

        tracer.record(now, self.trace, Stage::HelloReceived, device_id);
        // 比自己新的版本也接受：新版本对老版本只用双方都有的能力。
        if version < MIN_VERSION {
            return Ok(vec![self.reject(now, RejectReason::Version, tracer)]);
        }
        let Ok(suite) = negotiate(&self.cfg.policy, suites) else {
            return Ok(vec![self.reject(now, RejectReason::NoCommonSuite, tracer)]);
        };
        tracer.record(
            now,
            self.trace,
            Stage::SuiteNegotiated,
            u64::from(suite.id()),
        );
        let pending = Pending {
            device_id,
            caps: self.cfg.caps.common(caps),
            suite,
        };
        match &self.cfg.auth {
            Auth::Open => Ok(vec![self.establish(now, pending, tracer)]),
            Auth::Password(password) => {
                let (pake, pake_msg) = Pake::start(Side::Host, password);
                self.pake = Some(pake);
                self.pending = Some(pending);
                self.auth_start = Some(pake_msg.clone());
                self.state = State::AwaitAuth;
                tracer.record(now, self.trace, Stage::AuthRequested, device_id);
                Ok(vec![Control::AuthStart { msg: pake_msg }])
            }
        }
    }

    fn on_auth_start(
        &mut self,
        now: Time,
        msg: &Control,
        pake_msg: &[u8],
        tracer: &mut Tracer,
    ) -> Result<Vec<Control>, Error> {
        match (self.role, self.state) {
            // 控制端：被控端要求密码。
            (Role::Controller, State::HelloSent) => {
                tracer.record(now, self.trace, Stage::AuthRequested, 0);
                let Auth::Password(password) = &self.cfg.auth else {
                    // 没有密码可提供。
                    tracer.record(now, self.trace, Stage::AuthFailed, 0);
                    self.reject(now, RejectReason::Unauthorized, tracer);
                    return Ok(Vec::new());
                };
                let (pake, mine) = Pake::start(Side::Controller, password);
                let Ok(proofs) = pake.finish(pake_msg, &self.cfg.binding) else {
                    tracer.record(now, self.trace, Stage::AuthFailed, 0);
                    self.reject(now, RejectReason::Unauthorized, tracer);
                    return Ok(Vec::new());
                };
                let proof = proofs.mine;
                self.proofs = Some(proofs);
                self.state = State::AwaitAuth;
                Ok(vec![
                    Control::AuthStart { msg: mine },
                    Control::AuthProof { proof },
                ])
            }
            // 控制端：被控端因为重复的 Hello 重发了 AuthStart，忽略。
            (Role::Controller, State::AwaitAuth) => Ok(Vec::new()),
            // 被控端：收到控制端的交换消息。
            (Role::Host, State::AwaitAuth) => {
                let Some(pake) = self.pake.take() else {
                    // 已经收到过一条：重复的消息，忽略。
                    return Ok(Vec::new());
                };
                match pake.finish(pake_msg, &self.cfg.binding) {
                    Ok(proofs) => {
                        self.proofs = Some(proofs);
                        Ok(Vec::new())
                    }
                    Err(_) => {
                        tracer.record(now, self.trace, Stage::AuthFailed, 0);
                        Ok(vec![self.reject(now, RejectReason::Unauthorized, tracer)])
                    }
                }
            }
            (
                Role::Controller,
                State::Idle | State::Established { .. } | State::Rejected(_) | State::Closed,
            )
            | (
                Role::Host,
                State::Idle
                | State::HelloSent
                | State::Established { .. }
                | State::Rejected(_)
                | State::Closed,
            ) => Err(self.unexpected(msg)),
        }
    }

    fn on_auth_proof(
        &mut self,
        now: Time,
        msg: &Control,
        proof: &[u8; 32],
        tracer: &mut Tracer,
    ) -> Result<Vec<Control>, Error> {
        match self.state {
            State::AwaitAuth => {}
            State::Idle
            | State::HelloSent
            | State::Established { .. }
            | State::Rejected(_)
            | State::Closed => return Err(self.unexpected(msg)),
        }
        let verified = self.proofs.as_ref().is_some_and(|p| p.verify_peer(proof));
        match self.role {
            Role::Host => {
                let (Some(pending), Some(mine), true) =
                    (self.pending, self.proofs.as_ref().map(|p| p.mine), verified)
                else {
                    tracer.record(now, self.trace, Stage::AuthFailed, 0);
                    return Ok(vec![self.reject(now, RejectReason::Unauthorized, tracer)]);
                };
                tracer.record(now, self.trace, Stage::AuthPassed, pending.device_id);
                self.proofs = None;
                let accept = self.establish(now, pending, tracer);
                Ok(vec![Control::AuthProof { proof: mine }, accept])
            }
            Role::Controller => {
                if verified {
                    self.host_verified = true;
                    tracer.record(now, self.trace, Stage::AuthPassed, 0);
                } else {
                    // 对面不知道密码：不是我们要连的那台设备。
                    tracer.record(now, self.trace, Stage::AuthFailed, 0);
                    self.reject(now, RejectReason::Unauthorized, tracer);
                }
                Ok(Vec::new())
            }
        }
    }

    fn on_accept(
        &mut self,
        now: Time,
        msg: &Control,
        suite: Suite,
        session_id: u32,
        caps: Caps,
        tracer: &mut Tracer,
    ) -> Result<Vec<Control>, Error> {
        match (self.role, self.state) {
            (Role::Controller, State::HelloSent | State::AwaitAuth) => {
                // 做过密码交换的话，必须先核对过被控端的确认码才接受 Accept。
                let in_auth = matches!(self.state, State::AwaitAuth);
                if in_auth && !self.host_verified {
                    tracer.record(now, self.trace, Stage::AuthFailed, 0);
                    self.reject(now, RejectReason::Unauthorized, tracer);
                    return Err(self.unexpected(msg));
                }
                tracer.record(
                    now,
                    self.trace,
                    Stage::AcceptReceived,
                    u64::from(session_id),
                );
                // 对端选的套件必须在本端策略内，否则视为降级攻击或对端异常。
                if !self.cfg.policy.allows(suite) {
                    self.reject(now, RejectReason::NoCommonSuite, tracer);
                    return Err(Error::NoCommonSuite);
                }
                self.proofs = None;
                self.state = State::Established {
                    suite,
                    session_id,
                    peer: 0,
                    // 取交集时保留被控端带来的系统信息（不是能力位）。
                    caps: self.cfg.caps.common(caps).with_os(caps.os()),
                };
                tracer.record(now, self.trace, Stage::Established, u64::from(suite.id()));
                Ok(Vec::new())
            }
            // 重复的 Accept（对应重发的 Hello）：内容一致就忽略。
            (
                Role::Controller,
                State::Established {
                    suite: have,
                    session_id: sid,
                    ..
                },
            ) if have == suite && sid == session_id => Ok(Vec::new()),
            (
                Role::Controller,
                State::Idle | State::Established { .. } | State::Rejected(_) | State::Closed,
            )
            | (Role::Host, _) => Err(self.unexpected(msg)),
        }
    }

    fn on_reject(
        &mut self,
        now: Time,
        msg: &Control,
        reason: RejectReason,
        tracer: &mut Tracer,
    ) -> Result<Vec<Control>, Error> {
        match (self.role, self.state) {
            (Role::Controller, State::HelloSent | State::AwaitAuth) => {
                self.reject(now, reason, tracer);
                Ok(Vec::new())
            }
            (
                Role::Controller,
                State::Idle | State::Established { .. } | State::Rejected(_) | State::Closed,
            )
            | (Role::Host, _) => Err(self.unexpected(msg)),
        }
    }

    fn mark_closed(&mut self, now: Time, tracer: &mut Tracer) {
        match self.state {
            State::Closed => {}
            State::Idle
            | State::HelloSent
            | State::AwaitAuth
            | State::Established { .. }
            | State::Rejected(_) => {
                self.state = State::Closed;
                self.pake = None;
                self.proofs = None;
                tracer.record(now, self.trace, Stage::Closed, 0);
            }
        }
    }

    /// 主动关闭，返回要发给对端的 Bye。
    pub fn close(&mut self, now: Time, tracer: &mut Tracer) -> Control {
        self.mark_closed(now, tracer);
        Control::Bye
    }
}
