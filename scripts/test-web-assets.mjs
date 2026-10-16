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
  'js/confirm.js',
  'js/mock.js',
  'js/views/task-list.js',
  'js/views/task-editor.js',
  'js/views/plugin-list.js',
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
  for (const abs of targets) {
    const hits = nonCommentStars(readFileSync(abs, 'utf8'));
    if (hits.length) offenders.push(`${relative(REPO, abs)} → ${hits.join(' | ')}`);
  }
  check(
    offenders.length === 0,
    `界面文案里没有字面 Markdown（扫了 ${targets.length} 个文件）`,
    offenders.join(' ; '),
  );
}

// ---------------------------------------------------------------- 结果

console.log();
if (failures.length) {
  console.log(`❌ 前端资源检查未通过：${failures.length} 项失败（通过 ${passed} 项）`);
  for (const f of failures) console.log(`   - ${f}`);
  process.exit(1);
}
console.log(`✅ 前端资源检查全部通过（${passed} 项）`);
