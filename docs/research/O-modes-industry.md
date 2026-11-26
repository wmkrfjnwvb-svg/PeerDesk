# 远程桌面和串流的“模式”：同行调研

调研日期：2026-11-26。

可信度标记：
- [原文] 我读过原文页面，数字来自原文。
- [搜索摘要] 只看到搜索结果的摘要，没读到原文（页面 403 或没打开）。
- [二手] 第三方文章、论坛、评测。
- [推断] 我自己的判断，没有出处。

## 先说结论

1. 同行里真正叫“模式”的很少。能确认的有三类：
   - AnyDesk 的工作模式：效率（默认）、视频（Beta）、游戏（Beta）。
   - GeForce NOW 的画质预设，其中 Cinematic（电影画质串流）是 4:4:4、10 位 HDR、AV1、最高 100 Mbps。
   - Zoom 和 Teams 的音乐模式，核心做法是关掉声音处理并提高音频码率。
2. 视频类场景的共同做法：
   - 视频区域用视频编码、保帧率。
   - 静止文字用无损或高色度。
   - Chromium 的屏幕捕获文档把这个分类写得最清楚。
3. 业内没有一家把“音乐”和“电影”拆成两个远程桌面模式。PeerDesk 做这两个模式没有现成对标，要自己定参数。下面给出的数值有出处的部分已标明，其余是推断。

---

## 第 1 节 远程桌面和串流同行

### 1.1 Parsec

未读到原文。parsec 的帮助中心对脚本和浏览器都返回 403（Cloudflare 校验）。以下全部是搜索摘要。

- 设置项：分辨率、带宽上限、FPS、解码器、H.265、Prefer 4:4:4 Color、Constant FPS、10 位色。[搜索摘要]
- 4:4:4 需要先开 H.265，再在连接后的悬浮窗里开 “Prefer 4:4:4 Color”。官方提示 4:4:4 可能大幅增加负载，解码延迟高时先关它。[搜索摘要]
- 按用途给的推荐，不是一键模式：[搜索摘要]
  - 美术和动画：1920x1080，带宽上限 25 Mbps 以上，60 FPS 以上。
  - 程序员和写作：H.265、Constant FPS、Prefer 4:4:4 全开，带宽上限 25 Mbps 以上。
  - 游戏和 QA：Constant FPS 和 Prefer 4:4:4 关。
- 来源页面：
  - https://support.parsec.app/hc/en-us/articles/32381785123860-Improve-Stream-Quality-and-Color-Accuracy
  - https://support.parsec.app/hc/en-us/articles/32361397075604-Programmers-and-Writers
  - https://support.parsec.app/hc/en-us/articles/32361373629972-Gaming-Playtesting-Build-Reviews-and-QA
- 结论：Parsec 用“按职业给设置清单”代替模式。没有电影模式。

### 1.2 Moonlight / Sunshine

[原文] 读过。

- Sunshine 配置文档 https://docs.lizardbyte.dev/projects/sunshine/latest/md_docs_2configuration.html ：
  - `nvenc_preset` 默认 1，范围 P1 到 P7，数字越大压缩越好、编码延迟越高。
  - `nvenc_twopass` 默认 `quarter_res`。
  - `nvenc_spatial_aq` 默认关，建议低码率时开。
  - `nvenc_vbv_increase` 默认 0，范围 0 到 400，文档警告可能导致网络丢包。
  - `fec_percentage` 默认 20，范围 1 到 255。
  - `minimum_fps_target` 默认 0，表示最低帧率取串流帧率的一半，用于静止内容省带宽。
  - `hevc_mode`、`av1_mode` 默认 0，按编码器能力自动通告。
  - `stream_audio` 默认开。
- Moonlight 常见问题 https://github.com/moonlight-stream/moonlight-docs/wiki/Frequently-Asked-Questions ：
  - 码率至少比上行带宽低 1 Mbps。
  - 帧节奏（Android 客户端）有三档：“最低延迟”（解码后立刻渲染，可能丢帧）、“平衡”（推荐，最多缓冲 1 帧来吸收网络抖动和 V-Sync 漂移）、“最流畅”（从不丢帧，帧可能排队，延迟可能很高）。
- Moonlight README https://cdn.jsdelivr.net/gh/moonlight-stream/moonlight-qt@master/README.md ：Steam Link 构建上限 1080p、60 FPS、40 Mbps；支持 7.1 环绕声。
- 一个 fork 的预发布版有三档帧节奏：最低延迟 2.5 ms、平衡 4.5 ms（默认）、最流畅 6 ms。这是 fork 的预发布说明，不是官方。[二手] https://newreleases.io/project/github/Nonary/moonlight-qt/release/v6.1.0-vrr11
- 没查到：Moonlight 官方默认码率表。
- 结论：这里没有“电影模式”，但帧节奏三档（延迟换流畅）是现成的设计，电影模式可以直接参考。

### 1.3 NVIDIA GeForce NOW

