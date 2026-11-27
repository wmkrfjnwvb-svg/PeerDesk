PeerDesk 服务端（peerdesk-server）
====================================

信令和中继服务端：两台机器不在同一网络时，用来互相找到对方；打洞不通时转发数据（端到端加密，中继只见密文）。
普通用户用默认服务器即可，不需要它。自己部署时：

  peerdesk-server <配置文件>

配置示例见项目的 config 目录。生产部署推荐 Linux 服务端包（systemd 单元、安装脚本）或 Docker 镜像。
macOS 第一次运行前执行：xattr -d com.apple.quarantine peerdesk-server

------------------------------------------------------------------------

PeerDesk server (peerdesk-server): signaling and relay. Lets two machines on different networks find each other
and relays traffic when hole punching fails (end-to-end encrypted; the relay only sees ciphertext).
Regular users can use the default server. To self-host:

  peerdesk-server <config>

See the config folder of the project for examples. For production use the Linux server package (systemd unit,
install script) or the Docker image. On macOS run `xattr -d com.apple.quarantine peerdesk-server` first.
