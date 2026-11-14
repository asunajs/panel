// 面板自带的极简语法高亮（第 173 轮）。
//
// 为什么不引 Prism/highlight.js 那一套：
//   · 面板的 WebUI 是**零构建**的（index.html + app.css + app.js + ui.js 直接 include_str!），
//     多一个 40KB 的 tokenizer 只为给三个文本框上色，不划算；
//   · 我们要的语言只有两种：JS 与 JSON，规则加起来不到 40 行；
//   · 颜色必须走面板 token（两套主题自动跟着变），第三方主题表反而要拆。
//
// 输出是有转义的 HTML 片段（span.tok-*），由 CSS 决定颜色。
// 不做的事情：不解析语义（不区分函数声明与调用）、不折叠、不做 diff。
(function (global) {
  'use strict';

  const KW = new Set([
    'const', 'let', 'var', 'function', 'return', 'if', 'else', 'for', 'while', 'do',
    'switch', 'case', 'default', 'break', 'continue', 'new', 'class', 'extends', 'super',
    'this', 'try', 'catch', 'finally', 'throw', 'typeof', 'instanceof', 'await', 'async',
    'import', 'export', 'from', 'as', 'yield', 'delete', 'in', 'of', 'void', 'with',
    'null', 'undefined', 'true', 'false', 'NaN', 'Infinity', 'static', 'get', 'set',
  ]);

  function esc(s) {
    return s
      .replace(/&/g, '&amp;')
      .replace(/</g, '&lt;')
      .replace(/>/g, '&gt;');
  }

  const span = (cls, text) => '<span class="tok-' + cls + '">' + esc(text) + '</span>';

  // 字符串（含模板串）：JS 的三种引号都认，支持反斜杠转义
  const STR = /^(?:"(?:\\.|[^"\\])*"|'(?:\\.|[^'\\])*'|«(?:\\.|[^«\\])*«)/;
  const STR_RE = new RegExp(STR.source.replace(/«/g, String.fromCharCode(96)));

  const RULES = [
    [/^\/\/[^\n]*/, 'com'],
    [/^\/\*[\s\S]*?(?:\*\/|$)/, 'com'],
    [STR_RE, 'str'],
    [/^-?\b\d+(?:\.\d+)?(?:[eE][+-]?\d+)?\b/, 'num'],
    [/^[A-Za-z_$][\w$]*(?=\s*\()/, 'fn'],
    [/^[A-Za-z_$][\w$]*/, 'ident'],
    [/^[{}()[\];,.<>=+\-*/%!&|^~?:]+/, 'punc'],
    [/^\s+/, 'ws'],
  ];

  function js(code) {
    let out = '';
    let i = 0;
    let guard = 0;
    while (i < code.length && guard++ < 400000) {
      const rest = code.slice(i);
      let matched = false;
      for (const [re, cls] of RULES) {
        const m = re.exec(rest);
        if (!m) continue;
        const text = m[0];
        if (cls === 'ws') out += esc(text);
        else if (cls === 'ident') out += KW.has(text) ? span('kw', text) : esc(text);
        else if (cls === 'fn') out += span('fn', text);
        else out += span(cls, text);
        i += text.length;
        matched = true;
        break;
      }
      if (!matched) {
        out += esc(code[i]);
        i += 1;
      }
    }
    return out;
  }

  function json(code) {
    let out = '';
    let i = 0;
    const n = code.length;
    while (i < n) {
      const c = code[i];
      if (c === '"') {
        // 读到收尾的引号
        let j = i + 1;
        while (j < n) {
          if (code[j] === '\\') j += 2;
          else if (code[j] === '"') { j++; break; }
          else j++;
        }
        const text = code.slice(i, j);
        // 后面第一个非空白是冒号 → 这是键
        let k = j;
        while (k < n && /\s/.test(code[k])) k++;
        out += span(code[k] === ':' ? 'key' : 'str', text);
        i = j;
      } else if (c === '/' && code[i + 1] === '/') {
        let j = code.indexOf('\n', i);
        if (j < 0) j = n;
        out += span('com', code.slice(i, j));
        i = j;
      } else if (/[-0-9]/.test(c) && /^-?\d/.test(code.slice(i, i + 2))) {
        const m = /^-?\d+(?:\.\d+)?(?:[eE][+-]?\d+)?/.exec(code.slice(i));
        out += span('num', m[0]);
        i += m[0].length;
      } else if (/[A-Za-z_]/.test(c)) {
        const m = /^[A-Za-z_][\w]*/.exec(code.slice(i));
        const w = m[0];
        out += w === 'true' || w === 'false' || w === 'null' ? span('kw', w) : esc(w);
        i += w.length;
      } else if ('{}[],:'.includes(c)) {
        out += span('punc', c);
        i += 1;
      } else if (/\s/.test(c)) {
        out += esc(c);
        i += 1;
      } else {
        out += esc(c);
        i += 1;
      }
    }
    return out;
  }

  // 太长的文件不上色：一次 DOM 换 20 万个 span 会把标签页卡死。
  const MAX = 200000;

  function render(code, lang) {
    if (typeof code !== 'string' || !code) return '';
    if (code.length > MAX) return esc(code);
    return lang === 'json' ? json(code) : js(code);
  }

  /// 把高亮层与行号挂到一个 textarea 上（幂等：同一个 textarea 只挂一次）。
  function attach(textarea, opts) {
    if (!textarea || textarea.dataset.hl === '1') return textarea && textarea.__hl;
    const wrap = textarea.closest('.code-editor');
    if (!wrap) return null;
    const pre = wrap.querySelector('.code-hl > code') || wrap.querySelector('.code-hl');
    const gutter = wrap.querySelector('.code-gutter');
    const lang = (opts && opts.lang) || (textarea.dataset.lang || 'js');

    let lastLines = -1;
    const paint = () => {
      const code = textarea.value;
      // 太长的文件退成**纯文本模式**（第 182 轮）：mcloud.js 48.7 万字符全量分词 ≈
      // 6 万个 span，超限的文件每敲一键整棵重建一次 —— 输入法直接被打断（带中文
      // 注释的插件源码根本没法编辑，用户报障「源码太多导致功能失效」的就是它）。
      // 这时藏掉高亮层，让 textarea 自己显示自己的文字：原生编辑器的性能，行号照画。
      // 行数落回 MAX 以内会自动切回上色（下次 paint 重画 pre）。
      const plain = code.length > MAX;
      wrap.classList.toggle('plain', plain);
      if (!plain && pre) pre.innerHTML = render(code, lang);
      if (gutter) {
        const lines = code.split('\n').length;
        // 行数没变就不重画：光在一行里打字是大多数，别每次都重建几千行的号码串
        if (lines !== lastLines) {
          let g = '';
          for (let i = 1; i <= lines; i++) g += i + '\n';
          gutter.textContent = g;
          lastLines = lines;
        }
      }
    };
    const sync = () => {
      if (pre) {
        const box = pre.parentElement;
        if (box) {
          box.scrollTop = textarea.scrollTop;
          box.scrollLeft = textarea.scrollLeft;
        }
      }
      if (gutter) gutter.scrollTop = textarea.scrollTop;
    };

    let t = 0;
    const onInput = () => {
      if (t) clearTimeout(t);
      t = setTimeout(paint, 60);
    };
    textarea.addEventListener('input', onInput);
    textarea.addEventListener('scroll', sync, { passive: true });
    // 外部改 value（打开文件）时也要重画
    const api = {
      paint,
      sync,
      setValue(v) {
        textarea.value = v;
        paint();
        sync();
      },
    };
    textarea.dataset.hl = '1';
    textarea.__hl = api;
    paint();
    sync();
    return api;
  }

  global.PanelHL = { render, attach };
})(window);
