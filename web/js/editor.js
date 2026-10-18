/*
  web/js/editor.js —— 代码编辑器（自带，无外部依赖）。

  ## 为什么不是 CodeJar

  原计划是从 CDN 引 CodeJar。实测**加载不成功**：用户机器有外网，编辑器仍然降级成
  纯文本 —— 我把版本号写死成 `codejar@4.2.0`，那个版本在 jsDelivr/unpkg 上取不到，
  两个候选 URL 就一起挂了。而开发环境连不上外网，**没法验证哪个 URL 是对的**，
  只能继续猜。

  所以改成自带实现。理由不只是「懒得猜 URL」：

  - 这个项目的底线是**单一静态二进制 + 内网盒子能跑**。为了一万行里的三百行高亮，
    去依赖一个运行时才能发现的网络请求，是把「能跑」交给运气。
  - 前端资源是 `include_str!` 编译进二进制的，加载失败这件事**在编译期就该消失**。
  - 编辑器要的东西其实不多：contenteditable + 语法高亮 + caret 保持 + 撤销。
    高亮器本来就得自己写（CodeJar 也只提供钩子），剩下的部分不到 200 行。

  ## 自己写 contenteditable 编辑器，坑都在这几处

  1. **重画高亮会丢 caret** —— 所以每次重画前把 caret 换算成「纯文本偏移」，
     重画后再换算回去（`caretOffset` / `setCaret`）。
  2. **中文输入法** —— 合成期间（`isComposing`）绝不能重画 DOM，否则输入法候选
     会被打断。合成结束（`compositionend`）再画。
  3. **撤销** —— contenteditable 的原生撤销栈会被 `innerHTML` 重写清掉，
     所以自己做一份快照栈。
  4. **粘贴** —— 必须拦住，只取纯文本。直接粘贴会把网页的 HTML 塞进来。
  5. **回车/Tab** —— 原生行为会插入 `<div>` / `<br>`，让 DOM 不再是纯文本，
     之后 `textContent` 就与实际内容对不上了。两个键都自己接管。
*/

// ---------------------------------------------------------------- 语法高亮

// 面板只编辑 JavaScript：插件是 JS-only，脚本任务（.py/.sh）在面板上只填
// 元数据（路径 / 调度 / 运行时），不在这里改源码。原来那份 Python 词表已经
// 没有任何使用方了，留着只会让人以为面板还能编辑 Python。
const KEYWORDS = [
  'import', 'from', 'export', 'default', 'const', 'let', 'var', 'function',
  'return', 'if', 'else', 'for', 'while', 'do', 'break', 'continue', 'try',
  'catch', 'finally', 'throw', 'new', 'class', 'extends', 'super', 'this',
  'typeof', 'instanceof', 'in', 'of', 'async', 'await', 'yield', 'delete',
  'void', 'null', 'undefined', 'true', 'false',
];

function escapeHtml(s) {
  return s
    .replace(/&/g, '&amp;')
    .replace(/</g, '&lt;')
    .replace(/>/g, '&gt;');
}

/**
 * 把源码转成带 `<span class="tok-*">` 的 HTML。
 *
 * 做法是「一趟扫描」而不是连续 replace：注释 / 字符串 / 数字 / 关键字放在同一个
 * 正则的四个捕获组里按出现顺序匹配，这样**字符串里的关键字不会被误高亮** ——
 * 连续 replace 的写法一定会踩这个坑。
 *
 * @param {string} code
 */
