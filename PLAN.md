# PeerDesk development plan

This plan continues the original technical design without changing its core:
low latency first, QUIC with our own real-time layer on top, direct
connections first with relay fallback, Apache 2.0 plus a hosted cloud, relay
traffic billed and direct connections free, Windows + macOS + Linux, and a
Tauri client later.

What changes is the order of the work. The networking core, server cluster and
test harness are built; nothing yet captures a real screen. The next work goes
there.

## Where the code stands (2026-11-17)

Verified on Linux: `cargo test --workspace` passes all 278 tests, clippy with
`-D warnings` and `cargo fmt --check` are clean, and a real `host` / `connect`
session over loopback delivers frames, input and a file upload. On loopback,
QUIC datagrams add about 0.47 ms of median latency over plain UDP.

| Area | State |
| --- | --- |
| QUIC transport, device identity, fingerprints, password auth | Done |
| File transfer (multi-file, directories, resume, checksums) | Done |
| Signaling, hole punching, metered relay, server cluster, admin page | Done |
| Simulator and test harness | Done |
| Codec | Stand-in software tile codec; to be replaced by H.264 |
| Congestion controller | TODO (`crates/net/src/tls.rs:84`) |
| UPnP / PCP, IPv6, port prediction, birthday attack, TCP direct | Only as options in the planner (`crates/core/src/connect.rs`); not performed by the network layer |
| Screen capture, display window, input injection, clipboard | Not started |
| Tauri client, Windows service, signing | Not started |

## Changes to the engineering rules

1. **New crate `crates/platform`** for capture, encoding and input injection.
   It is the only crate allowed to use `unsafe` (OS APIs require it).
   `unsafe_code = "forbid"` stays for `core`, `net`, `client` and `server`.
2. **Coverage gate.** The 99% line gate stays for `core` and `net`. The
   platform crate is excluded; it is tested by smoke tests on real machines
   and CI runners instead.
3. **Codec.** Replace the tile codec with H.264: hardware encoders first,
   software (OpenH264) as fallback. This matches the design, which calls for
   hardware encoding and later AV1.

## Phase 0: Take over (about 1 week)

- [ ] Repository access for the new maintainer; merge or rebase `dev` into `main`.
- [ ] Get the design document shared with the new maintainer (it is the source of truth).
- [ ] Confirm CI is green on Windows and macOS (only Linux was verified locally).
- [ ] **Start code signing now**: Windows EV certificate, Apple Developer ID. Both take weeks.

## Phase 1: M1, real remote control

Platforms are done one after another inside M1; the acceptance criteria are unchanged.

1. **Windows host**
   - Capture: DXGI Desktop Duplication.
   - Encode: hardware H.264 through Media Foundation; OpenH264 fallback.
   - Input: `SendInput` using scancodes (physical keys, not characters).
   - Cursor shape and position sent on their own channel and drawn locally by the viewer.
2. **Viewer**: a minimal native window that decodes and presents frames. Tauri comes in Phase 4.
3. **Timestamps at six points**: capture, encode done, sent, received, decoded, on screen. Built alongside the Windows work.
4. **macOS**: ScreenCaptureKit, VideoToolbox, `CGEvent`, plus the Screen Recording and Accessibility permission flow.
5. **Linux**: X11 first, then Wayland through PipeWire and the desktop portal; VA-API; `uinput`.

**M1 acceptance (from the design):**
- [ ] All three platforms can control each other.
- [ ] Processing latency on each machine under 25 ms.
- [ ] QUIC datagram overhead over plain UDP no more than 2 ms on a real network (loopback: 0.47 ms).

## Phase 2: M2, transport quality

- [ ] Custom congestion controller plugged into quinn (delay-gradient based, random loss is not congestion).
- [ ] Per-frame byte budget enforced by the encoder; drop before capture, never queue after encoding.
- [ ] Loss recovery by referencing the last acknowledged frame; never send keyframes for recovery.
- [ ] FEC tuned against recorded real-network traces.
- [ ] The six strategies (stable, wireless, cellular, long distance, low data, TCP) behind one interface, validated by trace replay.

## Phase 3: M3, connection rate

- [ ] Evaluate iroh before building (adopt or only borrow ideas).
- [ ] Host side prepares an entry point: UPnP, NAT-PMP, PCP, IPv6 firewall pinholes.
- [ ] Port prediction and birthday attack implemented in the network layer.
- [ ] TCP direct dial when UDP is blocked; TCP 443 relay as last resort.
- [ ] Relay first, then migrate to direct in the background; choose path by measured quality.
- [ ] Connection diagnostics page and per-network-pair failure reporting.

## Phase 4: Product

- [ ] Tauri client, including file-transfer UI (the protocol is done).
- [ ] Windows service for the login screen and UAC (`SendSAS` for Ctrl+Alt+Del).
- [ ] macOS: handling for permission prompts on unattended Macs.
- [ ] Clipboard.
- [ ] Signed installers, with a backup certificate and a fast re-signing process.

## Phase 5: Hosted cloud and billing

- [ ] Accounts and billing on top of the existing relay metering.
- [ ] **Abuse handling before public launch**: reporting, rate limits, identity checks.
- [ ] Show users whether each session ran direct or through the relay, and what was billed.
- [ ] Decide the open billing questions from the design: which account pays, free allowance, behavior at zero balance.

## Risks

- **Revenue.** Relay bandwidth is cheap overseas, so relay-only billing earns little. Keep a paid business tier (per-seat or managed self-hosting) as a fallback; it fits Apache 2.0.
- **Signing and abuse.** Remote-control tools are used by scammers; a revoked certificate stops the product. Mitigated by Phase 0 signing and Phase 5 abuse handling.
- **Platform code needs real machines.** Capture, encoding and input cannot be verified in a Linux container; each platform needs a test machine or CI runner.
- **Unverified numbers.** Latency, bitrate and direct-rate targets in the design are starting points to be calibrated with the Phase 1 timestamps.
- **Positioning.** RustDesk is AGPL; PeerDesk is Apache 2.0, so companies can embed it in closed products. No GPL or AGPL code may enter the project.
