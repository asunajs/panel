#!/usr/bin/env bash
#
# plugin-panel 一键安装
#
#   curl -fsSL https://raw.githubusercontent.com/asunajs/panel/main/install.sh | bash
#
# 做四件事：检测架构 → 下载对应产物 → 强制校验（SHA256 + ELF 自检）→ 装进 PATH。
#
# 支持的平台：
#   aarch64 / arm64  → 64 位 ARM：旧手机（Termux）、ARM 盒子、树莓派 3/4/5
#   armv7l / armhf   → 32 位 ARM：树莓派 2、老盒子、部分电视盒子
#   x86_64  / amd64  → 普通 PC、VPS、开发机
#
# 环境变量：
#   PANEL_REPO     仓库，默认下面的 REPO（fork 后改这里）
#   PANEL_VERSION  版本 tag，默认 latest
#   PANEL_PREFIX   安装目录，默认 Termux 用 $PREFIX/bin，否则 ~/.local/bin
#   PANEL_NO_PATH  设为 1 则不提示改 PATH
#   PANEL_BASE_URL 直接指定产物下载目录（镜像 / 内网 / 本地文件），
#                  给了它就不再拼 GitHub 地址。只允许 https://；
#                  明文 http:// 要显式 PANEL_ALLOW_HTTP=1，
#                  离线 file:// 要显式 PANEL_ALLOW_FILE=1。例：
#                  PANEL_BASE_URL=https://mirror.example.com/plugin-panel bash -s
#                  PANEL_BASE_URL=file:///srv/dist PANEL_ALLOW_FILE=1 bash install.sh
#   PANEL_ALLOW_UNVERIFIED=1
#                  取不到 SHA256SUMS 时也继续装（**不推荐**：等于放弃完整性校验）
#
# 设计前提：校验和与二进制是同一个来源。所以「https 之外」的传输都当成可能被
# 中间人一起替换，必须由用户显式确认，而不是默认静默降级。

set -euo pipefail

# ---- fork 之后改这一行 ----
REPO="${PANEL_REPO:-asunajs/panel}"

VERSION="${PANEL_VERSION:-latest}"
BIN_NAME="plugin-panel"

die() { printf '\033[31m错误\033[0m %s\n' "$*" >&2; exit 1; }
info() { printf '\033[36m·\033[0m %s\n' "$*"; }
ok() { printf '\033[32m✓\033[0m %s\n' "$*"; }
warn() { printf '\033[33m警告\033[0m %s\n' "$*" >&2; }

# ---------- 1. 检测平台 ----------
case "$(uname -s)" in
    Linux) OS=linux ;;
    Darwin) OS=darwin ;;
    *) die "不支持的系统: $(uname -s)" ;;
esac

case "$(uname -m)" in
    aarch64|arm64) ARCH=aarch64 ;;
    armv7l|armv7|armhf) ARCH=armv7 ;;
    x86_64|amd64)  ARCH=x86_64 ;;
    *) die "不支持的架构: $(uname -m)（本项目出 aarch64、armv7 和 x86_64 产物）" ;;
esac

# 只发 Linux 静态产物 —— 静态链接意味着不挑发行版、不挑 libc
if [ "$OS" != linux ]; then
    die "只提供 Linux 产物（当前: $OS）。macOS 请本地编译：cargo build --release"
fi

# ---------- 2. 决定安装目录 ----------
IS_TERMUX=0
if [ -n "${TERMUX_VERSION:-}" ] || [ -d /data/data/com.termux ]; then
    IS_TERMUX=1
    : "${PREFIX:=/data/data/com.termux/files/usr}"
    INSTALL_DIR="${PANEL_PREFIX:-$PREFIX/bin}"
else
    INSTALL_DIR="${PANEL_PREFIX:-$HOME/.local/bin}"
fi

info "平台: $OS/$ARCH$([ $IS_TERMUX = 1 ] && echo ' (Termux)')"
info "安装到: $INSTALL_DIR"

# ---------- 3. 决定下载地址 + scheme 白名单 ----------
if [ -n "${PANEL_BASE_URL:-}" ]; then
    BASE="$PANEL_BASE_URL"
elif [ "$VERSION" = latest ]; then
    BASE="https://github.com/$REPO/releases/latest/download"
else
    BASE="https://github.com/$REPO/releases/download/$VERSION"
fi