export function highlight(code) {
  const kw = KEYWORDS.map((w) => w.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')).join('|');

  const re = new RegExp(
    `(\\/\\/[^\\n]*|\\/\\*[\\s\\S]*?\\*\\/)|('(?:\\\\.|[^'\\\\])*'|"(?:\\\\.|[^"\\\\])*"|\`(?:\\\\.|[^\`\\\\])*\`)|\\b(0x[0-9a-fA-F]+|\\d+\\.?\\d*)\\b|\\b(${kw})\\b`,
    'g',
  );

  let out = '';
  let last = 0;
  let m;
  while ((m = re.exec(code)) !== null) {
    out += escapeHtml(code.slice(last, m.index));
    const cls = m[1] ? 'tok-com' : m[2] ? 'tok-str' : m[3] ? 'tok-num' : 'tok-key';
    out += `<span class="${cls}">${escapeHtml(m[0])}</span>`;
    last = m.index + m[0].length;
  }
  out += escapeHtml(code.slice(last));
  return out;
}

// ---------------------------------------------------------------- caret 换算

/** 当前 caret 在纯文本里的偏移；不在编辑器内或没有选区时返回 null。 */
function caretOffset(el) {
  const sel = window.getSelection();
  if (!sel || sel.rangeCount === 0) return null;
  const r = sel.getRangeAt(0);
  if (!el.contains(r.startContainer)) return null;
  const pre = r.cloneRange();
  pre.selectNodeContents(el);
  pre.setEnd(r.startContainer, r.startOffset);
  return pre.toString().length;
}

/** 把 caret 放到纯文本偏移处。 */
function setCaret(el, offset) {
  const walker = document.createTreeWalker(el, NodeFilter.SHOW_TEXT);
  let acc = 0;
  let n;
  while ((n = walker.nextNode())) {
    const len = n.nodeValue.length;
    if (acc + len >= offset) {
      const r = document.createRange();
      r.setStart(n, Math.max(0, offset - acc));
      r.collapse(true);
      const sel = window.getSelection();
      sel.removeAllRanges();
      sel.addRange(r);
      return;
    }
    acc += len;
  }
  // 偏移落在末尾（或没有文本节点）：折到内容末尾
  const r = document.createRange();
  r.selectNodeContents(el);
  r.collapse(false);
  const sel = window.getSelection();
  sel.removeAllRanges();
  sel.addRange(r);
}

// ---------------------------------------------------------------- 编辑器

const UNDO_LIMIT = 200;
const UNDO_DEBOUNCE = 300;
const INDENT = '  ';

/**
 * 在容器里创建编辑器。
 *
 * @param {HTMLElement} host   编辑器容器（会被清空后重建内容）
 * @param {{value?: string, onUpdate?: (v: string) => void}} [opts]
 * @returns {{getValue(): string, setValue(v: string): void, focus(): void, isFallback: boolean, error: string|null}}
 */
export function createEditor(host, opts = {}) {
  const { value = '', onUpdate } = opts;

  host.textContent = '';

  const el = document.createElement('div');
  el.className = 'editor';
  el.contentEditable = 'true';
  el.spellcheck = false;
  el.setAttribute('role', 'textbox');
  el.setAttribute('aria-multiline', 'true');
  el.setAttribute('aria-label', '代码编辑器');
  host.appendChild(el);

  let composing = false;
  const history = [value];
  let histIdx = 0;
  let histTimer = null;

  /** 重画：保持 caret 位置。 */
  function redraw() {
    const off = caretOffset(el);
    el.innerHTML = highlight(el.textContent);
    if (off !== null) setCaret(el, off);
  }

  function notify() {
    if (onUpdate) onUpdate(el.textContent);
  }

  /**
   * 落一次撤销快照。
   *
   * `immediate = false`（打字）走防抖合并：连打 10 个字符算**一次**编辑，
   * 撤销一次就全退回去 —— 这是所有编辑器的习惯。
   *
   * `immediate = true` 用于**结构性**编辑（回车 / Tab / 粘贴）：用户预期它们
   * 各自可撤销。实测踩到过：回车和 Tab 挨得近，被防抖合并成一条，
   * 于是撤销一下直接跳回了几步之前的整段内容。
   */
  function pushHistory(immediate = false) {
    clearTimeout(histTimer);

    const commit = () => {
      const v = el.textContent;
      if (history[histIdx] === v) return;
      history.splice(histIdx + 1);
      history.push(v);
      histIdx = history.length - 1;
      if (history.length > UNDO_LIMIT) {
        history.shift();
        histIdx--;
      }
    };

    if (immediate) commit();
    else histTimer = setTimeout(commit, UNDO_DEBOUNCE);
  }

  /** 在 caret 处插入纯文本（自己接管回车 / Tab 都要用它，DOM 才能保持纯文本）。 */
  function insertText(text) {
    const sel = window.getSelection();
    if (!sel || sel.rangeCount === 0) return;
    const r = sel.getRangeAt(0);
    r.deleteContents();
    const node = document.createTextNode(text);
    r.insertNode(node);
    r.setStartAfter(node);
    r.collapse(true);
    sel.removeAllRanges();
    sel.addRange(r);
  }

  /** 当前行（caret 之前的文本里，最后一个换行之后的部分）的缩进。 */
  function currentIndent() {
    const before = (() => {
      const off = caretOffset(el);
      return off === null ? el.textContent : el.textContent.slice(0, off);
    })();
    const line = before.slice(before.lastIndexOf('\n') + 1);
    const m = line.match(/^[ \t]*/);
    return m ? m[0] : '';
  }

  function applyValue(v) {
    el.textContent = v;
    el.innerHTML = highlight(v);
  }

  // ---- 输入 ----
  el.addEventListener('input', () => {
    // 输入法合成期间不能动 DOM，否则候选框会被打断
    if (composing) return;
    redraw();
    notify();
    pushHistory();
  });

  el.addEventListener('compositionstart', () => {
    composing = true;
  });
  el.addEventListener('compositionend', () => {
    composing = false;
    redraw();
    notify();
    pushHistory();
  });

  // ---- 按键：回车 / Tab / 撤销 ----
  el.addEventListener('keydown', (e) => {
    const mod = e.metaKey || e.ctrlKey;

    if (mod && e.key.toLowerCase() === 'z') {
      e.preventDefault();
      if (e.shiftKey) {
        if (histIdx < history.length - 1) histIdx++;
      } else if (histIdx > 0) {
        histIdx--;
      }
      applyValue(history[histIdx]);
      notify();
      return;
    }
    if (mod && e.key.toLowerCase() === 'y') {
      e.preventDefault();
      if (histIdx < history.length - 1) histIdx++;
      applyValue(history[histIdx]);
      notify();
      return;
    }

    if (e.key === 'Tab') {
      e.preventDefault();
      insertText(INDENT);
      redraw();
      notify();
      pushHistory(true);
      return;
    }

    if (e.key === 'Enter') {
      // 自己接管：原生回车会插 <div>/<br>，DOM 就不再是纯文本了
      e.preventDefault();
      const indent = currentIndent();
      const before = el.textContent.slice(0, caretOffset(el) ?? el.textContent.length);
      // 行尾是 { : ( 时多缩进一级 —— 写函数体时最常用
      const extra = /[{:([][ \t]*$/.test(before) ? INDENT : '';
      insertText('\n' + indent + extra);
      redraw();
      notify();
      pushHistory(true);
    }
  });

  // ---- 粘贴：只取纯文本 ----
  el.addEventListener('paste', (e) => {
    e.preventDefault();
    const text = (e.clipboardData || window.clipboardData).getData('text/plain');
    if (!text) return;
    insertText(text);
    redraw();
    notify();
    pushHistory(true);
  });

  // ---- 失焦时也把撤销栈落定（不然最后一次编辑可能被 debounce 吞掉）----
  el.addEventListener('blur', () => {
    clearTimeout(histTimer);
    const v = el.textContent;
    if (history[histIdx] !== v) {
      history.splice(histIdx + 1);
      history.push(v);
      histIdx = history.length - 1;
    }
  });

  applyValue(value);

  return {
    getValue: () => el.textContent,
    setValue: (v) => {
      applyValue(v);
      history.length = 0;
      history.push(v);
      histIdx = 0;
    },
    focus: () => el.focus(),
    // 兼容原来的接口：现在没有「降级」这回事了，编辑器永远可用
    isFallback: false,
    error: null,
  };
}
