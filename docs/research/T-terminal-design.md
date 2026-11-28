# T: Remote terminal design (2026-11-28, main session)

Goal: run the host's shell inside our existing encrypted QUIC session, with no SSH port opened. The standard is iTerm2 class (see "Standards" in `N-terminal-files-ui-plan.md`). Research is in `industry-terminal.md`.
Core users run Agents (such as Claude Code) on their own machines all the time. Three things matter most:
- a long session never dies because the network dropped;
- 100 MB of output never freezes anything;
- they can approve the Agent's permission prompts from their phone.

## 1. Architecture (decided)

```
被控端                                                    控制端
shell ⇄ PTY ⇄ 读线程 ─→ 仿真器（alacritty_terminal）        仿真器 ─→ egui 画格子
                 │        └ 屏幕 + scrollback（快照的来源）    ↑
                 └→ 输出环形缓冲（4 MB，按字节序号）──QUIC 流──┘
键盘、粘贴、鼠标、改大小 ←──────────────────── 同一条流的反方向
```

- **Industry practice and our choice.** We follow tmux and RustDesk: the host keeps the PTY running and holds the screen plus scrollback.
  - We do not follow mosh's "sync only the visible screen", because mosh has no scrollback, and Agent users need to scroll back through output.
  - We do take mosh's idea for one case: when the controller cannot keep up, the host drops the intermediate output and sends the latest screen (see §4).
- **Two emulators.** One on the host, used for snapshots. One on the controller, used for display, selection, local scrollback and reflow when the size changes. Both use the same alacritty_terminal version (Apache-2.0).
- **One QUIC stream per tab.** A session (PTY) lives independently of the stream: closing the stream is a detach, and the PTY stays.
- **Code location.** A new crate `crates/term`:
  - pure logic, so it can be covered by tests: protocol, session table, snapshot, backpressure;
  - PTY glue (`pty_unix.rs` and `pty_windows.rs`), which only calls system interfaces and is excluded from coverage; the reason goes in `scripts/coverage.sh`.
  - The GUI widget goes in `crates/client/src/gui/terminal/`. Splitting out the pure parts (key encoding, colour table, selection) is what lets them be tested.

## 2. Protocol

- Capability bit `Caps::TERMINAL` and stream type `StreamKind::Terminal`.
- Framing is the same as files: a 4-byte length plus the message. The first message on a stream is one of `Open`, `Attach` or `List`.