- [原文] NVIDIA 新闻稿 https://nvidianews.nvidia.com/news/nvidia-blackwell-architecture-comes-to-geforce-now ：
  - Cinematic-Quality Streaming（CQS）支持“YUV 4:4:4 色度采样和 10 位 HDR”。
  - 使用 AV1 编码器，能随网速变化平滑调整。
  - 最高 100 Mbps。
  - 页面没给 CQS 专属的分辨率和帧率。
- [搜索摘要] 预设名称：Balanced、Competitive、Data Saver、Cinematic、Custom。Custom 里能调分辨率、帧率、码率、HDR。https://www.nvidia.com/en-us/geforce-now/release-highlights/
- [搜索摘要] 还有“按网络状况调整”选项：Optimal Latency（优先响应）和 Optimal Quality（优先画质）。同上页面。
- [二手] 某第三方站点给过一组每小时流量和分辨率：Data Saver 约 4 GB/h、720p；Balanced 约 10 GB/h、1080p；Competitive 约 5 GB/h、720p；Cinematic 约 12 GB/h、1440p。我没读到 NVIDIA 原文，这组数字不可靠，不要引用。来源线索：https://www.techbloat.com/best-geforce-now-settings-for-a-sharper-smoother-stream.html （我读了这页，它自己并没有列预设表，所以这组数字实际上来自搜索摘要里的另一个站点，出处不明。）
- [原文] 同一页的带宽要求（第三方转述 NVIDIA 要求页）：720p 60 FPS 15 Mbps；1080p 60 FPS 25 Mbps；1440p 120 FPS 35 Mbps；4K 120 FPS 45 Mbps。
- 结论：GeForce NOW 是唯一有“电影”预设的同行。它的电影档用的是 4:4:4、10 位 HDR、AV1 和高码率，不是更大的缓冲。

### 1.4 Steam Remote Play

- [搜索摘要] Valve 的支持文章说，质量设为 “Balanced” 或 “Fast” 会降低带宽占用和画质。高级选项里可以限制分辨率和带宽，并有 “Prioritize Network Traffic”。 https://help.steampowered.com/en/faqs/view/3E3D-BE6B-787D-A5D2 （我自己抓这页只拿到标题，正文没取到。）
- 三档：Fast、Balanced、Beautiful。[二手]
- [原文] https://store.steampowered.com/remoteplay 只说“实时视频编码加自定义低延迟网络协议”，没有任何数值。
- 没查到：每档的具体码率、分辨率。

### 1.5 Splashtop

- [搜索摘要] 质量选项：Original（默认）、Best Quality、Best Speed。Best Speed 是“性能优先于画质的视频压缩”。 https://support-splashtoponprem.splashtop.com/hc/en-us/articles/7032043975065
- [搜索摘要] FPS 档位里 Ultra High 为 60 FPS。H.265 和 4:4:4 只在 Business Access Performance 和 Enterprise 提供。 https://support-splashtopbusiness.splashtop.com/hc/articles/6193537936027 （我抓这页返回 403。）
- [二手] Splashtop 2026 年新闻稿称有 AI 优化编码器，并称最高 240 FPS，与帮助中心的 60 FPS 冲突，属宣传口径。https://www.splashtop.com/press/splashtop-introduces-ai-optimized-codec-to-power-smoother-workflows
- 没查到：专门针对“视频播放”的选项。我在搜索结果里没有看到。

### 1.6 AnyDesk

[原文] https://support.anydesk.com/docs/display.md

- 工作模式（仅 Windows）：
  - Efficiency（默认）：“针对一般任务优化”。
  - Video（Beta）：“让媒体播放更流畅”。
  - Gaming（Beta）：“增加鼠标捕获，用于沉浸式 3D 交互”。
- 画质预设：Best quality、Balanced（推荐日常）、Best performance（“为速度和低延迟优化”）。
- 页面没写模式对应的帧率、编码器、音频参数。
- 结论：这是同行里最接近 PeerDesk “模式”概念的做法，名字也叫“模式”，并且有视频模式和游戏模式。但没有公开具体改了什么。

### 1.7 RustDesk

- [搜索摘要] 图像质量有 Best、Balanced、Low、Custom。一个未合并的草案 PR 提到：Best 档在硬件编码器上改用恒定质量（CQP）；Balanced 码率比从 0.67 提到 1.0；Custom FPS 切回预设时重置为 30。 https://gh.r-mj.com/rustdesk/rustdesk/pull/14294 草案状态，不代表现行行为。
- 没查到：官方文档里的画质预设说明。
- 没有视频模式。

### 1.8 Chrome 远程桌面

- 没查到官方帧率或编码器说明。
- [搜索摘要] Chromium 源码 `remoting/protocol/webrtc_video_encoder_wrapper.cc` 里把帧间隔限制在 15 Hz 到 120 Hz 之间，三秒没有帧就请求关键帧（2020 年左右的快照）。https://chromium.googlesource.com/chromium/src.git/+/main/remoting/protocol/webrtc_video_encoder_wrapper.cc
- 没有视频模式。

### 1.9 国内产品

