#!/usr/bin/env node
/// 插件仓之间的「共享片段」一致性门禁。
///
/// 为什么要有：同一段胶水（类型同步脚本、面板契约 d.ts）在四个插件仓各有一份副本 ——
/// 这是**有意的**（插件要能单独 clone 下来跑），但副本一漂就成了四个版本，比不共享更糟。
/// 这里把「必须逐字节一致」的东西列出来，谁改了一份没同步，门禁直接红。
///
/// 用法：node scripts/check-plugin-shared.mjs [插件目录...]
///   不给参数时，自动找面板仓同级的、带 scripts/sync-types.mjs 的目录。
import { readFileSync, existsSync, readdirSync, statSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const PARENT = dirname(ROOT);

/** 每个插件仓里「必须和真源一致」的副本 → 真源（相对面板仓） */
const SHARED = [
  { copy: 'types/panel-ui.d.ts', source: 'web/ui.d.ts', what: '面板 JS 契约' },
  { copy: 'scripts/sync-types.mjs', source: 'tools/plugin-shared/sync-types.mjs', what: '类型同步脚本' },
  { copy: 'tsconfig.webui.base.json', source: 'tools/plugin-shared/tsconfig.webui.base.json', what: '界面编译选项' },
  { copy: 'types/panel-host.d.ts', source: 'tools/plugin-shared/panel-host.d.ts', what: '宿主 API 契约' },
  { copy: 'types/node-llrt.d.ts', source: 'tools/plugin-shared/node-llrt.d.ts', what: 'LLRT 的 node 子集' },
  { copy: 'tsconfig.node.base.json', source: 'tools/plugin-shared/tsconfig.node.base.json', what: '服务端编译选项' },
];

const args = process.argv.slice(2);
const repos = args.length
  ? args.map((a) => resolve(a))
  : readdirSync(PARENT)
      .map((n) => join(PARENT, n))
      .filter((p) => {
        try {
          return statSync(p).isDirectory() && existsSync(join(p, 'scripts/sync-types.mjs'));
        } catch {
          return false;
        }
      })
      .sort();

if (repos.length === 0) {
  console.error('没找到任何带 scripts/sync-types.mjs 的插件目录（可用参数指定）');
  process.exit(2);
}

let bad = 0;
for (const repo of repos) {
  const name = repo.split('/').pop();
  for (const item of SHARED) {
    const srcPath = join(ROOT, item.source);
    const copyPath = join(repo, item.copy);
    if (!existsSync(copyPath)) {
      console.log(`  · ${name}: 没有 ${item.copy}（跳过 ${item.what}）`);
      continue;
    }
    if (!existsSync(srcPath)) {
      console.log(`  ✗ ${name}: 真源不存在 ${item.source}`);
      bad += 1;
      continue;
    }
    const same = readFileSync(srcPath).equals(readFileSync(copyPath));
    if (same) {
      console.log(`  ✓ ${name}: ${item.copy} 与真源一致（${item.what}）`);
    } else {
      console.log(`  ✗ ${name}: ${item.copy} 与 ${item.source} 不一致（${item.what}）—— 跑 sync-types / 重拷脚本`);
      bad += 1;
    }
  }
}

console.log('');
console.log(bad === 0 ? `ok: ${repos.length} 个插件仓的共享片段一致` : `失败 ${bad} 处`);
process.exit(bad === 0 ? 0 : 1);
