# 调研：远程文件管理（2026-11-28，Sonnet 子代理读原文和我们的代码；【未核实】的要再核）

## 结论

我们的底座已经到常见产品的中上水平：BLAKE3 整文件校验、`.part` 断点续传、文件夹递归、双栏 + 队列 + 历史。
真正的缺口有四类：冲突策略、对远端的文件操作、传输给视频让路、权限和错误信息。这四类都是核心用户（开发者）最先踩到的。

## 一、竞品（只列有出处的）

- Splashtop：拖放、文件管理器、复制粘贴（只 Windows）、会话外传输（https://www.splashtop.com/features/file-transfer）。
- AnyDesk：独立文件管理器，会话内或单独的文件会话都能用（https://support.anydesk.com/File_Manager_and_File_Transfer）。
- TeamViewer：任务队列（社区帖）。用户抱怨只能覆盖、续传不可靠、大文件卡在 60–70%：
  https://community.teamviewer.com/English/discussion/25819 、https://community.teamviewer.com/English/discussion/52031
- Jump Desktop：Fluid 协议长期没有文件传输（2021 年的帖子，最新状态【未核实】）。
- Chrome Remote Desktop：侧栏上传下载，一次只能选一个文件（第三方文章）。
- RustDesk：第三方称不支持续传（不可信，官方文档未读到）。ToDesk：【未核实】。
- 开发者工具参照：Cyberduck 的冲突动作（提示、覆盖、比较、续传、重命名、重命名已有、跳过）、持久队列、并发和带宽上限
  （https://docs.cyberduck.io/cyberduck/transfer）。
- Android：要走 Storage Access Framework；Android 11 起不能授权内部存储根目录和 Download 目录
  （https://developer.android.com/training/data-storage/shared/documents-files）。

## 二、我们的代码：已有的

- 协议：Places / List / MakeDir / MakeFile / Put / Get（`crates/core/src/browse.rs:42-70`）；
  传输 Offer、Answer（已有字节和前缀哈希）、Start、Data（64 KB）、End（BLAKE3）、Result（`crates/core/src/file.rs`）。
- 续传：`.part` 文件、整文件哈希、落盘后改名（`fs_store.rs:107-119`）；回环测试只发缺的部分（`crates/net/tests/loopback.rs:280`）。
- 界面：左本机右对方的双栏、栏间拖放、从系统拖进窗口上传、队列置顶/取消/重试、进度速度剩余时间、历史最多 500 条。
- 权限：一个开关 `permissions.files`（`host.rs:537`）。

## 三、缺口和问题（按严重度）

1. **没有冲突策略，默认静默覆盖**：接收方对所有文件回 Accept（`file.rs` 的 Offer 分支），`finish` 直接改名覆盖（`fs_store.rs:117-118`）。
2. **目录语义**：
   - 一次最多 4096 个文件，超了整个任务失败（`file.rs:22`、`fs_store.rs:130`）：拖 node_modules、target 就挂，也没有 .gitignore 或排除；
   - 符号链接静默跳过（`fs_store.rs:126-128`），空目录不传；
   - 非 UTF-8 名字或带冒号的名字让整个扫描失败（`fs_store.rs:150-152`）；
   - `check_path` 在 Linux/macOS 上也拒绝 Windows 保留名（`file.rs:37-65`）。
3. **性能**：每个 64 KB 块重新 open 再 seek（`fs_store.rs:27-36`、`81-90`）；同步磁盘 IO 在异步循环里（`files.rs:123-126`）；
   续传前两端各把已有部分读一遍算哈希（2 GB 就各读 2 GB）；没有吞吐基准。
4. **并行和重连**：队列同时只跑一个；断线要手动重试，没有自动重连续传；取消直接 abort、不发 Abort，对端残留 `.part`（`transfers.rs:393-400`）。
5. **权限和安全**：
   - 一个开关管列目录、读、写、新建，路径是整盘绝对路径；
   - 没有只读模式、限定目录、逐会话确认；
   - 被控端的 `Progress` 传的是 default（`host.rs:589`），被控端用户看不到在传什么；
   - 能写 ~/.ssh、shell rc（已有键鼠控制，不算新增权限，但 MCP 和手机场景要有只读和目录白名单）；
   - 接收目录里的符号链接目录不检查，写入会穿过去。
6. **错误信息违反“给原因和办法”**：失败显示成 “x 没有传完（Some(Failed)）”（Debug 输出，`transfers.rs:296-305`）；
   Abort 原因截到 60 字符（`files.rs:131、189`）；`reason()` 只翻译 3 种错误。
7. **列目录**：阻塞的 `read_dir`、整个读进内存；2 万项上限但消息上限 4 MB（推断，未测）。
8. **操作缺失**：删除、重命名、移动、复制、预览、打开远端文件改完存回、搜索。
9. **测试**：没有断网、取消、冲突、并发、大文件、传输时视频是否流畅的测试；`files_window.rs` 不计入覆盖率。

## 四、传输层

- 一个任务一条 QUIC 流是对的，文件多时 2–3 条并行。
- 视频走数据报、文件走流，共用同一个拥塞控制。文件字节有没有计入实时层的带宽核算【未核实】。
  流优先级管不到数据报，要在应用层限速：文件速率 = 链路估计 − 视频 − 输入，留下限，加用户可调的上限。
- 中继是成本：默认限制经中继的文件速率或月流量，提示“直连后更快”；配额挂在账户订阅上。
- 压缩（设计建议）：zstd 按块，能力位协商；先压 64 KB 试探，压不动（图片、zip、视频）就直传。

## 五、v1 清单和验收（按核心用户价值）

必做：
1. 传输给视频让路：大文件上传时视频冻结 ≤ 1 s，输入延迟 p95 比空闲时高不过 20%；进 e2e，只升不降。
2. 断网自动续传：2 GB 中断 30 s 后自动续传，重发 ≤ 缺的部分 + 1 块；续传前的哈希准备 ≤ 3 s（前缀哈希改成分段）。
3. 冲突策略：覆盖、跳过、重命名、按大小和修改时间比较；默认不静默覆盖，有“应用到全部”。
4. 大目录：1 万个以上文件、node_modules 级目录能传完（流式清单），.gitignore 和排除模式；符号链接、空目录明确处理，不静默。
5. 吞吐：局域网回环 ≥ 500 MB/s 量级（块 ≥ 256 KB、复用文件句柄、阻塞 IO 放专用线程），基线进 e2e。
6. 错误信息：每个失败给原因和办法（磁盘满、权限、校验失败、源文件被改、对方关了文件权限），Abort 原因不截断。
7. 权限：只读模式、目录白名单、被控端可见的传输日志和进行中提示；MCP 和手机默认只读。
8. 取消：1 s 内两端释放，清理 `.part`。
9. 远端操作：删除（进回收站）、重命名、移动。

以后：并行流、带宽上限、zstd 压缩、预览、打开远端文件改完存回、剪贴板文件打磨、手机端（SAF、分享菜单）、从远程窗口拖到本机、校验和对比。
