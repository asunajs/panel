// 把**面板仓**的 `web/ui.d.ts` 同步到本仓 `types/panel-ui.d.ts`。
//
// 为什么要有这一步：面板给插件页的 JS 面（`window.PanelUI` + `$()` 拿到的控件成员）
// 是**面板的契约**，真源在 `panel-llrt/web/ui.d.ts`。插件不该抄一份自由发挥，
// 但也不能在 typecheck 时依赖同级目录存在（clone 下来就没了）。
// 折中：真源留面板，本仓留一份**逐字节相同**的副本，`--check` 用来抓「面板改了、这边没跟」。
//
// 同步的东西（都是「面板的契约」，插件只该有副本）：
//   ① panel-llrt/web/ui.d.ts                        → types/panel-ui.d.ts
//   ② panel-llrt/tools/plugin-shared/tsconfig.webui.base.json → tsconfig.webui.base.json
//
//   node scripts/sync-types.mjs          # 从面板拷一份过来（写盘）
//   node scripts/sync-types.mjs --check  # 只比对，不同就非零退出（放进门禁）
import { existsSync, mkdirSync, readFileSync, writeFileSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const DEST = join(ROOT, 'types/panel-ui.d.ts');
// 面板仓位置：环境变量优先，其次同级目录（本仓与面板通常是兄弟目录）
const CANDIDATES = [
  process.env.PANEL_REPO_DIR,
  join(ROOT, '..', '..', 'panel-llrt'),
  join(ROOT, '..', 'panel-llrt'),
].filter(Boolean);

const FILES = [
  { from: 'web/ui.d.ts', to: 'types/panel-ui.d.ts' },
  { from: 'tools/plugin-shared/tsconfig.webui.base.json', to: 'tsconfig.webui.base.json' },
  { from: 'tools/plugin-shared/panel-host.d.ts', to: 'types/panel-host.d.ts' },
  { from: 'tools/plugin-shared/node-llrt.d.ts', to: 'types/node-llrt.d.ts' },
  { from: 'tools/plugin-shared/tsconfig.node.base.json', to: 'tsconfig.node.base.json' },
  // 手机号泄露门禁（白名单制，见该文件头注释）
  { from: 'tools/plugin-shared/check-phone-leak.mjs', to: 'scripts/check-phone-leak.mjs' },
];

const check = process.argv.includes('--check');
let bad = 0;

for (const f of FILES) {
  const src = CANDIDATES.map((d) => join(d, f.from)).find((p) => existsSync(p));
  const dest = join(ROOT, f.to);

  if (!src) {
    console.error('找不到面板仓的 ' + f.from);
    console.error('提示：用 PANEL_REPO_DIR=<面板仓路径> 指一下（试过：' + CANDIDATES.join(' / ') + '）');
    process.exit(2);
  }

  const want = readFileSync(src, 'utf8');
  const have = existsSync(dest) ? readFileSync(dest, 'utf8') : '';

  if (check) {
    if (have !== want) {
      console.error('✗ ' + f.to + ' 与面板的 ' + f.from + ' 不一致 —— 跑 `node scripts/sync-types.mjs`');
      bad += 1;
    }
    continue;
  }
  mkdirSync(dirname(dest), { recursive: true });
  writeFileSync(dest, want);
  console.log('ok: ' + f.from + ' → ' + f.to);
}

if (check) {
  if (bad === 0) console.log('ok: 面板契约副本与真源一致（' + FILES.length + ' 份）');
  process.exit(bad === 0 ? 0 : 1);
}
