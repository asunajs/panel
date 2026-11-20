#!/usr/bin/env node
// 插件样式的静态闸门（第 156 轮）。
//
// 为什么要有：一次「修 bug」把 plugins/mcloud.webui/style.css 的 token 映射整段换成了
// 旧面板的名字（--fg: var(--text)），0.4.0 里根本没有 --text —— 于是 --fg 变无效值，
// 整页配色崩、组行变灰。这类改动编译不报错、单测照过，只有人眼能看出来。
// 所以把「不许做的事」写成可执行的检查，放在部署之前。
//
// 用法：node scripts/check-plugin-style.mjs [--verbose]
// 退出码：0 全过；1 有违规（每条都打印文件:行 + 为什么 + 怎么改）。
import { readFileSync, readdirSync, statSync } from 'node:fs';
import path from 'node:path';

const ROOT = process.cwd();
const PLUGINS = path.join(ROOT, 'plugins');
const verbose = process.argv.includes('--verbose');

/** 面板自己的 token：插件**不许定义**，只能读。 */
const PANEL_TOKENS = new Set([
  'bg', 'bg-grad', 'surface', 'surface-2', 'line', 'line-strong',
  'fg', 'fg-2', 'dim', 'accent', 'accent-fg', 'accent-soft',
  'ok', 'ok-soft', 'warn', 'warn-soft', 'err', 'on-err', 'err-soft',
  'scrim', 'shadow-sm', 'shadow-md', 'radius', 'radius-sm',
  'h-control', 'h-control-sm', 'r-control', 'pad-control', 'mono', 'sans',
]);

/** 允许出现颜色字面量的地方：得在同一行或上一行写这句注释并说明理由。 */
const ALLOW = 'style-lint: allow-color';

/** 手搓发码格（手机号输入框 + 「获取验证码」按钮）的豁免标记。
 *  只给「确实不该用 <p-sms-code>」的场合用：比如号码不是用户输入、而是从账号列表选的
 *  （cmcc 的短信登录就是这样），控件那格的输入框根本不出现。 */
const ALLOW_SMS = 'style-lint: allow-sms-handroll';

/** 原生对话框的豁免标记（一般不该用：见 R8）。 */
const ALLOW_DIALOG = 'style-lint: allow-native-dialog';

/** `node:fs` 子集之外的方法的豁免标记（见 R9）。 */
const ALLOW_FS = 'style-lint: allow-fs-missing';

const problems = [];
const warnings = [];
const files = [];

function walk(dir) {
  for (const name of readdirSync(dir)) {
    const p = path.join(dir, name);
    const st = statSync(p);
    if (st.isDirectory()) walk(p);
    else if (/\.(css|html|js)$/.test(name)) files.push(p);
  }
}

