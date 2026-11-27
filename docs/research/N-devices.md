# N. 核心用户第 1 阶段骨干：账户设备表、在线状态推送、本机 MCP（只读）方案（2026-11-27）

只是方案，没写代码、没编译。承接 `reports/PeerDesk 核心用户功能设计.md` 第 4、5、7 节（下面叫“设计文档”）。
标记：【已核实】本次打开了原文；【读代码】读了 dev 上的代码；【推断】自己的判断；【未核实】没查到。

## 0. 结论先行

1. **三件事共用一条骨干**：服务器上有一张“账户 → 设备”的表；控制端和 MCP 进程在一条长连接上订阅这张表的变化（在线、离线、改名、移除）；`peerdesk mcp` 只是这条订阅的另一个读者。
2. **分 6 步，每步能单独合进 dev**，合计约 3.5–4.5 周（一名熟悉代码的工程师，推断）：

   | 步 | 做什么 | 闭环（做完能演示什么） | 工作量 |
   |---|---|---|---|
   | S0 | 信令加能力协商（消息尾部追加一个能力字段） | 新客户端连老服务器不掉线，日志里说明“服务器版本旧” | 0.5–1 天 |
   | S1 | 账户设备表：入表、列出、改名、移除；命令行 `peerdesk account join`、`peerdesk devices` | 三台真实二进制入同一个账户，`peerdesk devices` 列出三台 | 4–5 天 |
   | S2 | 在线状态订阅推送（取代 `presence` 分支的轮询） | 300 台模拟设备上下线，订阅端几秒内看到，一次不漏 | 4–5 天 |
   | S3 | 设备自报状态（版本、系统、无人值守、是否有人在控制） | `peerdesk devices --watch` 看到某台“有人正在控制” | 2 天 |
   | S4 | 本机 MCP：`peerdesk mcp`（stdio，只读两到三个工具） | Claude Code 里问“我有哪些电脑在线”，答得出来 | 3–4 天 |
   | S5 | 图形界面接入：设备列表用服务器的表和推送的状态 | 列表里的状态点随真实上下线变化 | 3 天（和界面改版对齐） |

3. **和设计文档、常规做法不同的地方**（理由见第 5 节）：
   - 设备入表用**新的“入表令牌”**，不直接用现有的中继计费令牌。
   - 阶段 1 的存储是**文件日志加内存**，藏在一个接口后面；集群里同一个账户的设备和订阅者**都连同一个节点**（账户亲和），不做跨节点同步。
   - `presence` 分支的轮询**不合并**，直接换成订阅；老服务器上状态显示“未知（服务器版本旧）”。
   - MCP 服务器做成**两个时代都支持**（新规范的每请求元数据，和旧规范的 `initialize` 握手）。
4. **要产品负责人决定的有 8 条**，在第 6 节，每条有建议。最要紧的三条：被控端从“不需要账户”变成“要入账户”，免费用户的账户怎么来；阶段 1 用文件还是直接上数据库；掉电这类不告而别的离线，10 秒才发现能不能接受。

## 1. 现状和约束【读代码，dev 3ca4b71】

- **身份**：设备的身份是它的证书指纹（`signal::DeviceId`，32 字节），由 QUIC 连接本身证明，信令消息里不带。
- **信令**（`core/signal.rs`、`net/service.rs`、`net/rendezvous.rs`）：
  - 被控端开一条信令流发 `Register`，这条流一直开着，服务器在上面推 `Incoming`。流断了服务器就认为它下线（`Registry::disconnect`）。
  - 控制端每次连接临时开一条流发 `Connect` / `ConnectCode`。
  - 消息是“一个字节的种类 + 字段”。**解码器允许消息尾部多出字段**（`decode` 末尾的注释：新版本可以在消息末尾追加字段，老版本忽略）。
  - **不认识的消息种类会让服务器关掉这条流**（`serve_device` 里 `Err(_) => break Ok(())`，随后 `link.close`）。所以新消息不能直接发给老服务器：被控端的登记会跟着断。
  - 连接的空闲超时 10 秒、保活 2 秒（`net/tls.rs`）。设备掉电时服务器最多 10 秒后才知道。