# 校验和与产物同源 ⇒ 中间人能同时换掉两者，所以只信 https。
# 用 case 做白名单，不认识的 scheme 直接死，不做「猜一个能用就行」的降级。
HTTPS_ONLY=1
case "$BASE" in
    https://*)
        ;;
    http://*)
        if [ "${PANEL_ALLOW_HTTP:-0}" != 1 ]; then
            die "PANEL_BASE_URL 用的是明文 http://，拒绝安装。
  原因：SHA256SUMS 和二进制从同一个地址取，走明文时中间人可以把两者一起换掉，
        校验和只能防传输出错，防不了人。请改 https://，或换能上 TLS 的镜像。
  确实只能走明文内网（比如隔离环境里的镜像），显式加 PANEL_ALLOW_HTTP=1 再跑。"
        fi
        HTTPS_ONLY=0
        warn "http:// 下载没有传输层保护：校验和与二进制同源，中间人可以两个一起换。"
        ;;
    file://*)
        if [ "${PANEL_ALLOW_FILE:-0}" != 1 ]; then
            die "PANEL_BASE_URL 用的是 file://（离线分发），需要显式确认。
  确认真在装本地目录里的产物，就加 PANEL_ALLOW_FILE=1 再跑。"
        fi
        HTTPS_ONLY=0
        ;;
    *)
        die "不支持的 PANEL_BASE_URL scheme: ${BASE%%:*}（只允许 https://；http:// 要 PANEL_ALLOW_HTTP=1，file:// 要 PANEL_ALLOW_FILE=1）"
        ;;
esac

case "$ARCH" in
    armv7) ASSET="$BIN_NAME-armv7-linux-musleabihf" ;;
    *)     ASSET="$BIN_NAME-$ARCH-linux-musl" ;;
esac

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

fetch() {
    local url="$1" out="$2"
    # file:// 交给 cp：wget 不认 file，curl 的 file 支持又依赖编译选项
    case "$url" in
        file://*)
            cp "${url#file://}" "$out" 2>/dev/null || return 1
            return 0
            ;;
    esac
    if command -v curl >/dev/null 2>&1; then
        # 仅 https 时锁死协议：禁止 curl 被重定向降到 http，也禁止别的协议
        if [ "$HTTPS_ONLY" = 1 ]; then
            curl -fsSL --proto '=https' --proto-redir '=https' --max-time 120 "$url" -o "$out"
        else
            curl -fsSL --max-time 120 "$url" -o "$out"
        fi
    elif command -v wget >/dev/null 2>&1; then
        wget -qO "$out" --timeout=120 "$url"
    else
        die "需要 curl 或 wget（Termux: pkg install curl）"
    fi
}

info "下载 $ASSET …"
fetch "$BASE/$ASSET" "$TMP/$BIN_NAME" || die "下载失败。检查仓库名/网络，或该版本是否已发布产物。
  预期地址: $BASE/$ASSET"

# ---------- 4. 校验 SHA256（强制） ----------
if fetch "$BASE/SHA256SUMS" "$TMP/SHA256SUMS" 2>/dev/null; then
    EXPECT="$(grep " $ASSET\$" "$TMP/SHA256SUMS" | awk '{print $1}' || true)"
    # 清单在、但没有本资产条目 ⇒ 多半是拉到了别的版本的清单/被裁过的清单，不能当「校验通过」
    [ -n "$EXPECT" ] || die "SHA256SUMS 里没有 $ASSET 的条目，拒绝安装。
  清单里可能只有别的架构/别的版本。换对版本，或用官方 GitHub 地址重试。"

    if command -v sha256sum >/dev/null 2>&1; then
        ACTUAL="$(sha256sum "$TMP/$BIN_NAME" | awk '{print $1}')"
    else
        ACTUAL="$(shasum -a 256 "$TMP/$BIN_NAME" | awk '{print $1}')"
    fi
    [ "$EXPECT" = "$ACTUAL" ] || die "SHA256 校验失败！期望 $EXPECT，实际 $ACTUAL"
    ok "SHA256 校验通过（$ASSET）"
else
    # 拿不到校验和 ≠ 可以照装。默认拒绝；只有用户显式放弃校验才继续，且必须让他看见警告。
    if [ "${PANEL_ALLOW_UNVERIFIED:-0}" = 1 ]; then
        warn "取不到 SHA256SUMS，已按 PANEL_ALLOW_UNVERIFIED=1 跳过校验 —— 装进去的东西没有任何完整性保证。"
    else
        die "取不到 $BASE/SHA256SUMS，拒绝安装。
  没有校验和就确认不了装的是不是被替换过的二进制。
  离线/自建镜像确实没有清单的，显式加 PANEL_ALLOW_UNVERIFIED=1 再跑（不推荐）。"
    fi
fi

# ---------- 5. 产物自检（ELF + 体积） ----------
# 这里防两类「校验和过了但东西还是不对」的情况：镜像把错误页 200 返回了、
# 镜像放了个占位文本/别的架构的小文件。
MAGIC="$(od -An -tx1 -N4 "$TMP/$BIN_NAME" | tr -d ' \n')"
[ "$MAGIC" = 7f454c46 ] || die "产物不是 ELF 二进制（头部: ${MAGIC:-空}）。多半是下到了错误页或占位文件，别装。"

SIZE="$(wc -c < "$TMP/$BIN_NAME" | tr -d ' ')"
[ "${SIZE:-0}" -gt 1048576 ] || die "产物只有 ${SIZE:-0} 字节（< 1MB），不像真实的静态二进制，已中止。"

ok "产物自检通过（ELF, $((SIZE / 1024))KB）"