- ToDesk
  - [原文] 帮助中心 https://www.todesk.com/helpcenter/solo-305.html ：工具栏“显示”里有“真彩模式”，开启后色彩提升到无损，与 Windows 本地一致，多用于原画设计。硬编只支持 Nvidia 显卡，软编可选 AV1。“无损”和“智能无损”画质默认就是真彩色。
  - [原文] 页面横幅是宣传口径：“8K 无损原画，360 帧，3 ms 延迟”。
  - 帮助目录里有“连接模式，游戏模式怎么使用”的标题，正文没取到。
  - [二手] B 站文章称游戏模式优先响应速度，画质档为原画、超清、高清、流畅。未核实。 https://www.bilibili.com/read/cv43852307
  - 没查到：“影视模式”。我没找到官方有这个名字。
- 向日葵
  - [原文] https://service.oray.com/question/36756.html ：“真彩”模式使用无损无压缩的 4:4:4 色度采样，最高 4K。
  - [二手] 报道称画质最高 4K 144 帧，仅电脑控电脑（Windows），需要超级会员。 https://www.oray.com/news/36674.html
  - 没查到：视频或影音专用模式。
- UU 远程（网易）
  - [原文] 少数派 https://sspai.com/post/112405 ：“控制中心”的“画质”里有自动、原画、高清、清晰，也可手动调码率和帧率，最高 144 Hz；主被控端都支持时可开 HDR。文中没有给码率和帧率区间，也没提 4:4:4。
  - [搜索摘要] App Store 页面写有“4:4:4 真彩模式”。
  - [二手] 凤凰网科技称免费版开放 4K 144 Hz、500 Mbps 自定义码率、HDR、4:4:4。口气偏宣传，未核实。 https://tech.ifeng.com/c/8xFLAjFzHFR
- 结论：国内三家都在“真彩 4:4:4”上做文章，没有查到公开的“电影”或“音乐”模式。

---

## 第 2 节 会议和协作软件

### 2.1 Zoom

- Original Sound for Musicians
  - [原文] 官方德语页 https://support.zoom.com/hc/de/article?id=zm_kb&sysparm_article=KB0064200 ：开启后“关闭所有噪声抑制”。关闭后恢复你保存的噪声抑制设置。移动端只有这一个高级音频档。
  - 回声消除默认开。官方说“仅在用耳机隔离声音时才关”。
- High-fidelity music mode
  - [原文] 同一页：“关闭回声消除的后处理，并把音频编码质量提高到 48 kHz，96 kbps 单声道或 192 kbps 立体声”。需要专业声卡、话筒和耳机；输入设备要支持 48 kHz。
  - [原文] Zoom 博客 https://www.zoom.com/en/blog/high-fidelity-music-mode-professional-audio-on-zoom ：此模式“关闭回声消除和后处理，取消音频压缩，提高编码质量”，推荐有线网络，仅 Windows 和 Mac。博客没有写具体码率。
- Stereo audio：默认关，需要立体声话筒或声卡。[原文] 德语页。
- Optimize for video clip
  - [搜索摘要] 学校帮助页和 Zoom 镜像页：提高全屏视频播放时的屏幕共享帧率；勾选后自动勾选“共享声音”；要求上下行至少 1.5 Mbps；不看视频时开它可能让屏幕变模糊。 https://it.vcfa.edu/kb/zoom/optimizing-a-shared-video-clip-in-full-screen 、 https://online.berklee.edu/help/zoom/1844036-how-to-optimize-video-sharing-in-zoom
  - 没查到：优化后具体帧率数字。我抓 vcfa 页没取到正文。

### 2.2 Microsoft Teams

- [原文] https://support.microsoft.com/en-US/teams/notifications-settings/use-high-fidelity-music-mode-to-play-music-in-microsoft-teams ：
  - 32 kHz 采样率，带宽允许时 128 kbps，带宽不足时可降到 48 kbps。
  - 可以关闭噪声抑制、回声消除等音频效果。
  - 页面没说立体声还是单声道。
  - 建议用外接音频设备，不建议蓝牙。
  - 直播活动和空间音频下不可用。
  - 日常说话时要关掉此模式，省带宽，并且它压不住背景噪声。
- [原文] Azure 通信服务文档 https://learn.microsoft.com/en-us/azure/communication-services/concepts/voice-video-calling/music-mode ：同样 32 kHz、128 kbps、最低 48 kbps；“降低音频压缩以保持原声”；要求设备支持“两声道、32 kHz 或更高”。回声消除只有在音乐模式开启时才能单独开关。噪声抑制有 Off、Auto、Low、High 四档。
- [二手] Windows Central 引用路线图描述为“32 kHz 单声道 128 kbps”，与 Azure 文档的“两声道设备”要求不完全一致。我没有找到 Teams 本身确认立体声的来源。
- 结论：Teams 的音乐模式只有 32 kHz，也就是 16 kHz 带宽。这比 Zoom 的 48 kHz 低。

### 2.3 Google Meet

