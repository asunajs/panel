#!/usr/bin/env python3
"""CSS 里只许用**定义过**的 token，且不许写 fallback。

为什么：var(--bg-soft, #fafafc) 这种写法在变量不存在时会**静默回落**到那个浅色值，
深色主题下就冒出一块白 —— 第 24 轮用户抓到的那张「还没有令牌」卡片就是这么来的
（--bg-soft 从来没定义过）。带 fallback 的 var() 是最难查的一类样式 bug：
不报错、不警告，只在另一个主题下现形。

规则：
  1. 用到的变量必须在 :root / [data-theme=...] 里定义过；
  2. 不许写 fallback（var(--x, 值)），否则「变量名打错」会变成静默降级。
注释里的示例不算违规（先剥注释再扫）。
"""
import re
import sys

CSS = 'web/app.css'
src = open(CSS, encoding='utf-8').read()
stripped = re.sub(r'/\*.*?\*/', lambda m: '\n' * m.group(0).count('\n'), src, flags=re.S)

defined = set(re.findall(r'(--[a-z0-9-]+)\s*:', stripped))
uses = [(m.group(1), m.group(2)) for m in re.finditer(r'var\(\s*(--[a-z0-9-]+)\s*(,[^)]*)?\)', stripped)]

bad = []
for name, fb in uses:
    if name not in defined:
        bad.append(('未定义', name, (fb or '').strip()))
    elif fb:
        bad.append(('带 fallback', name, fb.strip()))

if bad:
    for kind, name, extra in bad:
        print('  \033[31m✗\033[0m %s 的变量 %s %s' % (kind, name, extra))
    print('红: %d 处（定义过 %d 个变量）' % (len(bad), len(defined)))
    sys.exit(1)

print('  \033[32m✓\033[0m CSS 只用定义过的 token，%d 个变量、%d 处引用、没有 fallback' % (len(defined), len(uses)))
