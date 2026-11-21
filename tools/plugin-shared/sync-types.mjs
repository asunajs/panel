// 把**面板仓**的 `web/ui.d.ts` 同步到本仓 `types/panel-ui.d.ts`。
//
// 为什么要有这一步：面板给插件页的 JS 面（`window.PanelUI` + `$()` 拿到的控件成员）
// 是**面板的契约**，真源在 `panel-llrt/web/ui.d.ts`。插件不该抄一份自由发挥，
// 但也不能在 typecheck 时依赖同级目录存在（clone 下来就没了）。
// 折中：真源留面板，本仓留一份**逐字节相同**的副本，`--check` 用来抓「面板改了、这边没跟」。
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

const src = CANDIDATES.map((d) => join(d, 'web/ui.d.ts')).find((p) => existsSync(p));
const check = process.argv.includes('--check');

if (!src) {
  console.error('找不到面板仓的 web/ui.d.ts（试过：' + CANDIDATES.join(' / ') + '）');
  console.error('提示：用 PANEL_REPO_DIR=<面板仓路径> 指一下。');
  process.exit(2);
}

const want = readFileSync(src, 'utf8');
const have = existsSync(DEST) ? readFileSync(DEST, 'utf8') : '';

if (check) {
  if (have !== want) {
    console.error('types/panel-ui.d.ts 与面板的 web/ui.d.ts 不一致：' + src);
    console.error('跑 `node scripts/sync-types.mjs` 同步。');
    process.exit(1);
  }
  console.log('ok: types/panel-ui.d.ts 与面板一致');
  process.exit(0);
}

mkdirSync(dirname(DEST), { recursive: true });
writeFileSync(DEST, want);
console.log('ok: 已同步 ' + src + ' → types/panel-ui.d.ts');
