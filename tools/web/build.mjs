#!/usr/bin/env node
// 面板 WebUI 的压缩构建（第 190 轮，用户要求「Rolldown 给前端资源压缩一下」）。
//
// 为什么要预构建并**提交产物**：面板是「cargo build 就能跑」的零构建工程，
// 构建机上不该出现 node。所以源文件留在 web/，产物落在 web/dist/ 并进仓库，
// src/web.rs 用 include_str! 把产物编进二进制。改了源文件忘了重建，由
// scripts/check-web-build.sh 拦住（它会重建一遍并比字节）。
//
// 为什么不 mangle 顶层名字：这四个文件是**经典脚本**（不是 ESM），彼此靠全局
// 名字协作，index.html 之外还有 ui-check 之类在 window 上直接取函数名
// （比如 typeof changeTokenBox）。改名会静默打断这些引用 —— 只做压缩和去空白。

import { readFileSync, writeFileSync, mkdirSync, rmSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import path from 'node:path';
import { rolldown } from 'rolldown';
import { transform } from 'lightningcss';

const here = path.dirname(fileURLToPath(import.meta.url));
const root = path.resolve(here, '../..');
const webSrc = path.join(root, 'web');
// 守卫脚本（scripts/check-web-build.sh）会把产物写到临时目录再和仓库里的比字节：
//   PANEL_WEB_OUT=/tmp/xxx node build.mjs
// 产物放在 web/ 根下（不是 web/dist/）：PANEL_WEB_DIR 那个开发口子按 web/<file> 从磁盘读，
// 放子目录里开发态会 500，而开发态正是调样式时最常用的形态。
const out = process.env.PANEL_WEB_OUT
  ? path.resolve(process.env.PANEL_WEB_OUT)
  : webSrc;

// 加载顺序必须和 index.html 里原来的一致（顺序反了会 ReferenceError 或行为变化）
const JS_PARTS = ['highlight.js', 'schedule.js', 'app.js'];

const kb = (n) => (n / 1024).toFixed(1) + 'KB';

const concat = JS_PARTS.map((f) => {
  const code = readFileSync(path.join(webSrc, f), 'utf8');
  // 每个文件之间留一行注释：出了运行时报错，堆栈/行号还能对回大致来源
  return '/* ==== ' + f + ' ==== */\n' + code;
}).join('\n');

mkdirSync(out, { recursive: true });
const entry = path.join(webSrc, '.entry.js');
writeFileSync(entry, concat);

const bundle = await rolldown({ input: entry });
const { output } = await bundle.generate({
  format: 'es',
  // compress: false —— oxc 的 compress 会把「本文件里没人引用」的顶层声明当死代码去掉，
  // 而面板这几个脚本是经典脚本：`changeTokenBox` 这种名字是给 index.html 之外的
  // 调用方（ui-check、CDP 里的 eval）用的，压缩器看不到那些引用 → 删掉就静默坏掉。
  // 只做去注释 + 去空白，够把 300KB 压到 90KB 量级。
  minify: { mangle: false, compress: false },
  comments: false,
});
const js = output.find((o) => o.type === 'chunk').code;
const jsOut = path.join(out, 'panel.min.js');
writeFileSync(jsOut, js);
rmSync(entry, { force: true });

const cssSrc = readFileSync(path.join(webSrc, 'app.css'));
const { code: css } = transform({ filename: 'app.css', code: cssSrc, minify: true });
const cssOut = path.join(out, 'panel.min.css');
writeFileSync(cssOut, css);

const rawJs = JS_PARTS.reduce((n, f) => n + readFileSync(path.join(webSrc, f)).length, 0);
console.log('JS  ' + JS_PARTS.join(' + ') + ': ' + kb(rawJs) + ' → ' + kb(js.length));
console.log('CSS app.css: ' + kb(cssSrc.length) + ' → ' + kb(css.length));
