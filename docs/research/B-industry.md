# B. 业界远程桌面与云游戏的传输、画质与 4K/8K 调研

> 调研日期：2026-11-20。标注说明：**[源码]** 出自开源代码，**[规范]** 出自协议规范或厂商 SDK 文档，**[官方]** 出自官方博客或文档，**[二手]** 出自媒体或社区，**[未核实]** 表示找不到一手来源，属推断或传闻。

## 0. 结论先行

1. 低延迟的系统都不用整流可靠的 TCP 来传视频。分成三派：
   - **UDP + 每帧 FEC + 参考帧失效 / 关键帧**：GameStream/Sunshine、GeForce NOW。
   - **UDP 上自研可靠传输 + 激进码率控制**：Parsec BUD、Citrix EDT、RDP-UDP 可靠模式。
   - **按帧或按消息独立的可靠流**：Airhand 每帧一条 QUIC 流，DCV 走 QUIC。
   RustDesk 仍是整流可靠传输（TCP、KCP、WebRTC SCTP），是反例。
2. 文字清晰有四条路线：
   - 原生 4:4:4：Parsec、Sunshine、GFN 的 Cinematic 模式、DCV。
   - 两路 4:2:0 拼 4:4:4：RDP AVC444。
   - 静止后补画质到无损（build-to-lossless）：Citrix、PCoIP、RDP 渐进式编解码、Airhand 的“静止后补画质”。
   - 按区域混合编码：文字区无损，运动区用 H.264。Citrix 选择性 H.264，RDP 8.1 的混合模式。
3. 4K60 硬件编码已经普及。8K60 在消费级只有 NVIDIA Ada/Blackwell 靠**多 NVENC 分帧编码（SFE）**、AMD RDNA4 号称 8K75 两条路可走。AV1 4:4:4 目前没有硬件能编。
4. 产品负责人关心的“切档卡顿”：业界的做法是**码率连续调、帧率靠丢采集帧、分辨率最后才动并加滞回**。分辨率切换要么用 AV1 参考帧缩放（GFN 宣称“AV1 + Reference Picture Resampling”），要么先建后切（make-before-break）。整套编解码器拆掉重建、再等一个大关键帧，就是“卡几秒”的典型原因（见 §3）。

---

## 1. 逐系统拆解

