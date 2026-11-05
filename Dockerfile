# syntax=docker/dockerfile:1

# =============================================================================
# 基于「发布产物」构建镜像 —— 不在 Docker 里重编 Rust
# =============================================================================
#
# 为什么是「消费产物」而不是「多阶段 cargo build」：
#   · `.github/workflows/release.yml` 已经为每个平台产出了**静态链接**的 musl
#     二进制（外加 SHA256SUMS）。`readelf -d` 没有 NEEDED，丢到任何 Linux 上都能跑，
#     所以运行时镜像里不需要 libc、不需要 Rust、不需要编译缓存。
#   · 在镜像里再编一遍 = 把一整条 Rust 工具链（rquickjs / bundled SQLite / vendored
#     OpenSSL）的十几分钟编译搬进构建，而且编出来的**不是** Release 里那一份字节 ——
#     「镜像里跑的是什么」和「Release 里发布的是什么」就对不上了。
#   · 这个文件只做三件事：拿到那份产物 → 校验 SHA256 → 装进最小运行时镜像。
#
# 产物来源（按优先级；两条路都**必须**过 SHA256，校验不过直接失败）：
#   1) 构建上下文里的 `dist/<产物名>`（+ 可选 `dist/SHA256SUMS`）—— 离线 / 内网 /
#      自己刚编出来的产物。`dist/` 已在 .gitignore 里，不会被提交。
#   2) `https://github.com/${PANEL_REPO}/releases/download/${PANEL_VERSION}/<产物名>`
#      —— 默认路径，CI 与 `docker compose build` 走这条。
#
# 产物名按构建目标自动选（与 release.yml 的 matrix 一一对应）：
#   linux/amd64  → plugin-panel-x86_64-linux-musl
#   linux/arm64  → plugin-panel-aarch64-linux-musl
#   linux/arm/v7 → plugin-panel-armv7-linux-musleabihf
#
# 用法：
#   docker build -t plugin-panel .                       # 默认版本见下面的 PANEL_VERSION
#   docker build -t plugin-panel --build-arg PANEL_VERSION=v0.2.2 .
#   mkdir -p dist && cp plugin-panel-x86_64-linux-musl SHA256SUMS dist/
#   docker build -t plugin-panel .                       # 自动改用 dist/ 里的本地产物
# =============================================================================

ARG ALPINE_VERSION=3.20

# -----------------------------------------------------------------------------
# 1) 取产物 + 校验
# -----------------------------------------------------------------------------
FROM alpine:${ALPINE_VERSION} AS artifact

ARG PANEL_REPO=asunajs/panel
ARG PANEL_VERSION=v0.2.2
# 覆盖下载基址（内网镜像 / 本地目录服务器）。留空 = 走 GitHub Release。
ARG ARTIFACT_BASE_URL=""
# 显式指定产物文件名（默认按 TARGETARCH 推导）。
ARG ARTIFACT_NAME=""
# 显式钉住摘要（没有 SHA256SUMS 时用；给了就以它为准）。
ARG ARTIFACT_SHA256=""
ARG TARGETARCH=amd64
ARG TARGETVARIANT=""

# 上下文里只可能用到 `dist/`（本地产物），其余由 .dockerignore 挡掉。
COPY . /src

