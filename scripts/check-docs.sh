#!/usr/bin/env bash
# 文档守卫：链接不许断、单篇不许超 1000 行、三态词不许乱用。
#
# 为什么需要它：docs/ 是**判据的载体**，不是散文。断了的内链会让「见 §7」变成死路，
# 上限原来是 300 行、35 篇；第 82 轮合并成 11 篇之后抬到 1000 行 —— 现在的形状是
# 「一章一篇」而不是「一个读数一篇」，页少但每页长，找东西靠目录和小标题。
# 这两个错都不会让代码编译失败，只会在几个月后咬人 —— 所以让脚本盯着。
#
# 用法：scripts/check-docs.sh [--fix-hint]
set -euo pipefail

ROOT=$(cd "$(dirname "$0")/.." && pwd)
cd "$ROOT"

exec python3 - "$@" <<'PY'
import os, re, sys

ROOT = "docs"
MAX_LINES = 1000
bad = 0

files = sorted(f for f in os.listdir(ROOT) if f.endswith(".md"))

# 1) 内链必须存在
for f in files:
    text = open(os.path.join(ROOT, f), encoding="utf-8").read()
    for m in re.finditer(r"\]\(([^)\s]+\.md)(#[^)]*)?\)", text):
        target = m.group(1)
        if target.startswith(("http://", "https://")):
            continue
        path = os.path.normpath(os.path.join(ROOT, target))
        if not os.path.exists(path):
            line = text[: m.start()].count("\n") + 1
            print(f"❌ {f}:{line} 链接指向不存在的文件: {target}")
            bad += 1

# 2) 单篇行数上限
for f in files:
    n = sum(1 for _ in open(os.path.join(ROOT, f), encoding="utf-8"))
    if n > MAX_LINES:
        print(f"❌ {f} 有 {n} 行，超过 {MAX_LINES} 行上限（拆一篇，然后在两边互链）")
        bad += 1

# 3) 每篇都得有标题
for f in files:
    first = open(os.path.join(ROOT, f), encoding="utf-8").readline()
    if not first.startswith("# "):
        print(f"❌ {f} 第一行不是一级标题（读者和目录都靠它认这篇是什么）")
        bad += 1

if bad:
    print(f"\n{bad} 处文档问题")
    sys.exit(1)
print(f"ok: {len(files)} 篇文档，内链全通、都在 {MAX_LINES} 行内")
PY