### 1.1 Parsec（BUD）
- **传输**：自研 BUD，基于 UDP，用 DTLS 1.2（OpenSSL）加密。带“类似 TCP 的基本可靠语义，但用高度定制的拥塞控制”。视频侧**没有任何缓冲**，所以必须在拥塞发生前检测到它，“在丢包和延迟出现之前把 12 Mb/s 链路刚好跑满”。码率由编码器直接调，网络层和编码层双向共享信息。[官方 BUD 博客](https://parsec.app/blog/a-networking-protocol-built-for-the-lowest-latency-interactive-game-streaming-1fd5a03a6007)；幻灯片 [Building a UDP protocol for cloud gaming](https://www.slideshare.net/BenjyBoxer/building-a-udp-protocol-for-cloud-gaming)。优先级依次是延迟、帧率、画质。
- **FEC / 丢包恢复**：官方没有公开。是否用 FEC、用 LTR 还是关键帧均 **[未核实]**。
- **画质与帧率**：H.265 和“Prefer 4:4:4 Color”是付费功能。4:4:4 编码只有 Windows 主机支持，Android 和网页端不能解 4:4:4。[支持文档](https://support.parsec.app/hc/en-us/articles/32381785123860-Improve-Stream-Quality-and-Color-Accuracy)
- **实测数字**：240 fps 测试用 GTX 1070 主机、1050 Ti 客户端、千兆局域网，带宽上限 100 Mbps，H.264。1000 fps 摄像机测得客户端落后约 2 帧，即**约 4–8 ms 总延迟**。官方同时说明平时只在 60 fps 下测试。[240fps 博客](https://parsec.app/blog/parsec-game-streaming-total-latency-at-240-frames-per-second-c0818cc0daa5)
- **HDR**：**[未核实]**，未找到官方 HDR 支持说明。

### 1.2 GameStream 协议：Moonlight / Sunshine
- **传输**：视频和音频走 RTP/UDP。控制通道是 ENet 的分叉（可靠 UDP），用 AES-GCM 加密。[Wolf 协议文档](https://games-on-whales.github.io/wolf/stable/protocols/control-specs.html)
- **FEC** [源码]：
  - Sunshine 对每帧单独做 Reed-Solomon（GF(2^8)），`fec_percentage` 默认 **20%**，可设 0–100。[configuration.md](https://github.com/LizardByte/Sunshine/blob/master/docs/configuration.md)
  - 每个 FEC 块最多 255 个分片，每帧最多 4 个块；超出时这一帧不做 FEC（`stream.cpp`）。
  - Moonlight 在 SDP 里要求 `minRequiredFecPackets=2`：小帧最少 2 个校验包，FEC 比例可能到 200%。
  - 对 GFE 主机，串流 4K 时把 FEC 降到 **5%** 以减少开销；同时关掉 BLL-FEC 和 recovery mode，因为它们会在帧中途改 FEC 比例，打破接收队列的假设。[SdpGenerator.c](https://github.com/moonlight-stream/moonlight-common-c/blob/master/src/SdpGenerator.c)
  - 音频 FEC：每 4 个包后跟 2 个 RS 校验包。[Wolf](https://games-on-whales.github.io/wolf/stable/protocols/rtp-opus.html)
- **发送节奏** [源码]：按约 80% × 1 Gbps 的速率分组发送，每批小于 64 KB 且不超过 64 个包，以适配 Linux GSO 和 Windows SO_SNDBUF 的行为。这是**局域网取向的固定节奏，不是拥塞控制**。
- **码率**：用户手动设定一个固定码率，**没有真正的自适应码率（ABR）**。Moonlight 默认值是“分辨率系数 × 帧率系数”：1080p60 为 20 Mbps，**4K60 为 80 Mbps**；超过 60 fps 时按开方增长，4K120 约 113 Mbps。[streamingpreferences.cpp](https://github.com/moonlight-stream/moonlight-qt/blob/master/app/settings/streamingpreferences.cpp)。对 GFE 主机，GFE 的动态分辨率（DRC）被关掉了。
- **丢包恢复** [源码]：客户端发送 `IDX_INVALIDATE_REF_FRAMES` 指出一段帧号，Sunshine 调用 `NvEncInvalidateRefFrames`（[nvenc_base.cpp](https://github.com/LizardByte/Sunshine/blob/master/src/nvenc/nvenc_base.cpp)）。不支持参考帧失效（RFI）的客户端严格等 IDR。也可以选择帧内刷新（intraRefreshPeriod=300）。参考帧数默认 5。
- **低延迟编码**：NVENC 用 `ULTRA_LOW_LATENCY` 调优；按客户端要求的 `slicesPerFrame` 切片，AV1 用 tile。`nvenc_split_encode` 把一帧拆给多个 NVENC，“显著降低编码延迟，压缩率略有损失”，只用于 HEVC/AV1。
- **HDR / 4:4:4 / AV1**：
  - HDR 需要 HEVC Main10 或 AV1 10-bit 编码。Linux 上必须用 KMS 采集，NvFBC 和 X11 不支持 HDR。
  - 协议定义了 H.264 High 4:4:4、HEVC RExt 4:4:4 8/10-bit、AV1 High 4:4:4 等格式位（[Limelight.h](https://github.com/moonlight-stream/moonlight-common-c/blob/master/src/Limelight.h)）。4:4:4 编码**只支持 Windows 主机**。
  - 社区讨论确认：**现有硬件都不能编 AV1 4:4:4**。另外 DXVA 没有定义 HEVC 4:4:4 解码配置，需要厂商私有 API。[LizardByte 讨论](https://github.com/orgs/LizardByte/discussions/220)
- **帧调度（frame pacing）**：Moonlight 有 V-Sync 和 frame pacing 开关，默认关闭，开启后用约一帧延迟换平滑。

### 1.3 Steam Remote Play
- **端口**：UDP 27031/27036，TCP 27036/27037。[Steam 支持](https://help.steampowered.com/en/faqs/view/3E3D-BE6B-787D-A5D2)。视频传输细节、是否使用 GameNetworkingSockets 均 **[未核实]**。
- **PyroWave（2025–2026）**：
  - Valve 在 Remote Play 加入了 Themaister 设计的 **PyroWave**：纯帧内、CDF 9/7 小波、全部用 Vulkan compute 实现、不做熵编码。[作者博客](https://themaister.net/blog/2025/06/16/i-designed-my-own-ridiculously-fast-game-streaming-video-codec-pyrowave/)
  - 1080p 编码约 0.13 ms（RX 9070 XT）。码率控制约 20 µs 就能精确命中码率上限。
  - 作者实测 1440p60 4:4:4、250 Mbps 下，输入到 GPU 空闲平均 12.8 ms。
  - 64×64 块各自独立编码，所以抗丢包。
  - 代价是带宽为常规编码器的 5–10 倍，要求千兆网络。2026-10 的 beta 客户端允许手动码率到 1 Gbit/s。[mixed-news](https://mixed-news.com/en/steam-client-beta-pyrowave-1-gbit-manual-bitrate-reset-latency/)
  - 对 PeerDesk 的意义：局域网和 VR 场景下可以作为“极低延迟档”。

### 1.4 NVIDIA GeForce NOW
- **传输**：UDP，专有拥塞控制。在 L4S Prague 需求逐条回应中自评大部分合规，“丢包按模式识别，不对所有类型的丢包都降速”，在无线环境用单向时延（OWD）/RTT 抖动启发式检测非 L4S 瓶颈并回退。[L4S 合规文档](https://l4steam.github.io/PragueReqs/GeforceNow_L4S_requirements_Compliance_and_Objections.pdf)（2021，出处托管在 l4steam，**[二手]**）。
- **2025 年 Low Latency Streaming**：服务器端 Reflex 加 Rivermax **硬件包间隔发送（packet pacing）**，网络侧 L4S（Comcast、DT、BT 等运营商），客户端 G-SYNC Cloud。宣称网络延迟低于 30 ms；1080p 可到 360 fps，最高 5K120。[NVIDIA 博客](https://blogs.nvidia.com/blog/geforce-now-thursday-gamescom-2025)；[Tom's Hardware](https://www.tomshardware.com/video-games/cloud-gaming/nvidias-geforce-now-gets-an-rtx-5080-upgrade-ultimate-subscribers-now-get-blackwell-benefits-in-the-cloud)
- **Cinematic Quality Streaming**：YUV **4:4:4**、10-bit HDR、**AV1 + 参考帧重采样（RPR）**，最高 **100 Mbps**。[The Shortcut](https://www.theshortcut.com/p/nvidia-geforce-now-rtx-5080-cinematic-quality-streaming)（**[二手]**；官方博客只写了“4:4:4 + advanced AV1”）。完整套件可能限于 60 fps。
- **动态分辨率**：GFE 时代 GameStream SDP 里就有 `drc.enable` 和分辨率表（见 Moonlight 源码），说明 NVIDIA 一直在用动态分辨率配合码率控制。

### 1.5 Google Stadia（已停服）/ Xbox Cloud Gaming
- **Stadia**：
  - 标准 WebRTC（DTLS-SRTP、RTP/RTCP），VP9 或 H.264，用 transport-cc 做发送端带宽估计。
  - 实测平均下行码率因游戏而异，1.95–37.7 Mbps；4K 只对 Pro 开放；网络变化时自动改分辨率和帧率。[arXiv 2009.09786](https://arxiv.org/abs/2009.09786)
  - Google 为 Stadia 自研了带宽估计器，处理内置 GCC 处理不了的场景。[bloggeek](https://bloggeek.me/cloud-gaming-virtual-desktops-and-webrtc)
  - **[未核实]**：没有找到官方 postmortem。
- **xCloud**：Series X 刀片服务器，1080p60 上限，实测约 10–17 Mbps。[PureXbox 转述 Xbox Wire](https://www.purexbox.com/news/2021/06/xbox_cloud_gaming_is_now_officially_powered_by_custom_xbox_series_x_hardware)。基于 WebRTC 是社区共识，**[未核实]** 官方细节。

### 1.6 Microsoft RDP
- **传输** [规范]：
  - MS-RDPEUDP v1 有两种模式：**可靠模式**（持续重传）和**不可靠模式**（不重传，保证及时性），外加 FEC 用于恢复随机丢包。[MS-RDPEUDP 概述](https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-rdpeudp/fe211d97-92dd-47e6-8fa3-b23f2c1a5af9)
  - 协商到 v3 后使用 **MS-RDPEUDP2**，规范描述为“基于 URCP 的速率控制”（URCP 为基于时延的速率控制）。[MS-RDPEUDP2](https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-rdpeudp2/9db34630-e880-4bfd-9d8d-50bc044c3288)
  - v1 的 FEC 是 XOR 构造，v2 是否保留 FEC：**[未核实]**。
- **图形** [规范]：
  - **AVC444**：一帧 4:4:4 拆成两路 4:2:0。主视图是标准 YUV420，辅助视图装剩下的色度；两路必须用同一个 H.264 编码器编码、同一个解码器解码。**色度没有变化时可以省掉第二路**。[MS-RDPEGFX 3.3.8.3.2](https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-rdpegfx/8131c1bc-1af8-4907-a05a-f72f4581160f)
  - 好处是用 4:2:0 硬件编解码器就能得到 4:4:4 文字。2015 年 1440p 可达 50 fps；每个显示器占一个硬件编码器，不够时退回软件编码。[RDP 10 AVC 博客](https://techcommunity.microsoft.com/t5/microsoft-security-and/remote-desktop-protocol-rdp-10-avc-h-264-improvements-in-windows/ba-p/249588)
  - **混合模式（RDP 8.1）**：图像区域用 H.264，文字用专有编解码器（ClearCodec）。
  - **RemoteFX Progressive**：基于小波，先发粗层再逐步补细节，是一种 build-to-lossless。
- **HDR**：**[未核实]**。

### 1.7 Citrix HDX
- **EDT**：基于 UDP 的**可靠**专有传输，不可用时退回 TCP（Adaptive Transport = EDT + TCP 回退）。音频可以拆到单独的不可靠 UDP 通道。默认开启 MTU 发现。VDA 2303 起有新的拥塞控制（技术预览）。[EDT 文档](https://docs.citrix.com/en-us/citrix-daas/hdx-transport/adaptive-transport/enlightened-data-transport.html)；[EDT 2023 博客](https://citrix.com/blogs/2023/04/25/turbo-charging-edt-for-unparalleled-experience-in-a-hybrid-world)
- **Thinwire 自适应显示**：
  - **选择性 H.264/H.265**：只在运动区域用视频编码，静态文字区用 2DRLE **无损**编码。
  - **Build-to-lossless**：画面活动时用 H.264/H.265，停下后“锐化到像素级无损”，帧率通常比“视觉无损 H.264 4:4:4”更高。[Thinwire 文档](https://docs.citrix.com/en-us/citrix-daas/graphics/thinwire.html)；[Tech Zone](https://docs.citrix.com/en-us/tech-zone/design/design-decisions/hdx-graphics)

### 1.8 Amazon DCV
- **传输**：2024.0 起默认同时支持 WebSocket（TCP）和 **QUIC**（UDP，RFC 9000），文档说高延迟、高丢包时 QUIC 更好。[admin guide](https://docs.aws.amazon.com/en_us/dcv/latest/adminguide/disable-quic.html)
- **能力**：官方演示过 4K60 over QUIC。[GameTech 博客](https://aws.amazon.com/blogs/gametech/stream-a-remote-qa-or-press-environment-with-nice-dcv-over-quic-udp-for-a-4k-monitor-at-60-fps/)。2022 年加入 YUV 4:4:4。[公告](https://www.amazonaws.cn/en/new/2022/nice-dcv-color-accuracy-game-controller/)
- **[未核实]**：QUIC 的具体实现、流的映射方式、空闲时补到无损的参数。

### 1.9 HP Anyware / PCoIP Ultra
- 三种工作模式：
  - **CPU Offload**：用 AVX2 编码，最高双 4K，可选 **Build-to-Lossless**。
  - **GPU Offload**：NVENC，可配 4:4:4 或 4:2:0。
  - **Auto-Offload**：按画面活动在两者间自动切换，既有 build-to-lossless 的画质又省带宽。
  [PCoIP Ultra 架构](https://anyware.hp.com/products/hp-anyware/2025.03/documentation/architecture-guide/pcoip-ultra)
- 最高 60 fps，取决于两端 CPU 和带宽；4K30 以上需要客户端支持 AVX2。传输为 UDP，是否用 FEC **[未核实]**。

### 1.10 AnyDesk / TeamViewer
- **AnyDesk**：自研 DeskRT 编解码器，面向屏幕内容。传输为 TCP 6568 或 443，加 UDP 直连，以中继兜底（**[二手]**）。“<16 ms / 60 fps” 是厂商宣传数字。
- **TeamViewer**：未找到可信的技术资料，**[未核实]**。

### 1.11 RustDesk [源码]
- **编解码**：VP8、VP9、AV1（libaom），硬件 H.264/H.265（hwcodec）。
- **传输**：TCP 中继（hbbr）、TCP 打洞、**KCP over UDP**，以及新加入的 **WebRTC**（[Cargo.toml](https://github.com/rustdesk/rustdesk/blob/master/Cargo.toml)）。注释说他们改了 webrtc-sctp 的 RTO 下限，因为 1 秒的 RTO 下限会让一次丢包耗费 1–3 秒。所有路径都是**整流可靠**，有队头阻塞。
- **码率控制**（[video_qos.rs](https://github.com/rustdesk/rustdesk/blob/master/src/server/video_qos.rs)）：用 TestDelay 探测往返延迟，以 150 ms 为好坏阈值；帧率 1–120（初始 15）按延迟加减；码率倍率每 3 秒调一次。属于粗粒度的应用层 AIMD。

### 1.12 Airhand（与 PeerDesk 同为 Rust + quinn，最值得对照）[源码 + 文档]
- **传输**：QUIC + BBR，quinn 实现。视频协议 v10 起**每帧一条单向流**，丢一个包只阻塞这一帧（每端最多 256 条帧流）。音频走 QUIC datagram，每个包附带前两包的副本（RED 风格）。控制通道在中继和直连两条路径上**双发**，接收端按计数器去重。
- **在途字节控制**：每帧都回 ACK，在途上限约为 `码率 × (基础 RTT + 50 ms)`。排队时延超过 30 ms 降码率，低于 10 ms 才加；丢包率超过 10% 降，超过 2% 不再加（`flow.rs` 常量 QUEUE_HIGH_MS=30、LOSS_HIGH=0.10）。网络慢时**跳到最新一帧**。
- **丢包恢复**：
  - 包头写明每帧引用的是哪一帧，接收端只把能解的帧交给解码器。
  - 后续帧到了、前面一帧等了 1.5×RTT+30 ms（60–800 ms）仍未到，就上报 `Lost{decoded}`。等这么久是为了比 QUIC 重传更晚，避免误报。
  - 主机放弃在途帧，从**最近的长期参考帧（LTR）**恢复：每 250 ms 标记一次，两个槽位轮换，最新的那个被客户端确认后才替换旧的。
  - 只能编关键帧时，同一位置第二次上报才编（KeyframeGate）。1 秒都恢复不了时走控制通道请求关键帧。
  - 实测 LTR 只在 MF 的 NVIDIA/Intel H.264 和 NVIDIA AV1 上可靠；HEVC LTR 在 NVIDIA 上“报告说遵守了，实际没遵守”，Intel AV1 也不遵守，所以都在建编码器时用冒烟测试检测出来后禁用。这一条**非常值得 PeerDesk 照搬**。
- **画质**：
  - **静止 200 ms 后**把最后一帧再编最多 8 次，只补残差，直到输出小于 512 B 为止。这是 build-to-lossless 的廉价版。
  - GPU 零拷贝：DXGI → D3D11 VideoProcessor 转 NV12 → MF 硬件 MFT。1080p 编码耗时：H.264 约 5 ms，HEVC 约 4.5 ms，AV1 约 3 ms（RTX 4080 笔记本）。
  - 客户端按显示像素请求分辨率，只缩放一次；本地绘制光标，形状按槽位缓存。
- **局限**：只有 4:2:0（BT.709 有限范围），没有 FEC，Linux 只有 OpenH264 软件编码，没有 HDR。[architecture.zh-CN.md / limitations.zh-CN.md](https://github.com/sqlpxc/airhand)

---

### 1.13 国内竞品实机对比（产品负责人实测）

**2026-11-25**，同一场景、同一网络，用的是 PeerDesk 网络算法重构之前的版本（经典估计器）：

- **网易 UU 远程**：画面比 PeerDesk 模糊一些，产品负责人估计画质低约 20%。
  - 编码 H.265，UDP（可能是中继，也可能是穿透后的 P2P）。
  - 约 2.6 Mbps 时 20 帧左右；400–500 kbps 时 6–7 帧。
  - 和我们窄带上的表现一个量级：几百 kbps 时 720p 以上画面在动只能几帧每秒，这是带宽算出来的上限。
  - 1080p 打字顺畅。PeerDesk 打字一样顺畅，画质明显好一档。
  - 还缺我们在同一场景下的码率和帧率：画质好一档要是靠多用一倍带宽换来的，就不算赢。
    下次对比时看工具条或统计日志。
  - 同码率下 H.265 比 H.264 小 30–40%，按理在窄带上占优，但它的码控看起来没有把清晰度放在前面。
  - 对我们：H.265 值得作为可量化的选项。Mac、Windows 的硬件编码都支持；专利授权要先查清。
    现在的路线是 H.264 为主，以后加 AV1。
- **ToDesk**：画面清晰度和 PeerDesk 差不多。刚连上几次，控制端的网络就断了。
  - 这是稳定性差距。我们的原则是网络不好时告诉用户原因，不静默断开。

这些是用户实测的观察，不是我们按指标量出来的。下一次对比要记下来的东西：
- 网络条件：带宽、往返时间、丢包；
- 双方的帧率、码率、首帧时间、最长等待；
- 是否中继、用的什么编码。

## 2. 4K / 8K 专题

### 2.1 硬件编解码能力（以厂商矩阵为准，部分 **[未核实]**）

| 厂商 | 4K60 / AV1 | 8K | 4:4:4 编码 | 会话数和其他 |
|---|---|---|---|---|
| NVIDIA NVENC | Ada（8 代）、Blackwell（9 代）支持 AV1；Ada 首次支持 AV1/HEVC 10-bit 8K60（[App Note](https://docs.nvidia.com/video-technologies/video-codec-sdk/13.0/nvenc-application-note/index.html)） | 单 NVENC 撑不到 8K60，要靠 **SFE**：HEVC 分 2–3 条水平 slice；AV1 先切 4 个 3840×2160 tile 再细分为 8 或 12 个 tile。两路 SFE 约 1.8 倍吞吐、BD-rate 损失 <2%；三路损失 <4%（[8K60 SFE 博客](https://developer.nvidia.com/blog/video-encoding-at-8k60-with-split-frame-encoding-and-nvidia-ada-lovelace-architecture/)） | H.264 和 HEVC 支持；**AV1 不支持**。Blackwell 新增 H.264/HEVC 4:2:2 编解码和 AV1 UHQ（[Puget](https://www.pugetsystems.com/?p=33461)） | 消费卡**每台机器最多 8 个并发会话**（[App Note 12.2](https://docs.nvidia.com/video-technologies/video-codec-sdk/12.2/pdf/NVENC_Application_Note.pdf)）。NVENC 数量：4090/5080 为 2，5090 为 3 |
| AMD AMF/VCN | RDNA3 起支持 AV1；RDNA4 新增 AV1 B 帧 | RDNA4 号称 HEVC/AV1 **8K75**（[guru3d](https://www.guru3d.com/story/amd-rx-9070-series-graphics-cards-introduce-av1-bframe-encoding-support/)）。AV1 FRAMESIZE 最大 8192×4352（[AMF 文档](https://github.com/GPUOpen-LibrariesAndSDKs/AMF/blob/master/amf/doc/AMF_Video_Encode_AV1_API.md)） | **不支持**（社区长期请求） | 有 LTR、帧内刷新、按 tile 输出（`OUTPUT_MODE_TILE`）、AV1 Switch Frame |
| Intel QSV/VPL | Arc 起支持 AV1 | Arc 号称 8K AV1 10-bit **[二手]** | HEVC 4:4:4（Gen11+）**[未核实]** | 改分辨率必出 IDR（见 §3） |
| Apple VideoToolbox | H.264/HEVC 编码；**没有 AV1 编码**；M3 起支持 AV1 解码 **[部分核实]** | 8K HEVC 解码 **[未核实]** | 4:4:4 硬件编码 **[未核实]** | 有低延迟码率控制 |
| VA-API（Linux） | Intel 和 AMD 均支持 AV1/HEVC 10-bit | 依硬件而定 | 很少 | Sunshine 的 HDR 依赖 KMS 采集 |

**解码端约束**：
- DXVA 没有 HEVC 4:4:4 解码配置（Intel 和 NVIDIA 各用私有 API）。
- **AV1 4:4:4 基本没有硬件解码**。
- WebCodecs 和浏览器对 4:4:4 的支持很差。
- 单个 NVDEC 在 P1 预设下是 8K 转码的瓶颈（约 120 fps 上限）。
- Blackwell 的 NVDEC 号称单路 8K75。
- 手机、电视和 Mac 的 8K 解码普遍缺失或不稳定。

结论：要做 4:4:4，可行的组合只有 **HEVC RExt 4:4:4（NVIDIA 和 Intel 两端）**，以及 **AVC444 式“两路 4:2:0”**。后者能在任何 4:2:0 硬件上工作，也适用于 WebCodecs，是**唯一普适**的方案。

### 2.2 码率区间（估算，**[部分未核实]**）
- **游戏 4K60**：
  - Moonlight 默认 80 Mbps（H.264 时代的系数）。
  - HEVC/AV1 在 40–80 Mbps 已经很好；GFN Cinematic 上限 100 Mbps。
  - 4K120 约 100–150 Mbps。
- **电影 4K HDR（10-bit）**：
  - 低延迟模式不用 B 帧和长预读，效率比离线编码低约 20–40%（经验估计）。
  - 接近透明画质大约需要 60–120 Mbps；UHD 蓝光峰值 100+ Mbps 可作参照。
- **8K60 AV1**：约 150–300 Mbps（估算）。超过 400 Mbps 时局域网才比较从容。
- **4:4:4 屏幕内容**：比 4:2:0 多约 10–50%，取决于色度变化。AVC444 的色度流不变时可以省掉。
- **超低延迟局域网档**（PyroWave 一类帧内小波）：250 Mbps–1 Gbps。

### 2.3 零拷贝 GPU 管线
- **Windows**：DXGI Desktop Duplication 或 WGC 采集 → D3D11/D3D12 纹理 → 用 VideoProcessor 或 compute shader 缩放并转 NV12/P010/AYUV → 送入 NVENC（`nvEncRegisterResource`）、AMF（DX11 surface）或 QSV/MFT（`MFCreateDXGISurfaceBuffer`）。Airhand 已经验证可行。
- **Linux**：KMS/DRM PRIME 或 PipeWire dmabuf → VA-API 导入，或经 EGL/CUDA 送入 NVENC。Sunshine 的 KMS 路径可以参考。
- **macOS**：ScreenCaptureKit 输出 IOSurface，直接交给 VT。
- **解码端**：D3D11VA/NVDEC/VT 解出纹理，直接上屏。浏览器端用 WebCodecs VideoFrame 画到 canvas 或 WebGPU，不读回内存。

### 2.4 子帧 / slice 输出（降延迟）
- **NVENC**：多 slice 或 tile；`reportSliceOffsets` 要求同步模式；HEVC 子帧读回和 SFE 互斥（[NVENC 编程指南](https://docs.nvidia.com/video-technologies/video-codec-sdk/13.0/nvenc-video-encoder-api-prog-guide/index.html)）。
- **AMF**：HEVC/AV1 支持 `OUTPUT_MODE_TILE`。
- **做法**：每个 slice 或 tile 一出来就发，接收端整帧到齐再解码。也可以在协议层把每帧流拆成 slice 粒度，让 FEC 和重传更早开始。4K60 下单帧编码约 4–8 ms，slice 输出能省 2–5 ms（估算）。

### 2.5 多编码器分块做 8K
- **首选 NVENC SFE**：一个会话、一条码流，对解码器透明。
- **次选：应用层拆成 2–4 个 4K 独立码流**（每个编码器负责一个象限），接收端用多个解码器拼接。缺点：
  - 块边界有接缝和码率分配不均的问题；
  - 恢复要按块做，正好能复用 Airhand 的“多路独立恢复”；
  - 占用多个会话名额（NVIDIA 消费卡总共 8 个）；
  - 客户端要有多路解码能力。

### 2.6 1.0 与以后

| 1.0 现实目标 | 以后 |
|---|---|
| 4K60 HEVC/AV1 10-bit，40–80 Mbps，零拷贝（Windows NVENC/AMF/QSV，macOS VT） | 8K60（SFE 或分块），需要 RTX 40/50 或 RDNA4 主机加 8K 解码客户端 |
| 文字清晰：静止补画质 + AVC444 式双 4:2:0（或在 HEVC 4:4:4 可用时用原生 4:4:4） | 区域混合编码（文字区无损），HDR 4:4:4 |
| HDR10（HEVC Main10/AV1 10-bit，Windows） | 120–240 fps 4K，帧内小波“局域网档” |
| 每帧独立流 + LTR/RFI + 小比例自适应 FEC | L4S/ECN、硬件包间隔发送、AV1 RPR 无缝切换分辨率 |

---

## 3. 无卡顿切换清晰度

**ToDesk 卡几秒的可能原因**（**[推断]**，ToDesk 不开源）：
- 换档时销毁并重建编码器，甚至重建 D3D 设备；
- 客户端同步关闭并重开解码器，期间清空画面或显示黑屏；
- 新分辨率从一个**不限大小的 IDR** 开始。按 20 Mbps 算，一个 4K IDR 约 0.5–1 MB，传输就要 200–400 ms，丢包重传再叠加；
- 抖动缓冲被清空、要重新起步。

几项叠在一起就是数秒。

### 3.1 各编码器“不出 IDR”能改什么

| 编码器 | 码率 | 帧率 | 分辨率 |
|---|---|---|---|
| NVENC | `nvEncReconfigureEncoder`（`resetEncoder=0, forceIDR=0`）可平滑改，ffmpeg 一直这么用 | 有报告称改 frameRate 会**自动出 IDR**（[论坛](https://forums.developer.nvidia.com/t/nvenc-generates-idr-frames-when-reconfiguring-the-codec-with-a-new-framerate/125469)），所以**不要改编码器帧率**，靠少喂帧 | 初始化时必须设置 `maxEncodeWidth/Height`，之后才能在会话内改分辨率。文档“建议”`forceIDR=1`（[编程指南 8.4](https://docs.nvidia.com/video-technologies/video-codec-sdk/13.0/nvenc-video-encoder-api-prog-guide/index.html)）。H.264/HEVC 换分辨率要新 SPS，**实际必须出 IDR**。AV1 不出 IDR 的路径 **[未核实]** |
| AMF | `TARGET_BITRATE` 等动态属性在下一次 `Submit()` 前生效 | 同上 | `FRAMESIZE` 是初始化参数，需要 `ReInit`，等于出关键帧 **[推断]**。AV1 有 **Switch Frame**，可用于码流间切换 |
| QSV/oneVPL | `Reset` 可以改 | — | 不超过初始化尺寸时可以改，但**必然插入 IDR 和新 SPS**（[oneVPL 规范](https://spec.oneapi.io/onevpl/2.8.0/appendix/VPL_apnds_c.html)） |
| MF 硬件 MFT | `CODECAPI_AVEncCommonMeanBitRate` 可动态改 | — | 要改输出类型，相当于重建并出 IDR **[部分未核实]** |
| VideoToolbox | `AverageBitRate`、`DataRateLimits` 可动态改 | — | 会话尺寸固定，只能新建会话并出关键帧 |
| 编解码标准层面 | — | — | **VP9/AV1 允许帧间改分辨率**，参考帧会被缩放（AV1 缩放比例有限制；WebRTC 的 libaom 封装会检查比例，[源码](https://webrtc.googlesource.com/src/+/HEAD/modules/video_coding/codecs/av1/libaom_av1_encoder_v2.cc)）。**VVC RPR** 支持 1/2 到 8 倍的缩放比例，不需要 IRAP 帧。GFN 宣称用了 “AV1 + RPR” |

### 3.2 推荐做法（按优先级）
1. **降级顺序：QP/码率 → 帧率 → 分辨率**。
   - 码率每帧都调，用 reconfigure 并且不出 IDR。
   - 帧率**只在采集或调度层丢帧**，不改编码器参数。码率控制的 VBV 按毫秒设，帧率变化时自动适应。
   - 分辨率要等“码率长时间（例如 5–10 秒）低于该档下限，且 QP 持续顶到上限”才降；升档要更保守，滞回 10–20 秒，并先用探测确认有余量。
   - 这也是 WebRTC quality scaler（用 QP 阈值触发缩放）和 RustDesk“先降码率再降帧率”的做法。
2. **分辨率切换先建后切（make-before-break）**：
   - 用第二个编码会话（NVENC 消费卡最多 8 个）先编新分辨率，第一帧是**限制大小的 IDR**：VBV 为 1–2 帧码率，然后靠静止补画质或残差把画质补上来。
   - 新路径用新的帧流，接收端在**第二个解码器实例**里先解。拿到第一帧可显示的画面后，在下一个 vsync 原子切换；旧码流此时才停。
   - 代价是 1–3 帧的双路编码和一个小 IDR。
   - 对 PeerDesk 来说，“每帧一条 QUIC 流”天然支持两路码流并存。
3. **能用 AV1 帧间缩放时就不出 IDR**：主机是 AV1 编码器、并且通过冒烟测试确认支持帧间改分辨率（参照 Airhand 测 LTR 的方法：编完后用 dav1d 或 ffmpeg 严格模式逐位比对）时，直接切。否则走第 2 条。
4. **固定编码分辨率时，用 AV1 superres 或“渲染尺寸 ≠ 编码尺寸”降低实际编码量**。**[未核实]**：硬件编码器是否支持。
5. **接收端**：
   - 解码器按最大分辨率预分配；切换时**保留最后一帧**，不能出现黑屏或清屏。
   - 显示区域尺寸不变，新分辨率由 GPU 缩放到同一窗口，避免布局跳动。
   - WebCodecs 下 `configure()` 只在关键帧上做，并且要在新解码器实例上做。
   - 抖动缓冲和音画同步状态不清空。
6. **IDR 本身也要防卡顿**：限制单帧最大字节；按节奏发送，不要一次突发；必要时用帧内刷新代替 IDR。
7. **UI 不要因为切档弹提示**。档位变化只在统计面板里展示。

---

## 4. 对比表（主观评价）

| 系统 | 视频传输 | 恢复 | 文字清晰 | 4K/HDR/高帧率 | 对 PeerDesk 的参考价值 |
|---|---|---|---|---|---|
| Parsec | UDP+DTLS，自研可靠传输，延迟敏感的拥塞控制 | 未公开 | 4:4:4（Win，付费） | 4K60、240fps（局域网 4–8 ms），HDR 未知 | 高：理念（不缓冲、编码器即码率控制） |
| Sunshine/Moonlight | RTP/UDP，固定码率，RS-FEC 20% | RFI（NVENC）或 IDR、帧内刷新 | 4:4:4（Win，H.264/HEVC） | 4K120、HDR、AV1 | 高：FEC 参数、RFI、SFE 开关 |
| GeForce NOW | UDP，自研拥塞控制，L4S，硬件包间隔发送 | 未公开 | 4:4:4 + AV1 RPR | 5K120、360fps、HDR、100 Mbps | 高：L4S 和 RPR 的方向 |
| Steam Remote Play | UDP（细节未公开） | 未公开 | PyroWave 4:4:4 | 4K60、局域网 1 Gbps 档 | 中：帧内小波局域网档 |
| Stadia/xCloud | WebRTC | WebRTC 标准机制 | 4:2:0 | Stadia 4K，xCloud 1080p | 低–中 |
| RDP | RDPEUDP（可靠或不可靠 + FEC）/ URCP | — | AVC444 + 渐进式编解码 + 混合模式 | 以办公为主 | **高：AVC444** |
| Citrix HDX | EDT（可靠 UDP）+ TCP 回退 | — | 选择性 H.264 + 2DRLE 无损 + build-to-lossless | 办公/3D | 高：区域混合编码 |
| DCV | QUIC / WebSocket | — | 4:4:4，补到无损 | 4K60 × 4 屏 | 中：QUIC 已可用于生产 |
| PCoIP Ultra | UDP | — | build-to-lossless（AVX2）+ NVENC 自动切换 | 4K60 | 中：CPU/GPU 双模 |
| AnyDesk/TV | TCP/UDP | 未知 | DeskRT | 60fps | 低 |
| RustDesk | TCP/KCP/WebRTC，全可靠 | 关键帧 | 4:2:0 | 一般 | 低（反面教材：队头阻塞） |
| Airhand | QUIC 每帧一条流 + BBR + 在途字节控制 | **LTR + 冒烟测试** | 静止补画质 | 1080p 实测，4:2:0 | **很高：同一技术栈** |

---

## 5. PeerDesk 应采纳的技术（按影响排序）

1. **按帧独立传输**（每帧一条 QUIC 流或 datagram 分片），从根上消除队头阻塞。主机放弃过时帧时 reset 对应的流。参考 Airhand v10。
2. **延迟驱动的应用层码率控制**：每帧 ACK、在途字节上限、排队时延阈值（如 10/30 ms）、跳到最新帧，编码器码率每帧 reconfigure。不要依赖 quinn 自带的 BBR/Cubic 决定视频码率。
3. **LTR / 参考帧失效恢复，并在建编码器时做冒烟测试**。不支持的编码器退回“限制大小的 IDR + 门控”。
4. **无卡顿切档**：降级顺序、滞回、先建后切、保留最后一帧（§3）。
5. **文字清晰**：静止补画质（1.0）→ AVC444 式双 4:2:0（1.0 后期）→ 原生 HEVC 4:4:4（可用时）→ 区域混合无损（以后）。
6. **零拷贝 GPU 管线和 slice/tile 级输出**。
7. **自适应 FEC**：按每帧的包数和丢包率选 0–20% 的 RS 冗余，小帧至少 2 个校验包（Moonlight 经验），只在 RTT 大于约 30 ms 或丢包率大于 1% 时开启。用于降低长 RTT 链路上的重传延迟。
8. **硬件包间隔发送**：每帧均匀铺在约 0.5–1 个帧间隔内发送，Linux 上用 GSO 和 `SO_TXTIME`。后续加 **L4S/ECN 反馈**（quinn 支持 ECN），跟进 GFN 的方向。
9. **HDR10**：HEVC Main10/AV1 10-bit，P010 零拷贝。
10. **8K**：NVENC SFE（`AUTO_FORCED`）或应用层分块，作为 1.x 之后的目标。另设局域网“帧内小波档”作为可选实验。

---

## 6. 未能核实的声明
- Parsec 的 FEC 和恢复机制、HDR 支持；Steam Remote Play 的传输细节；TeamViewer 的全部细节；AnyDesk 的传输方式。
- GFN “AV1 + RPR” 只见于媒体报道；NVENC AV1 能否在不出 IDR 的情况下改分辨率。
- RDPEUDP2 是否保留 FEC；AMF/MF 改分辨率是否一定出 IDR（仅凭文档推断）。
- Intel、Apple 的 4:4:4 和 8K 能力；§2.2 的码率数字为工程估算。
- ToDesk 卡顿的原因为推断。
