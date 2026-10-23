#!/usr/bin/env bash
# 让 ARM musl 产物变成 **static PIE**（ET_DYN + 无 PT_INTERP）。
#
# 为什么需要它：新版 Termux 的 termux-exec 会把程序交给 /system/bin/linker64 执行，
# 而 bionic 的 linker 只接受 ET_DYN —— 静态 ET_EXEC 会报
#   error: "…/plugin-panel" has unexpected e_type: 2
# 详见 docs/pitfalls.md 第 41 条。
#
# 为什么不直接 `-C link-arg=-static-pie`：rustc 在 ARM musl 上会**显式**传一整套
# 非 PIE 的启动/收尾目标文件 + `-static`。只补一个 `-static-pie` 不会换掉它们，
# 链接出来的东西要么仍是 ET_EXEC，要么是「ET_DYN 但没有自搬运启动代码」——一跑就 segfault
# （CI 上实测过两次）。
#
# 对照 rustc 给 x86_64-unknown-linux-musl（它本来就是 static PIE）传的参数，
# 差别只有这四处，所以这里原样替换：
#   -static      → -static-pie
#   crt1.o       → rcrt1.o      （自搬运的启动代码）
#   crtbegin.o   → crtbeginS.o  （PIC 版）
#   crtend.o     → crtendS.o
# 其余参数原样透传。
set -euo pipefail

REAL="${REAL_LINKER:-musl-gcc}"

args=()
for a in "$@"; do
    case "$a" in
        -static) args+=("-static-pie") ;;
        */self-contained/crt1.o) args+=("${a%crt1.o}rcrt1.o") ;;
        */self-contained/crtbegin.o) args+=("${a%crtbegin.o}crtbeginS.o") ;;
        */self-contained/crtend.o) args+=("${a%crtend.o}crtendS.o") ;;
        *) args+=("$a") ;;
    esac
done

exec "$REAL" "${args[@]}"
