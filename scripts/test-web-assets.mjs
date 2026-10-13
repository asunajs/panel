#!/usr/bin/env node
/*
  scripts/test-web-assets.mjs —— 主面板前端资源的静态检查。

  为什么需要它：前端没有构建步骤，也就没有编译期把关。一个写错的模块只会
  在浏览器里变成「白屏 + 控制台一行红字」，而面板本身还活着、HTTP 还 200 ——
  这是最难在服务端测试里发现的一类问题。所以这里做四件事：

    ① 文件清单：该有的文件都在（防止「加了新模块忘了在 assets.rs 登记」）
    ② 语法：每个 .js 都能按 ES Module 解析（用 node --check，.mjs 副本）
    ③ 结构：index.html 没有内联 CSS/JS；插件页面只引用允许的路径
    ④ 运行时约束：`plugins/js/*.js` 跑在嵌入式 QuickJS 里，
       不能用 process / require / fetch / setTimeout 这些浏览器或 Node 才有的东西

  用法：node scripts/test-web-assets.mjs
*/

import { readFileSync, existsSync, mkdtempSync, writeFileSync, rmSync, readdirSync } from 'node:fs';
import { execFileSync } from 'node:child_process';
import { join, dirname, resolve, relative } from 'node:path';
import { fileURLToPath } from 'node:url';
import { tmpdir } from 'node:os';

const REPO = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const WEB = join(REPO, 'web');
const PLUGINS = join(REPO, 'plugins');

let passed = 0;
const failures = [];

function check(ok, label, detail = '') {
  if (ok) {
    passed++;
    console.log(`  ✅ ${label}${detail ? ` — ${detail}` : ''}`);
  } else {
    failures.push(label);
    console.log(`  ❌ ${label}${detail ? ` — ${detail}` : ''}`);
  }
}

// ---------------------------------------------------------------- ① 文件清单

const EXPECTED = [
  'index.html',
  'css/reset.css',
  'css/theme.css',
  'css/layout.css',
  'css/components.css',
  'js/app.js',
  'js/api.js',
  'js/editor.js',
  'js/mock.js',
  'js/views/task-list.js',
  'js/views/task-editor.js',
  'js/views/plugin-list.js',
];

const PLUGIN_UI = ['index.html', 'style.css', 'app.js'];

console.log('① 文件清单');
for (const rel of EXPECTED) {
  check(existsSync(join(WEB, rel)), `web/${rel} 存在`);
}
for (const rel of PLUGIN_UI) {
  check(existsSync(join(PLUGINS, 'demo', rel)), `plugins/demo/${rel} 存在`);
}