- [原文] https://workspaceupdates.googleblog.com/2020/04/high-quality-video-audio-meet.html ：共享单个 Chrome 标签页时，视频和声音能让所有人流畅看到听到；共享整个窗口或整个屏幕不享受这个优化。没有任何帧率、分辨率、码率数字。
- [搜索摘要] 现在的帮助页说，共享 Chrome 标签页默认带该标签页的声音；Chrome 142 以上、macOS 14.2 以上或 Windows 11 以上，共享窗口或屏幕时也能共享设备声音。 https://support.google.com/meet/answer/9308856
- 没查到：Meet 里有没有“视频和动作”或“文字和图片”这类选项。搜索没有结果。
- [原文] Chromium 意向发布 https://groups.google.com/a/chromium.org/g/blink-dev/c/FUlvv_Lmk-s ：新增 `maintain-framerate-and-resolution`，关闭 WebRTC 内部的视频自适应，让应用自己做自适应；由 Google Meet 推动。

### 2.4 Discord

- [搜索摘要] 频道语音码率上限：普通服务器 96 kbps，Boost 1 级 128 kbps，2 级 256 kbps，3 级 384 kbps；舞台频道 64 kbps。 https://docs.discord.com/developers/resources/channel 我没抓到原文，来自搜索摘要。
- [搜索摘要] 开发者 SDK 里 WebRTC 的回声消除、降噪、自动增益默认开，Krisp 默认关，两者互斥。 https://docs.discord.com/developers/discord-social-sdk/development-guides/managing-voice-chat 这是 SDK 默认值，桌面端可能不同。
- [原文] Discord 工程博客 https://discord.com/blog/how-it-all-goes-live-an-overview-of-discords-streaming-technology ：
  - 直播基于 WebRTC，码率受最慢观众限制。
  - 网络剧烈下降时编码器先丢帧。
  - 不做几秒的缓冲，用低延迟换质量。
  - 支持 VP8 和 H264，部分平台有 HEVC 和 AV1，优先硬编。
  - 音视频分开发 RTP 包，接收端再同步。
  - 文章没有写 Opus 参数、码率数字或直播的具体分辨率。
- [二手] 2020 年报道：直播默认 720p 30 fps，Nitro Classic 1080p 60 fps，Nitro 到 4K。 https://pcgamer.com/discord-is-adding-built-in-gameplay-streaming-to-its-voice-channels
- 没查到：屏幕共享声音的码率、采样率、是否立体声、是否关了回声消除。

### 2.5 这一节的共同做法

| 产品 | 关掉什么 | 音频参数 | 来源可信度 |
|---|---|---|---|
| Zoom Original Sound | 全部噪声抑制，回声消除可选关 | 未给 | 原文 |
| Zoom 高保真 | 回声消除后处理，取消压缩 | 48 kHz，96 kbps 单声道或 192 kbps 立体声 | 原文（德语页） |
| Teams 高保真音乐 | 噪声抑制、回声消除可关 | 32 kHz，128 kbps，最低 48 kbps | 原文 |
| Discord | 未查到 | 频道码率上限 64 到 384 kbps | 搜索摘要 |

共同点有三条。
- 都靠用户手动开，并提示“说话时不要开”。
- 都要求耳机或外接设备，因为关掉回声消除后会啸叫。
- 都建议有线网络。

---

## 第 3 节 标准和底层机制

### 3.1 contentHint

[原文] W3C 规范 https://www.w3.org/TR/mst-content-hint/

视频：
- `motion`：适合摄像头、电影、游戏。允许量化瑕疵和缩小分辨率来保运动。低码率时优先帧率。在 RTCPeerConnection 里默认降级偏好是 maintain-framerate，也就是先降分辨率。
- `detail`：适合演示、网页、绘画、线稿。偏向每帧细节，不偏向流畅。默认 maintain-resolution，也就是先降帧率。
- `text`：和 detail 一样，另外可以用针对文字的编码工具。AV1 时会启用文字模式编码工具。默认 maintain-resolution。
- `""`：无提示，由浏览器猜。
- 显式设置的 `degradationPreference` 会盖过 contentHint 带来的默认值。

音频：
- `speech`：可以做噪声抑制。回声消除和自动增益默认开。
- `music`：“应调整或关闭面向语音的处理，避免失真”。回声消除、自动增益、噪声抑制默认都关。
- `speech-recognition`：三项处理默认都关。

### 3.2 degradationPreference

[原文] MDN https://developer.mozilla.org/en-US/docs/Web/API/RTCRtpSender/setParameters ：
- `balanced`：默认，在帧率和分辨率之间平衡降级。
- `maintain-framerate`：降分辨率保帧率。
- `maintain-resolution`：降帧率保分辨率。
- `maintain-framerate-and-resolution`：两者都保，可能在编码前丢帧；适合自己做质量优化的应用。

[原文] Chromium 意向发布帖：`maintain-framerate-and-resolution` 关闭浏览器内部自适应。

