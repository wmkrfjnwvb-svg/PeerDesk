# PeerDesk 对接操作系统自带远程桌面(RDP / ARD / VNC)调研

> 方法说明:本次仅做了 3 组网络检索(IronRDP、macOS 高性能共享、Teleport),其余内容来自既有知识,**逐条标注"未核实"**。落地前须对照上游仓库复核。检索结果多为第三方镜像站,可信度有限。

## 0. 结论先行

1. 推荐 **(a) 先行 + (b) 为主线**:第一阶段做通用 TCP 端口转发隧道,调用系统原生客户端(1~2 人周即可上线,零许可风险)。第二阶段嵌入 **IronRDP**(RDP)与自研/封装的纯 Rust RFB 客户端(VNC/ARD),在 PeerDesk 查看器内渲染。
2. **(c) 网关转码**作为远期可选:仅在目标机无法用原生协议高效传输(如跨洋高延迟、需统一编码)时才有价值,成本最高。
3. 许可方面:**不要链接 libvncclient(GPL)与 TigerVNC(GPL)**。FreeRDP 为 Apache-2.0,可链接但需 FFI/C 构建,体积和构建复杂度高。IronRDP 为 MIT/Apache-2.0 双许可(未核实具体表述,请看仓库 LICENSE),与项目最匹配。

## 1. RDP 客户端库