// assets.rs 的登记表和磁盘实际文件必须一致。
// 这条在 Rust 侧也有一条测试（web_assets_are_complete），这里再查一遍是因为
// 前端开发者通常不会跑 cargo test —— 这个脚本是他们唯一会跑的。
const assetsRs = readFileSync(join(REPO, 'src/assets.rs'), 'utf8');
const registered = [...assetsRs.matchAll(/asset!\("([^"]+)"\)/g)].map((m) => m[1]).sort();
const onDisk = [];
(function walk(dir, base) {
  for (const e of readdirSync(dir, { withFileTypes: true })) {
    const p = join(dir, e.name);
    if (e.isDirectory()) walk(p, base);
    else onDisk.push(relative(base, p).replace(/\\/g, '/'));
  }
})(WEB, WEB);
onDisk.sort();
check(
  JSON.stringify(registered) === JSON.stringify(onDisk),
  'src/assets.rs 的登记表与 web/ 实际文件一致',
  `表里 ${registered.length} 个，磁盘 ${onDisk.length} 个`,
);

// ---------------------------------------------------------------- ② 语法

console.log('\n② ES Module 语法');
const tmp = mkdtempSync(join(tmpdir(), 'webcheck-'));
try {
  const allJs = [...EXPECTED, ...PLUGIN_UI.map((f) => `../plugins/demo/${f}`)]
    .filter((f) => f.endsWith('.js'))
    .map((f) => (f.startsWith('../') ? join(REPO, f.slice(3)) : join(WEB, f)));

  for (const abs of allJs) {
    // node --check 对 .js 按 CommonJS 解析，会把 `import` 报成语法错 ——
    // 复制成 .mjs 才能按 ES Module 解析。
    const copy = join(tmp, `${allJs.indexOf(abs)}.mjs`);
    writeFileSync(copy, readFileSync(abs));
    try {
      execFileSync(process.execPath, ['--check', copy], { stdio: 'pipe' });
      check(true, `${relative(REPO, abs)} 语法正确`);
    } catch (e) {
      check(false, `${relative(REPO, abs)} 语法正确`, (e.stderr || '').toString().trim().split('\n')[0]);
    }
  }
} finally {
  rmSync(tmp, { recursive: true, force: true });
}

// ---------------------------------------------------------------- ③ 结构

console.log('\n③ HTML 结构');
const index = readFileSync(join(WEB, 'index.html'), 'utf8');
for (const css of ['reset', 'theme', 'layout', 'components']) {
  check(index.includes(`href="/css/${css}.css"`), `index.html 引入了 /css/${css}.css`);
}
check(
  /<script type="module" src="\/js\/app\.js">/.test(index),
  'index.html 用 ES Module 方式引入入口',
);
check(!/<style[\s>]/.test(index), 'index.html 里没有内联 <style>');
check(!/\son\w+\s*=/.test(index), 'index.html 里没有内联事件属性（onclick= 之类）');
// 查之前先去掉 HTML 注释：文件顶部的说明里就写着 `<script type="module">`，
// 不去掉的话这条检查永远失败 —— 一个永远失败的检查等于没有检查。
const indexNoComments = index.replace(/<!--[\s\S]*?-->/g, '');
check(
  !/<script(?![^>]*\bsrc=)[^>]*>[\s\S]*?\S[\s\S]*?<\/script>/.test(indexNoComments),
  'index.html 里没有内联脚本体',
);

// 插件页面只该引用主面板的 CSS 变量（/css/*）和自己的相对资源。
const pluginHtml = readFileSync(join(PLUGINS, 'demo/index.html'), 'utf8');
const refs = [...pluginHtml.matchAll(/(?:href|src)="([^"]+)"/g)].map((m) => m[1]);
const badRefs = refs.filter(
  (r) => !r.startsWith('/css/') && !/^[a-z0-9._-]+$/i.test(r) && !r.startsWith('http'),
);
check(badRefs.length === 0, '插件页面只引用主面板 CSS 或自己的相对资源', badRefs.join(', '));
check(
  pluginHtml.includes('href="style.css"'),
  '插件页面引用了自己的 style.css',
);

// 插件页面的 JS 必须调**自己的** API，而不是主面板的
const pluginJs = readFileSync(join(PLUGINS, 'demo/app.js'), 'utf8');
check(
  pluginJs.includes("'/plugins/demo/api'"),
  '插件页面调的是自己的 /plugins/demo/api/*',
);
check(
  !pluginJs.includes("'/api/state'"),
  '插件页面不去调主面板的 /api/state（边界：各管各的）',
);

// ---------------------------------------------------------------- ④ CSS 只用变量

console.log('\n④ CSS 约束');
for (const name of ['layout.css', 'components.css', 'reset.css']) {
  const css = readFileSync(join(WEB, 'css', name), 'utf8');
  // 去掉注释再查，否则注释里举的例子会被误判
  const body = css.replace(/\/\*[\s\S]*?\*\//g, '');
  const hex = [...body.matchAll(/#[0-9a-fA-F]{3,8}\b/g)].map((m) => m[0]);
  check(
    hex.length === 0,
    `css/${name} 里没有硬编码色值（颜色只走 theme.css 的变量）`,
    hex.slice(0, 4).join(' '),
  );
}
// 徽章必须「要么完整显示、要么换行」，绝不能被 flex 压缩。
//
// 这条守的是一个**用户实测出来的回归**：插件管理页的卡片里，名字 + 两个徽章 +
// 两个按钮在 280px 的列宽里放不下，浏览器就把 `.badge` 压成了竖排的
// 「已/加/载」（中文能任意断行，所以它比按钮「好压」）。这类问题静态检查
// 本来抓不到，但成因很具体、修法也很具体，所以值得钉一条。
const componentsCss = readFileSync(join(WEB, 'css/components.css'), 'utf8');
const badgeRule = componentsCss.match(/\n\.badge \{([\s\S]*?)\n\}/);
check(!!badgeRule, '找得到 .badge 规则');
if (badgeRule) {
  const body = badgeRule[1];
  check(/flex:\s*none/.test(body), '.badge 设了 flex:none（不会被压缩）');
  check(/white-space:\s*nowrap/.test(body), '.badge 设了 white-space:nowrap（不会竖排断行）');
}
// 卡片头也要允许换行，否则窄屏下会去压缩子项而不是折行
const headRule = componentsCss.match(/\n\.card-head \{([\s\S]*?)\n\}/);
check(
  !!headRule && /flex-wrap:\s*wrap/.test(headRule[1]),
  '.card-head 允许换行（窄屏不压缩子项）',
);

// 主题持久化：只有**用户主动切换**才写 localStorage。
//
// 这条守的是一个真实踩到的坑：上一版在初始化时把「跟随系统偏好」算出来的结果
// 写回了 localStorage，于是那个值看起来像用户的选择。后来把默认主题改成深色，
// 所有加载过旧页面的人却都卡在旧值上 —— 默认值改了但谁都不生效。
const appJs = readFileSync(join(WEB, 'js/app.js'), 'utf8');
const themeCalls = [...appJs.matchAll(/applyTheme\(([^)]*)\)/g)].map((m) => m[1]);
const persisting = themeCalls.filter((args) => args.includes('true'));
check(
  themeCalls.length > 0 && persisting.length === 2,
  '只有用户主动切换主题时才写 localStorage（初始化不写）',
  `${themeCalls.length} 处调用，其中 ${persisting.length} 处带 persist`,
);

// 前后端字段名契约。
//
// 这条守的是一个真实踩到的静默 bug：清理失效任务的接口返回 `{"jobs":[...]}`，
// 而前端按语义猜成了 `res.invalid` —— 结果**有失效任务时界面永远显示「没有失效任务」**。
// 静态检查、冒烟（测的是 API）、单元测试（测的是后端）全都不会报。
// 跨边界的字段名对不上是这个项目里最容易发生、又最难发现的一类错。
{
  const webRs = readFileSync(join(REPO, 'src/web.rs'), 'utf8');
  const fn = webRs.match(/async fn cleanup_jobs[\s\S]*?\n\}/);
  const keys = fn
    ? [...fn[0].matchAll(/json!\(\{([^}]*)\}/g)].flatMap((m) =>
        [...m[1].matchAll(/"(\w+)":/g)].map((x) => x[1]),
      )
    : [];
  check(keys.includes('jobs'), '清理接口（src/web.rs）返回的字段里有 jobs', keys.join(','));

  const taskListJs = readFileSync(join(WEB, 'js/views/task-list.js'), 'utf8');
  const read = taskListJs.match(/const invalid = res\.(\w+)/);
  check(
    !!read && keys.includes(read[1]),
    '前端读的清理结果字段名与后端返回一致',
    `前端读 res.${read ? read[1] : '?'}，后端返回 ${keys.join('/')}`,
  );
}

const pluginCss = readFileSync(join(PLUGINS, 'demo/style.css'), 'utf8');
check(
  [...pluginCss.replace(/\/\*[\s\S]*?\*\//g, '').matchAll(/#[0-9a-fA-F]{3,8}\b/g)].length === 0,
  '插件样式也复用主面板变量（没有自己写死颜色）',
);
check(
  pluginCss.includes('var(--'),
  '插件样式确实用上了主面板的 CSS 变量',
);

// ---------------------------------------------------------------- ⑤ 安全相关的静态约束
//
// 这几条是从旧版页面的前端套件搬过来的（那些用例检查的是服务端渲染的 HTML 常量，
// 现在新版页面是外置文件，但「登录页洁癖」和「写请求带护栏头」这两件事一样重要）。
console.log('\n⑤ 安全约束');

const apiJs = readFileSync(join(WEB, 'js/api.js'), 'utf8');
check(apiJs.includes("'x-panel': '1'") || apiJs.includes('x-panel'), '写请求会带 x-panel 护栏头（防 CSRF）');

// 任务卡片用**事件委托**（在容器上监听 + closest 找按钮），而不是给每张卡片的每个
// 按钮单独绑监听 —— 卡片是重新渲染出来的，逐按钮绑必然漏掉新渲染的那批。
// 旧版页面正是「按钮 DOM 和后端接口都写好了、唯独漏了 click 监听」，这条守的就是那个坑。
const taskListJs = readFileSync(join(WEB, 'js/views/task-list.js'), 'utf8');
check(/addEventListener\('click'/.test(taskListJs) && /closest\('button/.test(taskListJs),
  '任务卡片用事件委托处理按钮点击');

const webRs2 = readFileSync(join(REPO, 'src/web.rs'), 'utf8');
const loginPage = webRs2.match(/const LOGIN_PAGE: &str = r##"([\s\S]*?)"##;/);
check(!!loginPage, '在 src/web.rs 里找得到 LOGIN_PAGE');
if (loginPage) {
  const lp = loginPage[1];
  check(lp.includes('action="/api/login"'), '登录页表单指向 /api/login');
  check(/name="token"/.test(lp), '登录页有令牌输入框');
  check(/type="password"/.test(lp), '令牌输入框是密码类型（不明文显示）');
  check(/name="next"/.test(lp), '登录页带 next（登录完回原页面）');
  // 未认证时不该有任何脚本：登录页去拉 /api/state 只会拿到 401
  check(!lp.includes('<script'), '登录页没有任何 JS');
  check(!lp.includes('__JOB_ROWS__'), '登录页不渲染任何面板数据');
}

// 主面板不该在 HTML 里写死内联脚本（新版是外置模块；旧版是内联的，已删）
const indexHtml = readFileSync(join(WEB, 'index.html'), 'utf8');
check(
  !/<script(?![^>]*\bsrc=)[^>]*>[\s\S]*?\S[\s\S]*?<\/script>/.test(
    indexHtml.replace(/<!--[\s\S]*?-->/g, ''),
  ),
  '主面板没有内联脚本（全部走外置 ES Module）',
);

// ---------------------------------------------------------------- ⑥ QuickJS 约束

console.log('\n⑥ 嵌入式 QuickJS 的约束（plugins/js/*.js）');
// 这些名字在嵌入式 QuickJS 里**不存在**（实测见 docs/host-api.md §4.1）。
// 插件里写了它们，加载期就抛 ReferenceError —— 而同一份文件在 Node 里
// 调试时是好的，于是变成「本地能跑、上机就炸」。
// 用**标识符边界**匹配，不是 substring：
//   - `http_fetch(` 里含有 `fetch(`，用 substring 查会把正确的 host-api 调用报成违规
//   - `typeof process !== 'undefined'` 是合法的特性探测（host-api-demo.js 就这么写），
//     只有真正**使用**这个全局才算违规
const FORBIDDEN = [
  ['process', /(?<![.\w$])process\b/],
  ['require', /(?<![.\w$])require\s*\(/],
  ['module.exports', /(?<![.\w$])module\.exports/],
  ['__dirname', /(?<![.\w$])__dirname\b/],
  ['setTimeout', /(?<![.\w$])setTimeout\s*\(/],
  ['setInterval', /(?<![.\w$])setInterval\s*\(/],
  ['setImmediate', /(?<![.\w$])setImmediate\s*\(/],
  ['fetch', /(?<![.\w$])fetch\s*\(/],
  ['new URL', /(?<![.\w$])new\s+URL\s*\(/],
  ['TextEncoder', /(?<![.\w$])TextEncoder\b/],
  ['TextDecoder', /(?<![.\w$])TextDecoder\b/],
  ['structuredClone', /(?<![.\w$])structuredClone\b/],
  ['localStorage', /(?<![.\w$])localStorage\b/],
  ['document', /(?<![.\w$])document\b/],
  ['window', /(?<![.\w$])window\b/],
];

for (const f of readdirSync(join(PLUGINS, 'js'))) {
  if (!f.endsWith('.js')) continue;
  const src = readFileSync(join(PLUGINS, 'js', f), 'utf8')
    .replace(/\/\*[\s\S]*?\*\//g, '')
    .replace(/\/\/[^\n]*/g, '');

  const hits = FORBIDDEN.filter(([name, re]) => {
    if (!re.test(src)) return false;
    // 特性探测（typeof X）是允许的：插件本来就要能同时跑在 Node 和嵌入式宿主里
    const ident = name.split(/[.\s(]/)[0];
    return !new RegExp(`typeof\\s+${ident}\\b`).test(src);
  }).map(([name]) => name);

  check(hits.length === 0, `plugins/js/${f} 没用嵌入式宿主没有的全局`, hits.join(', '));
}

// ---------------------------------------------------------------- 结果

console.log();
if (failures.length) {
  console.log(`❌ 前端资源检查未通过：${failures.length} 项失败（通过 ${passed} 项）`);
  for (const f of failures) console.log(`   - ${f}`);
  process.exit(1);
}
console.log(`✅ 前端资源检查全部通过（${passed} 项）`);