没查到：Chromium 或 libwebrtc 里这些偏好在编码器里具体怎么实现（量化参数上限、丢帧策略）。W3C 规范完整页我只读了前 20 万字符，没找到 `RTCDegradationPreference` 的正式定义段落，内容以 MDN 和 contentHint 规范为准。

### 3.3 Opus

- [原文] RFC 7587 https://www.rfc-editor.org/rfc/rfc7587 （20 ms 帧）：
  - 窄带语音 8 到 12 kbps。
  - 宽带语音 16 到 20 kbps。
  - 全带语音 28 到 40 kbps。
  - 全带单声道音乐 48 到 64 kbps。
  - 全带立体声音乐 64 到 128 kbps。
  - RTP 时钟固定 48000 Hz；`stereo` 默认 0（单声道）；`useinbandfec` 默认 0；`ptime` 默认 20 ms；`maxplaybackrate` 默认 48000。
- [原文] Opus 官方 API 文档 https://opus-codec.org/docs/opus_api-1.3.1/group__opus__encoder.html ：
  - `OPUS_APPLICATION_VOIP`：给语音最佳质量，做高通滤波，强调共振峰和谐波，可选带内 FEC。
  - `OPUS_APPLICATION_AUDIO`：给非语音（如音乐）最佳质量，也用于音乐、广播和延迟要求小于 15 ms 的场合。
  - `OPUS_APPLICATION_RESTRICTED_LOWDELAY`：关闭语音优化模式，换取略低的延迟；只能在新建或刚重置的编码器上设置；文档要求谨慎使用。
- [原文] Xiph 推荐设置 https://wiki.xiph.org/Opus_Recommended_Settings ：
  - 音乐流媒体：立体声 64 到 96 kbps。
  - 音乐存档：立体声 96 到 128 kbps，128 kbps 可变码率“基本透明”。
  - 环绕声：5.1 为 128 到 256 kbps。
  - 语音：单声道 10 到 24 kbps。
  - 默认帧长 20 ms，音乐不建议超过 20 ms 的帧（极低码率除外）。
  - 约 19 kbps 以下 Opus 倾向把立体声降为单声道。
- 没查到：RFC 6716 里 AUDIO 与 VOIP 在内部算法上的细节。我没有读 RFC 6716 原文。

---

## 第 4 节 投屏遇到视频

### 4.1 AirPlay

- 没读到 Apple 的协议文档。这一条主要是二手。
- [搜索摘要] Nagra 的 OpenTV 文档：镜像时由手机取内容并渲染到接收端；AirPlay 流式播放时由接收端自己下载内容，发送端只交出流地址和授权。 https://docs.nagra.vision/opentv-platform-2.x-documentation/current/Default/streaming-to-airplay 这是 OpenTV 实现指南，不是 Apple 通用规范。
- [搜索摘要] Plex 论坛：开着镜像时，iPhone 会把视频重新编码再发给电视；Plex 的“投放”按钮走的是普通 AirPlay，不是镜像。 https://forums.plex.tv/t/if-airplay-to-a-tv-is-enabled-before-playback-starts-ios-app-transcodes-instead-of-direct-streaming/820864
- [原文] Apple WWDC19 第 507 场 https://developer.apple.com/videos/play/wwdc2019/507/ ：“AirPlay 2 视频让你把视频从 Apple 设备分享到智能电视”；电视的无缝切换不如 iOS 或 Apple TV。是否由接收端自己取流，这一页没明说，是我的推断。
- 没有证据表明：系统镜像时检测到视频会自动改走“视频模式”。是否发生在应用层（应用自己用 AVPlayer 的外部播放）是确定的，系统级自动切换我没查到。
- 对 PeerDesk 的含义 [推断]：AirPlay 的“视频直传”需要接收端能自己拿到片源。远程桌面里片源在浏览器里，我们拿不到地址，这条路走不通。能借鉴的只是“视频走单独的通道”。

### 4.2 Chromecast 投标签页

- [原文] Chromium 设计文档 https://www.chromium.org/developers/design-documents/auto-throttled-screen-capture-and-mirroring/ ：
  - 把内容分成交互内容（幻灯片、文章）和动画内容（视频、体育）。
  - 交互内容：分辨率拉满，不主动控帧率，目标码率保持不变。
  - 动画内容：分辨率“积极且立即”下调以保帧率；回升要“30 秒持续低负载”后才升一步；捕获节奏对齐检测到的动画帧率，文档例子是 24 FPS 视频，忽略一个 60 FPS 的转圈动画；延迟增加、画质下降来补偿，因为丢帧很显眼。
  - 检测方法：近期变化事件按像素数加权给损伤区域投票，需要至少 2/3 的多数；时间戳要有规律间隔且没有长停顿。
  - 分辨率用固定档位，16:9 的 1920x1080 例子是每档 90 行，每 3 秒最多变一次。
