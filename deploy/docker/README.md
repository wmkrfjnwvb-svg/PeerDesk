# 用 Docker 部署 PeerDesk 服务端集群

本目录给出：

| 文件 | 作用 |
|---|---|
| `Dockerfile` | 多阶段构建：node 构建管理页面，musl 静态编译 `peerdesk-server`，最终镜像基于 distroless（没有 shell），以非 root 运行 |
| `compose.yml` | 三节点示例集群：node1 付费，node2、node3 免费，同一台机器、同一个 bridge 网络 |
| `node1.toml` `node2.toml` `node3.toml` | 各节点的配置模板（`${...}` 是占位符） |
| `init.sh` | 生成随机密钥（`.env`）和实际配置（`run/node*.toml`） |
| `server.toml` | 镜像内置的默认配置（单机，只开信令不开中继） |

示例集群是为了在一台机器上试用和压测。正式部署请把节点放在不同的机器上（见最后一节）。

## 1. 启动示例集群

前提：Docker 和 Docker Compose v2（`docker compose version`）。

```bash
cd deploy/docker
./init.sh 1.2.3.4        # 参数是这台机器客户端能访问到的 IP；不写则自动探测公网 IP
docker compose up -d --build
docker compose ps
```

`init.sh` 做两件事（可重复运行，已有 `.env` 就沿用密钥）：

1. 生成 `.env`：`cluster_secret`、`billing_secret`、`admin_token` 三个随机密钥和公网 IP（权限 0600，已在 `.gitignore` 里，**不要提交**）。
2. 把 `node*.toml` 里的占位符替换掉，写到 `run/node*.toml`，容器挂载的是这几个生成的文件。
   Docker 不会替换挂载文件内容里的 `${...}`，所以不能直接挂模板。

镜像第一次构建要编译 Rust，几分钟；之后有缓存。

端口（宿主机一侧）：

| 节点 | 角色 | 信令 UDP | 中继 UDP | 管理页面（只在本机） |
|---|---|---|---|---|
| node1 | 付费 | 4433 | 3478 | http://127.0.0.1:8081 |
| node2 | 免费 | 4434 | 3479 | http://127.0.0.1:8082 |
| node3 | 免费 | 4435 | 3480 | http://127.0.0.1:8083 |

查看服务器指纹（客户端要填）：

```bash
docker compose logs node1 | grep 指纹
```

客户端的“服务器”填 `<公网IP>:4433` 和这个指纹即可，其他两个节点会通过集群自动告诉客户端，
node1 不可用时客户端会自动换节点（故障切换）。

停止和清理：`docker compose down`（保留数据卷，指纹不变）；`docker compose down -v` 会删除数据卷，
服务器身份随之作废，所有客户端要重新填指纹。

## 2. 角色怎么工作

- **付费集群（`role = "paid"`）**：会员（带会员令牌）的中继优先走这里，没有免费额度限制，用量记在会员账户上。
- **免费集群（`role = "free"`）**：没有令牌的用户走这里，受免费额度限制。
- **VIP 优先付费集群，满了用免费集群**：付费节点的负载超过 `paid_load_limit_permille`（默认 900，即 90%）时，
  会员也改走免费节点，保证连得上。负载按 `relay_capacity_mbps` 算；填 0 则按会话数算（每 100 个会话算满）。
- **免费额度**：每台设备每个窗口最多转发 `free_device_mb`（默认 200 MB），窗口是 `free_window_secs`
  （默认 600 秒，也就是每 10 分钟 200 MB）；每个公网 IP 另有 `free_ip_mb`（默认 600 MB）。填 0 表示不限。
  用完后中继停止转发并通知客户端，直连不受影响。三项都可以在 `nodeN.toml` 里改，改完
  `docker compose restart nodeN`（模板改了要先 `./init.sh` 重新生成）。
- 直连（打洞）不走中继，不占额度，也不花服务器流量。

## 3. 签发会员令牌

会员令牌绑定一个账户和流量额度（GB）。两种办法：

- 管理中心：浏览器打开 `http://127.0.0.1:8081`，用管理令牌（`.env` 里的 `PEERDESK_ADMIN_TOKEN`）签发。
  页面“管理”标签里填管理令牌和额度（0 表示不限），点签发，令牌只显示一次，复制给用户。
- 命令行：

  ```bash
  source .env
  curl -X POST -H "Authorization: Bearer $PEERDESK_ADMIN_TOKEN" \
       http://127.0.0.1:8081/api/accounts -d '{"quota_gb":50}'
  # 返回 {"account":"…","token":"…"}
  ```

