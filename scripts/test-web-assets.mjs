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
import { fileURLToPath, pathToFileURL } from 'node:url';
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
  'js/prefs.js',
  'js/editor.js',
  'js/confirm.js',
  'js/mock.js',
  'js/views/task-list.js',
  'js/views/task-editor.js',
  'js/views/script-list.js',
  'js/views/script-editor.js',
  'js/views/plugin-list.js',
  'js/views/market.js',
  'js/views/plugin-source.js',
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
// `web/` 是「要发出去的前端」，登记表里的东西都会被编进二进制、对外提供。
// **有一个例外**：vendor 的 LICENSE —— 用户口径是"许可留在项目里就行，
// 没必要编进二进制、也没必要对外提供"，所以它躺在仓库里（就在 fork 旁边）
// 但不进登记表。下面这条例外清单把这件事写死，同时**校验它确实还在仓库里**
// （不然文件被删了、例外还留着，就成了"静默豁免"）。
const NOT_SHIPPED = ['vendor/puredashboard/LICENSE'];
const onDisk = [];
(function walk(dir, base) {
  for (const e of readdirSync(dir, { withFileTypes: true })) {
    const p = join(dir, e.name);
    if (e.isDirectory()) walk(p, base);
    else {
      const rel = relative(base, p).replace(/\\/g, '/');
      if (!NOT_SHIPPED.includes(rel)) onDisk.push(rel);
    }
  }
})(WEB, WEB);
onDisk.sort();
check(
  JSON.stringify(registered) === JSON.stringify(onDisk),
  'src/assets.rs 的登记表与 web/ 实际文件一致',
  `表里 ${registered.length} 个，磁盘 ${onDisk.length} 个`,
);
for (const rel of NOT_SHIPPED) {
  check(
    existsSync(join(WEB, rel)),
    `例外清单里的 ${rel} 还在仓库里（它是"不内嵌"，不是"可以删"）`,
  );
}

// ---------------------------------------------------------------- ② 语法