- [原文] Engadget https://www.engadget.com/2017/04/25/chrome-improves-casting-video-quality ：开发版加了开关，投标签页时视频全屏后，Chrome 直接把视频的原始码流发到电视。
- [搜索摘要] Google 帮助页：标签页投屏默认 High（720p），卡顿时降到 Standard（480p）；标签页投屏最高 720p。 https://support.google.com/chromecast/answer/3269219?hl=en
- [二手] 旧版 Softpedia 报道的 Extreme 档（720p 更高码率）和可自定义最小最大码率、最大帧率。 https://news.softpedia.com/news/Google-Cast-Beta-Gives-Chromecast-Users-1080p-458152.shtml
- 没查到：当前 Chrome 里叫“优化视频”的选项。没有找到这个名字。

### 4.3 Miracast

- [搜索摘要] Wi-Fi 联盟技术概述：H.264 是高清和超高清的必选编码；4K60 是可选。传输用 RTSP 控制加 RTP 数据。认证不规定最大延迟。 https://tools.barco.com/kb-downloads/4814/Wi-Fi_CERTIFIED_Miracast_Technical_Overview_20170725.pdf 我没读到原文，来自搜索摘要。
- 没查到：Miracast 是否检测视频并切换模式。没有找到相关证据。

---

## 第 5 节 电影场景的缓冲、帧节奏和视频区域检测

### 5.1 起播缓冲

- [原文] ExoPlayer（Android Media3）默认值 https://developer.android.com/reference/androidx/media3/exoplayer/DefaultLoadControl ：
  - 起播需要缓冲 1000 ms（`DEFAULT_BUFFER_FOR_PLAYBACK_MS`）。
  - 卡顿后恢复需要 2000 ms。
  - 稳态缓冲目标 `DEFAULT_MIN_BUFFER_MS` 和 `DEFAULT_MAX_BUFFER_MS` 都是 50000 ms。
  - 本地播放起播 1000 ms，卡顿后恢复 1000 ms。
- 老版本（2.9.6）曾是起播 2500 ms，卡顿后 5000 ms。[二手] https://www.akamai.com/blog/performance/enhancing-video-streaming-quality-for-exoplayer-part-2-exoplayers-buffering-strategy-how-to-lower
- [搜索摘要] Netflix 与斯坦福的 2014 年论文（BBA）：起播阶段缓冲几乎没有信息量，用吞吐估计更有用。课程笔记里的例子阈值是约 10 秒以下选最低码率，约 60 秒以上选最高码率；这是示意，不是 Netflix 生产值。 https://web.stanford.edu/class/cs244/papers/sigcomm2014-video.pdf
- 没查到：Netflix 和 YouTube 现在实际的起播缓冲秒数。我没有找到它们的官方披露。
- 重要区别 [推断]：点播播放器缓冲的是“未来的内容”，可以放几十秒。远程桌面的视频是实时抓屏，没有未来内容可缓冲，只能加播放端的抖动缓冲，用延迟换平滑。电影不需要交互，所以可以接受几百毫秒的额外延迟。

### 5.2 帧节奏

- [原文] Wikipedia 3:2 下拉 https://en.wikipedia.org/wiki/Three-two_pull_down ：
  - 24 帧电影转 60 Hz 要用 2-3-2-3 节奏，4 个电影帧循环一次。
  - 页面把“24 帧转 59.94 帧的 60 Hz 逐行显示”也算在这个词里。
  - 页面没有展开讲抖动（judder）的成因，也没提 120 Hz 或 24 Hz 显示的解法。
- [原文] Chromium 设计文档：动画内容的捕获节奏对齐检测到的动画帧率，例子是 24 FPS 视频。
- [原文] Moonlight 常见问题：帧节奏分“最低延迟、平衡、最流畅”三档，最多缓冲 1 帧来吸收抖动和 V-Sync 漂移。
- [推断] 在 60 Hz 显示器上播 24 fps 内容，每帧显示 3 个或 2 个刷新周期，交替出现，这就是抖动。远程桌面如果按 60 fps 一帧不漏地发，发送端负担是 60 fps 的编码，而内容只有 24 fps 的变化。更省的做法是检测到 24 fps 的节奏后只编变化的帧，接收端按显示器刷新出帧。没有同行公开这样做，这是设计建议，不是业界做法。

### 5.3 视频区域检测

- Citrix HDX Thinwire
  - [原文] https://docs.citrix.com/en-us/citrix-virtual-apps-desktops/2311/graphics/thinwire ：自适应显示技术“识别运动图像（视频、运动中的 3D）”，只对变化的区域用视频编码；这是“For actively changing regions”，等价于默认的“Use video codec when preferred”。其余桌面内容用 2DRLE（或客户端支持时用 MDRLE）无损编码。活动期间用视频编码，停止后逐步锐化为像素级无损。文章没给运动检测的数值阈值。
  - [搜索摘要] “Optimize for 3D graphics workload” 默认关；开启后关闭无损文字检测，全屏用 H.264、H.265 或 AV1。 https://docs.citrix.com/en-us/citrix-virtual-apps-desktops/2311/policies/reference/ica-policy-settings/graphics-policy-settings