| Direction | Message | Content |
|---|---|---|
| C→H | `Open { cols, rows }` | Open a new session (the host's default login shell) |
| C→H | `Attach { id, cols, rows, have: Option<u64> }` | Reattach to an existing session. `have` is the output sequence number the controller already has |
| C→H | `List` | List the sessions |
| H→C | `Opened { id, shell, start: u64 }` | Session created; output counts from `start` |
| H→C | `Snapshot { at: u64, bytes }` | Escape sequences that reproduce the screen plus recent scrollback (§3). Output after this starts at `at` |
| H→C | `Output { at: u64, bytes }` | Output bytes, `at` = byte sequence number of the first byte |
| H→C | `Sessions { list }` | id, title (OSC 0/2), start time, last output time, whether it has exited |
| H→C | `Exit { code }` | The shell exited. The session is gone once this is sent |
| C→H | `Input { bytes }` | Keyboard, paste, mouse reports (encoded by the controller) |
| C→H | `Resize { cols, rows }` | Window size changed (host does SIGWINCH / ResizePseudoConsole) |
| C→H | `Close` | End the session (kill the process tree) |
| H→C | `Refused { reason }` | No permission, no logged-in user, too many sessions. The reason says what to do |

- **Exact resume on reconnect.**
  - If the bytes from `have` onward are still in the host's ring buffer, the host sends `Output` from `have`. Nothing is lost and the controller's scrollback is untouched. Eternal Terminal works this way.
  - Otherwise the host sends a `Snapshot`.
- **Priority.** Terminal streams get a higher QUIC priority than file and clipboard streams. Ctrl+C is not delayed when output piles up, because input travels in the other direction and the host writes it to the PTY as soon as it arrives.

## 3. Snapshot

- **What it is.** The host's emulator state is serialized into a sequence of escape codes. A brand-new emulator that eats this sequence ends up in the same state. That way the controller doesn't need a second code path for snapshots. It is the same idea as tmux redrawing on attach.
- **What it contains, in order:**
  1. reset;
  2. the last N scrollback lines (default 10,000, capped at 2 MB), with SGR attributes; wrapped lines end without CRLF so reflow keeps working;
  3. the main screen;
  4. if the alternate screen is active, `?1049h` and then the alternate screen;
  5. scroll region, origin mode, autowrap, cursor keys, keypad mode, bracketed paste, mouse modes (1000/1002/1003/1006), focus reporting, cursor shape and visibility, title, and finally the cursor position.
- **Oracle (hard gate).**
  - The bytes of recorded real terminal sessions go into emulator A.
  - Serialize A, feed the result into a new emulator B of the same size, and compare cell by cell: character, wide-character flag, foreground, background, attributes. Also compare the cursor, every mode in item 5, and scrollback line by line.
  - Recorded material, captured with `script` and stored in `crates/term/tests/data/`:
    - `ls --color`, `git log --graph --color`;
    - vim editing (alternate screen);
    - htop / top refreshing;
    - a Claude Code full-screen session (alternate screen plus mouse);
    - Chinese output and emoji;
    - an over-wide line that wraps.
  - Also add a random test that cuts the byte stream at arbitrary points: snapshot after every few KB, so a cut in the middle of an escape sequence is covered.
  - We don't only use synthetic data: random bytes alone would mostly test garbage input, not real usage.

## 4. Backpressure and 100 MB output

- **The host always reads the PTY.** The read thread → emulator → ring buffer, so the shell is never stalled because the controller is slow. alacritty parses at the 100 MB/s level; measure it before relying on that.
- **Each stream has its own send backlog limit.** The limit is max(256 KB, estimated rate × 250 ms). When the backlog goes over the limit:
  - stop sending `Output` and mark the stream as behind;
  - once the backlog has drained, send one `Snapshot`, then go back to sending `Output`.
- **What the controller shows.** It draws a separator line in its own UI: "Skipped about N lines of output (the network couldn't keep up); full content is on the host." We never insert fake text into the terminal content. The host's own scrollback keeps everything (subject to its limit).
- **Controller.** It parses received bytes for at most 4 ms per frame; anything left over goes to the next frame. It only draws the visible rows, and caches each row by the emulator's damage information. The UI frame time p99 must stay ≤ 33 ms.

## 5. Lifecycle and security

- **Lifecycle.**
  - The host keeps at most 8 sessions.
  - A session ends when the shell exits or `Close` is sent. A controller disconnect does not end it.
  - When the host program restarts, sessions are gone. tmux-style persistence across restarts is out of scope; the docs say so.
- **When the shell exits.** The tab stays open and shows "Process exited (code N)"; pressing Enter opens a new one. That matches iTerm2's default. RustDesk closes the tab directly.
- **Permission.** A new `Permissions::terminal` setting, **off by default**. Turning it off immediately disconnects all terminal streams and ends their sessions. Without permission the host replies `Refused` with the reason and where to turn it on.
- **Identity.** The shell runs as the logged-in user:
  - Unix: the login shell from `getpwuid`, started with `-l`;
  - Windows: pwsh → powershell → cmd;
  - Windows service: start it with the logged-in user's token via `WTSQueryUserToken`. If no one is logged in, refuse and say why. **Never hand out a SYSTEM shell.**
- **Environment.** `TERM=xterm-256color`, `COLORTERM=truecolor`, `TERM_PROGRAM=PeerDesk`; inherit the user's locale. On macOS make sure the locale is UTF-8, otherwise Chinese comes out garbled. RustDesk 1.4.7 fixed exactly this.
- **Audit log.** Who (the controller's device code), when, opened, attached, closed and exited. Content is not recorded. Kept in the host log directory.
- **OSC 52.** Writing to the clipboard is off by default. When it is requested, the controller shows a non-blocking prompt bar: "The terminal wants to write to the clipboard [Allow once] [Always allow for this device]".

## 6. Controller (desktop, T2)

- **Rendering.**
  - Monospace font: bundle one with an OFL licence and note it in `THIRD_PARTY_NOTICES.md`; CJK falls back to system fonts.
  - Each row is laid out as colour runs, with wide characters taking two cells.
  - Cursor: block, beam or underline, blinking.
  - Minimum contrast (like iTerm2): if foreground and background are too close, adjust the brightness of the foreground.
- **Input.**
  - xterm encoding: arrow keys follow cursor-key mode, F1–F12, Alt sends an ESC prefix, Ctrl+letter.
  - Pastes are wrapped with bracketed paste; IME uses egui's Ime events.
  - When the app turns on mouse reporting, use SGR 1006.
  - Mouse wheel: on the main screen it scrolls the scrollback; on the alternate screen without mouse mode it is converted to arrow keys (1007).
- **Copy and paste.** Mac uses Cmd+C/V; Windows and Linux use Ctrl+Shift+C/V. Double-click selects a word, triple-click selects a line. Links are clickable (OSC 8 and URL recognition).
- **Tabs.** One session per tab. Opening the terminal first runs `List`, and existing sessions are offered to reattach. Agent users mostly reattach.
- **Colour schemes.** A few built-in ones with clear licences, following light and dark mode. Pick a font and size. Ctrl +/- zooms.
- **Status.** Show the connecting / reconnecting / disconnected states in the tab. While reconnecting, the content stays visible and dimmed, and input is disabled. Once back, it continues from `have`.

## 7. Acceptance (measured; thresholds only go up)

| Item | How to measure | Threshold |
|---|---|---|
| Snapshot fidelity | §3 oracle, all recordings plus random cuts | 100% of cells match |
| Reconnect | Real binaries + simulated network: cut for 30 s, restore, measure until the first keystroke echoes | ≤ 2 s; scrollback has no gap (exact resume) or has a separator line (snapshot) |
| 100 MB output | Host runs `cat` on a 100 MB real log | Host and controller don't stall; UI frame time p99 ≤ 33 ms; Ctrl+C takes effect within 1 s |
| Echo latency | Key → echo appears, over loopback / LAN | p95 ≤ 30 ms; with RTT 50 ms, p95 ≤ RTT + 20 ms |
| Emulation | Major vttest items (screenshot comparison); esctest pass rate | Pass rate only goes up |
| Real Agent | Claude Code full-screen mode runs on all three OSes, and the mouse can click its options | Real-machine checklist |

## 8. Steps

| Step | Content | Who |
|---|---|---|
| T1a | `crates/term`: protocol encoding and decoding, session table, snapshot plus oracle tests (recordings plus random cuts), backpressure state machine (pure logic) | Sonnet, reviewed by main session |
| T1b | PTY glue (Unix portable-pty, Windows ConPTY), host stream handling, permission setting, audit log; net integration test with a real `/bin/sh` and the reconnect measurement | Sonnet |
| T2a | Controller emulator plus egui terminal widget (rendering, input encoding, resize, scrollback), terminal entry in the GUI, tabs | Opus subagent (high quality bar) |
| T2b | Selection, copy and paste, IME, mouse, links, colour schemes and fonts, OSC 52 prompt, status display; screenshot tests | Opus subagent |
| T3 | Android terminal view plus Smart Keys row | Later |
| T4 | BEL, OSC 9/777/133 notifications to the phone | Together with accounts and push |
