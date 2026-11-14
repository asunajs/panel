#!/usr/bin/env bash
# 让 ARM musl 产物变成 **static PIE**（ET_DYN + 无 PT_INTERP）。
#
# 为什么需要它：新版 Termux 的 termux-exec 会把程序交给 /system/bin/linker64 执行，
# 而 bionic 的 linker 只接受 ET_DYN —— 静态 ET_EXEC 会报
#   error: "…/plugin-panel" has unexpected e_type: 2
# 详见 docs/pitfalls.md 第 41 条。
#
# rustc 在 ARM musl 上会**显式**传一整套非 PIE 的东西：
#   -static  -no-pie  crt1.o  crtbegin.o  crtend.o
# 只补一个 `-static-pie` 没用 —— 后面的 `-no-pie` 会把它盖掉，crt 也不会被换。
# 对照 rustc 给 x86_64-unknown-linux-musl（本来就是 static PIE）传的参数，这里做四处替换：
#   -static      → -static-pie          （GCC 的 static PIE 链接）
#   -no-pie      → 丢弃                 （不然它会把 -static-pie 盖回去）
#   crt1.o       → rcrt1.o              （自搬运的启动代码）
#   crtbegin.o   → crtbeginS.o          （PIC 版）
#   crtend.o     → crtendS.o
# 其余参数原样透传。
set -euo pipefail

REAL="${REAL_LINKER:-musl-gcc}"

args=()
for a in "$@"; do
    case "$a" in
        -static) args+=("-static-pie") ;;
        -no-pie) : ;;
        */self-contained/crt1.o) args+=("${a%crt1.o}rcrt1.o") ;;
        */self-contained/crtbegin.o) args+=("${a%crtbegin.o}crtbeginS.o") ;;
        */self-contained/crtend.o) args+=("${a%crtend.o}crtendS.o") ;;
        *) args+=("$a") ;;
    esac
done

exec "$REAL" "${args[@]}"