- Microsoft RDP / Azure Virtual Desktop
  - [原文] https://learn.microsoft.com/en-us/azure/virtual-desktop/graphics-encoding ：
    - 混合模式：先检测帧里有没有视频，有就送 H.264 视频编码；没有再判断增量、运动、缓存，再区分文字和图片，各用不同的编码器。
    - 一般远程会话约 80% 的图形数据是文字，所以文字用专门的编码器。
    - 全屏视频编码（H.264 或 H.265）“帧率更高、体验更好”，但带宽和资源更多；对以文字为主的内容表现不如混合模式；适合 3D 建模、CAD 和视频播放编辑。
    - H.265 比 H.264 同画质省 25% 到 50% 数据。
    - 色度默认 4:2:0；要 4:4:4 需要开 H.264 全屏编码并把图像质量设为 High，带宽更高。
    - 官方推荐“多媒体重定向”：把视频的码流直接发到本地，本地解码，并在正确位置渲染，比任何编码配置都好，也降低远端 CPU 负担。
- VMware / Omnissa Blast
  - [搜索摘要] 默认 H.264 用 4:2:0；High Color Accuracy 关闭 4:2:0 用 4:4:4，代价是很多 GPU 不支持 4:4:4 硬解，会退回 CPU 解码。有“动态编码器切换”：视频优化编码器（H.264 4:2:0 或 4:4:4）与文字优化编码器（Blast Codec 或 Adaptive）之间切换。 https://techzone.omnissa.com/resource/omnissa-blast-extreme-optimization-guide 我抓这页返回 404，来自搜索摘要。
- 结论：视频区域检测是企业产品的标配做法。核心思路是“视频区域用视频编码保帧率，其余用文字友好编码，静止后锐化”，PeerDesk 的“块编码对照、静止补清晰”的方向与此一致。

---

## 第 6 节 对 PeerDesk 的建议

先说前提。PeerDesk 的原则是“网络差时宁可慢，也不能糊到没法用”。电影和音乐模式是用户主动选的，可以改变这个取舍：电影模式以流畅优先，音乐模式以声音保真优先。但必须在工具条里明说“现在是电影模式，文字可能变糊”。这与“不静默降级”一致，因为是用户自己选的。

### 6.1 电影模式

| 参数 | 建议值 | 依据 |
|---|---|---|
| 降级顺序 | 先降分辨率，再降画质，帧率最后动 | contentHint `motion` 默认 maintain-framerate（W3C，原文） |
| 目标帧率 | 跟随内容：检测到 24、25、30 fps 内容就按其节奏，不强行 60 fps | Chromium 捕获文档的 24 FPS 例子（原文） |
| 分辨率回升 | 下调立即，回升等 30 秒低负载再升一档 | Chromium 捕获文档（原文） |
| 播放端抖动缓冲 | 起步 100 到 200 ms，可调到 500 ms；需要实测 | 推断。依据：Moonlight “平衡”档缓冲 1 帧，“最流畅”档不丢帧（原文）；ExoPlayer 本地播放起播 1000 ms（原文）。电影不需要交互，可以比游戏多缓冲，具体数字没有同行出处 |
| 色度 | 默认 4:2:0 | Parsec 的游戏场景关 4:4:4（搜索摘要）；Blast 和 AVD 默认 4:2:0（原文）。电影画面是自然图像，4:4:4 收益小、代价大 |
| 码率上限 | 1080p 24 到 30 fps：先试 15 到 25 Mbps | GeForce NOW 1080p 60 fps 要 25 Mbps（二手）；Steam Link 构建上限 40 Mbps（原文）。电影帧率低，可以比 60 fps 低一些。这个数是推断，要靠 e2e 实测定 |
| 编码器 | H.264 或 HEVC 硬编，打开空间自适应量化 | Sunshine 建议低码率时开 spatial AQ（原文）；HEVC 比 H.264 省 25% 到 50%（微软，原文） |
| 前向纠错 | 保持，不要因为电影模式关掉 | Sunshine 默认 20%（原文） |
| 静止补清晰 | 视频区域之外照常补；视频区域暂停后 0.5 秒内补清晰 | 沿用 PeerDesk 现有门槛。Citrix 的“停止后逐步锐化”（原文） |
| 视频区域检测 | 先不做区域，做整屏模式；以后再做区域检测 | 区域检测规则见 Chromium 文档：加权投票，至少 2/3 的多数（原文） |
| 声音 | 立体声 48 kHz，Opus AUDIO 模式，128 kbps；5.1 以后再说 | Opus 立体声音乐 64 到 128 kbps（RFC 7587，原文） |
| 音画同步 | 声音跟着视频缓冲一起延迟 | 推断，Discord 音视频分包再同步（原文） |

电影模式要写明的限制：
- 画面是实时抓屏，不是点播。没有“未来内容”可缓冲，所以不能照搬 Netflix 的几十秒缓冲。
- 受 DRM 保护的视频可能抓到黑屏。这一点我没有查证，需要单独测。
- 没有同行公开电影模式的数值。上表“依据”写“推断”的行，必须靠 e2e 实测定值。

