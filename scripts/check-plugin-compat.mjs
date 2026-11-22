#!/usr/bin/env node
/// 插件「能力 ↔ min_panel」双向体检（第 255 轮）。
///
/// 用法：
///   node scripts/check-plugin-compat.mjs              # 报告（永远 exit 0，除非读文件出错）
///   node scripts/check-plugin-compat.mjs --strict     # 发版/CI 用：写低 / 等面板发版 都红
///   node scripts/check-plugin-compat.mjs --strict --allow-pending   # 只看写低
///   node scripts/check-plugin-compat.mjs --explain    # 打印能力表（版本从哪来的）
///
/// 判据（R7）：
///   · 用了版本 X 才有的能力，却声称 min_panel < X  → ❌ **写低了**：装上去跑不起来
///   · 声称 min_panel > 实际用到的最高版本          → ⚠️ **写高了**：里面大概有白写的兼容分支
///   · 相等                                        → ✅
///
/// 扫的是**产物**（dist/panel-llrt/*.js 与 *.webui/），没有产物就退到源码目录 ——
/// 判据看的是「装到面板上的那份」。
import { existsSync, readdirSync, readFileSync, statSync } from 'node:fs';
import { dirname, join, relative, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

import { CAPABILITIES, cmpVersion, maxVersion } from './panel-capabilities.mjs';

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const PARENT = dirname(ROOT);
const argv = process.argv.slice(2);
const strict = argv.includes('--strict');
const allowPending = argv.includes('--allow-pending');
const explain = argv.includes('--explain');

if (explain) {
  console.log('能力表（since = 面板首次提供它的版本）：\n');
  for (const c of CAPABILITIES) {
    console.log(`  ${String(c.since).padEnd(7)} ${c.id}${c.note ? '   — ' + c.note : ''}`);
  }
  console.log('\n版本来源：面板仓里 `git log -S<标记>` 找首个提交，再 `git describe --tags --contains` 反查。');
  process.exit(0);
}

function walk(dir, out = []) {
  for (const name of readdirSync(dir)) {
    const p = join(dir, name);
    const st = statSync(p);
    if (st.isDirectory()) {
      if (name === 'node_modules' || name === '.git') continue;
      walk(p, out);
    } else if (/\.(js|mjs|html|css)$/.test(name)) {
      out.push(p);
    }
  }
  return out;
}

/** 找插件仓：同级目录里带 package.json + 任意 *.panel.json 的。 */
function findPlugins() {
  return readdirSync(PARENT)
    .map((n) => join(PARENT, n))
    .filter((p) => {
      try {
        if (!statSync(p).isDirectory()) return false;
        if (!existsSync(join(p, 'package.json'))) return false;
        const hasManifest = readdirSync(p).some((f) => f.endsWith('.panel.json')) ||
          (existsSync(join(p, 'plugins')) && readdirSync(join(p, 'plugins')).some((f) => f.endsWith('.panel.json')));
        return hasManifest;
      } catch {
        return false;
      }
    })
    .sort();
}

/** 主清单：仓库根的 `<name>.panel.json`（多清单时取版本最大的那份口径，这里只读 min_panel）。 */
function readManifests(repo) {
  const out = [];
  for (const f of readdirSync(repo)) {
    if (f.endsWith('.panel.json')) out.push(join(repo, f));
  }
  const sub = join(repo, 'plugins');
  if (existsSync(sub)) {
    for (const f of readdirSync(sub)) {
      if (f.endsWith('.panel.json')) out.push(join(sub, f));
    }
  }
  return out;
}

function artifactFiles(repo) {
  const dist = join(repo, 'dist', 'panel-llrt');
  if (existsSync(dist)) return walk(dist);
  // 没有构建产物就退到源码（mgmusic/panel-store 的 dist 结构不同，这里宽松兜底）
  const cands = ['src', 'webui', 'dist'].map((d) => join(repo, d)).filter((d) => existsSync(d));
  return cands.flatMap((d) => walk(d));
}

let bad = 0;
let warned = 0;
let pending = 0;
const rows = [];

for (const repo of findPlugins()) {
  const name = repo.split('/').pop();
  const manifests = readManifests(repo);
  if (manifests.length === 0) continue;

  const declared = new Set();
  for (const m of manifests) {
    try {
      declared.add(String(JSON.parse(readFileSync(m, 'utf8')).min_panel || ''));
    } catch {
      /* 坏清单交给别的门禁 */
    }
  }
  const minPanel = [...declared][0] || '';

  const files = artifactFiles(repo);
  const texts = files.map((f) => ({ file: f, text: readFileSync(f, 'utf8') }));
  const manifestText = manifests.map((f) => readFileSync(f, 'utf8')).join('\n');

  const used = [];
  for (const cap of CAPABILITIES) {
    const hay = cap.where === 'manifest' ? manifestText : null;
    const hit = hay !== null
      ? cap.pattern.test(hay)
      : texts.some((t) => cap.pattern.test(t.text));
    if (hit) used.push(cap);
  }

  const required = maxVersion(used.map((c) => c.since));
  const needsUnreleased = used.filter((c) => c.since === 'dev').map((c) => c.id);
  const cmp = minPanel ? cmpVersion(minPanel, required) : -1; // 没写 min_panel 视为写低
  // 用到了**面板还没发版**的能力：不是插件的错，是「等面板发版 + 提 min_panel」这一步还没做
  const status = needsUnreleased.length > 0 ? 'UNRELEASED' : cmp < 0 ? 'LOW' : cmp > 0 ? 'HIGH' : 'OK';
  rows.push({ name, minPanel, required, status, used, needsUnreleased });

  if (status === 'LOW') bad += 1;
  if (status === 'HIGH') warned += 1;
  if (status === 'UNRELEASED') pending += 1;
}

const pad = (s, n) => String(s).padEnd(n);
console.log('插件                 min_panel   实际需要   判定');
console.log('──────────────────────────────────────────────────────────');
for (const r of rows) {
  const mark = r.status === 'OK' ? '✅ 一致'
    : r.status === 'LOW' ? '❌ 写低了（装不上/跑不起来）'
    : r.status === 'UNRELEASED' ? '⏳ 用了面板未发版的能力：' + r.needsUnreleased.join(', ')
    : '⚠️  写高了（有白写的兼容）';
  console.log(`${pad(r.name, 20)} ${pad(r.minPanel || '(缺)', 11)} ${pad(r.required, 10)} ${mark}`);
}

if (process.env.SHOW_USED === '1') {
  for (const r of rows) {
    console.log(`\n${r.name}:`);
    for (const c of r.used) console.log(`  ${c.since}  ${c.id}`);
  }
}

console.log('');
if (bad === 0 && warned === 0 && pending === 0) {
  console.log(`ok: ${rows.length} 个插件的 min_panel 与用到的能力一致`);
  process.exit(0);
}
console.log(`写低 ${bad} 个 · 写高 ${warned} 个 · 等面板发版 ${pending} 个（共 ${rows.length} 个）`);
if (pending > 0) {
  console.log('⏳ 那几个用了面板还没发布的能力：面板发版后把 min_panel 提到该版本，别提前写「dev」。');
}
if (bad > 0) {
  console.log('提示：把 min_panel 提到「实际需要」那一列的值；写高的则相反 —— 去掉用不上的兼容分支，或把版本降回真实下限。');
}
// 发版/CI：写低必须红；「等面板发版」默认也红（插件没法声明一个还不存在的版本），
// 开发期想只看写低就用 --allow-pending。
const fail = bad > 0 || (pending > 0 && !allowPending);
process.exit(strict && fail ? 1 : 0);
