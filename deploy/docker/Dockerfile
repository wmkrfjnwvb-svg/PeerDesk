# PeerDesk 服务端镜像：静态 musl 二进制，最终镜像里只有它和配置，没有 shell、没有 libc。
# 构建（在仓库根目录）：docker build -f deploy/docker/Dockerfile -t peerdesk-server .
#
# 管理页面（web/dist）由 crates/server/build.rs 嵌进二进制；没有 web/dist 时嵌占位页面。
# 这里先用 node 构建管理页面，保证镜像里是完整页面。

# ---------- 1. 管理页面 ----------
FROM node:22-bookworm-slim AS web
WORKDIR /src/web
COPY web/package.json web/package-lock.json ./
RUN npm ci --no-audit --no-fund
COPY web/ ./
RUN npm run build

# ---------- 2. 服务端（musl 静态链接） ----------
FROM rust:1-bookworm AS build
# 服务端不链接视频编解码，只有 ring 要编译 C/汇编，用 musl-gcc 即可。
RUN apt-get update \
    && apt-get install -y --no-install-recommends musl-tools \
    && rm -rf /var/lib/apt/lists/*
WORKDIR /src
# 先放工具链文件：rustup 按它安装固定版本，再给这个版本加 musl 目标。
COPY rust-toolchain.toml ./
RUN rustup toolchain install \
    && rustup target add x86_64-unknown-linux-musl
COPY Cargo.toml Cargo.lock rustfmt.toml clippy.toml ./
COPY crates ./crates
COPY config ./config
COPY --from=web /src/web/dist ./web/dist
ENV CC_x86_64_unknown_linux_musl=musl-gcc
RUN cargo build --release --locked --target x86_64-unknown-linux-musl -p peerdesk-server \
    && mkdir -p /out/var/lib/peerdesk /out/etc/peerdesk \
    && cp target/x86_64-unknown-linux-musl/release/peerdesk-server /out/peerdesk-server
COPY deploy/docker/server.toml /out/etc/peerdesk/server.toml

# ---------- 3. 最终镜像 ----------
FROM gcr.io/distroless/static-debian12:nonroot
COPY --from=build /out/peerdesk-server /usr/local/bin/peerdesk-server
COPY --from=build /out/etc/peerdesk/server.toml /etc/peerdesk/server.toml
# 数据目录（服务器身份：证书和私钥、设备码、用量账本）。挂命名卷时会继承这里的属主（nonroot = 65532）。
COPY --from=build --chown=65532:65532 /out/var/lib/peerdesk /var/lib/peerdesk
VOLUME /var/lib/peerdesk
# 4433/udp 信令，3478/udp 中继，7946/udp 集群心跳，8080/tcp 管理页面。
EXPOSE 4433/udp 3478/udp 7946/udp 8080/tcp
USER nonroot
ENTRYPOINT ["/usr/local/bin/peerdesk-server"]
CMD ["/etc/peerdesk/server.toml"]
