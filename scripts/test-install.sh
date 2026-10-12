#!/usr/bin/env bash
#
# install.sh 的端到端测试。
#
# 为什么需要它：install.sh 是「用户唯一会跑的东西」，但它整条链路（下载 / 校验 / 安装）
# 都依赖 GitHub Release 和 Linux 环境 —— 开发机是 macOS，没法直接跑。
# 这里用 PATH 里的 shim 假造出 Linux + 一个本地「Release」，把六个分支都走一遍：
#
#   1. 正常安装（含 SHA256 校验、可执行位、安装到 PANEL_PREFIX）
#   2. 产物被篡改 → 必须拒绝安装
#   3. 不支持的架构 → 必须报错退出
#   4. 非 Linux（macOS）→ 必须报错退出
#   5. Termux → 装到 $PREFIX/bin
#   6. 产物不存在（下载失败）→ 必须报错退出
#
# 用法: bash scripts/test-install.sh

set -uo pipefail

REPO_DIR="$(cd "$(dirname "$0")/.." && pwd)"
INSTALL="$REPO_DIR/install.sh"
ASSET="plugin-panel-x86_64-linux-musl"

WORK="$(mktemp -d "${TMPDIR:-/tmp}/panel-install-test.XXXXXX")"
trap 'rm -rf "$WORK"' EXIT

SHIM="$WORK/shim"
FIXTURE="$WORK/release"
PREFIX="$WORK/prefix"
mkdir -p "$SHIM" "$FIXTURE" "$PREFIX"

FAILED=0

check() { # check <结果 0/1> <描述> [细节]
    if [ "$1" = 0 ]; then
        printf '  ✅ %s%s\n' "$2" "${3:+ — $3}"
    else
        printf '  ❌ %s%s\n' "$2" "${3:+ — $3}"
        FAILED=$((FAILED + 1))
    fi
}

contains() { # contains <文本> <子串>
    case "$1" in *"$2"*) return 0 ;; *) return 1 ;; esac
}

# ---------- shim：假 uname / 假 curl / sha256sum ----------
# 假 uname：由 PANEL_FAKE_OS / PANEL_FAKE_ARCH 控制，默认 Linux + x86_64
cat > "$SHIM/uname" <<'EOF'
#!/bin/sh
case "$1" in
    -s) echo "${PANEL_FAKE_OS:-Linux}" ;;
    -m) echo "${PANEL_FAKE_ARCH:-x86_64}" ;;
    *)  echo "${PANEL_FAKE_OS:-Linux}" ;;
esac
EOF

# 假 curl：把 URL 的最后一段当文件名，从 PANEL_FIXTURE 里取（模拟 Release 下载）
cat > "$SHIM/curl" <<'EOF'
#!/bin/sh
out=""
url=""
while [ $# -gt 0 ]; do
    case "$1" in
        -o) out="$2"; shift 2 ;;
        -*) shift ;;
        *)  url="$1"; shift ;;
    esac
done
name="${url##*/}"
[ -f "$PANEL_FIXTURE/$name" ] || exit 22   # 404
cp "$PANEL_FIXTURE/$name" "$out"
EOF

# macOS 没有 sha256sum，用一个转发到真实实现的 shim —— 顺便验证脚本走的是 sha256sum 分支。
# 用绝对路径找，避免 shim 目录在 PATH 最前面时又调回自己。
cat > "$SHIM/sha256sum" <<'EOF'
#!/bin/sh
# coreutils 的 sha256sum：算法已经写在名字里，直接转发
for c in /usr/bin/sha256sum /bin/sha256sum; do
    [ -x "$c" ] && exec "$c" "$@"
done
# macOS 只有 shasum，默认是 SHA-1（40 位），必须显式指定 -a 256（64 位）
for c in /usr/bin/shasum /bin/shasum; do
    [ -x "$c" ] && exec "$c" -a 256 "$@"
done
echo "找不到 sha256sum/shasum" >&2
exit 127
EOF

chmod +x "$SHIM/uname" "$SHIM/curl" "$SHIM/sha256sum"

# 生成校验和：本机有什么用什么（Linux 用 sha256sum，macOS 只有 shasum）
sha256_of() {
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$@"
    else
        shasum -a 256 "$@"
    fi
}

