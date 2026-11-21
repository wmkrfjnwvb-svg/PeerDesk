# 部署 PeerDesk 服务端（Linux）

目标：在一台 Linux 机器上跑 `peerdesk-server`，让家里和办公室的两台电脑通过它互相找到对方并连接。

## 1. 服务端做什么

| 功能 | 说明 | 端口 |
|---|---|---|
| 信令 | 被控端在这里登记在线；控制端用设备指纹找到它，交换双方的公网地址 | UDP 4433 |
| 打洞 | 服务器告诉两边对方的公网地址，两边同时发包，在各自的 NAT 上打通一条直连通道 | 同上 |
| 中继 | 直连打不通（对称 NAT、严格防火墙）时，数据经服务器转发 | UDP 3478 |
| 管理页面 | 在线设备数、中继转发总量（`/api/status`） | TCP 8080，只绑定本机 |

**成本。** 信令和打洞只有几个很小的包，流量可以忽略。**中继**不一样：画面和输入全部经过服务器，
占用服务器的上行和下行带宽各一份。一路 1080p 远程桌面的中继流量，静止时很小，操作和滚动画面时可到几 Mbps
（具体数字以你的实际画面为准，管理页面的 `/api/status` 里有转发总量可以自己量）。
按流量计费的云服务器（常见的是出方向流量收费）要留意这一项；包月带宽的机器就是看峰值够不够。
能直连的连接不走中继，所以直连成功率越高，服务器越省钱，体验也越好。

## 1.1 多节点集群

一个节点够用时不需要集群。要做付费/免费分流、故障切换或分摊中继流量，可以跑多个节点：
付费节点（`role = "paid"`）优先服务会员，免费节点（`role = "free"`）服务其他人并限额
（默认每台设备每 10 分钟 200 MB，可配置），满了自动溢出。节点之间用 `cluster_secret` 认证，会员令牌和中继票据用 `billing_secret` 签名。
Docker 三节点示例、加节点、签发会员令牌、防火墙端口和单二进制多节点部署步骤见
[deploy/docker/README.md](../deploy/docker/README.md)。服务端并发压力测试在提交信息里写 `[load]` 触发（同一份说明里有介绍）。

## 2. 前提

- 一台有**公网 IP** 的 Linux 机器（x86-64，有 systemd）。家里的机器放在 NAT 后面也行，但要在路由器上把
  UDP 4433 和 UDP 3478 转发到它，并用 `--public-ip` 指定路由器的公网 IP。
- 放行两个 **UDP** 端口：4433（信令）和 3478（中继）。注意是 UDP，不是 TCP。
- root 权限。

## 3. 安装

1. 在 CI 的构建产物里下载 `peerdesk-server-<提交>-linux-x64.tar.gz`（提交信息带 `[pkg]` 时生成，
   产物名 `peerdesk-<提交>-linux-server`，解压后得到这个 tar.gz），传到服务器。
2. 安装：

   ```bash
   tar xzf peerdesk-server-*-linux-x64.tar.gz
   cd peerdesk-server-*-linux-x64
   sudo ./install.sh                      # 自动探测公网 IP
   sudo ./install.sh --public-ip 1.2.3.4  # 探测不到、或机器在 NAT 后面时手工指定
   ```

   脚本做的事：创建系统用户 `peerdesk`；二进制装到 `/usr/local/bin/peerdesk-server`；
   配置写到 `/etc/peerdesk/server.toml`（已存在就保留）；systemd 单元装到
   `/etc/systemd/system/peerdesk-server.service`；启动并设为开机自启；最后打印服务器指纹。
   可以重复运行。
3. 检查状态：

   ```bash
   systemctl status peerdesk-server
   curl -s http://127.0.0.1:8080/api/status
   ```

不用 systemd 时：`./peerdesk-server server.toml`，把 `data_dir` 改成一个自己能写的目录。

## 4. 防火墙

```bash
# ufw（Ubuntu / Debian）
sudo ufw allow 4433/udp
sudo ufw allow 3478/udp

# firewalld（CentOS / Rocky / Fedora）
sudo firewall-cmd --permanent --add-port=4433/udp --add-port=3478/udp
sudo firewall-cmd --reload
```

云服务器还有一层**安全组**（阿里云、腾讯云、AWS、Azure 等）：在控制台给这台实例的入方向规则添加
UDP 4433 和 UDP 3478，来源 `0.0.0.0/0`。只改系统防火墙不改安全组，外面仍然连不上。
管理端口 8080 不要放行。

## 5. 服务器指纹

客户端靠指纹确认连到的是你的服务器，而不是别人冒充的。指纹是服务器证书的 SHA-256（64 个十六进制字符）。

- `install.sh` 结束时会打印。
- 随时可以从日志里再查：`journalctl -u peerdesk-server | grep 指纹`，启动时有一行
  `信令 0.0.0.0:4433，服务器指纹 <64 位十六进制>`。

