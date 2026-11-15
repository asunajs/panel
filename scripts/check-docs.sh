#!/usr/bin/env bash
# 文档守卫：链接不许断、单篇不许超 1000 行。
#
# 为什么需要它：docs/ 是**判据的载体**，不是散文。断了的内链会让「见 §7」变成死路，
# 上限原来是 300 行、35 篇；第 82 轮合并成 11 篇之后抬到 1000 行 —— 现在的形状是
# 「一章一篇」而不是「一个读数一篇」，页少但每页长，找东西靠目录和小标题。
# 这两个错都不会让代码编译失败，只会在几个月后咬人 —— 所以让脚本盯着。
#
# 扫描是**递归**的（docs/user/、docs/handoff/ 子目录也在守卫里）——
# 2026-11-15 文档分区成「用户区 docs/user/ + 开发/agent 区 docs/ 根」之后，
# 只扫顶层会让整个用户区逃出守卫。
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

docs = []
for base, _, files in os.walk(ROOT):
    for f in files:
        if f.endswith(".md"):
            docs.append(os.path.join(base, f))

# 1) 内链必须存在（相对**所在文档的目录**解析：user/ 里的链接从 user/ 出发算）
for rel in sorted(docs):
    text = open(rel, encoding="utf-8").read()
    base = os.path.dirname(rel)
    for m in re.finditer(r"\]\(([^)\s]+\.md)(#[^)]*)?\)", text):
        target = m.group(1)
        if target.startswith(("http://", "https://")):
            continue
        path = os.path.normpath(os.path.join(base, target))
        if not os.path.exists(path):
            line = text[: m.start()].count("\n") + 1
            print(f"❌ {rel}:{line} 链接指向不存在的文件: {target}")
            bad += 1

# 2) 单篇行数上限
for rel in sorted(docs):
    n = sum(1 for _ in open(rel, encoding="utf-8"))
    if n > MAX_LINES:
        print(f"❌ {rel} 有 {n} 行，超过 {MAX_LINES} 行上限（拆一篇，然后在两边互链）")
        bad += 1

# 3) 每篇都得有标题
for rel in sorted(docs):
    first = open(rel, encoding="utf-8").readline()
    if not first.startswith("# "):
        print(f"❌ {rel} 第一行不是一级标题（读者和目录都靠它认这篇是什么）")
        bad += 1

if bad:
    print(f"\n{bad} 处文档问题")
    sys.exit(1)
print(f"ok: {len(docs)} 篇文档（含 user/ 与 handoff/ 子目录），内链全通、都在 {MAX_LINES} 行内")
PY