- **账户**（`core/billing.rs`、`server/accounting.rs`）：
  - 账户号 8 字节。令牌 `pdk1-<账户号>-<每月中继额度>-<签名>`，服务器用计费密钥做 HMAC，集群所有节点同一个密钥，**不需要账户数据库**。
  - 令牌没有有效期，不能单独吊销（只能换计费密钥）。
  - 现在只有控制端在 `Connect` 里带令牌，“被控端不需要账户”。
- **集群**（`core/cluster.rs`、`server/node.rs`）：节点之间用带 HMAC 的 UDP 心跳互相发现，心跳里顺带各节点本月的中继用量。没有共享存储。设备登记只在它连上的那个节点的内存里。
- **服务器的数据目录**里现在是文本文件：设备码簿、按月的用量日志（一行一笔，追加写）。
- **客户端**：设备簿是本机 `devices.txt`（`gui/book.rs`），已经为同步留了稳定 id、修改时间、墓碑。没有 JSON 解析库（`server/json.rs` 只会输出）。
- **`presence` 分支**（没编译过）：`Presence` / `PresenceReply` 两个消息，30 秒问一次，一次最多 64 台，服务器按来源 IP 限频。

## 2. 业内怎么做

### 2.1 Tailscale：设备列表和在线状态【已核实】