RUN set -eu; \
    apk add --no-cache ca-certificates curl file; \
    case "${TARGETARCH}${TARGETVARIANT}" in \
      amd64) DEFAULT_ASSET="plugin-panel-x86_64-linux-musl" ;; \
      arm64) DEFAULT_ASSET="plugin-panel-aarch64-linux-musl" ;; \
      armv7) DEFAULT_ASSET="plugin-panel-armv7-linux-musleabihf" ;; \
      *) echo "不支持的构建目标：TARGETARCH=${TARGETARCH} TARGETVARIANT=${TARGETVARIANT}" >&2; \
         echo "release.yml 只发布 amd64 / arm64 / armv7 三档产物。" >&2; exit 1 ;; \
    esac; \
    ASSET="${ARTIFACT_NAME:-$DEFAULT_ASSET}"; \
    BASE="${ARTIFACT_BASE_URL:-https://github.com/${PANEL_REPO}/releases/download/${PANEL_VERSION}}"; \
    mkdir -p /out; \
    if [ -f "/src/dist/${ASSET}" ]; then \
      echo "== 产物来源：构建上下文 dist/${ASSET}"; \
      cp "/src/dist/${ASSET}" "/out/${ASSET}"; \
      if [ -f /src/dist/SHA256SUMS ]; then cp /src/dist/SHA256SUMS /tmp/SHA256SUMS; fi; \
    else \
      echo "== 产物来源：${BASE}/${ASSET}"; \
      curl -fsSL --connect-timeout 20 --retry 5 --retry-connrefused --retry-delay 3 \
        -o "/out/${ASSET}" "${BASE}/${ASSET}"; \
      curl -fsSL --connect-timeout 20 --retry 5 --retry-connrefused --retry-delay 3 \
        -o /tmp/SHA256SUMS "${BASE}/SHA256SUMS"; \
    fi; \
    cd /out; \
    if [ -n "${ARTIFACT_SHA256}" ]; then \
      echo "== 校验（ARTIFACT_SHA256 钉住的摘要）"; \
      echo "${ARTIFACT_SHA256}  ${ASSET}" > /tmp/pin.sums; \
      sha256sum -c /tmp/pin.sums; \
    elif [ -f /tmp/SHA256SUMS ]; then \
      echo "== 校验（SHA256SUMS）"; \
      grep -E "[[:space:]]${ASSET}\$" /tmp/SHA256SUMS > /tmp/pin.sums; \
      test -s /tmp/pin.sums || { echo "SHA256SUMS 里没有 ${ASSET} 这一条" >&2; exit 1; }; \
      sha256sum -c /tmp/pin.sums; \
    else \
      echo "既没有 ARTIFACT_SHA256，也没有 SHA256SUMS —— 拒绝把一个没校验过的二进制装进镜像" >&2; \
      exit 1; \
    fi; \
    echo "== 产物形态（架构必须与构建目标一致）"; \
    file "/out/${ASSET}"; \
    case "${TARGETARCH}${TARGETVARIANT}" in \
      amd64) file "/out/${ASSET}" | grep -q "x86-64" ;; \
      arm64) file "/out/${ASSET}" | grep -q "aarch64" ;; \
      armv7) file "/out/${ASSET}" | grep -qi "ARM" ;; \
    esac; \
    mv "${ASSET}" /out/plugin-panel; \
    chmod 0755 /out/plugin-panel; \
    ls -l /out/plugin-panel

# -----------------------------------------------------------------------------
# 2) 运行时：一个静态二进制 + busybox
# -----------------------------------------------------------------------------
FROM alpine:${ALPINE_VERSION}

ARG ALPINE_VERSION=3.20
ARG PANEL_REPO=asunajs/panel
ARG PANEL_VERSION=v0.2.2

LABEL org.opencontainers.image.title="plugin-panel" \
      org.opencontainers.image.description="Rust + QuickJS 动态插件任务面板（镜像基于官方 Release 静态产物构建）" \
      org.opencontainers.image.source="https://github.com/${PANEL_REPO}" \
      org.opencontainers.image.version="${PANEL_VERSION}" \
      org.opencontainers.image.base.name="docker.io/library/alpine:${ALPINE_VERSION}"

# ca-certificates：面板自身走 rustls 内置根证书，但插件与脚本任务里的 curl/wget 要用；
# tzdata：`PANEL_TZ` 由 chrono-tz 内置库处理，脚本任务里的 date 仍然要 /usr/share/zoneinfo。
# busybox 自带 wget，healthcheck 用它，不再额外装东西。
# 外部运行时（Node / Bun / Python）**不在镜像里**：QuickJS 插件开箱即用；
# 要跑 Node/Bun 插件或 .py 脚本，自己基于这个镜像加一层装 nodejs/python3 即可
# （面板侧用 PANEL_NODE / PANEL_BUN / PANEL_PYTHON 指路径）。
RUN apk add --no-cache ca-certificates tzdata \
 && addgroup -g 1000 panel \
 && adduser -D -u 1000 -G panel -h /panel -s /sbin/nologin panel \
 && mkdir -p /panel/data /panel/plugins /panel/tasks /panel/logs \
 && chown -R panel:panel /panel

COPY --from=artifact /out/plugin-panel /usr/local/bin/plugin-panel

# 面板的相对路径（`data/panel.db`、`plugins/`、`tasks/`、`logs/`）都是相对**工作目录**的，
# 所以工作目录就是它的数据根；compose 把这四个子目录挂出来即可（与官方文档的目录布局一致）。
WORKDIR /panel

# 容器里必须绑 0.0.0.0，否则端口映射进来也连不上。
ENV BIND_ADDR=0.0.0.0:8787 \
    PANEL_LOG_DIR=/panel/logs \
    HOME=/panel

EXPOSE 8787

# 匿名存活探针（不需要令牌），见 src/web.rs 的 healthz。
# 端口与上面的 BIND_ADDR 绑定：改了监听端口，这里也要一起改。
HEALTHCHECK --interval=30s --timeout=5s --start-period=10s --retries=3 \
  CMD wget -q -O /dev/null http://127.0.0.1:8787/healthz || exit 1

USER panel

# ENTRYPOINT 是面板本体，所以 `docker compose run --rm panel hash-token` 这类
# 子命令（hash-token / digest / build-info / backup / restore）直接可用。
ENTRYPOINT ["/usr/local/bin/plugin-panel"]