| 项目 | 许可 | 状态 | 可跑在任意字节流 |
|---|---|---|---|
| [IronRDP](https://github.com/Devolutions/IronRDP) (Devolutions) | MIT/Apache-2.0(未核实) | 活跃,Devolutions 生产使用 | 是:核心 crate 为 sans-IO 设计(`ironrdp-connector`/`ironrdp-session` 不绑定 socket),你自己提供读写流 |
| rdp-rs | MIT | 基本停滞,仅部分实现 | 理论可,但功能过旧(无 EGFX) |
| FreeRDP(FFI) | Apache-2.0 | 最完整,业界事实标准 | 可:通过自定义 BIO / transport 回调,或在本地 loopback 上起监听桥接 |

**IronRDP 功能(依据[检索结果](https://www.mintlify.com/Devolutions/IronRDP/introduction),均为第三方文档,需复核)**
- NLA/CredSSP:支持,基于 `sspi` crate;Kerberos、NTLM 均有,但 Kerberos 经隧道时 KDC 可达性是另一个问题。
- 图形:RemoteFX、位图、RLE 等较成熟;[EGFX/H.264 的客户端解码](https://github.com/Devolutions/IronRDP/issues)据称"部分支持(仅解码)",**AVC444 状态未核实**,需自行验证并预估要补 H.264 解码器(可用 openh264 / 平台硬解)。
- 剪贴板(CLIPRDR):有;RDPDR 驱动器重定向:有 `ironrdp-rdpdr` crate,成熟度未核实;音频(RDPSND):有基础实现;多显示器:协议层有,客户端 UI 需自行实现。
- RD Gateway(HTTP/RPC-over-HTTPS、WebSocket):IronRDP 有 `ironrdp-tls`/gateway 相关工作,**完整 RD Gateway 支持未核实**;对 PeerDesk 并不关键,因为我们自己的隧道已取代 Gateway。
- RDP over UDP(RDP-UDP / MS-RDPEUDP):**未核实,大概率不支持**。FreeRDP 3.x 有部分支持。
- 体积:纯 Rust,按需启用 feature,二进制增量通常是几 MB 量级(估计,未实测);若加 H.264 解码器另计。

**FreeRDP 要点**:覆盖最全(EGFX AVC444、RDPDR、音频、多显示器、Gateway、UDP 传输),但是 C 库,依赖 OpenSSL、ffmpeg/openh264 等;静态链接时需逐个确认第三方库许可(如 ffmpeg 若启用 GPL 组件会污染)。Tauri 打包三平台交叉编译成本高。可作为 IronRDP 功能不足时的"兜底后端"。

## 2. VNC / RFB

- **纯 Rust**:[vnc-rs](https://github.com/HsuJv/vnc-rs)(支持 Raw/Zrle/Tight 等,许可据记忆为 MIT/Apache,**未核实**)、`vnc` crate(老旧)。成熟度普遍低于 IronRDP,需实测对 TightVNC/x11vnc/wayvnc 的兼容性。
- **libvncclient**:LibVNCServer 为 **GPL-2.0+**,不得链接进 Apache-2.0 产品(只能作为独立进程调用,且分发时仍需遵守 GPL)。TigerVNC 同为 GPL,同样规避。
- **编码**:Tight(JPEG)与 ZRLE 是通行基线;H.264 编码在 RFB 中并无统一标准(TigerVNC 有 H.264 提案、UltraVNC 有扩展、Apple 的是私有),**互通性未核实**,不要把"VNC 上跑 H.264"作为产品承诺。
- **macOS Screen Sharing / ARD**:
  - 认证:除标准 VNC Auth(type 2)外,Apple 使用 **ARD 认证 type 30**:服务端发 DH 参数(generator、prime、公钥),客户端回传 128 字节密文,内含用户名/密码各 64 字节,以 DH 共享密钥的 MD5 作 AES-128-ECB 密钥加密。算法描述来自既有知识,**未核实**,可对照开源实现(如 libvncclient 的 ARD 代码仅作参考阅读,**不要拷贝以免许可污染**,应基于协议说明独立实现)。
  - 另有 type 33(MS-Logon II)等扩展,macOS 常见 30 与 36(未核实)。
  - 高性能模式:macOS 14+(Sonoma)、**双方均为 Apple silicon**、使用 UDP 5900-5902、支持 4:4:4、HDR、立体声、最高 60fps([Apple 官方说明](https://support.apple.com/guide/remote-desktop/apdf8e09f5a9/mac),[9to5Mac](https://9to5mac.com/2026/02/05/how-to-use-macos-high-performance-screen-sharing-for-lower-latency-and-better-color-video/))。协议**私有且无公开规范**,第三方客户端基本无法使用,第三方只能回落标准 VNC 模式(Tight/ZRLE,画质与帧率较差)。另有报告其模式选择提示在脚本化 `vnc://` 连接时反复出现([来源](https://nunn-blog.pages.dev/2023/12/22/screen-sharing-is-unscriptable.html))。**结论**:macOS 想要好体验,需让 PeerDesk 在 macOS 目标机上直接运行自己的 host(用 ScreenCaptureKit + VideoToolbox),而非依赖 Screen Sharing;Apple 原生客户端经 TCP 隧道只能使用标准模式或不可用(UDP)。

## 3. Linux 服务端

| 组件 | 协议 | 要点 |
|---|---|---|
| gnome-remote-desktop | RDP(基于 FreeRDP)、VNC | 需 GNOME 3.38+ / Wayland + PipeWire;支持"远程登录/headless"(GNOME 46+ 系统级,需用户名/密码和 TLS 证书,GNOME 47 前后细节未核实);RDP 使用 NLA,凭据存 libsecret/keyring;屏幕共享模式需用户已登录 |
| xrdp | RDP | Apache-2.0;为每个连接拉起独立会话(Xorg/xorgxrdp 或 Xvnc 后端),适合 headless 服务器;NLA 支持有限(多用 TLS/RDP 安全层),**是否支持 EGFX/H.264 取决于版本,未核实**;Wayland 桌面共享不是其强项 |
| KDE KRdp | RDP(基于 FreeRDP) | Plasma 6 新增,依赖 PipeWire 与 xdg-desktop-portal 的屏幕捕获授权,仅共享已有会话,不提供虚拟会话(未核实当前进展) |
| wayvnc | RFB | 面向 wlroots 合成器(Sway/Hyprland),**ISC 许可**(依赖 neatvnc,同为 ISC,未核实);支持 Tight/ZRLE 等,H.264 为实验性(未核实);GNOME/KDE 上不可用 |

这些服务端均作为**独立进程**运行,即使 GPL 也与 PeerDesk 无链接关系,仅作为被连接对象,无许可问题。

## 4. Windows 主机端

- 具备 RDP **主机**能力:Pro、Enterprise、Education、Server;**Home 不能被远程连入**(只能做客户端)。
- Home 的选项:①升级到 Pro;②RDP Wrapper 等第三方补丁(违反 EULA,系统更新易失效,不建议内置);③安装 VNC 服务端(TightVNC/UltraVNC,多为 GPL,仅作被连接方);④**让 PeerDesk 自带 Windows host**(DXGI 捕获 + 硬编),这才是 Home 用户的统一答案。
- 注意:RDP 会锁定本地控制台会话(登录同一用户时),这是与"共享屏幕"类产品的体验差异。

## 5. 已有产品的做法

- **Apache Guacamole**([官网](https://guacamole.apache.org/)):浏览器 ↔ guacamole-client(Java)↔ **guacd**(C 守护进程)↔ RDP/VNC/SSH,guacd 内用 FreeRDP 与 libvncclient 解码,再以文本 Guacamole 协议推送绘图指令。即典型的**架构 (c)**。项目为 Apache-2.0,但 guacd 依赖 FreeRDP/libvncclient 等,衍生分发时需逐库核对(未核实其对 libvncclient 的具体处理)。
- **Remmina**:GTK 客户端,插件调用 FreeRDP/libvncclient,GPL,无参考许可价值。
- **Royal TS / Devolutions RDM / Microsoft Windows App**:原生嵌入客户端(Devolutions 用 IronRDP 与 FreeRDP;Windows App 用微软自研),均支持 RD Gateway 与跳板,属于**架构 (b)**。
- **Termius**:以 SSH 隧道/端口转发为主,RDP 能力有限(未核实)。
- **RustDesk**:有 TCP 端口转发(`--port-forward`)与"RDP 转发"选项,即**架构 (a)**;其 AGPL 许可,不可借鉴代码。
- **Tailscale + RDP**:Tailscale 提供三层 overlay,RDP 原样跑在其上(NLA 照常),无需转码——等价于"网络层隧道"。
- **Cloudflare Access 浏览器 RDP**:Cloudflare 在浏览器内渲染 RDP(基于 Cloudflare 自家网关与 Wasm 客户端),属 (b)/(c) 混合(实现细节未核实)。
- **Teleport Desktop Access**:早期版本[用 Rust 的 rdp-rs fork 经 CGO 调用,在 TDP 协议与 RDP 之间转换](https://beta.pkg.go.dev/github.com/zmb3/teleport/v11@v11.3.3/lib/srv/desktop/rdp/rdpclient);后期改用 IronRDP 的说法仅来自[聚合站](https://openapps.pro/packages/ironrdp?page=2),**未核实**。它是 (c) 类网关,另用 Windows 本地用户证书实现免密登录。

## 6. 架构方案对比

### (a) TCP 端口转发 + 原生客户端
- 延迟:RDP/VNC 本身的延迟 + P2P 路径延迟;**QUIC 的 stream 复用不改善 TCP-in-QUIC 的队头阻塞**(RDP 本身 TCP 语义,需 TCP-over-QUIC 流;丢包下表现类似普通 TCP)。
- 流量:原生协议流量,无转码,无额外开销。
- 许可:零风险,不链接任何东西。
- 工作量:最小。需处理本地监听端口、随机端口分配、授权 UX。
- 安全:凭据完全由原生客户端与目标机直接协商,PeerDesk 不接触。**NLA 经隧道没问题**:CredSSP 绑定的是 TLS 通道公钥,TLS 端到端终结于目标机,隧道只搬运字节;但连接目标须写成 `localhost:port` 时 SPN/Kerberos 会失败,只能走 NTLM,且需注意 mstsc 对 `localhost` 的凭据委派限制(未核实)。隧道端点须双向认证,防止别人访问本机转发口。
- 缺点:无法统一 UI;用户需要自己装 mstsc/Windows App/Remmina;macOS 高性能模式依然不可用。

### (b) 内嵌协议客户端
- 延迟:可对 UI 事件、帧缓冲拥有控制,最好;可用 PeerDesk 自己的 QUIC datagram 做输入。
- 流量:原生协议流量;若服务端支持 EGFX/H.264 则良好,否则回落位图/RemoteFX 带宽较大。
- 许可:IronRDP + 自研 RFB,干净。
- 工作量:大(见下);RDP 兼容性长尾(各 Windows 版本、NLA 失败、证书、分辨率变化)是主要风险。
- 安全:凭据由 PeerDesk 持有,需放入系统钥匙串、禁止明文落盘、内存清零;TLS 证书校验需做 TOFU 提示(RDP 证书多为自签)。统一审计、统一 UI 是优势。

### (c) 网关转码
- 在远端 LAN 内跑 PeerDesk gateway:本地连 RDP/VNC,解码后重新编码为 PeerDesk 视频流经 P2P 发出。
- 延迟:多一次解码+编码,增加 20~60ms(估计);但对高丢包 WAN 更友好(可用 PeerDesk 自适应码率/FEC)。
- 流量:可统一 H.264/AV1 码率控制,显著低于位图类 RDP;但 CPU/GPU 压力在网关上。
- 许可:网关在服务端,可用 IronRDP;若要用 FreeRDP 也可(Apache)。
- 工作量:最大(需要帧级解码器、编码器、输入回传、剪贴板映射、会话管理)。
- 安全:网关**持有凭据**,成为高价值目标;NLA 在网关与目标机之间终结,客户端不再与目标机端到端。需 RBAC、凭据保险库(类 Teleport/Guacamole)。

## 7. 许可证清单

| 组件 | 许可 | 能否链接进 Apache-2.0 |
|---|---|---|
| IronRDP | MIT/Apache-2.0(未核实) | 可 |
| FreeRDP | Apache-2.0(2.0 起;更早为 GPL 的版本勿用) | 可,但注意其所链接的 ffmpeg 等依赖 |
| rdp-rs | MIT | 可(但已停滞) |
| vnc-rs | 未核实(MIT/Apache 倾向) | 复核后可 |
| libvncclient / LibVNCServer | GPL-2.0+ | **不可** |
| TigerVNC | GPL-2.0+ | **不可** |
| Guacamole / guacd | Apache-2.0 | 可,但其依赖需逐一核对 |
| xrdp | Apache-2.0 | 作为外部进程无关 |
| gnome-remote-desktop | GPL-2.0+ | 外部进程,无关 |
| KRdp | KDE 的 LGPL/GPL 系(未核实) | 外部进程,无关 |
| wayvnc | ISC | 外部进程,无关 |
| RustDesk | AGPL-3.0 | **不可** 借鉴代码 |

## 8. 推荐方案与分阶段计划

**主张**:不要一开始就做内嵌客户端。先用(a)证明价值——只要隧道可靠,Windows Pro/Linux xrdp 用户立刻可用;内嵌客户端(b)再替换其体验;(c)留待有明确需求时做。macOS 与 Windows Home 的"最佳体验"路径应是 PeerDesk 自有 host,而不是 OS 自带服务,不要在 ARD 高性能模式上投入。

| 阶段 | 内容 | 估算(人周) |
|---|---|---|
| P0 | 技术验证:IronRDP 对 Win10/11 Pro、Server 2019/2022、xrdp、gnome-remote-desktop 连通性;确认 EGFX/H.264、剪贴板现状;vnc-rs 对 wayvnc/Tight 测试 | 2 |
| P1 | 架构 (a):通用 TCP 转发(基于 P2P 流)、授权策略、本机回环绑定、"一键打开 mstsc/Windows App/Remmina/vnc://" | 2~3 |
| P2 | 架构 (b)-RDP:IronRDP 集成到 Tauri 查看器(帧缓冲渲染、键鼠、剪贴板、凭据钥匙串、证书 TOFU、NLA) | 8~12 |
| P3 | RDP 增强:H.264(AVC420/444)硬解、音频、RDPDR、多显示器、重连/动态分辨率 | 6~10 |
| P4 | 内嵌 RFB(vnc-rs 评估或自研)+ ARD type 30 认证 + Tight/ZRLE | 5~8 |
| P5 | (可选) 架构 (c) 网关:先做 RDP→H.264 转码,含凭据保险库 | 12~20 |
| 持续 | 兼容性测试矩阵与回归 | 1/月 |

**关键风险**:IronRDP 的 EGFX 与 AVC444 状态;RDP 兼容长尾;NLA/Kerberos 经隧道的 SPN 问题;macOS 高性能模式无法被第三方使用;GPL 依赖的无意引入(应在 CI 加 `cargo deny` 许可检查)。

## 未能核实清单
IronRDP 具体许可文本与 EGFX/AVC444/RD Gateway/UDP 现状;vnc-rs 许可;Teleport 是否迁移到 IronRDP;Cloudflare 浏览器 RDP 内部实现;gnome-remote-desktop headless 的版本细节;KRdp 许可与功能;wayvnc 的 H.264;ARD type 30 的字节级细节;各二进制体积与延迟数字(均为估计)。