来源：[`tailcfg/tailcfg.go`](https://raw.githubusercontent.com/tailscale/tailscale/main/tailcfg/tailcfg.go)（协调服务器和客户端之间的数据结构）、[Auth keys 文档](https://tailscale.com/kb/1085/auth-keys)。

- **长连接加增量**：客户端发 `MapRequest{Stream: true}`，“the client wants to receive multiple MapResponses over the same HTTP connection”。第一条回复带完整的 `Peers`（“It will be set in the first MapResponse for a long-polled request/response”），之后是增量：`PeersChanged`、`PeersRemoved`，以及更轻的 `PeersChangedPatch`。
- **在线状态是单独的小增量**：`OnlineChange map[NodeID]bool`（“changes the value of a Peer Node.Online value”）、`PeerSeenChange`（值为 false 表示“the peer is gone”，为 true 表示 LastSeen 是现在）。
- **在线的定义**：`Node.Online` 是“whether the node is currently connected to the coordination server”；nil 表示“unknown, or the current node doesn't have permission to know”。`LastSeen`“is not updated when Online is true”。
- **保活**：`MapResponse.KeepAlive`“represents an empty message just to keep the connection alive”。
- **入网凭据和设备身份分开**：auth key“let you register new nodes without needing to sign in using a web browser”，“authenticates a device as the user who generated the key”；有效期 1–90 天；可一次性、可复用、可预批准、可自动打标签。**“Revoking a key does not deauthorize nodes using the key”**，过期也一样（“any device authorized by it remains authorized until its node key expires”）。要去掉设备得在设备页删。

对我们的含义：订阅式推送、首条全量后续增量、在线状态单独走小增量、“未知”和“不在线”分开、入表凭据用完就和设备身份无关——这些都照做。

### 2.2 MCP 规范（2026-07-28 版）【已核实】

来源：[传输总览](https://modelcontextprotocol.io/specification/latest/basic/transports)、[stdio](https://modelcontextprotocol.io/specification/2026-07-28/basic/transports/stdio.md)、[版本与兼容](https://modelcontextprotocol.io/specification/2026-07-28/basic/versioning.md)、[授权](https://modelcontextprotocol.io/specification/2026-07-28/basic/authorization)、[Tools](https://modelcontextprotocol.io/specification/latest/server/tools)。

- **stdio 的分帧**：客户端把服务器当子进程启动；“Messages are delimited by newlines, and MUST NOT contain embedded newlines”；“The server MUST NOT write anything to its stdout that is not a valid MCP message”；日志可以写 stderr。标准输入关了服务器应当尽快退出。
- **没有会话**：“There is no negotiation handshake. Every request carries its protocol version”，放在 `_meta.io.modelcontextprotocol/*` 里。服务器“MUST implement `server/discover`”。服务器不能主动发请求（“The server MUST NOT write JSON-RPC requests to stdout”）。
- **新旧两个时代**：2025-11-25 及更早的版本要先 `initialize` 握手。服务器“MAY implement both behaviors”；只支持新版的服务器遇到旧客户端是“Fails … Legacy clients have no fall-forward mechanism”。双时代的服务器按客户端怎么开头来选：带新式 `_meta` 的请求按新版无状态处理，`initialize` 请求按旧版处理。
- **stdio 不走 OAuth**：“Implementations using an STDIO transport SHOULD NOT follow this specification, and instead retrieve credentials from the environment.”
- **工具**：`tools/list`、`tools/call`；结果里 `content` 加可选的 `structuredContent`（“a tool that returns structured content SHOULD also return the serialized JSON in a TextContent block”）；业务错误用 `isError: true`，不认识的工具是 JSON-RPC 错误 `-32602`。工具列表“SHOULD return tools in a deterministic order”。
- **服务器必须做的**：“Validate all tool inputs / Implement proper access controls / Rate limit tool invocations / Sanitize tool outputs”。注解不可信：“clients MUST consider tool annotations to be untrusted unless they come from trusted servers”。
- **没读到的**：`ToolAnnotations` 的字段定义（`readOnlyHint` 等）在 schema 页的后半部分，页面太长没取到。实现 S4 之前要对着 schema 原文核对字段名和默认值。【未核实】

## 3. 数据模型和接口

### 3.1 设备属于账户

```
账户 Account
  id            8 字节，就是现在计费用的 AccountId（不变）
  rev           账户内单调递增的修订号：这张表每改一次加 1（订阅端凭它续传）

设备 Device（主键：账户 id + 设备指纹）
  fingerprint   32 字节，证书指纹（现有的 DeviceId）
  row           账户内的稳定编号（u32，从 1 起，不复用）——给界面和 MCP 用，不把指纹暴露出去
  name          电脑名（设备入表时报，之后账户成员可以改；最长 64 个字）
  os            系统（现有的 PeerOs）
  role          能被控 / 只做控制端（入表时报）
  added_at      入表时间
  changed_rev   最近一次修改时的账户修订号
  removed       墓碑：移除时的修订号（留 30 天，之后清掉）

运行时状态（只在内存里，不落盘；重启后从设备重新登记恢复）
  online        现在有没有登记着的信令流
  last_seen     上次在线的时间（下线时落盘，在线时不更新，和 Tailscale 一样）
  status        设备自报的状态（S3）：程序版本、无人值守开没开、有没有人在控制、状态时间
```

- **计费挂账户**：计费仍然只看 `AccountId`，这张表不参与计费判断。设备数的软上限（设计文档第 8 节）以后在入表时按账户档位检查，接口上入表可以被拒绝并说明原因。
- **设备指纹重新生成**：等于一台新设备，要重新入表；旧的那行不会再上线，由用户移除（或者以后按“多久没上线”自动清）。账户和计费不受影响。
- **一个指纹只能属于一个账户**。已经在别的账户里的设备入表会被拒绝并说明，先在原账户移除。
- **克隆镜像**造成两台机器同一个指纹：阶段 1 表现为一台设备在两个地址之间跳。先只在服务器日志里记“同一指纹同时从两个地址登记”，S3 把它做成状态里的一项。

### 3.2 最小的账户身份，和以后怎么接正式账户

阶段 1 没有登录。账户由运营方用服务器命令发出来（托管云是我们发，自建是管理员自己发）：

- `peerdesk-server account new` → 打印账户号和一枚**入表令牌**。
- 入表令牌：`pde1-<账户号>-<过期时间>-<随机数>-<签名>`，和计费令牌用同一个计费密钥签，但前缀不同、互不通用。默认 7 天有效，可以指定 1–90 天；可以多次使用（部署一批机器用同一枚）。
- 设备入表以后，**身份就是它的证书指纹**：之后的登记、列表、订阅都不再带令牌，服务器按指纹查它属于哪个账户。令牌过期或者泄漏，不影响已经入表的设备；要去掉设备就移除它（和 Tailscale 的 auth key 一样）。

以后接正式账户（待办第 5 项）时：
- 账户表多出邮箱、登录方式、订阅档位；**账户号不变，设备表不变**。
- “登录”在服务器内部就是给这台设备签一枚入表令牌再入表，协议不用改。
- 计费令牌 `pdk1` 可以继续用于中继额度，也可以改成“设备已入表的账户自动带额度”，那时控制端 `Connect` 里的令牌就可以省掉。

### 3.3 接口（存储）

```rust
/// 账户设备表的存储。阶段 1 的实现是“追加日志 + 内存”，以后换数据库只换这个实现。
pub trait DeviceStore: Send + Sync {
    fn account_of(&self, device: &DeviceId) -> Option<AccountId>;
    fn enroll(&self, account: AccountId, device: DeviceInfo) -> Result<Enrolled, EnrollError>;
    fn list(&self, account: AccountId, since_rev: u64) -> Changes;   // 全量（since 为 0）或增量
    fn rename(&self, account: AccountId, row: u32, name: &str) -> Result<u64, StoreError>;
    fn remove(&self, account: AccountId, row: u32) -> Result<u64, StoreError>;
    fn seen(&self, device: &DeviceId, unix_s: u64);                  // 下线时记 last_seen
}
```

纯逻辑（表、修订号、增量、墓碑、订阅者的扇出）放在 `core` 里，不做 IO，像 `signal::Registry` 那样单元测试；文件读写和网络在 `server`、`net`。

## 4. 分步方案

每步的测试都按 CLAUDE.md：纯逻辑写单元测试；真实二进制的闭环放进现有的 `crates/net/tests` 和闭环脚本；覆盖率门槛 99% 不降；改了 `core`、`net`、`server` 会触发端到端任务。和传输无关，不用跑 144 次测量。

### S0 信令能力协商

- **为什么先做**：老服务器遇到不认识的消息会关流（第 1 节），所以客户端必须先知道服务器会什么，再发新消息。
- **协议**：
  - `Registered`（服务器 → 被控端）尾部追加 `features: u32`。
  - `Checked`（服务器 → 任何客户端，回答 `Check`）尾部追加 `features: u32`。控制端和 MCP 进程不登记，用 `Check` 问。
  - 老服务器不追加，客户端读到的是 0；老客户端忽略多出的字段。不用改流的种类，不占握手的能力位（`proto::Caps` 是会话握手用的，和信令无关）。
  - 位：`DEVICES = 1`（S1 的消息）、`WATCH = 2`（S2）、`STATUS = 4`（S3）。
- **服务端**：两处回复带上自己支持的位。
- **客户端**：`rendezvous` 里把 `features` 存进 `Registration` 和一个新的 `ServerInfo`。
- **测试**：编码往返；老格式（没有尾部字段）解出 0；新客户端对老服务器的集成测试（用一个不追加字段的假服务器）断言“不发新消息、登记不断”。
- **回归测试**：把“不认识的消息让登记断线”这件事本身写成测试，守住以后加消息的人。
- **影响**：无行为变化。

### S1 账户设备表

- **协议**（都在信令流上，只在服务器有 `DEVICES` 位时发）：

  | 消息 | 方向 | 内容 | 回复 |
  |---|---|---|---|
  | `Enroll` | 设备 → 服务器 | 入表令牌、电脑名、系统、角色 | `Enrolled{账户号, row, rev}` 或 `Refused{原因码, 说明}` |
  | `Devices` | 成员 → 服务器 | `since_rev` | 若干条 `DeviceRows{rev, 行…, more}`（分页，每条不超过信令消息长度上限） |
  | `Rename` | 成员 → 服务器 | row、新名字 | `Changed{rev}` 或 `Refused` |
  | `Remove` | 成员 → 服务器 | row | `Changed{rev}` 或 `Refused` |
  | `Leave` | 设备 → 服务器 | 无（自己退出账户） | `Changed{rev}` |

  - “成员”就是这条连接的证书指纹在表里、没被移除。不是成员一律 `Refused{不是这个账户的设备}`，不泄露账户里有什么。
  - `Refused` 的原因码固定几种，每种在客户端有一句“原因加解决办法”的话（令牌过期、令牌不对、已经在别的账户、账户设备数到上限、名字太长）。
- **服务端**：
  - `core/devices.rs`（新）：表、修订号、增量、墓碑；入表令牌的签发和验证放 `core/billing.rs` 旁边。
  - `server`：`FileDeviceStore`——`数据目录/devices/devices.log` 追加写（一行一笔：入表、改名、移除、last_seen），启动时读回；写了一半的最后一行跳过（和用量日志同样的做法）。日志超过一定大小时重写成快照。
  - 命令：`peerdesk-server account new`、`account token --account <号> --days <N>`、`account devices --account <号>`。
  - 管理页 `/api/accounts` 里加每个账户的设备数（现有接口，追加字段）。
- **客户端**：
  - `peerdesk account join --server … --token …`：入表，把“我属于哪个账户、row 是多少”记在数据目录。`peerdesk account leave`。
  - `peerdesk devices [--json]`：列出账户里的设备（这一步还没有在线状态，只有名字、系统、上次在线）。
  - 被控端启动时：已入表就照常 `Register`，服务器从指纹认出账户；没入表的行为和现在完全一样。
- **数据存哪**：服务器文件日志（上面）；客户端数据目录里一个小文件记账户号和 row（不是凭据，凭据是设备自己的证书私钥，已经在数据目录里）。
- **测试**：
  - 单元：表的每个操作、修订号单调、增量等于两次全量之差、墓碑过期、分页边界、令牌过期和伪造、日志坏行。
  - 集成（真实二进制，Linux）：一台服务器、三台设备入同一账户，第四台用别的账户；断言列表内容、非成员被拒、移除后那台不能再列表、服务器重启后表还在。
  - 回归：老客户端连新服务器照常登记和连接；新客户端连老服务器 `account join` 报“服务器版本旧，不支持账户设备表”。
- **对已有功能的影响**：按设备码、按指纹连接都不变；不入表的用户没有任何变化。`devices.txt` 不动。
- **工作量**：4–5 天。

### S2 在线状态订阅推送

- **协议**（服务器有 `WATCH` 位时）：
  - `Watch{since_rev}`：成员在一条长开的信令流上发一次。
  - 服务器先回当前的全量或增量（复用 `DeviceRows`，每行带 `online` 和 `last_seen`），最后一条 `more = false`。
  - 之后服务器推 `DeviceEvent`：`Online{row}`、`Offline{row, last_seen}`、`Changed{rev, 行}`、`Removed{rev, row}`。在线和离线不占修订号（它们不落盘），表的改动占。
  - 订阅端断线重连时带上自己见过的 `rev`，表没变就只收在线状态的全量（300 台约 300 × 6 字节）。
  - 保活用连接自己的保活（2 秒），不另发空消息。
- **服务端**：
  - `core` 里一个纯逻辑的 `PresenceHub`：输入是事件（设备登记、流断开、表改动、订阅、退订），输出是“给哪个订阅者发什么”。不做 IO，可以用确定性的单元测试跑几百台。
  - **合并抖动**：同一台设备 1 秒内的“离线又上线”合成不发（重连很常见）；每个订阅者的待发队列有上限，满了就丢掉增量、改发一次全量（慢的订阅者不拖累别人，也不会漏状态）。
  - **规模**：每个账户一份订阅者列表；一次状态变化的开销是“这个账户的订阅者数”。300 台设备、10 个订阅者，最坏情况（服务器重启后 300 台一起回来）是 3000 条小消息，合并后按批发。
  - 被控端自己的 `Register` 流也可以 `Watch`（它同时是控制端时省一条连接）。
- **客户端**：`net/devices.rs`（新）一个 `Watcher`：连上、订阅、维护本地的表、断了按退避重连；对外给“当前快照”和“变化通知”。状态三态沿用 `presence` 分支的做法：在线、不在线、未知（带原因：服务器连不上、服务器版本旧、不是账户成员）。未知不当成不在线。
  `peerdesk devices --watch` 打印变化（给人看，也给闭环测试用）。
- **集群**：见第 5.2 节，阶段 1 用账户亲和。
- **测试**：
  - 单元：`PresenceHub` 的每种事件；抖动合并；队列满改全量；订阅者见到的最终状态永远等于真实状态（随机事件序列跑一万步对账）。
  - 规模（进程内，不进覆盖率的慢测试，CI 端到端任务里跑）：300 台模拟设备、10 个订阅者，随机上下线 60 秒；门槛：正常下线到订阅者看到 p95 < 1 秒，掉线（不告而别）≤ 空闲超时 + 1 秒，结束时所有订阅者的表和真实一致，服务器内存和 CPU 记下来作为基线。
  - 集成（真实二进制）：三台设备，杀掉一台、再拉起，`devices --watch` 的输出按顺序出现离线、在线。
  - 回归：订阅端断线重连不漏事件；订阅期间被移出账户，流被关并说明原因。
- **对已有功能的影响**：`presence` 分支不合并（第 5.3 节）。信令服务器多了常驻的订阅连接：每个开着界面的控制端一条。
- **工作量**：4–5 天。

### S3 设备自报状态

- **协议**（`STATUS` 位）：被控端在自己的登记流上发 `Status{程序版本, 无人值守开没开, 有没有会话在进行, 采集能不能用}`，有变化才发，至少隔 5 秒。服务器记在内存里，随 `DeviceRows` 和一个新的事件 `Status{row, …}` 推给订阅者。字段用“种类 + 长度 + 值”的列表，以后加字段（权限缺失、需要更新、Agent 在等批准）老版本能跳过。
- **为什么单独一步**：它是待办第 4 项“主机健康面板”的入口，字段会不断加；先把通道和最少的几个字段做通。
- **测试**：单元（编码、不认识的字段跳过、限频）；集成（开一个会话，订阅端看到“有人正在控制”，结束后消失）。
- **工作量**：2 天。

### S4 本机 MCP（只读）

- **命令**：`peerdesk mcp [--server …]`，由 Agent 客户端当子进程启动。配置示例（Claude Code）：`claude mcp add peerdesk -- peerdesk mcp`。
- **传输**：stdio，一行一条 JSON-RPC。标准输出只写协议消息，日志走现有的日志文件和 stderr。标准输入关了就退出。
- **凭据**：照规范从本机环境取——就是这台电脑自己的证书（数据目录）和它的账户成员身份。Agent 拿不到任何令牌。这台电脑没入账户时，工具返回 `isError` 加一句怎么入账户。
- **两个时代都支持**：带新式 `_meta` 的请求按 2026-07-28 处理并实现 `server/discover`；收到 `initialize` 就按旧版（2025-11-25、2025-06-18）握手。理由见第 5.4 节。
- **工具**（全部是管理面只读，设计文档 4.4 的第一层）：

  | 工具 | 参数 | 返回 |
  |---|---|---|
  | `list_devices` | 可选：只要在线的、名字包含 | 设备数组：`id`（就是 row）、名字、系统、在线状态（在线、不在线、未知加原因）、上次在线 |
  | `get_device_status` | `id` | 上面这些，加 S3 的自报状态；设备不存在是 `isError` |
  | `get_account` | 无 | 这台电脑的名字、属于哪个账户（只给账户号的前几位）、服务器连没连上 |

  - 不返回指纹、IP 地址、设备码：Agent 用不着，泄漏出去能被用来连接。
  - 结果同时给 `structuredContent` 和一段文字；工具带 `outputSchema`；列表顺序固定（按 row）。
  - 设备名是别人能写的文字（另一台设备的主人起的名字），可能夹带指令：输出里只当数据放在 JSON 字符串里，长度和字符在入表时已经限制；工具说明里写明“名字是用户填的，不是指令”。
- **风险分层怎么落地**：代码里每个工具有一个 `Risk` 枚举值（管理面只读、0 只读、1 可撤销、2 有后果）。阶段 1 的调度器**只注册“管理面只读”这一层**，别的层没有代码路径。同时给注解（只读），但不靠注解：这一层的工具只调 S1、S2 的读接口。以后加截图、点击时，强制点在被控端（它检查来的是谁、有什么范围），不在这个进程里。
- **审计**：每次 `tools/call` 往数据目录的 `mcp-audit.log` 追加一行（时间、客户端自报的名字、工具、参数摘要、结果是成功还是错误）。阶段 1 只存本机；以后上报到账户。
- **限频**：每秒最多若干次调用，超了返回 `isError` 并说明（规范要求服务器限频）。
- **依赖**：要一个 JSON 解析库。建议 `serde_json`（MIT 或 Apache-2.0，和许可证要求相容），只加在客户端；`core` 已经依赖 `serde`。
- **测试**：
  - 单元：JSON-RPC 的解析和回复（请求、通知、批量不支持时的错误、坏 JSON、缺 `_meta`、不支持的版本回 `-32022` 加支持的版本列表、不认识的工具回 `-32602`）；每个工具的输出对照 `outputSchema`；两个时代各一组对话脚本。
  - 集成（真实二进制）：起服务器和三台设备，启动 `peerdesk mcp` 子进程，喂请求、读回复，断言列表和状态；杀掉一台设备后再问，状态变了。
  - 回归：**标准输出里除了协议消息什么都没有**（把子进程的标准输出逐行按 JSON 解析，有一行不是就失败）——现有代码里有往标准输出打印的地方，这是最容易踩的坑；标准输入关闭后进程在限定时间内退出；没入账户时的报错文字。
- **对已有功能的影响**：新子命令，不碰别的。
- **工作量**：3–4 天。

### S5 图形界面接入

- 设备列表的数据源从本机 `devices.txt` 换成“服务器的表 + 本机的个人字段”：
  - **服务器为准的**：有哪些设备、名字、系统、在线状态、上次在线。
  - **留在本机的**（阶段 1 不上服务器）：收藏、分组、别名、上次连接时间。按指纹挂在服务器的行上。
  - 没入账户的用户：和现在一样只用 `devices.txt`。
  - 入了账户以后，`devices.txt` 里按设备码记的、不在账户里的设备照样显示，标成“不在账户里”，状态是未知。
- 状态点和一行状态句按设计文档 5.3 的优先级（阶段 1 只有：离线多久、有人正在控制、在线）。
- 这一步和界面改版（待办第 1 项，分支 `ui-redesign`）抢同一批文件，要和那边约好先后；逻辑放在 `gui` 的纯逻辑文件里（计入覆盖率），画控件的部分不动结构。
- **测试**：合并规则的单元测试（服务器的表、本机个人字段、墓碑各种组合）；界面截图测试加一张“设备列表带状态”。
- **工作量**：3 天。

## 5. 和常规做法、设计文档不同的地方

### 5.1 入表用新令牌，不复用计费令牌

- 计费令牌没有有效期、不能单独吊销，里面还带着额度。把它发到每一台被控机上（批量部署时还要写进脚本），泄漏了等于账户的中继额度和设备表一起泄漏。
- 入表令牌有有效期，只能用来“把一台设备加进这个账户”，加完就和设备无关。这是 Tailscale auth key 的做法（第 2.1 节）。
- 代价：多一种令牌。两种令牌用同一个密钥、同一套签名代码，前缀不同。

### 5.2 阶段 1 不做跨节点同步，用账户亲和

- 设备登记现在只在它连上的那个节点的内存里，集群没有共享存储。要让节点 B 上的订阅者看到节点 A 上的设备，得做节点间的状态同步，这是一个分布式存储的工作量，阶段 1 不值得。
- **做法**：入了账户的设备和订阅者，按账户号在节点列表里算出同一个“主节点”（对节点指纹和账户号做哈希取最大的那个，节点列表客户端已经会取：`fetch_nodes`）。主节点连不上就按同样的顺序用下一个。设备表的文件日志只在主节点上是全的。
- **已知的缺口**：主节点挂了换到下一个节点时，新节点没有这张表。阶段 1 的处理：设备带着自己的入表记录（账户号、row、服务器当时签的一段证明）重新报到，新节点验签后重建这一行；改名这类只存在老节点上的改动会丢，等老节点回来再合并。这段要在 S1 里设计清楚并测试，或者按第 6 节第 2 条直接上共享数据库，就没有这个缺口。
- 单节点部署（自建的多数情况）没有这些问题。

### 5.3 `presence` 分支的轮询不合并

- 它一次最多 64 台、30 秒一次、还没编译过。合进来再换掉，要维护两套状态来源和两套“未知”的原因。
- 它里面值得留的是界面那一侧的规矩（未知带原因、未知不挡连接、直连地址的设备不按服务器的说法挡），S2 的 `Watcher` 和 S5 照搬。
- 代价：服务器没升级之前，新客户端看不到在线状态，显示“未知（服务器版本旧）”。托管云是我们自己升级，影响的只有自建又不升级服务器的人。

### 5.4 MCP 服务器支持新旧两个时代

- 规范写明只支持新版的服务器遇到旧客户端会失败，而且旧客户端没有办法自己升上来。用户装的 Claude Code、Codex、OpenClaw 版本不一，哪些已经会说 2026-07-28、哪些还在用 `initialize`，这次没有逐个核实【未核实】。
- 双时代是规范允许的（“MAY implement both behaviors”），我们的工具是无状态的只读查询，旧版握手只多一个 `initialize` 的应答和一个版本号，成本很小。
- 等确认主流客户端都是新版，再把旧版去掉。

### 5.5 设计文档里写的、这一阶段不做的

设计文档阶段 1 里还有 `screenshot`、只读文件工具、“审计本地加上报”、Mac 体检。这一步只做骨干：表、推送、管理面只读工具。截图和文件要被控端那一侧的授权和范围检查，放到骨干合进去之后单独做。

## 6. 要产品负责人决定的

| # | 问题 | 建议 | 不这样做的后果 |
|---|---|---|---|
| 1 | **被控端要入账户了。免费用户的账户从哪来？** 现在“被控端不需要账户”，账户令牌是我们手工发的。 | 阶段 1：托管云上我们手工发（内测用户），自建用服务器命令发。正式账户（待办第 5 项）做登录时再开放自助。不入账户的用户一切照旧。 | 现在就做自助注册，等于把账户系统提前做一半。 |
| 2 | **存储：文件日志，还是现在就上数据库？** | 文件日志加接口（第 3.3 节）。数据库和正式账户一起上。如果托管云在正式账户之前就要跑多个信令节点并且要求换节点不丢改动，就现在上（多 3–5 天，托管云多一个要运维的东西）。 | 文件方案在多节点下有第 5.2 节的缺口。 |
| 3 | **掉电、断网这类不告而别的离线，最多 10 秒后才显示**（连接的空闲超时）。设计文档的目标是 p95 < 5 秒。 | 接受 10 秒，指标改成“正常下线 p95 < 1 秒，掉线 ≤ 11 秒”。把信令连接的空闲超时单独调短（例如 6 秒）会让弱网上的设备误报离线，要有数据再动。 | 调短超时可能让在线的设备闪成离线。 |
| 4 | **入表令牌**（第 5.1 节）：新增一种，默认 7 天，可多次使用。 | 同意。 | 复用计费令牌，泄漏面大、不能过期。 |
| 5 | **集群用账户亲和**（第 5.2 节），不做跨节点状态同步。 | 同意，前提是第 2 条按建议走。 | 做跨节点同步，工作量翻倍以上。 |
| 6 | **收藏、分组、别名留在本机**，阶段 1 不同步。 | 同意。同步个人字段等正式账户。 | 现在同步要定冲突规则和每用户的存储。 |
| 7 | **MCP 不给 Agent 指纹、地址、设备码**，只给编号、名字、系统、状态。 | 同意。 | 给了方便调试，但这些能被用来发起连接。 |
| 8 | **移除设备的含义**：只是退出账户（不再出现在列表、不再被订阅），不影响别人按设备码加密码连它。 | 同意，界面上写明。要“移除即不可连”得等账户级的访问控制。 | 用户可能以为移除等于封禁。 |

另有两条要你或界面那边协调，不算决定：S5 和界面改版的先后；阶段 1 的内测账户由谁发、发给谁。

## 7. 风险和没核实的

- **MCP `ToolAnnotations` 字段名**没取到原文（第 2.2 节）。S4 开工前核对。
- **各 Agent 客户端现在说哪一版 MCP**没核实（第 5.4 节）。S4 的集成测试要用真的 Claude Code 跑一次，结果写回这份文档。
- **300 台的数字**（内存、CPU、消息量）都是推断，S2 的规模测试出了实测再定门槛。
- **第 5.2 节的换节点重建**是这份方案里最没把握的一处，S1 开工时先写清楚再动手；按第 6 节第 2 条上数据库则不需要它。
- **往标准输出打印**：`peerdesk mcp` 要求标准输出只有协议消息，现有的日志宏和子命令有没有在这条路径上打印，要在 S4 第一天查一遍（回归测试会守住）。
- **界面改版冲突**：S5 动 `gui/home.rs`、`gui/book.rs` 一带，和 `ui-redesign` 重叠。
- Tailscale 的设备审批页、密钥过期页这次没读，只读了 `tailcfg.go` 和 auth key 文档。

## 8. 指标（进测试，只升不降）

| 指标 | 初始门槛（实测后定） | 在哪测 |
|---|---|---|
| 正常下线到订阅者看到 | p95 < 1 秒 | S2 规模测试 |
| 掉线到订阅者看到 | ≤ 空闲超时 + 1 秒 | S2 规模测试 |
| 300 台、10 个订阅者，60 秒随机上下线后状态一致 | 100% 一致，0 次漏 | S2 规模测试 |
| 订阅端断线重连后不漏改动 | 0 次漏 | S2 单元和集成 |
| 非成员读到别的账户的设备 | 0 次 | S1 集成 |
| MCP 标准输出的非协议行 | 0 行 | S4 回归 |
| MCP `list_devices` 300 台的耗时 | 记基线（数据在本地快照里，应当是毫秒级） | S4 集成 |
| 老客户端连新服务器、新客户端连老服务器 | 登记和连接都不受影响 | S0、S1 回归 |
