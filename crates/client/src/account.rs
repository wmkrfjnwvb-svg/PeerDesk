//! 账户和账户下的设备表（命令行）：`peerdesk account join|status|leave`、`peerdesk devices [rename|remove]`。
//!
//! 设备表以服务器为准（见 `docs/research/N-devices.md`）。这台设备用入表令牌加入账户一次，之后它的身份就是自己的
//! 证书（数据目录里的密钥），不再需要令牌。数据目录里的 `account.txt` 只记“加入了哪台服务器上的哪个账户、编号多少”，
//! 不是凭据：别人拿到它也冒充不了这台设备。
//!
//! 不加入账户的用户不受影响：设备码、指纹连接照旧。

use std::io::Write;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use peerdesk_core::billing::{account_hex, parse_account, AccountId};
use peerdesk_core::crypto::Policy;
use peerdesk_core::devices::Role;
use peerdesk_core::proto::PeerOs;
use peerdesk_core::signal::DeviceRow;
use peerdesk_net::identity::{parse_hex, to_hex};
use peerdesk_net::rendezvous::{AccountSession, Server};
use peerdesk_net::{Fingerprint, Identity, NetError, Node};

use crate::{usage_error, EXIT_FAILED, EXIT_OK};

/// 记“加入了哪个账户”的文件名。
pub const FILE: &str = "account.txt";

/// 这台设备加入的账户。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Membership {
    /// 哪台服务器。
    pub server: SocketAddr,
    /// 服务器的指纹（加入时给了就记下，之后必须一致）。
    pub pin: Option<Fingerprint>,
    /// 账户号。
    pub account: AccountId,
    /// 这台设备在账户里的编号。
    pub row: u32,
}

impl Membership {
    /// 写成文件内容。
    #[must_use]
    pub fn to_text(&self) -> String {
        format!(
            "server={}\npin={}\naccount={}\nrow={}\n",
            self.server,
            self.pin.as_ref().map_or_else(|| "-".to_owned(), to_hex),
            account_hex(&self.account),
            self.row
        )
    }

    /// 从文件内容读；缺项或写坏了为 `None`。
    #[must_use]
    pub fn from_text(text: &str) -> Option<Self> {
        let field = |key: &str| {
            text.lines()
                .filter_map(|l| l.split_once('='))
                .find(|(k, _)| k.trim() == key)
                .map(|(_, v)| v.trim())
        };
        let pin = match field("pin")? {
            "-" => None,
            hex => Some(parse_hex(hex).ok()?),
        };
        Some(Self {
            server: field("server")?.parse().ok()?,
            pin,
            account: parse_account(field("account")?)?,
            row: field("row")?.parse().ok()?,
        })
    }

    /// 读数据目录里的记录；没加入过账户（或文件坏了）为 `None`。
    #[must_use]
    pub fn load(data_dir: &Path) -> Option<Self> {
        Self::from_text(&std::fs::read_to_string(data_dir.join(FILE)).ok()?)
    }

    fn server(&self) -> Server {
        Server {
            addr: self.server,
            pin: self.pin,
        }
    }
}

/// 系统的名字（[`PeerOs`] 的数值）。
#[must_use]
pub const fn os_name(os: u8) -> &'static str {
    match os {
        1 => "Windows",
        2 => "macOS",
        3 => "Linux",
        _ => "未知系统",
    }
}

/// 本机的系统，写成 [`PeerOs`] 的数值。
fn current_os() -> u8 {
    match PeerOs::current() {
        PeerOs::Unknown => 0,
        PeerOs::Windows => 1,
        PeerOs::Mac => 2,
        PeerOs::Linux => 3,
    }
}