console.log('\n② ES Module 语法');
const tmp = mkdtempSync(join(tmpdir(), 'webcheck-'));
try {
  // 第三方 vendor 的 JS 也要过语法检查：上游文件坏了、或者我们升级时拷漏了，
// 症状都是「浏览器里某个组件不出现」，而这条检查能当场说出来是哪个文件。
const vendorJs = [];
(function walkJs(dir, base) {
  if (!existsSync(dir)) return;
  for (const e of readdirSync(dir, { withFileTypes: true })) {
    const p = join(dir, e.name);
    if (e.isDirectory()) walkJs(p, base);
    else if (e.name.endsWith('.js')) vendorJs.push(relative(base, p).replace(/\\/g, '/'));
  }
})(join(WEB, 'vendor'), WEB);
const allJs = [...EXPECTED, ...PLUGIN_UI.map((f) => `../plugins/demo/${f}`), ...vendorJs]
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
// select 的箭头画在 background 上（见 components.css 的 `.select`），必须**垂直居中**。
//
// 为什么钉这条：`background-position` 的百分比是**对齐点**，不是左上角 ——
// 写 `50%` 就是"背景中心对容器中心"（已居中）；写成 `calc(50% - 3px)`
// （按左上角去减半高）会整体**偏高 3px**。我第一版就是这么写的，
// 静态检查、单测全绿，用户一眼看出来「箭头没上下居中」。
// 所以把 y 钉成 50%/center，不许再加减。
const selectRule = componentsCss.match(/\n\.select \{([\s\S]*?)\n\}/);
check(!!selectRule, '找得到 .select 规则');
if (selectRule) {
  check(
    /appearance:\s*none/.test(selectRule[1]),
    '.select 关掉了原生外观（不然浏览器会再画一个自己的箭头）',
  );
  const bgPos = selectRule[1].match(/background-position:\s*([\s\S]*?);/);
  check(!!bgPos, '.select 里写了 background-position（箭头靠它定位）');
  if (bgPos) {
    const layers = bgPos[1]
      .split(',')
      .map((s) => s.trim().split(/\s+/))
      .filter((l) => l.length);
    const bad = layers.filter((l) => !['50%', 'center'].includes(l[l.length - 1]));
    check(
      bad.length === 0,
      '.select 的箭头垂直居中（background-position 的 y 必须是 50%/center）',
      bad.map((l) => l.join(' ')).join(' ; '),
    );
  }
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

// 插件引擎 / 脚本任务：接口名、路由、按钮与「Python 只剩脚本任务」这条边界。
//
// 这一组守的是跨边界的**命名**：前端拼的 `/api/scripts/jobs`、`data-engine`、
// `#/scripts/new` 任何一处改了而另一处没跟上，静态检查、后端单测都不会报，
// 只有点下去才发现。脚本任务与插件引擎是这一轮新增的面，所以单独钉住。
{
  const apiSrc2 = readFileSync(join(REPO, 'web/js/api.js'), 'utf8');
  for (const fn of ['setPluginRuntime', 'listScripts', 'createScriptJob', 'editScriptJob']) {
    check(new RegExp(`export function ${fn}\\b`).test(apiSrc2), `api.js 导出了 ${fn}`);
  }
  check(apiSrc2.includes("'/api/scripts'"), 'api.js 的 listScripts 调 /api/scripts');
  check(
    apiSrc2.includes("'/api/scripts/jobs'"),
    'api.js 的 createScriptJob 调 POST /api/scripts/jobs',
  );
  check(
    apiSrc2.includes('/api/scripts/jobs/${encodeURIComponent(name)}/edit'),
    'api.js 的 editScriptJob 调 POST /api/scripts/jobs/{name}/edit',
  );
  check(
    apiSrc2.includes("method: 'POST'") && apiSrc2.includes('body'),
    '脚本任务写接口是 POST + JSON body',
  );

  // 测试运行只剩 js：`runSnippet('py', …)` / `/api/test/py` / `runPy` 都不许回来。
  check(!/runPy\b/.test(apiSrc2), 'api.js 里没有 runPy（Python 不是插件语言）');
  check(!/\/api\/test\/py/.test(apiSrc2), 'api.js 里没有 /api/test/py');
  check(!/\/api\/test\/\$\{lang\}/.test(apiSrc2), 'runSnippet 不再拼 /api/test/{lang}');
  check(
    /lang !== 'js'/.test(apiSrc2) && apiSrc2.includes("'/api/test/js'"),
    'runSnippet 只接受 js 并调 /api/test/js',
  );
  // 后端 TestRunReq 没有 engine 字段；前端不要发一个会被静默忽略的参数。
  check(!apiSrc2.includes('body.engine'), 'runSnippet 不发送后端未实现的 engine 字段');

  // 插件列表：引擎徽章 + 下拉框 + 自定义解释器路径，且只列三种插件引擎。
  const pluginListSrc = readFileSync(join(REPO, 'web/js/views/plugin-list.js'), 'utf8');
  check(pluginListSrc.includes('setPluginRuntime'), 'plugin-list.js 导入并调用 setPluginRuntime');
  check(pluginListSrc.includes('data-engine'), 'plugin-list.js 有引擎下拉框（data-engine）');
  check(pluginListSrc.includes('data-runtime-apply'), 'plugin-list.js 有「应用引擎」按钮');
  check(
    /ENGINE_KINDS\s*=\s*\[\s*'quickjs'\s*,\s*'node'\s*,\s*'bun'\s*\]/.test(pluginListSrc),
    '插件引擎只列 quickjs / node / bun（python / sh 是脚本运行时）',
  );
  check(pluginListSrc.includes('runtimes'), 'plugin-list.js 读 /api/state 的 runtimes 做可用性');
  check(pluginListSrc.includes('unavailable'), '不可用的引擎选项会被禁用并带原因');
  check(!pluginListSrc.includes('badge lang js'), '插件卡片不再重复挂 JavaScript 徽章');
  check(!pluginListSrc.includes('没有自带配置页'), '没有配置页的插件卡片不再写说明');

  // 插件商城：入口只在插件列表页，不占侧边栏导航。
  const marketSrc = readFileSync(join(REPO, 'web/js/views/market.js'), 'utf8');
  const appSrcMarket = readFileSync(join(WEB, 'js/app.js'), 'utf8');
  const apiSrcMarket = readFileSync(join(WEB, 'js/api.js'), 'utf8');
  check(pluginListSrc.includes('href="#/market"'), '插件列表页有「插件商城」跳转入口');
  check(!index.includes('href="#/market"'), '插件商城不作为侧边栏导航项');
  check(appSrcMarket.includes("market: '插件商城'"), 'app.js 有插件商城标题');
  check(appSrcMarket.includes("view === 'market'"), 'app.js 有 #/market 路由分支');
  check(marketSrc.includes('data-install') && marketSrc.includes('installMarketPlugin'),
    'market.js 有安装按钮与安装调用');
  check(marketSrc.includes('data.proxy'), 'market.js 显示公开库代理状态');
  check(apiSrcMarket.includes('/api/market') && apiSrcMarket.includes('installMarketPlugin'),
    'api.js 有商城目录与安装接口');
  check(marketSrc.includes('source_type === \'ts\'') || marketSrc.includes("source_type === 'ts'"),
    'market.js 区分单文件 TS');
  const catalog = JSON.parse(readFileSync(join(REPO, 'marketplace/catalog.json'), 'utf8'));
  check(catalog.schema === 1, '商城目录 schema=1');
  const mcloud = (catalog.plugins || []).find((p) => p.id === 'mcloud-plugin');
  check(!!mcloud && mcloud.repo === 'asunajs/mcloud-plugin' && mcloud.type === 'release',
    '官方目录收录 mcloud release');
  check(
    !!mcloud && (mcloud.provides || []).includes('mcloud-daily'),
    'mcloud 条目声明了 provides',
  );
  const quark = (catalog.plugins || []).find((p) => p.id === 'quark');
  check(
    !!quark &&
      quark.repo === 'asunajs/panel-single-plugins' &&
      quark.type === 'file' &&
      quark.path === 'quark/quark.ts',
    '官方目录收录单文件 TS 插件 quark',
  );

  // 执行引擎设置默认隐藏，只有「设置」里开启才显示（用户口径）。
  //
  // 这里不只查关键字：直接跑一遍 prefs 模块，确认默认值与开关都成立。
  // 关键字检查只能证明"写了"，跑一遍才能证明"默认是隐藏的"。
  const prefsSrc = readFileSync(join(WEB, 'js/prefs.js'), 'utf8');
  check(
    /v === null \? fallback : v === '1'/.test(prefsSrc),
    'prefs.js 没存过值时按默认（隐藏）走',
  );
  check(
    pluginListSrc.includes('showPluginEngineUi'),
    'plugin-list.js 按本地偏好决定是否渲染执行引擎设置',
  );
  check(
    /runtimeRow\s*=\s*showEngine\s*\?/.test(pluginListSrc),
    '关闭时执行引擎设置整块不渲染（不是用 CSS 藏起来）',
  );
  // 设置页的实现从 app.js 挪到了 views/settings.js（分 tab 之后），这里跟着挪。
  const settingsSrc = readFileSync(join(WEB, 'js/views/settings.js'), 'utf8');
  check(settingsSrc.includes('s-engine-ui'), '设置页有「显示执行引擎设置」开关');
  check(settingsSrc.includes('setPluginEngineUi'), '设置页的开关会写回本地偏好');
  // 字面 Markdown 由下面的「界面文案」扫描统一管（它 walk 整个 web/js，已经包含这个文件）

  {
    const store = new Map();
    const prevLocalStorage = globalThis.localStorage;
    globalThis.localStorage = {
      getItem: (k) => (store.has(k) ? store.get(k) : null),
      setItem: (k, v) => store.set(k, String(v)),
      removeItem: (k) => store.delete(k),
    };
    try {
      const prefs = await import(
        `${pathToFileURL(join(WEB, 'js/prefs.js')).href}?t=${Date.now()}`
      );
      check(prefs.showPluginEngineUi() === false, '执行引擎设置默认隐藏');
      prefs.setPluginEngineUi(true);
      check(prefs.showPluginEngineUi() === true, '设置里开启后执行引擎设置显示');
      prefs.setPluginEngineUi(false);
      check(prefs.showPluginEngineUi() === false, '设置里关闭后重新隐藏');
    } finally {
      if (prevLocalStorage === undefined) delete globalThis.localStorage;
      else globalThis.localStorage = prevLocalStorage;
    }
  }

  // 插件任务页只放插件任务：脚本任务全部在脚本任务页，一个页面不混两类。
  const taskListSrc2 = readFileSync(join(REPO, 'web/js/views/task-list.js'), 'utf8');
  check(taskListSrc2.includes("j.kind !== 'script'"),
    'task-list.js 过滤掉 kind=script（插件任务页不混脚本任务）');
  check(!taskListSrc2.includes("ctx.navigate('#/scripts/new')"),
    '插件任务页没有「新建脚本任务」入口');
  check(!taskListSrc2.includes('#/scripts'),
    '插件任务页不引用脚本任务路由');
  check(taskListSrc2.includes('插件任务（'), '插件任务页标题明确写「插件任务」');
  check(taskListSrc2.includes('data-kind="plugin"'), '插件任务卡片带 data-kind=plugin');
  check(!taskListSrc2.includes('badge lang js'), '插件任务卡片不再重复挂 JavaScript 徽章');
  check(
    taskListSrc2.includes('task-summary') && taskListSrc2.includes('task-details'),
    '插件任务卡片默认只露摘要，详情折叠',
  );
  check(taskListSrc2.includes('source:'), 'task-list.js 的 fromJob 保留 source');

  // 脚本总览与编辑器：调对接口、用既有任务接口，不自己发明第三套。
  const scriptListSrc = readFileSync(join(REPO, 'web/js/views/script-list.js'), 'utf8');
  check(scriptListSrc.includes('listScripts'), 'script-list.js 用 listScripts');
  check(scriptListSrc.includes("'__script__'"), 'script-list.js 用保留插件名 __script__');
  check(scriptListSrc.includes('TASKS_DIR'), 'script-list.js 显示 TASKS_DIR 提示');
  check(scriptListSrc.includes('fileCard') && scriptListSrc.includes('panelJobCard'),
    'script-list.js 同时渲染目录脚本与在面板创建的脚本');
  check(scriptListSrc.includes('改运行时间'), '目录脚本可以改运行时间');
  check(scriptListSrc.includes('editJob(SCRIPT_PLUGIN'), '改运行时间走已有的 editJob');
  check(scriptListSrc.includes('目录脚本（') && scriptListSrc.includes('面板创建的脚本（'),
    '脚本任务页把目录脚本 / 面板创建的脚本分成两段');
  check(scriptListSrc.includes("rt === 'python3'") && scriptListSrc.includes("rt === 'sh'"),
    '脚本任务页映射了 python3 / sh 运行时');
  check(scriptListSrc.includes('任务目录') && scriptListSrc.includes('在面板创建'),
    '脚本任务页用「任务目录 / 在面板创建」区分来源');
  check(
    scriptListSrc.includes('task-summary') && scriptListSrc.includes('task-details'),
    '脚本任务卡片默认只露摘要，详情折叠',
  );
  check(!scriptListSrc.includes('#/tasks'), '脚本任务页不跳到插件任务页');
  check(!taskListSrc2.includes("'__script__'"), '插件任务页不出现内部脚本插件名');

  // 运行记录横幅：脚本任务不能露内部名 `__script__`，也不能链回插件任务页。
  const logViewSrc = readFileSync(join(REPO, 'web/js/views/log-view.js'), 'utf8');
  check(logViewSrc.includes("run.plugin === '__script__'"), '运行记录认出脚本任务');
  check(logViewSrc.includes("'看脚本任务'") && logViewSrc.includes("'#/scripts'"),
    '脚本运行记录链到脚本任务页');
  check(logViewSrc.includes('function pluginLabel') && logViewSrc.includes("'脚本任务'"),
    '日志筛选不露内部脚本插件名');
  check(
    logViewSrc.includes('/api/logs/history') && logViewSrc.includes('historyGen'),
    '运行视图会从 JSONL 读历史日志（重启后仍能查看）',
  );

  const scriptEditorSrc = readFileSync(join(REPO, 'web/js/views/script-editor.js'), 'utf8');
  check(scriptEditorSrc.includes('createScriptJob'), 'script-editor.js 用 createScriptJob');
  check(scriptEditorSrc.includes('editScriptJob'), 'script-editor.js 用 editScriptJob');
  check(scriptEditorSrc.includes("editJob('__script__'"),
    '目录任务的调度覆盖走已有的 editJob');
  check(scriptEditorSrc.includes("'python3'") && scriptEditorSrc.includes("'sh'"),
    '脚本编辑器支持 node / bun / python3 / sh 运行时');
  check(!/language|SAMPLE_CODE|Python 高亮/.test(scriptEditorSrc),
    '脚本编辑器不冒充代码编辑器（没有语言选择 / Python 高亮）');

  // 路由与导航：新页面必须能从 hash 路由和侧边栏到达。
  const appSrc = readFileSync(join(REPO, 'web/js/app.js'), 'utf8');
  check(appSrc.includes("from './views/script-list.js'"), 'app.js 导入 script-list 视图');
  check(appSrc.includes("from './views/script-editor.js'"), 'app.js 导入 script-editor 视图');
  check(appSrc.includes("parts[0] === 'scripts'"), 'app.js 注册了 #/scripts 路由');
  check(appSrc.includes("parts[1] === 'new'") && appSrc.includes("parts[1] === 'edit'"),
    'app.js 注册了 #/scripts/new 与 #/scripts/edit/{name}');
  check(appSrc.includes("view === 'script-editor'"), 'app.js 渲染 script-editor 视图');
  const indexSrc = readFileSync(join(WEB, 'index.html'), 'utf8');
  check(indexSrc.includes('href="#/scripts"') && indexSrc.includes('data-view="scripts"'),
    '侧边栏有「脚本任务」入口');
  check(/<span>插件任务<\/span>/.test(indexSrc), '侧边栏「全部任务」改名为「插件任务」');
  check(/<span>脚本任务<\/span>/.test(indexSrc), '侧边栏「脚本文件」改名为「脚本任务」');
  check(appSrc.includes("tasks: '插件任务'"), '页面标题：插件任务');
  check(appSrc.includes("scripts: '脚本任务'"), '页面标题：脚本任务');
  check(appSrc.includes('>插件任务</span>') && appSrc.includes('>脚本任务</span>'),
    '仪表盘把插件任务与脚本任务分开列表');
  check(appSrc.includes("j.kind !== 'script'") && appSrc.includes("j.kind === 'script'"),
    '仪表盘按 kind 拆分两类任务');
  check(!appSrc.includes('任务概览'), '仪表盘不再用一张混合的「任务概览」表');

  // 装配完整性：assets.rs 必须登记这两个新视图（否则生产模式 404）。
  const assetsSrc = readFileSync(join(REPO, 'src/assets.rs'), 'utf8');
  for (const rel of ['js/views/script-list.js', 'js/views/script-editor.js']) {
    check(assetsSrc.includes(`asset!("${rel}")`), `src/assets.rs 登记了 ${rel}`);
  }

  // Python 只作为脚本运行时存在：插件/任务编辑器里不该再出现「Python 插件」那条路。
  for (const f of ['web/js/views/task-editor.js', 'web/js/views/plugin-source.js']) {
    const src = readFileSync(join(REPO, f), 'utf8');
    check(!/runPy|\/api\/test\/py|\bSAMPLE_CODE\[/.test(src),
      `${f} 里没有 Python 试跑 / Python 示例代码`);
  }
  check(
    !readFileSync(join(REPO, 'web/js/views/task-editor.js'), 'utf8').includes('badge lang js'),
    '任务编辑器不再重复挂 JavaScript 徽章',
  );
  const mockSrc = readFileSync(join(REPO, 'web/js/mock.js'), 'utf8');
  check(!/SAMPLE_CODE\.py|lang: 'py'|from host_api/.test(mockSrc),
    'mock.js 里没有 Python 示例任务 / 示例代码');
  check(!/setLanguage|KEYWORDS\.py/.test(readFileSync(join(REPO, 'web/js/editor.js'), 'utf8')),
    'editor.js 没有死掉的 Python 高亮分支');
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

// 卡片头部布局的三条硬约束（都有实测教训，见 components.css 的注释）
{
  // **先剥掉注释**再查属性：这些规则的说明本身就在注释里提到 `margin-left: auto`
  // （解释为什么不能用它），不剥的话检查会被自己的说明文字绊倒 —— 实测就是这么红的。
  const css = componentsCss.replace(/\/\*[\s\S]*?\*\//g, '');
  const head = css.match(/\.card-head \{([\s\S]*?)\}/);
  check(!!head && /justify-content:\s*space-between/.test(head[1]),
    '.card-head 用 space-between 分开「标题组」和「按钮组」');
  const acts = css.match(/\.card-actions \{([\s\S]*?)\}/);
  check(!!acts && !/margin-left:\s*auto/.test(acts[1]),
    '.card-actions 不写 margin-left:auto（换行后会飘在卡片中间）');
  check(/\.card-head-main\s*\{/.test(css),
    '有 .card-head-main：标题+徽章必须包一层，否则会被 space-between 摊开');
  // 两处卡片都要用这个包装层
  for (const f of ['web/js/views/task-list.js', 'web/js/views/plugin-list.js']) {
    check(readFileSync(join(REPO, f), 'utf8').includes('card-head-main'),
      `${f} 的卡片头部用了 .card-head-main`);
  }
  // 那张重复的提示不该再回到卡片里
  check(!readFileSync(join(REPO, 'web/js/views/task-list.js'), 'utf8')
    .includes('表达式由插件代码声明</span>'),
    '卡片里没有重复的「表达式由插件代码声明」可见文案');
}

// ---------------------------------------------------------------- 视图必须导入它用到的接口
//
// 这条检查钉的是一个真实的静默 bug：`task-list.js` 里新加的「撤销覆盖」按钮调了
// `editJob`，但那个文件**没有 import 它** —— 点击时抛 `ReferenceError`，
// 被 async 事件处理函数吞掉：没有报错、没有提示、什么都没发生。
// 界面测试只点按钮是发现不了的（除非同时断言服务端状态）。
{
  const apiSrc = readFileSync(join(REPO, 'web/js/api.js'), 'utf8');
  const exported = new Set(
    [...apiSrc.matchAll(/export\s+(?:async\s+)?function\s+(\w+)/g)].map((m) => m[1]),
  );
  check(exported.size > 5, `api.js 导出了 ${exported.size} 个函数`);

  const viewFiles = EXPECTED.filter((f) => f.startsWith('js/views/'));
  check(viewFiles.length >= 3, `检查 ${viewFiles.length} 个视图文件`);
  for (const f of viewFiles) {
    const src = readFileSync(join(WEB, f), 'utf8').replace(/\/\*[\s\S]*?\*\//g, '').replace(/^\s*\/\/.*$/gm, '');
    // 用 `[^}]*` 而不是 `[\s\S]*?`：后者会从**上一个** `import {` 开始匹配、
    // 把 `editor.js` 的导入也吞进来，于是捕获到的名字带着 "import { " 前缀，
    // 检查器就误报「少 import: getState」（实测踩到 —— 检查器自己也写错过）。
    const importedSet = new Set();
    for (const m of src.matchAll(/import\s*\{([^}]*)\}\s*from\s*'\.\.\/api\.js'/g)) {
      for (const part of m[1].split(',')) {
        const name = part.trim();
        if (name) importedSet.add(name);
      }
    }
    const imported = importedSet;
    const missing = [...exported].filter(
      (name) => !imported.has(name) && new RegExp(`(?<![\\w.])${name}\\s*\\(`).test(src),
    );
    check(
      missing.length === 0,
      `${f} 用到的接口都 import 了`,
      missing.length ? `少 import: ${missing.join(', ')}` : '',
    );
  }
}

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

// 插在标题和列表之间的容器：**空的时候不能留空隙**。
// 直接给 margin-bottom 的话，没点「清理失效任务」时页面上会凭空多一块空白。
// 用 :not(:empty) 才对 —— 这条同时也是用户指出的「结果卡片贴着任务卡片」的修法。
check(
  /\.stack:not\(:empty\)/.test(componentsCss),
  '插在中间的结果容器用 :not(:empty) 控制间距（空的时不占位置）',
);

const webRs2 = readFileSync(join(REPO, 'src/web.rs'), 'utf8');
const loginPage = webRs2.match(/const LOGIN_PAGE: &str = r##"([\s\S]*?)"##;/);
check(!!loginPage, '在 src/web.rs 里找得到 LOGIN_PAGE');
if (loginPage) {
  const lp = loginPage[1];
  check(lp.includes('action="/api/login"'), '登录页表单指向 /api/login');
  check(/name="token"/.test(lp), '登录页有口令输入框');
  check(/type="password"/.test(lp), '口令输入框是密码类型（不明文显示）');
  check(/name="next"/.test(lp), '登录页带 next（登录完回原页面）');

  // ⚠ 第二十一轮起登录页**必须有**一段 JS：口令要在浏览器里先算成摘要，
  // 明文不许离开浏览器（见 docs/security.md §21）。
  // 但原来那条"未认证页面不许碰面板数据"的约束仍然成立 —— 只是判据从
  // 「有没有 JS」换成了「这段 JS 有没有去 fetch 面板接口」。
  check(lp.includes('<script'), '登录页带口令摘要脚本');
  check(lp.includes('crypto.subtle'), '摘要优先走 WebCrypto');
  check(/function sha256\(/.test(lp), '带纯 JS 的 SHA-256 兜底（http:// 局域网不是安全上下文）');
  check(lp.includes('preventDefault'), '提交前先拦住原生提交');
  check(lp.includes('<noscript>'), '没有 JS 时明确告知（否则是个"没反应的按钮"）');

  // 蜜罐：一个藏起来的「账号」框。它的全部价值建立在「人看不见它」上，
  // 而这一点太容易被一次好心的改动弄丢（把 display:none 删了、或给它加了可见的提示）。
  // 丢了之后不是报错，是**真人开始被自己的面板封在门外** —— 所以静态钉住。
  const form = lp.slice(lp.indexOf('<form'), lp.indexOf('</form>'));
  check(/name="username"/.test(form), '蜜罐字段在 <form> 里（在外面就不会随提交发出去，等于没有）');
  check(/\.acct \{ display: none; \}/.test(lp), '蜜罐被 CSS 藏住');
  check(/tabindex="-1"/.test(form), '蜜罐不在 tab 序里（键盘用户不会停在一个看不见的框上）');
  check(/type="text"/.test(form), '蜜罐是 type=text：像真账号框，才钓得到批量脚本');
  // 发给浏览器的那一串里不许有解释 —— HTML / CSS 注释同样是攻击能读的文本
  check(
    !/蜜罐|honeypot|trap/.test(lp),
    '页面里没有泄露蜜罐用途的字样（类名与注释都算：写明等于教对方留空）',
  );
  // 未认证的页面不该自己去拉面板接口：那只会拿到 401，还会把登录页变成一个有副作用的页面
  check(!/fetch\(|XMLHttpRequest|\/api\/state/.test(lp), '登录页不拉任何面板接口');
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

// ---------------------------------------------------------------- ⑥b 配色对比度

console.log('\n⑥b 配色对比度');

// 为什么要有这条：换配色是"看着好看"就能改的事，而对比度是**能不能读**的事。
// 第二十七轮把 accent 从蓝换成粉时就踩到了 —— Latte 官方的粉 #ea76cb 在它自己的
// 底色上只有 2.34:1，当**文字**色根本不合格（--accent 在这个项目里既当底又当字：
// primary 按钮、链接、token 高亮、控制台 system 行、激活的导航项）。
// 肉眼看不出来，算一下就知道。所以钉在这里。
function luminance(hex) {
  const h = hex.replace('#', '');
  const chan = [0, 2, 4].map((i) => parseInt(h.slice(i, i + 2), 16) / 255);
  const lin = chan.map((c) => (c <= 0.03928 ? c / 12.92 : ((c + 0.055) / 1.055) ** 2.4));
  return 0.2126 * lin[0] + 0.7152 * lin[1] + 0.0722 * lin[2];
}
function contrast(a, b) {
  const la = luminance(a);
  const lb = luminance(b);
  return (Math.max(la, lb) + 0.05) / (Math.min(la, lb) + 0.05);
}

const themeCss = readFileSync(join(WEB, 'css/theme.css'), 'utf8').replace(/\/\*[\s\S]*?\*\//g, '');
// 两个主题块：`:root, :root[data-theme='dark']` 和 `:root[data-theme='light']`
const themeBlocks = [
  ['dark', /:root,\s*\n:root\[data-theme='dark'\]\s*\{([\s\S]*?)\n\}/.exec(themeCss)?.[1] || ''],
  ['light', /:root\[data-theme='light'\]\s*\{([\s\S]*?)\n\}/.exec(themeCss)?.[1] || ''],
];
const readToken = (block, name) =>
  new RegExp(`--${name}:\\s*(#[0-9a-fA-F]{6})`).exec(block)?.[1] || '';

for (const [name, block] of themeBlocks) {
  check(!!block, `找得到 ${name} 主题块`);
  const bg = readToken(block, 'bg');
  for (const [token, min] of [
    ['accent', 4.5],   // 当文字色用，AA 正文标准
    ['text', 4.5],
    ['error', 4.5],
    ['success', 4.5],  // 也当文字用（徽章文字、日志级别、编辑器 token）
    ['warning', 4.5],
    // UI 组件那一档（WCAG 1.4.11 非文本对比度，3:1）：边框是"这里能输入/能点"
    // 的唯一视觉线索，1.80:1 会让人看不出输入框在哪。
    // 注意 --border / --border-soft **故意不查**：它们现在只用在卡片和分隔线上，
    // 而那些容器另有自己的面色（--surface）来区分，1.4.11 管的是"识别组件所必需的边界"。
    // 这个区分不是文字游戏 —— 第二十七轮就是因为没分这一档，才发现输入框边框不合格。
    ['border-control', 3.0],
  ]) {
    const c = readToken(block, token);
    if (!c) continue;
    const ratio = contrast(c, bg);
    check(
      ratio >= min,
      `${name} 主题：--${token} ${c} 在 --bg ${bg} 上 ${ratio.toFixed(2)}:1（要 ≥${min}）`,
    );
  }

  // ---- 文字三档 × **每一个它可能落在的底**（第二十八轮补）----
  //
  // 为什么要逐对算，而不是像上面那样只算 --bg：`--text-mute` 原来是 overlay0
  // #6c7086 —— 它在 --bg 上是 3.36:1（"看着还行"，所以一直没被发现），
  // 而它**待得最多的地方是卡片**：在 --surface 上 2.57:1、悬停底上 1.87:1。
  // 表现是"表头、卡片小字、统计标签糊成一片"，肉眼只知道"不好看"，
  // 不知道是哪一档、更不知道改到多少才算够。算出来就清楚了。
  //
  // 阈值：正文 AA 是 4.5:1。悬停底（--surface-2）放宽到 4.0 —— 文字只是**扫过**它
  // （悬停是瞬时的），而且那一档同时是分隔线色，压暗它会连带把分隔线也压没。
  // ⚠ 放宽的额度写在这里：谁要再往下调，得先改这一行。
  for (const token of ['text', 'text-dim', 'text-mute']) {
    const c = readToken(block, token);
    if (!c) continue;
    for (const [bgName, min] of [
      ['bg', 4.5],
      ['bg-deep', 4.5],
      ['surface', 4.5],
      ['surface-2', 4.0],
    ]) {
      const b = readToken(block, bgName);
      if (!b) continue;
      const ratio = contrast(c, b);
      check(
        ratio >= min,
        `${name} 主题：文字 --${token} ${c} 在 --${bgName} ${b} 上 ${ratio.toFixed(2)}:1（要 ≥${min}）`,
      );
    }
  }

  // ---- 三档必须**依次变淡**（设计不变量）----
  //
  // 上面那些算的是"每档都够不够读"，这条管的是"三档还分不分得开"：
  // 都合格但取同一个值的写法（dim == mute）会让层级消失，页面上"次要信息"
  // 和"更次要信息"长得一样。方向随主题相反 —— 暗色是越淡越暗，亮色是越淡越浅。
  const lum = (t) => luminance(readToken(block, t));
  // 方向随主题相反：暗色是"越次要越暗"（亮度递减），亮色是"越次要越浅"（亮度递增）。
  const dir = name === 'dark' ? -1 : 1;
  const d1 = lum('text-dim') - lum('text');
  const d2 = lum('text-mute') - lum('text-dim');
  check(
    Math.sign(d1) === dir && Math.abs(d1) > 0.01,
    `${name} 主题：--text-dim 要比 --text 淡一档（亮度差 ${d1.toFixed(3)}，要 ${dir > 0 ? '正' : '负'}）`,
  );
  check(
    Math.sign(d2) === dir && Math.abs(d2) > 0.01,
    `${name} 主题：--text-mute 要比 --text-dim 再淡一档（亮度差 ${d2.toFixed(3)}，要 ${dir > 0 ? '正' : '负'}）`,
  );
}

// ---------------------------------------------------------------- ⑦ 第三方 vendor

console.log('\n⑦ 第三方组件（web/vendor/）');

// 说明：`④ CSS 约束` 里那条「不许硬编码色值」只查我们自己写的 css/*.css，
// 是**故意**不查 vendor 的 —— 上游的样式里有 250 处 `var(--pd-x, #hex)` 兜底，
// 那是它的写法，不是我们要改的东西。别为了"统一"把它加进那个循环。

const VENDOR = join(WEB, 'vendor/puredashboard');
if (existsSync(VENDOR)) {
  // 1) 每个 vendor CSS 都必须在 index.html 里被 link ——
  //    上游是「一个组件一份样式」，漏 link 的症状是组件没样式（而不是报错）。
  const vendorCss = readdirSync(VENDOR).filter((f) => f.endsWith('.css'));
  check(vendorCss.length > 0, 'vendor 目录里有样式文件');
  for (const css of vendorCss) {
    check(
      index.includes(`href="/vendor/puredashboard/${css}"`),
      `index.html 引入了 /vendor/puredashboard/${css}`,
    );
  }

  // 2) FORK.md 必须**列全**目录里的每个文件，并记下上游版本 ——
  //    这条是给升级用的：少了哪个文件、从哪个 commit 来的，diff 的时候全靠它。
  const fork = readFileSync(join(VENDOR, 'FORK.md'), 'utf8');
  check(/commit[^\n]*`[0-9a-f]{7,}`/.test(fork), 'FORK.md 记了上游 commit');
  check(/v\d+\.\d+\.\d+/.test(fork), 'FORK.md 记了上游版本号');
  const notListed = readdirSync(VENDOR)
    .filter((f) => f !== 'FORK.md')
    .filter((f) => !fork.includes(f));
  check(
    notListed.length === 0,
    'FORK.md 列出了 vendor 目录里的每个文件',
    notListed.join(', '),
  );

  // 3) 许可必须随产物走（MIT 要求保留版权声明）
  check(existsSync(join(VENDOR, 'LICENSE')), 'vendor 目录里带着上游的 LICENSE');
  check(/MIT/.test(fork), 'FORK.md 写明了上游许可');
}

// ---------------------------------------------------------------- ⑧ 界面文案里不许写 Markdown

console.log('\n⑧ 界面文案');

// 为什么有这条：界面上真的出现过**星号原样显示**——
//
//     这里只留最近 40 行，且**只有插件日志与面板日志的混合流**。
//
// `**` 是 Markdown 的强调语法，而 `.hint` 是纯 HTML，不会渲染它。
// 这种错**别的检查一个都抓不到**：语法正确、链接正确、对比度也正确 ——
// 只有真看一眼才发现，而它一眼就能看出"这界面没做过"。
//
// 判据：源码里**不在注释里**的 `**`。
// 字符串里的 `**` 一定是这个毛病（要强调就用 `<b>`）；
// 代码里的 `**` 是幂运算符，本仓库目前一处都没有 —— 真要用，就在这一行
// 写成算式并在这里开个口子，别偷偷绕过去。
//
// ⚠ 不查 `vendor/`：那是上游的代码（同「不许硬编码色值」那条的口径）。
const STAR = /\*\*/;

/**
 * 找出「模板串里的 HTML 注释」中的反引号。
 *
 * 为什么单列一条：面板的界面是**拼字符串**出来的（`root.innerHTML = \`…\``），
 * 里面常写 HTML 注释解释"这块为什么这么排"。而模板串的定界符就是反引号 ——
 * 注释里写一个反引号，模板串会**提前结束**，后面那半截变成语法垃圾。
 *
 * ⚠ 关键：这种截断**不一定**是语法错误 —— 它可能恰好还能被解析
 * （第二十八轮我就在 `views/log-view.js` 里踩到了：`\`.select\` / \`.input\``
 * 被解析成"字符串 除以 什么"，`node --check` 全绿，而浏览器里整个日志页起不来、
 * 面板卡在「连接中…」）。所以「语法正确」那条**盖不住**它，必须单独查。
 */
function backticksInHtmlComments(src) {
  const hits = [];
  let inHtml = false;
  src.split('\n').forEach((line, i) => {
    let rest = line;
    for (;;) {
      if (!inHtml) {
        const o = rest.indexOf('<!--');
        if (o === -1) break;
        inHtml = true;
        rest = rest.slice(o + 4);
        continue;
      }
      const c = rest.indexOf('-->');
      const inner = c === -1 ? rest : rest.slice(0, c);
      if (inner.includes('`')) hits.push(`${i + 1}: ${line.trim().slice(0, 80)}`);
      if (c === -1) break;
      inHtml = false;
      rest = rest.slice(c + 3);
    }
  });
  return hits;
}

/** 找出**不在注释里**的 `**`，返回 `行号: 该行内容`。 */
function nonCommentStars(src) {
  const hits = [];
  let inBlock = false; // JS 块注释
  let inHtml = false; // 模板串里的 HTML 注释（用户看不见，不算）
  src.split('\n').forEach((line, i) => {
    let code = line;
    // ① HTML 注释（多行）
    if (inHtml) {
      const end = code.indexOf('-->');
      if (end === -1) return;
      inHtml = false;
      code = code.slice(end + 3);
    }
    // ② JS 块注释（多行）
    if (inBlock) {
      const end = code.indexOf('*/');
      if (end === -1) return;
      inBlock = false;
      code = code.slice(end + 2);
    }
    // ③ 行内块注释，④ 未闭合的 `/*`（**先截断再判**，否则 JSDoc 的 `/**` 会被误报）
    code = code.replace(/\/\*[\s\S]*?\*\//g, '');
    const open = code.indexOf('/*');
    if (open !== -1) {
      code = code.slice(0, open);
      inBlock = true;
    }
    // ⑤ 行注释
    const lc = code.indexOf('//');
    if (lc !== -1) code = code.slice(0, lc);
    // ⑥ 行内 / 未闭合的 HTML 注释
    const hc = code.indexOf('<!--');
    if (hc !== -1) {
      const hEnd = code.indexOf('-->', hc);
      if (hEnd === -1) {
        code = code.slice(0, hc);
        inHtml = true;
      } else {
        code = code.slice(0, hc) + code.slice(hEnd + 3);
      }
    }
    if (STAR.test(code)) hits.push(`${i + 1}: ${line.trim().slice(0, 80)}`);
  });
  return hits;
}

{
  // 自己走一遍文件（不借别处的变量：那些变量有自己的作用域，借了会 ReferenceError，
  // 而这条检查要的是"扫全部界面代码"，包括 web/js 与 plugins/ 下所有 .js）
  const targets = [];
  const walk = (d) => {
    if (!existsSync(d)) return;
    for (const e of readdirSync(d, { withFileTypes: true })) {
      const p = join(d, e.name);
      if (e.isDirectory()) walk(p);
      // 排除 vendor（上游代码）；用正则而不是拼分隔符，省得在意平台差异
      else if (e.name.endsWith('.js') && !/[\\/]vendor[\\/]/.test(p)) targets.push(p);
    }
  };
  walk(join(WEB, 'js'));
  walk(join(REPO, 'plugins'));

  const offenders = [];
  const tickOffenders = [];
  for (const abs of targets) {
    const src = readFileSync(abs, 'utf8');
    const hits = nonCommentStars(src);
    if (hits.length) offenders.push(`${relative(REPO, abs)} → ${hits.join(' | ')}`);
    const ticks = backticksInHtmlComments(src);
    if (ticks.length) tickOffenders.push(`${relative(REPO, abs)} → ${ticks.join(' | ')}`);
  }
  check(
    offenders.length === 0,
    `界面文案里没有字面 Markdown（扫了 ${targets.length} 个文件）`,
    offenders.join(' ; '),
  );
  check(
    tickOffenders.length === 0,
    '模板串里的 HTML 注释里没有反引号（写了会把模板串截断）',
    tickOffenders.join(' ; '),
  );
}


// ---------------------------------------------------------------- 插件 iframe 的 sandbox

/*
  为什么单独钉这一条：`sandbox` 里同时有 `allow-scripts` 与 `allow-same-origin` 时，
  如果被框文档与父页**同源**，它就能 `parent.document` 摘掉 sandbox 属性再重载自己
  —— 也就是说**同源模式下 sandbox 不是安全边界**。

  这个项目 v0.1.5 起默认就是同源（插件是可信代码），所以这里钉的不是「逃逸不存在」
  （它存在），而是：
    ① sandbox 只有一个生成处（视图里硬编码 → 改一处漏一处）；
    ② 同源模式下 `allow-same-origin` **必须**在（去掉它插件页就用不了会话/存储）；
    ③ 隔离开启时 `pluginUiUrl` 真的拼到第二个 origin；
    ④ 界面在未隔离时会明确告诉操作者「它不是边界」。
*/
async function checkPluginFrameSandbox() {
  const apiPath = join(WEB, 'js', 'api.js');
  // api.js 顶层不碰 location/document，可以直接 import（Node 里跑得动）。
  globalThis.location = { protocol: 'http:', hostname: '127.0.0.1' };
  const api = await import(pathToFileURL(apiPath).href);

  api.setPluginUiOrigin(null, null); // 退回同源（默认）
  const same = api.pluginFrameSandbox();
  check(
    same.includes('allow-same-origin'),
    '同源模式下 sandbox 仍带 allow-same-origin（去掉它插件页就调不通自己的接口）',
    same,
  );
  check(
    same.includes('allow-scripts') && same.includes('allow-forms'),
    'sandbox 保留 allow-scripts / allow-forms',
    same,
  );
  check(api.pluginIsolationEnabled() === false, 'pluginIsolationEnabled() 在同源时为 false');
  check(
    api.PLUGIN_ISOLATION_HINT.includes('同源') && api.PLUGIN_ISOLATION_HINT.includes('不是安全边界'),
    '界面文案明说「同源 + sandbox 不是安全边界」（不解释它就会被当成隔离）',
    api.PLUGIN_ISOLATION_HINT.slice(0, 40),
  );

  api.setPluginUiOrigin(null, 8788); // 第二个 origin
  check(api.pluginIsolationEnabled() === true, 'pluginIsolationEnabled() 在跨源时为 true');
  check(
    api.pluginFrameSandbox() === same,
    'sandbox 只有一个生成处（两种模式同一份，视图里不许再写一遍）',
  );
  check(
    api.pluginUiUrl('/plugins/x/index.html').startsWith('http://127.0.0.1:8788/'),
    '跨源时 pluginUiUrl 拼到第二个 origin',
    api.pluginUiUrl('/plugins/x/index.html'),
  );

  api.setPluginUiOrigin(null, null);
  check(
    api.pluginUiUrl('/plugins/x/index.html') === '/plugins/x/index.html',
    '退回同源时 pluginUiUrl 保持相对路径',
  );

  // 静态面：视图里不许硬编码 sandbox 串
  const offenders = [];
  const walk = (dir) => {
    for (const e of readdirSync(dir, { withFileTypes: true })) {
      const abs = join(dir, e.name);
      if (e.isDirectory()) walk(abs);
      else if (e.name.endsWith('.js')) {
        const src = readFileSync(abs, 'utf8');
        if (/sandbox\s*=\s*"allow-/.test(src)) offenders.push(relative(REPO, abs));
      }
    }
  };
  walk(join(WEB, 'js'));
  check(
    offenders.length === 0,
    'sandbox 属性只由 pluginFrameSandbox() 生成（视图里没有硬编码）',
    offenders.join(' ; '),
  );
}

await checkPluginFrameSandbox();

// ---------------------------------------------------------------- 结果

console.log();
if (failures.length) {
  console.log(`❌ 前端资源检查未通过：${failures.length} 项失败（通过 ${passed} 项）`);
  for (const f of failures) console.log(`   - ${f}`);
  process.exit(1);
}
console.log(`✅ 前端资源检查全部通过（${passed} 项）`);