/** 把注释抹掉（保留行号）：检查只看真正的声明。 */
function stripComments(text) {
  return text
    .replace(/\/\*[\s\S]*?\*\//g, (m) => m.replace(/[^\n]/g, ' '))
    .replace(/<!--[\s\S]*?-->/g, (m) => m.replace(/[^\n]/g, ' '));
}

function add(file, line, rule, why, fix) {
  problems.push({ file: path.relative(ROOT, file), line, rule, why, fix });
}

walk(PLUGINS);

for (const file of files) {
  const raw = readFileSync(file, 'utf8');
  const lines = raw.split('\n');
  const text = stripComments(raw).split('\n');
  const isCss = file.endsWith('.css');

  for (let i = 0; i < text.length; i += 1) {
    const line = text[i];
    const no = i + 1;
    const allowHere = (lines[i] || '').includes(ALLOW) || (lines[i - 1] || '').includes(ALLOW);

    // R1 不许定义面板的 token
    for (const m of line.matchAll(/--([a-z0-9-]+)\s*:/g)) {
      if (PANEL_TOKENS.has(m[1])) {
        add(file, no, 'R1 面板 token 被重定义',
          '--' + m[1] + ' 是**面板的**名字，插件定义它会盖掉主题（深色下整片变色）。',
          '删掉这条；要用就 var(--' + m[1] + ')。旧面板的短名字请映射到 0.4.0 的 token，方向别反。');
      }
    }

    // R2 颜色字面量
    if (isCss && !allowHere) {
      // 只看值那一半：white-space: nowrap 里的 white 不是颜色。
      const colon = line.indexOf(':');
      const value = colon >= 0 ? line.slice(colon + 1) : '';
      const colorish = /(#[0-9a-fA-F]{3,8}\b|\brgba?\(|\bhsla?\(|(?<![\w-])(white|black)(?![\w-]))/g;
      const decl = colon > 0;
      if (decl) {
        for (const m of value.matchAll(colorish)) {
          add(file, no, 'R2 颜色字面量',
            '写死了「' + m[1] + '」—— 换主题时它不会跟着变。',
            '换成 token（--fg / --surface / --line / --accent …）；确实必须写死（二维码底、图表色板）就加注释 /* ' + ALLOW + ' 理由 */。');
        }
      }
    }

    // R3 !important
    if (isCss && /!important/.test(line)) {
      add(file, no, 'R3 !important',
      '插件靠 !important 压面板，等于把主题锁死；下次面板调整数值你这边就错位。',
      '提高选择器特异度（比如 #add-dialog .dialog）或直接给面板提组件需求。');
    }

    // R4 prefers-color-scheme
    if (isCss && /prefers-color-scheme/.test(line)) {
      add(file, no, 'R4 prefers-color-scheme',
      '系统深浅色 ≠ 用户在面板里选的主题；这条媒体查询会把用户的选择盖掉。',
      '主题跟随只走 boot.js 的三条来路（?theme= → localStorage → 系统），CSS 里只认 [data-theme]。');
    }

    // R5 撤掉面板画过的控件样式（历史事故：勾选框被擦没）
    if (isCss && /input\[[^\]]*type *= *['"]?(checkbox|radio)/.test(line)) {
      const block = lines.slice(i, i + 12).join(' ');
      if (/background:\s*none/.test(block) || /border:\s*0/.test(block)) {
        add(file, no, 'R5 控件样式被撤掉',
          '0.4.0 的勾选框是面板画的（appearance:none + SVG 勾）。这里 background:none / border:0 会把它擦成空白。',
          '只留布局（flex / margin / 宽高），别碰 border、background、appearance。');
      }
    }
  }

  // ⚠ 这里原来还有一条 R6「必须有 --style-ver」—— 那是插件项目产物上的一个自检标记，
  // 不属于面板的准则（而且 plugins/ 是产物，产物形态由插件项目决定）。删掉，
  // 免得它拦住正常部署。
}

// R9 LLRT 没有的 `node:fs` 方法（第 198 轮）
//
// LLRT 的 `node:fs` 是**子集**（面板仓 docs/engine.md 记着：16 / 105）。实测存在的是：
//   accessSync chmodSync lstatSync mkdirSync mkdtempSync readFileSync readdirSync
//   renameSync rmSync rmdirSync statSync symlinkSync writeFileSync（+ constants / promises）
//
// **`existsSync` 不在里面**，`unlinkSync` / `appendFileSync` / `copyFileSync` / `realpathSync` /
// `openSync` 也不在。而 Node 有 —— 所以 TS 里 `import fs from 'node:fs'` 一路编译通过，
// 装到面板上第一次读配置就 `TypeError: not a function`（cmcc 就这么炸的：
// `/plugins/cmcc/api/status` 502 plugin_busy，日志里只有一句 `detail=TypeError: not a function`，
// 连栈都没有）。
//
// 写法：存在性判断用 `statSync` 包 try/catch（或 `accessSync`），删除用 `rmSync(p, {force:true})`。
const FS_MISSING = [
  ['existsSync', 'statSync 包 try/catch（或 accessSync）'],
  ['unlinkSync', 'rmSync(path, { force: true })'],
  ['appendFileSync', 'readFileSync 读出来拼上再 writeFileSync'],
  ['copyFileSync', 'readFileSync + writeFileSync'],
  ['realpathSync', '用 host.plugin_dir() / plugin_data_dir() 拿绝对路径'],
  ['openSync', 'LLRT 没有 fd 系列，直接用 readFileSync / writeFileSync'],
];
for (const file of files) {
  if (!file.endsWith('.js')) continue;
  const raw = readFileSync(file, 'utf8');
  if (raw.includes(ALLOW_FS)) continue;
  const text = stripComments(raw).split('\n');
  for (let i = 0; i < text.length; i += 1) {
    for (const [name, fix] of FS_MISSING) {
      // 两种写法都要抓：`fs.existsSync(…)`（方法调用）和裸 `existsSync(…)`（解构后调用）
      const re = new RegExp('(?:^|[^.\\w$]|\\.)' + name + '\\s*\\(');
      if (!re.test(text[i])) continue;
      // 自己定义了同名函数（polyfill）就不算 —— 只看"直接调 fs 上的方法"
      if (new RegExp('function\\s+' + name + '\\b').test(raw)) continue;
      warnings.push({
        file: path.relative(ROOT, file),
        line: i + 1,
        rule: 'R9 node:fs 子集',
        why: '`' + name + '()` 在 LLRT 的 `node:fs` 里**没有**（只有 16 个方法），'
          + 'Node 有 ⇒ 编译通过、运行时 `TypeError: not a function`。',
        fix: '改成 ' + fix + '。清单见面板仓 docs/engine.md 的模块表；'
          + '确实要保留就写注释 ' + ALLOW_FS + ' 理由。',
      });
    }
  }
}

// R8 原生对话框（第 197 轮）：插件里不许再用 confirm() / alert() / prompt()。
//
// 为什么：那是**浏览器自己的窗口** —— macOS 上跑到屏幕正中、和面板完全不是一个东西
// （用户截图就是那块「localhost:3030 显示 …」），iframe 里还可能被浏览器直接拦掉。
// 面板自己第 100 轮起就不用了（自绘 askConfirm），插件现在也有：`PanelUI.confirm()` /
// `PanelUI.prompt()`（都在 /ui.js 里，返回 Promise）。
//
// 只提醒不拦部署：`alert()` 在某些调试分支里无害，但正经的交互都该换掉。
for (const file of files) {
  if (!file.endsWith('.js')) continue;
  const raw = readFileSync(file, 'utf8');
  if (raw.includes(ALLOW_DIALOG)) continue;
  // 抹掉字符串字面量再找：`'confirm( 只是提示文案'` 不该被当成调用（试过，会误报）。
  const text = stripComments(raw)
    .split('\n')
    .map((line) => line.replace(/'(?:[^'\\]|\\.)*'/g, "''").replace(/"(?:[^"\\]|\\.)*"/g, '""').replace(/`(?:[^`\\]|\\.)*`/g, '``'));
  for (let i = 0; i < text.length; i += 1) {
    // 只看「裸调用」：`window.confirm(` / `confirm(`；跳过 `askConfirm(` / `PanelUI.confirm(`
    const m = /(?:^|[^.\w$])(window\.)?(confirm|alert|prompt)\s*\(/.exec(text[i]);
    if (!m) continue;
    if (/askConfirm|PanelUI\.\s*$/.test(text[i].slice(0, m.index + (m[0] || '').length))) continue;
    warnings.push({
      file: path.relative(ROOT, file),
      line: i + 1,
      rule: 'R8 原生对话框',
      why: '用了浏览器的原生 `' + m[2] + '()` —— 那是另一个窗口，和面板长得完全不一样；'
        + 'iframe 里还可能被浏览器拦掉。',
      fix: '换成面板的：`await PanelUI.confirm({text, okLabel, danger})` → true/false；'
        + '`await PanelUI.prompt({title, label, value})` → 字符串 / null（都在 /ui.js）。'
        + '真要保留原生框就写注释 ' + ALLOW_DIALOG + ' 理由。',
    });
  }
}

// R7 手搓发码格（第 195 轮）：面板早就把「手机号 + 行内获取验证码 + 重发倒计时」做成
// 共享控件 <p-sms-code>（web/ui.js，第 182 轮从 mcloud 登录页提上来的）。但控件做完之后
// 各插件还在各写一遍——同一格三种长相、倒计时三份实现、关弹窗不停定时器各踩一次。
// 所以这里只提醒、不拦部署：命中「11 位手机号输入框 + 带验证码字样的按钮 + 没有 p-sms-code」。
//
// 为什么是 warning：有的场景确实不该用控件（号码从账号列表选、不出现输入框），
// 那种就在文件里写 style-lint: allow-sms-handroll + 理由。
for (const file of files) {
  if (!file.endsWith('.html')) continue;
  const raw = readFileSync(file, 'utf8');
  if (raw.includes(ALLOW_SMS)) continue;
  const text = stripComments(raw);
  if (/<p-sms-code\b/i.test(text)) continue;
  const phoneInput = /<input[^>]*maxlength\s*=\s*["']11["']/i.test(text);
  const sendBtn = /<(?:button|p-button)\b[^>]*>[^<]*(?:获取|发送|重发)?验证码/i.test(text);
  if (phoneInput && sendBtn) {
    warnings.push({
      file: path.relative(ROOT, file),
      rule: 'R7 手搓发码格',
      why: '这一页自己画了「手机号输入框 + 获取验证码按钮」。面板有共享控件 <p-sms-code>：'
        + '数字门禁、行内按钮、60 秒重发倒计时、关弹窗停表都在里面（web/ui.js）。',
      fix: '换成 <p-sms-code id="…" length="11" seconds="60"></p-sms-code>，监听 send 事件发请求、'
        + '成功后 el.startResend(60)、发码期间 el.busy = true。'
        + '确实不该用（号码从账号列表选、不出现输入框）就写注释 ' + ALLOW_SMS + ' 理由。',
    });
  }
}

function printWarnings() {
  if (!warnings.length) return;
  console.log('');
  console.log('提示 ' + warnings.length + ' 条（不拦部署，但建议改）：');
  for (const w of warnings) {
    console.log('  ' + w.file + '  [' + w.rule + ']');
    console.log('    为什么：' + w.why);
    console.log('    怎么改：' + w.fix);
  }
}

if (!problems.length) {
  const n = files.filter((f) => f.endsWith('.css')).length;
  if (verbose) console.log('扫了 ' + files.length + ' 个文件（其中 ' + n + ' 个 CSS）');
  printWarnings();
  console.log('ok: 插件样式检查全过（token / 颜色 / !important / 主题 / 控件）');
  process.exit(0);
}

console.error('插件样式有 ' + problems.length + ' 处违规：');
for (const p of problems) {
  console.error('  ' + p.file + (p.line ? ':' + p.line : '') + '  [' + p.rule + ']');
  console.error('    为什么：' + p.why);
  console.error('    怎么改：' + p.fix);
}
process.exit(1);
