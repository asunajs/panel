#!/usr/bin/env bash
# 让 ARM musl 产物变成 **static PIE**（ET_DYN + 无 PT_INTERP）。
#
# 为什么需要它：新版 Termux 的 termux-exec 会把程序交给 /system/bin/linker64 执行，
# 而 bionic 的 linker 只接受 ET_DYN —— 静态 ET_EXEC 会报
#   error: "…/plugin-panel" has unexpected e_type: 2
# 详见 docs/pitfalls.md 第 41 条。
#
# 为什么不直接 `-C link-arg=-static-pie`：rustc 在 aarch64/armv7-musl 上会**显式**传
#   self-contained/crt1.o + -nostartfiles + -static
# GCC 的 -static-pie 不会替换掉这些显式参数，结果是一个「ET_DYN 但没有自搬运启动代码」
# 的二进制 —— 一跑就 segfault（CI 上实测过两次）。所以这里做两件事：
#   1. -static        → -static-pie
#   2. crt1.o         → rcrt1.o（musl 的 static PIE 启动代码，会自己搬运重定位）
# 其余参数原样透传。
set -euo pipefail

REAL="${REAL_LINKER:-musl-gcc}"

args=()
for a in "$@"; do
    case "$a" in
        -static) args+=("-static-pie") ;;
        */self-contained/crt1.o) args+=("${a%crt1.o}rcrt1.o") ;;
        *) args+=("$a") ;;
    esac
done

exec "$REAL" "${args[@]}"