# ---------- 6. 安装 ----------
if [ "${EUID:-$(id -u)}" = 0 ]; then
    warn "你正在把从网络下载的二进制装成 root 的二进制 —— 出问题就是整机；能不用 root 就别用。"
fi

mkdir -p "$INSTALL_DIR"
install -m 755 "$TMP/$BIN_NAME" "$INSTALL_DIR/$BIN_NAME" 2>/dev/null \
    || { cp "$TMP/$BIN_NAME" "$INSTALL_DIR/$BIN_NAME"; chmod 755 "$INSTALL_DIR/$BIN_NAME"; }

ok "已安装: $INSTALL_DIR/$BIN_NAME"

# ---------- 6.5 真的能跑起来吗 ----------
# 校验和、ELF 头、体积都过了，仍然可能「在这台机器上跑不起来」：
#   - 架构不对（装错产物、32 位系统装了 64 位）
#   - 非静态产物（自己用 NDK 编的 aarch64-linux-android 加了 -no-pie，
#     Android 10+ 的 linker 会直接拒绝：has unexpected e_type: 2）
# 跑一次 build-info 是最诚实的检查 —— 它能跑，后面才谈得上能跑。
# 只警告不中止：二进制已经装好了，用户可能就是想先装上再说。
if "$INSTALL_DIR/$BIN_NAME" build-info >/dev/null 2>&1; then
    ok "自检：二进制能在这台机器上执行"
else
    # 末尾的 || true 不能省：脚本是 set -euo pipefail，被执行的二进制返回
    # 126/127 时管道状态会让整个安装脚本以那个码退出（这里本来只是警告）。
    RUN_ERR="$("$INSTALL_DIR/$BIN_NAME" build-info 2>&1 | head -3 || true)"
    warn "装好了，但**在这台机器上执行失败**：${RUN_ERR:-（没有输出）}"
    case "$RUN_ERR" in
        *e_type*)
            warn "Termux 的 termux-exec 会把程序交给 /system/bin/linker64 执行，"
            warn "而 linker 只接受 PIE（ET_DYN）—— 静态 ET_EXEC 会被当场拒绝。"
            warn "这是产物形态的问题，不是你装错了：v0.1.9 及以前的 ARM 产物就是 ET_EXEC。"
            warn "换带 static PIE 的版本（v0.1.10+）即可。自检："
            warn "    readelf -h $INSTALL_DIR/$BIN_NAME | grep Type    # 要看到 DYN"
            ;;
        *"Exec format error"*|*"cannot execute"*|*"bad CPU type"*)
            warn "架构不匹配：本机 $(uname -m)，装的产物是 $BIN_NAME。"
            ;;
    esac
    warn "排查见 docs/pitfalls.md 的「Android / Termux」一节。"
fi

# ---------- 7. PATH 提示 ----------
case ":$PATH:" in
    *":$INSTALL_DIR:"*) ;;
    *)
        if [ "${PANEL_NO_PATH:-0}" != 1 ]; then
            echo
            info "把下面这行加到 shell 配置里（~/.bashrc 或 ~/.zshrc）："
            echo "    export PATH=\"$INSTALL_DIR:\$PATH\""
            [ $IS_TERMUX = 1 ] && echo "    （Termux 一般已经在 PATH 里了，重开终端试试）"
        fi
        ;;
esac

echo
ok "装好了。启动（**先设令牌**，否则会拒绝启动）："
echo "    TOKEN=\$(head -c 24 /dev/urandom | od -An -tx1 | tr -d ' \\n')"
echo "    PANEL_TOKEN_HASH=\$(printf %s \"\$TOKEN\" | $BIN_NAME hash-token) $BIN_NAME"
echo "    # 登录时粘贴上面那个 \$TOKEN（面板里不存明文，存的是它的哈希）"
echo
echo "  面板默认监听 http://127.0.0.1:8787"
echo "  常用环境变量："
echo "    BIND_ADDR=0.0.0.0:8787   改监听地址（想让别的设备访问就设这个）"
echo "    PLUGINS_DIR=./plugins    插件目录"
echo "    DB_PATH=./data/panel.db  SQLite 路径"
echo
echo "  ⚠️  令牌是**必须**的：没配就拒绝启动，跟绑哪个地址无关"
echo "     （反代 nginx/caddy 也跑在本机，转发来的请求源地址就是 127.0.0.1 ——"
echo "      所以「绑回环 = 安全」不成立，没有「本机免登录」这种模式）"
echo "    PANEL_TOKEN=\$(head -c 24 /dev/urandom | od -An -tx1 | tr -d ' \\n') $BIN_NAME"
echo "    然后在浏览器打开面板会跳到登录页，粘贴那串令牌即可"
echo
echo "  推荐：本地只存「盐+哈希」，不存明文（拿到那个文件也登不上）："
echo "    printf '%s' "\$TOKEN" | $BIN_NAME hash-token   # 生成 PANEL_TOKEN_HASH"