/// “多久以前”：`then_s` 为 0 表示不知道。
#[must_use]
pub fn ago(now_s: u64, then_s: u64) -> String {
    if then_s == 0 {
        return "还没有记录".to_owned();
    }
    let d = now_s.saturating_sub(then_s);
    match d {
        0..=59 => "刚刚".to_owned(),
        60..=3_599 => format!("{} 分钟前", d / 60),
        3_600..=86_399 => format!("{} 小时前", d / 3_600),
        _ => format!("{} 天前", d / 86_400),
    }
}

fn json_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len().saturating_add(2));
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c if u32::from(c) < 0x20 => out.push_str(&format!("\\u{:04x}", u32::from(c))),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// 设备列表写成给人看的表。`me` 是本机的编号。
#[must_use]
pub fn table(rows: &[DeviceRow], me: u32, now_s: u64) -> String {
    let mut out = String::from("编号  名称                      系统      角色    上次在线\n");
    for r in rows.iter().filter(|r| !r.removed) {
        let role = if Role::from_id(r.role) == Role::Host {
            "被控"
        } else {
            "控制端"
        };
        let mine = if r.row == me { "（本机）" } else { "" };
        out.push_str(&format!(
            "{:<5} {:<25} {:<9} {:<7} {}{mine}\n",
            r.row,
            r.name,
            os_name(r.os),
            role,
            ago(now_s, r.last_seen_s),
        ));
    }
    out
}

/// 设备列表写成 JSON（给脚本和以后的 MCP 用）。不含指纹：用编号指设备。
#[must_use]
pub fn json(rows: &[DeviceRow], me: u32, rev: u64) -> String {
    let items: Vec<String> = rows
        .iter()
        .filter(|r| !r.removed)
        .map(|r| {
            format!(
                "{{\"id\":{},\"name\":{},\"os\":{},\"role\":{},\"last_seen\":{},\"this_device\":{}}}",
                r.row,
                json_string(&r.name),
                json_string(os_name(r.os)),
                json_string(if Role::from_id(r.role) == Role::Host {
                    "host"
                } else {
                    "controller"
                }),
                r.last_seen_s,
                r.row == me
            )
        })
        .collect();
    format!("{{\"rev\":{rev},\"devices\":[{}]}}", items.join(","))
}

fn wall_s() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// 用本机身份连上服务器，开一条账户会话，执行 `f`。
fn with_session<T>(
    data_dir: &Path,
    server: &Server,
    f: impl AsyncFnOnce(&mut AccountSession) -> Result<T, NetError>,
) -> Result<T, NetError> {
    let identity = Identity::load_or_create(data_dir)?;
    crate::remote::runtime()?.block_on(async {
        let bind: SocketAddr = if server.addr.is_ipv6() {
            ([0u16; 8], 0).into()
        } else {
            ([0, 0, 0, 0], 0).into()
        };
        let node = Node::bind(bind, &identity, &Policy::recommended())?;
        let result = tokio::time::timeout(std::time::Duration::from_secs(20), async {
            let mut session = AccountSession::open(&node, server).await?;
            let result = f(&mut session).await;
            session.close();
            result
        })
        .await
        .unwrap_or(Err(NetError::Timeout("服务器 20 秒没有回应")));
        node.close();
        result
    })
}

/// 命令行参数。
#[derive(Debug, Default, PartialEq, Eq)]
struct Args {
    server: Option<SocketAddr>,
    pin: Option<Fingerprint>,
    token: Option<String>,
    name: Option<String>,
    controller: bool,
    json: bool,
    data_dir: Option<PathBuf>,
    /// 不带横线的参数（子命令之后的）。
    rest: Vec<String>,
}