# ---------- 造一个「Release」 ----------
make_release() {
    rm -f "$FIXTURE"/*
    printf '#!/bin/sh\necho "fake plugin-panel"\n' > "$FIXTURE/$ASSET"
    chmod +x "$FIXTURE/$ASSET"
    ( cd "$FIXTURE" && sha256_of "$ASSET" > SHA256SUMS )
}

run_install() { # run_install <输出文件> [额外环境变量...]
    local log="$1"; shift
    env -i \
        PATH="$SHIM:/usr/bin:/bin:/usr/sbin:/sbin" \
        HOME="$WORK/home" \
        TMPDIR="${TMPDIR:-/tmp}" \
        PANEL_FIXTURE="$FIXTURE" \
        PANEL_BASE_URL="https://example.invalid/releases/latest/download" \
        PANEL_NO_PATH=1 \
        "$@" \
        bash "$INSTALL" > "$log" 2>&1
}

mkdir -p "$WORK/home"

echo "install.sh: $INSTALL"
echo "工作目录:   $WORK"
echo

# ---------- 1. 正常安装 ----------
echo "1) 正常安装"
make_release
OUT="$(run_install "$WORK/1.log" PANEL_PREFIX="$PREFIX"; echo "$?")"
check "$([ "$OUT" = 0 ]; echo $?)" "安装脚本退出码为 0" "exit=$OUT"
LOG="$(cat "$WORK/1.log")"
contains "$LOG" "SHA256 校验通过" && check 0 "SHA256 校验通过" || check 1 "SHA256 校验通过"
[ -x "$PREFIX/plugin-panel" ] && check 0 "产物已装到 PANEL_PREFIX 且可执行" "$PREFIX/plugin-panel" \
                              || check 1 "产物已装到 PANEL_PREFIX 且可执行"
contains "$("$PREFIX/plugin-panel" 2>&1)" "fake plugin-panel" \
    && check 0 "装进去的确实是下载的产物" || check 1 "装进去的确实是下载的产物"
contains "$LOG" "已安装: $PREFIX/plugin-panel" && check 0 "安装路径提示正确" || check 1 "安装路径提示正确"

# ---------- 2. 篡改产物 ----------
echo "2) 产物被篡改"
make_release
printf '\n# 被篡改的字节\n' >> "$FIXTURE/$ASSET"
OUT="$(run_install "$WORK/2.log" PANEL_PREFIX="$PREFIX"; echo "$?")"
check "$([ "$OUT" != 0 ]; echo $?)" "校验失败时拒绝安装（退出码非 0）" "exit=$OUT"
contains "$(cat "$WORK/2.log")" "SHA256 校验失败" && check 0 "给出校验失败提示" || check 1 "给出校验失败提示"

# ---------- 3. 不支持的架构 ----------
echo "3) 不支持的架构"
OUT="$(run_install "$WORK/3.log" PANEL_FAKE_ARCH=mips PANEL_PREFIX="$PREFIX"; echo "$?")"
check "$([ "$OUT" != 0 ]; echo $?)" "不支持的架构退出码非 0" "exit=$OUT"
contains "$(cat "$WORK/3.log")" "不支持的架构" && check 0 "给出架构错误提示" || check 1 "给出架构错误提示"

# ---------- 4. 非 Linux ----------
echo "4) 非 Linux（macOS）"
OUT="$(run_install "$WORK/4.log" PANEL_FAKE_OS=Darwin PANEL_PREFIX="$PREFIX"; echo "$?")"
check "$([ "$OUT" != 0 ]; echo $?)" "非 Linux 退出码非 0" "exit=$OUT"
contains "$(cat "$WORK/4.log")" "只提供 Linux 产物" && check 0 "给出平台错误提示" || check 1 "给出平台错误提示"

# ---------- 5. Termux ----------
echo "5) Termux"
TPREFIX="$WORK/termux-usr"
mkdir -p "$TPREFIX"
make_release
OUT="$(run_install "$WORK/5.log" TERMUX_VERSION=0.118 PREFIX="$TPREFIX"; echo "$?")"
check "$([ "$OUT" = 0 ]; echo $?)" "Termux 下退出码为 0" "exit=$OUT"
[ -x "$TPREFIX/bin/plugin-panel" ] && check 0 "装到 \$PREFIX/bin" "$TPREFIX/bin/plugin-panel" \
                                   || check 1 "装到 \$PREFIX/bin"

# ---------- 6. 下载失败 ----------
echo "6) 产物不存在"
rm -f "$FIXTURE/$ASSET"
OUT="$(run_install "$WORK/6.log" PANEL_PREFIX="$PREFIX"; echo "$?")"
check "$([ "$OUT" != 0 ]; echo $?)" "下载失败时退出码非 0" "exit=$OUT"
contains "$(cat "$WORK/6.log")" "下载失败" && check 0 "给出下载失败提示" || check 1 "给出下载失败提示"

echo
if [ "$FAILED" != 0 ]; then
    echo "❌ $FAILED 项失败"
    exit 1
fi
echo "✅ install.sh 全部用例通过"