### 6.2 音乐模式

| 参数 | 建议值 | 依据 |
|---|---|---|
| 音频应用模式 | `OPUS_APPLICATION_AUDIO`（不用 VOIP） | Opus 官方文档：AUDIO 为音乐，VOIP 做语音高通和共振峰强调（原文） |
| 声道和采样率 | 立体声，48 kHz | Zoom 高保真 48 kHz（原文）；RFC 7587 时钟 48000 Hz（原文）；Teams 只有 32 kHz，是反面参照 |
| 码率 | 默认 128 kbps；弱网降到 64 kbps；上限 192 kbps | RFC 7587 立体声音乐 64 到 128 kbps（原文）；Xiph 流媒体 64 到 96，存档 96 到 128（原文）；Zoom 高保真 192 kbps 立体声（原文） |
| 帧长 | 20 ms；追求低延迟时 10 ms | Xiph：音乐不建议超过 20 ms（原文）。Opus 的 10 ms 帧长是官方支持值（RFC 7587），但 10 ms 在同码率下质量更低，需要实测 |
| 带内 FEC | 开，丢包高时更依赖 | RFC 7587 `useinbandfec`（原文）。注意 Opus 在 VOIP 模式下才倾向用 FEC，AUDIO 模式下是否有效我没有核实 |
| 声音处理 | 主机音频是系统输出，不是话筒，所以不做回声消除、降噪、自动增益 | 推断。对照：W3C `music` 提示默认三项全关（原文）；Zoom Original Sound 关全部噪声抑制（原文） |
| 话筒回传（如果有） | 音乐模式下提示用耳机，并关回声消除 | Zoom：只在用耳机时才关回声消除（原文） |
| 视频 | 降到低帧率（例如 15 到 30 fps），把码率让给声音 | 推断。没有同行这样做的出处 |
| 网络差时 | 声音码率最后让步；先降视频 | 推断。与 PeerDesk “清晰度最后让步”原则一致 |
| 抖动缓冲 | 比办公模式大，起步 60 到 100 ms，需要实测 | 推断，无出处 |

音乐模式要写明的限制：
- Zoom 和 Teams 的“音乐模式”主要针对话筒里的音乐（演奏者）。PeerDesk 的场景是远端电脑播放音乐，声音来自系统输出，处理链不同。这条是推断，需要确认 PeerDesk 当前有没有话筒回传。
- 蓝牙耳机会限制本地回放质量。Teams 官方建议不用蓝牙（原文）。

### 6.3 哪些值得先做

按“收益大、成本低、有出处”排序：

1. 音乐模式的音频参数（最先做）。
   - 改动小：Opus 用 AUDIO 模式、立体声、48 kHz、128 kbps。
   - 有 RFC 和官方文档出处，不依赖视频管线。
   - 可以直接进 e2e：比较编码前后频谱，检查没有被降噪切掉。
2. 电影模式的降级顺序（先降分辨率保帧率）。
   - 就是切换 PeerDesk 已有的降级顺序，规范有明确说法。
   - 注意 PeerDesk 现有顺序是“画质 → 帧率 → 分辨率”。电影模式要改成“分辨率 → 画质，帧率最后”，这与办公模式相反，e2e 门槛要分模式设。
3. 电影模式的播放端抖动缓冲和帧节奏。
   - 用户最能感觉到的是流畅度。
   - Moonlight 已有三档设计可抄。
   - 缓冲秒数没有公开数据，需要先做可调参数，再用 e2e 实测定值。
4. 色度 4:4:4 只给办公模式做（可以晚做）。
   - 同行都把它放在“文字清晰”一侧：Parsec 给程序员、Citrix、AVD、Blast 都是文字场景。电影模式保持 4:2:0。
5. 视频区域检测（最晚做）。
   - 工作量最大，但是企业产品的标配，也是 Chrome 投屏的核心逻辑。
   - 可以先做整屏模式切换，用户手动选，区域检测放到后面。
6. 码率、缓冲的具体数值：全部要靠 e2e 实测，报告里推断的数字只能当起点。

### 6.4 没查到、要小心的地方

- Parsec 帮助中心原文全部没读到，只有搜索摘要。
- Splashtop 的性能选项页返回 403。
- Omnissa Blast 优化指南 404。
- Discord 音频码率只有搜索摘要，且屏幕共享声音的参数没查到。
- Google Meet 里有没有“优化视频”类选项没查到。
- Chromecast “优化视频”选项没找到这个名字。
- AirPlay 的 URL 交接机制没读到 Apple 原文。
- Miracast 的具体码率和延迟没查到，也没证据它会检测视频。
- Netflix、YouTube 当前的起播缓冲没查到。
- Chrome 远程桌面的视频策略没查到。
- ToDesk 的“影视”模式没查到；向日葵、UU 的视频专用模式没查到。
- Chromium 里 contentHint 与 degradationPreference 在编码器内部的实现细节没读到。
- Opus RFC 6716 原文没读。