fn parse(args: &[String]) -> Result<Args, String> {
    let mut parsed = Args::default();
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        let mut value = |flag: &str| {
            it.next()
                .cloned()
                .ok_or_else(|| format!("{flag} 需要一个值"))
        };
        match arg.as_str() {
            "--server" => {
                let v = value(arg)?;
                parsed.server = Some(v.parse().map_err(|_| format!("--server 的值不对：{v}"))?);
            }
            "--server-pin" => {
                parsed.pin = Some(parse_hex(&value(arg)?).map_err(|e| e.to_string())?);
            }
            "--token" => parsed.token = Some(value(arg)?),
            "--name" => parsed.name = Some(value(arg)?),
            "--data-dir" => parsed.data_dir = Some(PathBuf::from(value(arg)?)),
            "--controller" => parsed.controller = true,
            "--json" => parsed.json = true,
            flag if flag.starts_with("--") => return Err(format!("未知选项：{flag}")),
            _ => parsed.rest.push(arg.clone()),
        }
    }
    Ok(parsed)
}

fn failed(out: &mut dyn Write, e: &NetError) -> u8 {
    let _ = writeln!(out, "错误：{e}");
    EXIT_FAILED
}

const NOT_JOINED: &str =
    "这台设备还没有加入账户。先运行：peerdesk account join --server <地址> --token <入表令牌>";

/// `peerdesk account join|status|leave`。
pub fn cmd_account(args: &[String], out: &mut dyn Write) -> u8 {
    let Some((sub, rest)) = args.split_first() else {
        return usage_error(out, "account 需要子命令：join、status 或 leave");
    };
    let parsed = match parse(rest) {
        Ok(p) if p.rest.is_empty() => p,
        Ok(p) => return usage_error(out, &format!("多余的参数：{}", p.rest.join(" "))),
        Err(e) => return usage_error(out, &e),
    };
    let data_dir = parsed
        .data_dir
        .clone()
        .unwrap_or_else(crate::remote::default_data_dir);
    match sub.as_str() {
        "join" => {
            let (Some(addr), Some(token)) = (parsed.server, parsed.token.as_deref()) else {
                return usage_error(out, "account join 需要 --server 和 --token");
            };
            let server = Server {
                addr,
                pin: parsed.pin,
            };
            let name = parsed
                .name
                .clone()
                .filter(|n| !n.trim().is_empty())
                .unwrap_or_else(crate::local::device_name);
            // 取不到电脑名时给一个能认出来的名字，之后可以改
            let name = if name.trim().is_empty() {
                format!("{} 电脑", os_name(current_os()))
            } else {
                name
            };
            let role = if parsed.controller {
                Role::Controller
            } else {
                Role::Host
            };
            let result = with_session(&data_dir, &server, async |s| {
                s.enroll(token, &name, current_os(), role).await
            });
            match result {
                Ok(enrolled) => {
                    let membership = Membership {
                        server: addr,
                        pin: parsed.pin,
                        account: enrolled.account,
                        row: enrolled.row,
                    };
                    if let Err(e) = std::fs::create_dir_all(&data_dir)
                        .and_then(|()| std::fs::write(data_dir.join(FILE), membership.to_text()))
                    {
                        let _ = writeln!(
                            out,
                            "已加入账户，但记不下来（{}：{e}）。检查数据目录的权限后再运行一次 join（不会重复加入）",
                            data_dir.display()
                        );
                        return EXIT_FAILED;
                    }
                    let _ = writeln!(
                        out,
                        "已加入账户 {}，这台设备叫“{name}”，编号 {}",
                        account_hex(&enrolled.account),
                        enrolled.row
                    );
                    EXIT_OK
                }
                Err(e) => failed(out, &e),
            }
        }
        "status" => {
            match Membership::load(&data_dir) {
                Some(m) => {
                    let _ = writeln!(
                        out,
                        "账户 {}，编号 {}，服务器 {}",
                        account_hex(&m.account),
                        m.row,
                        m.server
                    );
                }
                None => {
                    let _ = writeln!(out, "{NOT_JOINED}");
                }
            }
            EXIT_OK
        }
        "leave" => {
            let Some(m) = Membership::load(&data_dir) else {
                let _ = writeln!(out, "{NOT_JOINED}");
                return EXIT_FAILED;
            };
            // 编号 0：自己退出
            match with_session(&data_dir, &m.server(), async |s| s.remove(0).await) {
                Ok(_)
                | Err(NetError::Refused(peerdesk_core::devices::Refusal::NotMember)) => {
                    // 服务器说已经不在账户里了（被别的设备移除了）：本机的记录也清掉
                    let _ = std::fs::remove_file(data_dir.join(FILE));
                    let _ = writeln!(
                        out,
                        "已退出账户 {}。别人仍然可以用设备码和密码连这台电脑；要重新加入，用入表令牌再 join 一次",
                        account_hex(&m.account)
                    );
                    EXIT_OK
                }
                Err(e) => failed(out, &e),
            }
        }
        other => usage_error(out, &format!("account 没有这个子命令：{other}")),
    }
}

