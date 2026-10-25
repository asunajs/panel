#!/usr/bin/env node
/**
 * 文档链接与索引检查（审查 L12 的 CI 化）。
 *
 * 查三件事，都是**脚本能发现、人眼会漏**的：
 *
 * 1. 相对链接指向的文件是否存在（改名/删文件之后最容易断）
 * 2. 链接里的 `#锚点` 是否真的对应一个标题
 *    —— 这一条比第 1 条值钱：文件还在，锚点失效，点了只是跳到页首，**不报错**
 * 3. `docs/` 下的每份文档是否都进了 `docs/README.md` 的索引
 *
 * 退出码 1 = 有问题。CI 直接跑它。
 */
import { readFileSync, readdirSync, existsSync, statSync } from 'node:fs';
import { join, dirname, resolve, relative } from 'node:path';

const ROOT = resolve(import.meta.dirname, '..');

/** 收集所有要检查的 markdown 文件（仓库根 README + docs/ 递归）。 */
function collect() {
  const out = [];
  const push = (p) => {
    if (existsSync(p) && statSync(p).isFile() && p.endsWith('.md')) out.push(p);
  };
  push(join(ROOT, 'README.md'));
  push(join(ROOT, 'CHANGELOG.md'));
  const walk = (dir) => {
    for (const e of readdirSync(dir, { withFileTypes: true })) {
      const p = join(dir, e.name);
      if (e.isDirectory()) walk(p);
      else push(p);
    }
  };
  if (existsSync(join(ROOT, 'docs'))) walk(join(ROOT, 'docs'));
  return out;
}

/**
 * 标题 → GitHub 风格的锚点。
 *
 * 规则（够用即可，不求完全等价）：去掉行内代码反引号与链接标记、转小写、
 * 去掉除字母/数字/汉字/连字符/空格之外的字符、空格变连字符。
 * ⚠ 中文标题的锚点里汉字是**保留**的，所以不能只留 [a-z0-9-]。
 */
function slug(text) {
  return text
    .replace(/`([^`]*)`/g, '$1')
    .replace(/\[([^\]]*)\]\([^)]*\)/g, '$1')
    .trim()
    .toLowerCase()
    // ⚠ 下划线要保留：GitHub 的锚点里 `ENGINE_WORKERS` 仍是 `engine_workers`。
    .replace(/[^\p{L}\p{N}\s_-]/gu, '')
    // ⚠ 每个空格变**一个**连字符，不折叠：GitHub 对 `a / b` 产生的是 `a--b`。
    .replace(/\s/g, '-');
}

/** 一个文件里所有标题产生的锚点（含重复标题的 -1/-2 后缀）。 */
function anchorsOf(file) {
  const text = readFileSync(file, 'utf8');
  const seen = new Map();
  const out = new Set();
  let inFence = false;
  for (const line of text.split('\n')) {
    if (/^\s*```/.test(line)) {
      inFence = !inFence;
      continue;
    }
    if (inFence) continue;
    const m = /^#{1,6}\s+(.*)$/.exec(line);
    if (!m) continue;
    const base = slug(m[1]);
    const n = seen.get(base) ?? 0;
    seen.set(base, n + 1);
    out.add(n === 0 ? base : `${base}-${n}`);
  }
  return out;
}

const problems = [];
const anchorCache = new Map();
const files = collect();

// ---- 1 & 2：链接与锚点 ----
for (const file of files) {
  const text = readFileSync(file, 'utf8');
  let inFence = false;
  const lines = text.split('\n');
  for (let i = 0; i < lines.length; i++) {
    const line = lines[i];
    if (/^\s*```/.test(line)) {
      inFence = !inFence;
      continue;
    }
    if (inFence) continue;
    // [文字](目标) —— 目标里不能有空格或右括号（够用；我们的链接都长这样）
    const re = /\[([^\]]*)\]\(([^)\s]+)\)/g;
    let m;
    while ((m = re.exec(line)) !== null) {
      const target = m[2];
      if (/^(https?:|mailto:)/.test(target)) continue;
      const [pathPart, anchor] = target.split('#');
      const where = `${relative(ROOT, file)}:${i + 1}`;
      if (pathPart === '') {
        // 纯锚点：指向本文件
        if (anchor && !anchorsOf(file).has(anchor)) {
          problems.push(`${where} 锚点不存在: #${anchor}`);
        }
        continue;
      }
      const dest = resolve(dirname(file), pathPart);
      if (!existsSync(dest)) {
        problems.push(`${where} 文件不存在: ${pathPart}`);
        continue;
      }
      if (anchor && dest.endsWith('.md')) {
        if (!anchorCache.has(dest)) anchorCache.set(dest, anchorsOf(dest));
        if (!anchorCache.get(dest).has(anchor)) {
          problems.push(`${where} 锚点不存在: ${pathPart}#${anchor}`);
        }
      }
    }
  }
}

// ---- 3：docs 索引完整性 ----
const index = join(ROOT, 'docs', 'README.md');
if (existsSync(index)) {
  const idx = readFileSync(index, 'utf8');
  const walk = (dir) => {
    for (const e of readdirSync(dir, { withFileTypes: true })) {
      const p = join(dir, e.name);
      if (e.isDirectory()) walk(p);
      else if (e.name.endsWith('.md') && e.name !== 'README.md') {
        const rel = relative(join(ROOT, 'docs'), p);
        if (!idx.includes(rel)) problems.push(`docs/README.md 未索引: ${rel}`);
      }
    }
  };
  walk(join(ROOT, 'docs'));
}

// ---- 4：设置项与文档对账 ----
//
// 这一条是**审查 L12 的直接教训**：`RUN_KEEP_DAYS` 和 `PLUGIN_KEEPALIVE_MS`
// 都在设置页里存在，却一份 markdown 都没写。用户照着文档部署就永远不知道有它们。
//
// 判据：`settings.rs` 的 DEFS 里每个 `key` 都必须在 `docs/configuration.md` 出现。
// 反过来不查 —— 文档里可以写不在设置页的变量（比如只在环境里生效的那些）。
{
  const src = join(ROOT, 'src', 'settings.rs');
  const cfg = join(ROOT, 'docs', 'configuration.md');
  if (existsSync(src) && existsSync(cfg)) {
    const code = readFileSync(src, 'utf8');
    const doc = readFileSync(cfg, 'utf8');
    const keys = new Set();
    for (const m of code.matchAll(/key:\s*"([A-Z0-9_]+)"/g)) keys.add(m[1]);
    for (const k of [...keys].sort()) {
      if (!doc.includes('\`' + k + '\`') && !doc.includes(k)) {
        problems.push(`docs/configuration.md 没写设置项: ${k}`);
      }
    }
  }
}

if (problems.length) {
  console.error(`文档检查失败，${problems.length} 处：`);
  for (const p of problems) console.error('  ' + p);
  process.exit(1);
}
console.log(`文档检查通过（${files.length} 个文件）`);