把 `token` 给用户，客户端里填入“会员令牌”。令牌由 `billing_secret` 签名，集群里任何节点都认；
用量记在账户上（`/api/usage` 查看），不挂在设备指纹上。管理令牌不写时，所有写操作都被拒绝。

## 4. 往已有集群里加节点

1. 在新机器上装好节点（Docker 或单二进制，见下文），配置里：
   - `node_id` 取一个集群里没用过的数字，`role` 选 `free` 或 `paid`；
   - `cluster_secret`、`billing_secret`、`admin_token` **和现有节点完全相同**；
   - `advertise` 填其他节点能访问到的地址，`peers` 至少写一个现有节点的集群地址；
   - `signal_advertise`、`relay_advertise` 填客户端能访问到的公网地址。
2. 让现有集群认识它：二选一
   - 管理中心的“加入节点”里填新节点的集群地址（`advertise`）；
   - `curl -X POST -H "Authorization: Bearer $PEERDESK_ADMIN_TOKEN" http://127.0.0.1:8081/api/peers -d '{"addr":"新节点IP:7946"}'`
3. 在管理中心的集群总览里确认新节点是“存活”。客户端下次连接时会从集群拿到新的节点列表。

用 compose 加第四个节点：复制 `node3.toml` 为 `node4.toml`（改 `node_id`、`advertise`、端口，`peers` 里加上它），
在 `compose.yml` 里仿照 node3 加一个服务（新的固定 IP、新的映射端口、新的数据卷），`init.sh` 里的循环加上 4。

## 5. 防火墙

每个节点需要放行：

| 端口 | 协议 | 谁访问 | 说明 |
|---|---|---|---|
| 信令（示例里 4433–4435） | UDP | 所有客户端 | 登记、找到对方、协助打洞 |
| 中继（示例里 3478–3480） | UDP | 所有客户端 | 直连不通时转发 |
| 集群（7946） | UDP | 仅其他节点 | 心跳。分机器部署时只对其他节点的 IP 放行 |
| 管理页面（8080） | TCP | 仅你自己 | 不要对公网开放，用 SSH 隧道访问 |

云服务器还要在安全组里放行同样的端口。Docker 发布的端口会绕过 `ufw` 的规则，管理端口因此
只映射到 `127.0.0.1`；要限制来源时请改 `DOCKER-USER` 链或云安全组。

## 6. 单二进制部署多节点（不用 Docker）

每台机器一个节点，复用 `packaging/linux-server`，详见 `docs/deploy-server.md`：

1. 在 CI 的构建产物里取 `peerdesk-server-<提交>-linux-x64.tar.gz`（提交信息带 `[pkg]` 时生成），传到每台机器。
2. `sudo ./install.sh --public-ip <该机器公网IP>`：装二进制和 systemd 单元，生成 `/etc/peerdesk/server.toml`。
3. 编辑 `/etc/peerdesk/server.toml`，加上集群字段（以 node1 为例，其他节点只改 `node_id`、`role`、地址）：

   ```toml
   node_id = 1                 # 每台不同，不能为 0
   role = "paid"               # 或 "free"
   http_listen = "127.0.0.1:8080"
   cluster_listen = "0.0.0.0:7946"
   advertise = "10.0.0.11:7946"            # 其他节点能访问到的地址
   peers = ["10.0.0.12:7946", "10.0.0.13:7946"]
   cluster_secret = "…至少 16 个字符，所有节点相同…"
   billing_secret = "…所有节点相同…"
   admin_token = "…管理令牌…"
   signal_advertise = "<本机公网IP>:4433"
   ```

   密钥用 `head -c 32 /dev/urandom | od -An -tx1 | tr -d ' \n'` 生成。`install.sh` 已经写好了
   `signal_listen`、`relay_listen`、`relay_advertise`、`data_dir`。
4. `sudo systemctl restart peerdesk-server`，然后 `curl -s http://127.0.0.1:8080/api/cluster` 看节点是否互相发现。

## 7. 压力测试

仓库里有一个服务端并发压力测试：三个真实节点跑在本进程里，大量被控端登记、许多控制端同时连接、
经中继推送数据，输出登记和连接的成功率与延迟、中继吞吐、CPU 时间：

```bash
cargo test --release -p peerdesk-server --test load -- --ignored --nocapture
# 参数：PEERDESK_LOAD_DEVICES（默认 200）、PEERDESK_LOAD_SESSIONS（默认 50）、PEERDESK_LOAD_SECONDS（默认 5）
```

结果写在 `target/load/summary.md`。CI 里提交信息带 `[load]` 才会运行，摘要会显示在 Actions 页面，
失败时才上传文件。提交信息带 `[pkg]` 时，CI 还会验证本目录的 Docker 镜像能构建。