/// `peerdesk devices [--json]`、`peerdesk devices rename <编号> <新名字>`、`peerdesk devices remove <编号>`。
pub fn cmd_devices(args: &[String], out: &mut dyn Write) -> u8 {
    let parsed = match parse(args) {
        Ok(p) => p,
        Err(e) => return usage_error(out, &e),
    };
    let data_dir = parsed
        .data_dir
        .clone()
        .unwrap_or_else(crate::remote::default_data_dir);
    let words: Vec<&str> = parsed.rest.iter().map(String::as_str).collect();
    let row_of = |text: &str| text.parse::<u32>().ok().filter(|r| *r > 0);
    enum Op<'a> {
        List,
        Rename(u32, &'a str),
        Remove(u32),
    }
    let op = match words.as_slice() {
        [] => Op::List,
        ["rename", row, name] => match row_of(row) {
            Some(row) => Op::Rename(row, name),
            None => return usage_error(out, "devices rename 的编号应为正整数"),
        },
        ["remove", row] => match row_of(row) {
            Some(row) => Op::Remove(row),
            None => return usage_error(out, "devices remove 的编号应为正整数"),
        },
        _ => {
            return usage_error(
                out,
                "用法：devices [--json] | devices rename <编号> <新名字> | devices remove <编号>",
            )
        }
    };
    let Some(m) = Membership::load(&data_dir) else {
        let _ = writeln!(out, "{NOT_JOINED}");
        return EXIT_FAILED;
    };
    let server = m.server();
    match op {
        Op::List => match with_session(&data_dir, &server, async |s| s.devices(0).await) {
            Ok(all) => {
                let text = if parsed.json {
                    format!("{}\n", json(&all.rows, m.row, all.rev))
                } else {
                    table(&all.rows, m.row, wall_s())
                };
                let _ = write!(out, "{text}");
                EXIT_OK
            }
            Err(e) => failed(out, &e),
        },
        Op::Rename(row, name) => {
            match with_session(&data_dir, &server, async |s| s.rename(row, name).await) {
                Ok(_) => {
                    let _ = writeln!(out, "{row} 号设备改名为“{name}”");
                    EXIT_OK
                }
                Err(e) => failed(out, &e),
            }
        }
        Op::Remove(row) => {
            match with_session(&data_dir, &server, async |s| s.remove(row).await) {
                Ok(_) => {
                    let _ = writeln!(
                        out,
                        "{row} 号设备已移出账户（它不再出现在列表里；别人仍然可以用设备码和密码连它）"
                    );
                    if row == m.row {
                        let _ = std::fs::remove_file(data_dir.join(FILE));
                    }
                    EXIT_OK
                }
                Err(e) => failed(out, &e),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::indexing_slicing)]
    use super::*;
    use crate::EXIT_USAGE;
    use peerdesk_core::billing::issue_enroll;
    use peerdesk_core::devices::MemoryStore;
    use peerdesk_net::service::{Accounts, LocalDirectory, SignalService};
    use std::sync::Arc;

    const KEY: &[u8] = b"billing secret";
    const A: AccountId = [0xa1; 8];

    type Cmd = fn(&[String], &mut dyn Write) -> u8;

    fn run(f: Cmd, args: &[&str]) -> (u8, String) {
        let args: Vec<String> = args.iter().map(|a| (*a).to_owned()).collect();
        let mut out = Vec::new();
        let code = f(&args, &mut out);
        (code, String::from_utf8(out).unwrap())
    }

    fn temp(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("peerdesk-account-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// 一台开了（或没开）账户设备表的服务器。
    struct TestServer {
        addr: String,
        pin: String,
        _signal: SignalService,
        _rt: tokio::runtime::Runtime,
    }

    fn server(with_accounts: bool) -> TestServer {
        let rt = crate::remote::runtime().unwrap();
        let id = Identity::generate().unwrap();
        let (addr, signal) = rt.block_on(async {
            let node = Arc::new(
                Node::bind(([127, 0, 0, 1], 0).into(), &id, &Policy::recommended()).unwrap(),
            );
            let addr = node.local_addr().unwrap().to_string();
            let accounts = with_accounts.then(|| Accounts {
                store: Arc::new(MemoryStore::new()),
                key: KEY.to_vec(),
            });
            let signal = SignalService::start_with_accounts(
                node,
                None,
                Arc::new(LocalDirectory::new(None, None)),
                None,
                accounts,
            );
            (addr, signal)
        });
        TestServer {
            addr,
            pin: to_hex(&id.fingerprint()),
            _signal: signal,
            _rt: rt,
        }
    }

    #[test]
    fn membership_file_round_trips() {
        let m = Membership {
            server: "203.0.113.5:4433".parse().unwrap(),
            pin: Some([7; 32]),
            account: A,
            row: 12,
        };
        assert_eq!(Membership::from_text(&m.to_text()), Some(m));
        let no_pin = Membership { pin: None, ..m };
        assert_eq!(Membership::from_text(&no_pin.to_text()), Some(no_pin));
        for bad in [
            "",
            "server=x\npin=-\naccount=a1a1a1a1a1a1a1a1\nrow=1\n",
            "server=1.2.3.4:5\npin=zz\naccount=a1a1a1a1a1a1a1a1\nrow=1\n",
            "server=1.2.3.4:5\npin=-\naccount=xyz\nrow=1\n",
            "server=1.2.3.4:5\npin=-\naccount=a1a1a1a1a1a1a1a1\nrow=x\n",
            "server=1.2.3.4:5\npin=-\naccount=a1a1a1a1a1a1a1a1\n",
        ] {
            assert_eq!(Membership::from_text(bad), None, "{bad:?}");
        }
        assert_eq!(Membership::load(&temp("nofile")), None);
    }

    #[test]
    fn lists_are_readable_and_json_hides_fingerprints() {
        let row = |n: u32, name: &str, os, role: Role, seen, removed| DeviceRow {
            row: n,
            fingerprint: [0xee; 32],
            name: name.to_owned(),
            os,
            role: role.id(),
            last_seen_s: seen,
            removed,
        };
        let rows = [
            row(1, "mac-mini", 2, Role::Host, 0, false),
            row(2, "办公室 \"PC\"", 1, Role::Controller, 1_000, false),
            row(3, "走了的", 3, Role::Host, 1_000, true),
        ];
        let text = table(&rows, 2, 1_000 + 7_200);
        assert!(text.contains("mac-mini") && text.contains("macOS") && text.contains("被控"));
        assert!(text.contains("2 小时前（本机）"), "{text}");
        assert!(text.contains("还没有记录") && !text.contains("走了的"), "{text}");
        let j = json(&rows, 2, 9);
        assert_eq!(
            j,
            "{\"rev\":9,\"devices\":[\
             {\"id\":1,\"name\":\"mac-mini\",\"os\":\"macOS\",\"role\":\"host\",\"last_seen\":0,\"this_device\":false},\
             {\"id\":2,\"name\":\"办公室 \\\"PC\\\"\",\"os\":\"Windows\",\"role\":\"controller\",\"last_seen\":1000,\"this_device\":true}]}"
        );
        assert!(!j.contains("eeee"), "不给指纹");
        assert_eq!(json_string("a\\b\nc"), "\"a\\\\b\\u000ac\"");
        assert_eq!(
            [ago(100, 0), ago(100, 90), ago(1_000, 100), ago(100_000, 100), ago(1_000_000, 100)],
            ["还没有记录", "刚刚", "15 分钟前", "27 小时前", "11 天前"]
        );
        assert_eq!(
            [os_name(0), os_name(1), os_name(2), os_name(3), os_name(9)],
            ["未知系统", "Windows", "macOS", "Linux", "未知系统"]
        );
        assert!(current_os() <= 3);
        assert!(wall_s() > 1_700_000_000);
    }

    #[test]
    fn usage_errors_are_explained() {
        let cases: [(Cmd, Vec<&str>); 13] = [
            (cmd_account, vec![]),
            (cmd_account, vec!["frobnicate"]),
            (cmd_account, vec!["join"]),
            (cmd_account, vec!["join", "--server", "nope", "--token", "t"]),
            (cmd_account, vec!["join", "--server"]),
            (cmd_account, vec!["join", "--server-pin", "zz"]),
            (cmd_account, vec!["join", "--bogus"]),
            (cmd_account, vec!["status", "extra"]),
            (cmd_devices, vec!["--bogus"]),
            (cmd_devices, vec!["rename", "x", "name"]),
            (cmd_devices, vec!["rename", "0", "name"]),
            (cmd_devices, vec!["remove", "x"]),
            (cmd_devices, vec!["what"]),
        ];
        for (f, args) in cases {
            assert_eq!(run(f, &args).0, EXIT_USAGE, "{args:?}");
        }
    }

    /// 闭环：两台设备（两个数据目录）加入同一个账户，互相列得出；改名、移除、退出。
    #[test]
    fn join_list_rename_remove_and_leave() {
        let s = server(true);
        let (d1, d2) = (temp("d1"), temp("d2"));
        let (p1, p2) = (d1.to_str().unwrap(), d2.to_str().unwrap());
        let token = issue_enroll(KEY, A, u64::MAX / 2, 1);
        // 没加入：先说怎么加入
        let cases: [(Cmd, Vec<&str>); 2] = [
            (cmd_devices, vec!["--data-dir", p1]),
            (cmd_account, vec!["leave", "--data-dir", p1]),
        ];
        for (f, args) in cases {
            let (code, text) = run(f, &args);
            assert_eq!(code, EXIT_FAILED);
            assert!(text.contains("peerdesk account join"), "{text}");
        }
        let (code, text) = run(cmd_account, &["status", "--data-dir", p1]);
        assert_eq!(code, EXIT_OK);
        assert!(text.contains("还没有加入账户"), "{text}");
        // 令牌不对：说原因
        let (code, text) = run(
            cmd_account,
            &["join", "--server", &s.addr, "--token", "junk", "--data-dir", p1],
        );
        assert_eq!(code, EXIT_FAILED);
        assert!(text.contains("入表令牌不对"), "{text}");
        assert_eq!(Membership::load(&d1), None);
        // 加入
        let (code, text) = run(
            cmd_account,
            &[
                "join", "--server", &s.addr, "--server-pin", &s.pin, "--token", &token, "--name",
                "书房的 Mac", "--data-dir", p1,
            ],
        );
        assert_eq!(code, EXIT_OK, "{text}");
        assert!(text.contains("编号 1") && text.contains("书房的 Mac"), "{text}");
        let (code, text) = run(
            cmd_account,
            &["join", "--server", &s.addr, "--token", &token, "--controller", "--data-dir", p2],
        );
        assert_eq!(code, EXIT_OK, "{text}");
        assert_eq!(Membership::load(&d2).unwrap().row, 2);
        assert_eq!(Membership::load(&d1).unwrap().pin.map(|p| to_hex(&p)), Some(s.pin.clone()));
        let (_, text) = run(cmd_account, &["status", "--data-dir", p1]);
        assert!(text.contains("a1a1a1a1a1a1a1a1") && text.contains("编号 1"), "{text}");
        // 列出：两台都在，标出本机
        let (code, text) = run(cmd_devices, &["--data-dir", p2]);
        assert_eq!(code, EXIT_OK, "{text}");
        assert!(text.contains("书房的 Mac") && text.contains("控制端"), "{text}");
        assert_eq!(text.matches("（本机）").count(), 1);
        let (code, text) = run(cmd_devices, &["--json", "--data-dir", p1]);
        assert_eq!(code, EXIT_OK);
        assert!(text.starts_with("{\"rev\":2,\"devices\":[{\"id\":1,\"name\":\"书房的 Mac\""), "{text}");
        // 改名、改不存在的
        let (code, text) = run(cmd_devices, &["rename", "2", "笔记本", "--data-dir", p1]);
        assert_eq!(code, EXIT_OK, "{text}");
        assert!(run(cmd_devices, &["--data-dir", p1]).1.contains("笔记本"));
        let (code, text) = run(cmd_devices, &["rename", "9", "x", "--data-dir", p1]);
        assert_eq!(code, EXIT_FAILED);
        assert!(text.contains("没有这个编号"), "{text}");
        // 第一台把第二台移出账户：第二台再查就被拒绝并说明；它 leave 时本机记录清掉
        let (code, text) = run(cmd_devices, &["remove", "2", "--data-dir", p1]);
        assert_eq!(code, EXIT_OK, "{text}");
        let (code, text) = run(cmd_devices, &["--data-dir", p2]);
        assert_eq!(code, EXIT_FAILED);
        assert!(text.contains("不在账户里"), "{text}");
        let (code, _) = run(cmd_account, &["leave", "--data-dir", p2]);
        assert_eq!(code, EXIT_OK);
        assert_eq!(Membership::load(&d2), None);
        // 第一台移除自己（等于退出）：本机记录清掉
        let (code, _) = run(cmd_devices, &["remove", "1", "--data-dir", p1]);
        assert_eq!(code, EXIT_OK);
        assert_eq!(Membership::load(&d1), None);
        // 重新加入再 leave
        run(cmd_account, &["join", "--server", &s.addr, "--token", &token, "--data-dir", p1]);
        assert_eq!(Membership::load(&d1).unwrap().row, 3);
        let (code, text) = run(cmd_account, &["leave", "--data-dir", p1]);
        assert_eq!(code, EXIT_OK, "{text}");
        assert!(text.contains("已退出账户"), "{text}");
        for d in [d1, d2] {
            let _ = std::fs::remove_dir_all(d);
        }
    }

    /// 回归（兼容）：服务器没开账户设备表（老服务器对客户端也是这个样子）时，说清楚原因，不留下半截记录。
    #[test]
    fn a_server_without_device_tables_says_so() {
        let s = server(false);
        let dir = temp("old");
        let p = dir.to_str().unwrap();
        let token = issue_enroll(KEY, A, u64::MAX / 2, 1);
        let (code, text) = run(
            cmd_account,
            &["join", "--server", &s.addr, "--token", &token, "--data-dir", p],
        );
        assert_eq!(code, EXIT_FAILED);
        assert!(text.contains("没有开账户设备表") && text.contains("升级服务器"), "{text}");
        assert_eq!(Membership::load(&dir), None);
        // 服务器连不上：报错，不挂住
        let (code, text) = run(
            cmd_account,
            &["join", "--server", "127.0.0.1:9", "--token", &token, "--data-dir", p],
        );
        assert_eq!(code, EXIT_FAILED);
        assert!(text.starts_with("错误："), "{text}");
        let _ = std::fs::remove_dir_all(dir);
    }
}
