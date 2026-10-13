#!/usr/bin/env bash
#
# plugin-panel 一键安装
#
#   curl -fsSL https://raw.githubusercontent.com/asunajs/panel/main/install.sh | bash
#
# 做四件事：检测架构 → 下载对应产物 → 校验 SHA256 → 装进 PATH。
#
# 支持的平台：
#   aarch64 / arm64  → 旧手机（Termux）、ARM 盒子、树莓派 3/4/5
#   x86_64  / amd64  → 普通 PC、VPS、开发机
#
# 环境变量：
#   PANEL_REPO     仓库，默认下面的 REPO（fork 后改这里）
#   PANEL_VERSION  版本 tag，默认 latest
#   PANEL_PREFIX   安装目录，默认 Termux 用 $PREFIX/bin，否则 ~/.local/bin
#   PANEL_NO_PATH  设为 1 则不提示改 PATH
#   PANEL_BASE_URL 直接指定产物下载目录（镜像 / 内网 / 本地文件），
#                  给了它就不再拼 GitHub 地址。例：
#                  PANEL_BASE_URL=https://mirror.example.com/plugin-panel bash -s
#                  PANEL_BASE_URL=file:///srv/dist bash install.sh

set -euo pipefail

# ---- fork 之后改这一行 ----
REPO="${PANEL_REPO:-asunajs/panel}"

VERSION="${PANEL_VERSION:-latest}"
BIN_NAME="plugin-panel"

die() { printf '\033[31m错误\033[0m %s\n' "$*" >&2; exit 1; }
info() { printf '\033[36m·\033[0m %s\n' "$*"; }
ok() { printf '\033[32m✓\033[0m %s\n' "$*"; }

# ---------- 1. 检测平台 ----------
case "$(uname -s)" in
    Linux) OS=linux ;;
    Darwin) OS=darwin ;;
    *) die "不支持的系统: $(uname -s)" ;;
esac

case "$(uname -m)" in
    aarch64|arm64) ARCH=aarch64 ;;
    x86_64|amd64)  ARCH=x86_64 ;;
    *) die "不支持的架构: $(uname -m)（本项目只出 aarch64 和 x86_64 产物）" ;;
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

# ---------- 3. 下载 ----------
if [ -n "${PANEL_BASE_URL:-}" ]; then
    BASE="$PANEL_BASE_URL"
elif [ "$VERSION" = latest ]; then
    BASE="https://github.com/$REPO/releases/latest/download"
else
    BASE="https://github.com/$REPO/releases/download/$VERSION"
fi

ASSET="$BIN_NAME-$ARCH-linux-musl"

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

fetch() {
    if command -v curl >/dev/null 2>&1; then
        curl -fsSL "$1" -o "$2"
    elif command -v wget >/dev/null 2>&1; then
        wget -qO "$2" "$1"
    else
        die "需要 curl 或 wget（Termux: pkg install curl）"
    fi
}

info "下载 $ASSET …"
fetch "$BASE/$ASSET" "$TMP/$BIN_NAME" || die "下载失败。检查仓库名/网络，或该版本是否已发布产物。
  预期地址: $BASE/$ASSET"

# ---------- 4. 校验 ----------
if fetch "$BASE/SHA256SUMS" "$TMP/SHA256SUMS" 2>/dev/null; then
    EXPECT="$(grep " $ASSET\$" "$TMP/SHA256SUMS" | awk '{print $1}' || true)"
    if [ -n "$EXPECT" ]; then
        if command -v sha256sum >/dev/null 2>&1; then
            ACTUAL="$(sha256sum "$TMP/$BIN_NAME" | awk '{print $1}')"
        else
            ACTUAL="$(shasum -a 256 "$TMP/$BIN_NAME" | awk '{print $1}')"
        fi
        [ "$EXPECT" = "$ACTUAL" ] || die "SHA256 校验失败！期望 $EXPECT，实际 $ACTUAL"
        ok "SHA256 校验通过"
    fi
else
    info "（该版本没有 SHA256SUMS，跳过校验）"
fi

# ---------- 5. 安装 ----------
mkdir -p "$INSTALL_DIR"
install -m 755 "$TMP/$BIN_NAME" "$INSTALL_DIR/$BIN_NAME" 2>/dev/null \
    || { cp "$TMP/$BIN_NAME" "$INSTALL_DIR/$BIN_NAME"; chmod 755 "$INSTALL_DIR/$BIN_NAME"; }

ok "已安装: $INSTALL_DIR/$BIN_NAME"

# ---------- 6. PATH 提示 ----------
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
echo "    PANEL_TOKEN=\$(head -c 24 /dev/urandom | od -An -tx1 | tr -d ' \\n') $BIN_NAME"
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
