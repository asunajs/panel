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

const problems = [];
const files = [];

function walk(dir) {
  for (const name of readdirSync(dir)) {
    const p = path.join(dir, name);
    const st = statSync(p);
    if (st.isDirectory()) walk(p);
    else if (/\.(css|html)$/.test(name)) files.push(p);
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

  // R6 版本标记（用来识别「CSS 换了、JS 没换」的半新半旧状态）
  if (isCss && file.endsWith(path.join('mcloud.webui', 'style.css'))) {
    if (!/--style-ver\s*:/.test(raw)) {
      add(file, 0, 'R6 缺 --style-ver', '自检要靠它分辨浏览器用的是哪一份样式表。', '在 :root 里加 --style-ver: <n>;');
    }
  }
}

if (!problems.length) {
  const n = files.filter((f) => f.endsWith('.css')).length;
  if (verbose) console.log('扫了 ' + files.length + ' 个文件（其中 ' + n + ' 个 CSS）');
  console.log('ok: 插件样式检查全过（token / 颜色 / !important / 主题 / 控件 / 版本号）');
  process.exit(0);
}

console.error('插件样式有 ' + problems.length + ' 处违规：');
for (const p of problems) {
  console.error('  ' + p.file + (p.line ? ':' + p.line : '') + '  [' + p.rule + ']');
  console.error('    为什么：' + p.why);
  console.error('    怎么改：' + p.fix);
}
process.exit(1);