指纹保存在 `/var/lib/peerdesk` 里的证书和私钥上，重启、升级不会变。

## 6. 两台电脑怎么用

两台电脑的 PeerDesk 窗口最下面都有“服务器（用设备码连接）”，填一次就记住：

- **服务器地址**：`<公网IP>:4433`
- **服务器指纹**：上一步的 64 位十六进制（防止连到冒充的服务器）

然后：

1. 家里的电脑（被控端）：勾选“允许远程控制”。macOS 上先按窗口里的权限向导授权。
   登记成功后“本机”里会显示 9 位**设备码**（例如 `123 456 789`）和**密码**。设备码固定不变，
   服务器重启也不变（存在服务器的 `/var/lib/peerdesk/device-codes.txt`）。
2. 办公室的电脑（控制端）：在“对方设备码或地址”里输入设备码，填密码，点连接。

连接建立后会显示走的是直连还是中继。命令行：被控端 `peerdesk host --server <IP>:4433 --server-pin <指纹>`
会打印设备码；控制端 `peerdesk connect --server <IP>:4433 --server-pin <指纹> --device <设备码> --password <密码>`，
会打印连接路径和各阶段耗时。

## 6.1 出问题时发日志

- 客户端：窗口里“服务器”一栏点“打开日志文件夹”，把 `peerdesk.log` 发出来。里面有连接各阶段的耗时
  （连服务器、查询对方、直连尝试、中继）、直连失败的原因，以及会话中每秒一行的统计：
  发送端（帧数、画面码率、线上码率、目标码率、帧率上限、编码耗时、在途数据、丢失、重传、RTT），
  接收端（帧数、码率、两帧最长间隔即卡顿、作废、纠错恢复、坏帧、RTT）。不记录画面、键盘内容和密码。
  日志超过 8 MB 自动轮换，最多占 16 MB。
- 服务端：`journalctl -u peerdesk-server --since "1 hour ago"`，有设备登记、牵线（对方是否在线、中继是否分配）。

## 7. 升级、卸载、日志

**升级**：下载新包，解压，再运行一次 `sudo ./install.sh`。二进制和 systemd 单元会更新，
`/etc/peerdesk/server.toml` 和 `/var/lib/peerdesk` 保留，指纹不变。重启期间已有的会话会断开。
新版配置里有新增选项时，对照包里的 `server.toml` 手工加。

**卸载**：

```bash
sudo systemctl disable --now peerdesk-server
sudo rm /etc/systemd/system/peerdesk-server.service /usr/local/bin/peerdesk-server
sudo systemctl daemon-reload
# 下面两项会删除服务器身份，指纹随之作废；以后重装要让所有客户端重新填指纹
sudo rm -r /etc/peerdesk /var/lib/peerdesk
sudo userdel peerdesk
```

**日志**：

```bash
journalctl -u peerdesk-server -f          # 实时
journalctl -u peerdesk-server --since today
```

## 8. 排错

**连不上服务器（超时）**——端口被挡。依次确认：
1. 服务在跑：`systemctl status peerdesk-server`；监听了：`sudo ss -ulnp | grep -E '4433|3478'`。
2. 系统防火墙放行了 UDP（第 4 节），云安全组也放行了。
3. 在服务器上抓包：`sudo tcpdump -ni any udp port 4433`，同时让客户端连；一个包都没有，就是路径上被挡
   （安全组、运营商、公司网络禁止 UDP 出站）。公司网络常常只放行 TCP，换个网络试一下可以快速判断。

**只能走中继，直连始终不成功**——不一定是故障：任意一端在对称 NAT（部分公司网络、运营商级 NAT）后面时，
打洞本来就失败。确认中继地址正确：`/etc/peerdesk/server.toml` 里 `relay_advertise` 必须是服务器的公网
IP 和 3478，不是内网地址；改完 `sudo systemctl restart peerdesk-server`。直连率低会增加服务器流量成本，
但连接仍然可用。

**中继也不通，信令正常**——几乎都是 UDP 3478 没放行，或 `relay_advertise` 写错。

**指纹不匹配**——客户端报指纹对不上。常见原因：`/var/lib/peerdesk` 被删除或机器重装，服务器重新生成了身份，
指纹变了。先用 `journalctl -u peerdesk-server | grep 指纹` 看当前指纹，确认这确实是你的服务器
（不是被人冒充，也不是填错地址），再把新指纹填到所有客户端。以后重装前先备份 `/var/lib/peerdesk`。

**服务启动失败**——`journalctl -u peerdesk-server -n 50` 里有原因，常见的是配置文件语法错误
（未知字段会报错）、端口被占用，或配置里还留着 `__PUBLIC_IP__`（用 `install.sh --public-ip` 重装或手工改）。
