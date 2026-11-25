#!/usr/bin/env node
// 把插件目录从「散件」收拢成「一个插件一个目录」（第 269 轮）。
//
//   node scripts/migrate-plugin-layout.mjs --dir ~/panel-3030/plugins --dry
//   node scripts/migrate-plugin-layout.mjs --dir ~/panel-3030/plugins --apply
//
// 搬什么：
//   mgmusic.js  mgmusic.panel.json  mgmusic.webui/      →  mgmusic/
//   mcloud-daily/  mcloud-daily.panel.json  …           →  mcloud/        （一包多插件）
//   mcloud.panel.json  mcloud.webui/                    →  mcloud/
//
// 为什么可以搬：面板的发现顺序是 `<name>.js` → `<name>/index.js` → `<name>/<name>.js`，
// 清单与界面也两级查找（见 docs/plugins.md「产物形状」）。搬完形状变了、行为不变。
//
// ⚠ 两个前提：
//   1. 面板版本 ≥ 0.9.2（老面板不认收拢形状，搬完会「装了但看不见」）；
//   2. 插件项目的 `deploy:panel` 也改成了收拢输出 —— 否则下次部署又把散件写回来。
// 脚本会在动手前检查第一条，第二条只在报告里提醒（它管不了别的仓）。
import { cpSync, existsSync, mkdirSync, readdirSync, renameSync, statSync } from 'node:fs';
import { execFileSync } from 'node:child_process';
import { dirname, join, resolve } from 'node:path';

const args = process.argv.slice(2);
const flag = (n) => {
  const i = args.indexOf(n);
  return i >= 0 ? args[i + 1] : null;
};
const APPLY = args.includes('--apply');
const ROOT = resolve(flag('--dir') || 'plugins');
if (!existsSync(ROOT)) {
  console.error(`❌ 目录不存在：${ROOT}`);
  process.exit(1);
}

/** 面板版本够不够（要 ≥ 0.9.2）。读不到就不拦，只提示。 */
function panelVersionOk() {
  const bin = join(dirname(ROOT), 'panel');
  if (!existsSync(bin)) return { ok: true, why: '没找到 panel 二进制，跳过版本检查' };
  try {
    const out = execFileSync(bin, ['version'], { encoding: 'utf8' });
    const m = /(\d+)\.(\d+)\.(\d+)/.exec(out);
    if (!m) return { ok: true, why: '版本号解析不出来，跳过检查' };
    const [maj, min, pat] = [Number(m[1]), Number(m[2]), Number(m[3])];
    const ok = maj > 0 || min > 9 || (min === 9 && pat >= 2);
    return { ok, why: `面板 ${m[0]}（要 ≥ 0.9.2）` };
  } catch {
    return { ok: true, why: '跑 panel version 失败，跳过检查' };
  }
}

const entries = readdirSync(ROOT);
function isDirectory(p) {
  try {
    return statSync(p).isDirectory();
  } catch {
    return false;
  }
}

/** 一个插件名 → 它该去的属地目录名。 */
function groupOf(name) {
  // ① 一包多插件优先判：`mcloud-daily` → `mcloud/`（前缀目录在且有入口）。
  //    必须先判这条 —— `mcloud-daily/` 自己也是个目录（老目录形态），
  //    先判「自己就是目录」会把它单独拎出去，包就散了。
  const head = name.split('-')[0];
  if (head !== name && isDirectory(join(ROOT, head)) && hasEntry(join(ROOT, head))) {
    return head;
  }
  // ② 自己就是目录（`mcloud/`、`yangshipin/`）：属地就是它自己
  if (isDirectory(join(ROOT, name))) return name;
  // ③ 否则自己建一个目录
  return name;
}

/** 这个目录里有没有插件入口（`index.js` 或目录同名 `.js`）。 */
function hasEntry(dir) {
  return existsSync(join(dir, 'index.js')) || existsSync(join(dir, `${dir.split('/').pop()}.js`));
}

const plans = [];
const seen = new Set();
for (const n of entries) {
  const bare = n.replace(/\.panel\.json$/, '');
  const stem = bare
    .replace(/\.js$/, '')
    .replace(/\.webui$/, '')
    .replace(/\.js\.bak$/, '');
  if (n === 'README.md' || n.startsWith('.') || n.endsWith('.bak')) continue;
  if (!/\.(js|panel\.json|webui|js\.bak)$/.test(n) && !isDirectory(join(ROOT, n))) continue;
  const g = groupOf(stem);
  const src = join(ROOT, n);
  const dst = join(ROOT, g, n);
  if (src === join(ROOT, g) || src === dst) continue; // 已经在属地里
  if (seen.has(src)) continue;
  seen.add(src);
  plans.push({ name: stem, group: g, from: n, to: join(g, n) });
}

const v = panelVersionOk();
console.log(`目录：${ROOT}`);
console.log(`面板：${v.why}${v.ok ? '' : '  ⚠ 太老，搬完会认不出收拢形状'}`);
if (!plans.length) {
  console.log('✓ 已经是收拢形状（没有可搬的散件）');
  process.exit(0);
}
console.log(`\n待搬 ${plans.length} 项：`);
for (const p of plans) console.log(`  ${p.from.padEnd(30)} → ${p.to}`);

if (!APPLY) {
  console.log('\n（只是预览。加 --apply 才动手；动手前会先整目录备份到 <data>/backup-layout-<时间戳>/）');
  process.exit(0);
}
if (!v.ok) {
  console.error('\n❌ 面板版本不够，先升级面板再搬（搬完它认不出这些形状）。');
  process.exit(1);
}

// 备份：整目录复制到数据目录下（不移动，出错还能原样回来）
const stamp = new Date().toISOString().replace(/[-:T]/g, '').slice(0, 14);
const backup = join(dirname(ROOT), `backup-layout-${stamp}`);
cpSync(ROOT, backup, { recursive: true });
console.log(`\n已备份到 ${backup}`);

let moved = 0;
for (const p of plans) {
  const from = join(ROOT, p.from);
  const to = join(ROOT, p.to);
  mkdirSync(dirname(to), { recursive: true });
  try {
    renameSync(from, to);
    moved += 1;
    console.log(`  ✓ ${p.from} → ${p.to}`);
  } catch (e) {
    console.error(`  ✗ ${p.from}：${e.message}`);
  }
}
console.log(`\n搬了 ${moved}/${plans.length} 项。重启面板后核对：/readyz 的 failed_plugins 应为 0。`);
