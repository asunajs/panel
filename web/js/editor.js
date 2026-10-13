/*
  web/js/editor.js —— CodeJar 封装。

  三件事：
    1. 从 CDN 动态加载 CodeJar（ESM），失败时**降级到原生 textarea**
    2. 提供语言感知的轻量语法高亮（js / py）
    3. 对视图层暴露一个稳定的小接口：getValue / setValue / setLanguage / focus

  为什么要降级：面板跑在低配盒子上，可能是**内网离线**环境，CDN 拿不到。
  这时候编辑器必须还能用 —— 只是没有高亮，而不是整个页面白屏。
  降级状态通过 `isFallback` 暴露出去，视图可以据此提示用户。
*/

const CDN_CANDIDATES = [
  'https://cdn.jsdelivr.net/npm/codejar@4.2.0/codejar.js',
  'https://unpkg.com/codejar@4.2.0/codejar.js',
];

/** 加载结果缓存：多个编辑器实例只尝试一次。 */
let codeJarPromise = null;

/**
 * 动态加载 CodeJar。
 * @returns {Promise<{CodeJar: Function|null, error: string|null}>}
 */
export function loadCodeJar() {
  if (codeJarPromise) return codeJarPromise;

  codeJarPromise = (async () => {
    const errors = [];
    for (const url of CDN_CANDIDATES) {
      try {
        const mod = await import(/* @vite-ignore */ url);
        if (typeof mod.CodeJar === 'function') {
          return { CodeJar: mod.CodeJar, error: null };
        }
        errors.push(`${url}：没有导出 CodeJar`);
      } catch (e) {
        errors.push(`${url}：${e.message || e}`);
      }
    }
    return { CodeJar: null, error: errors.join('；') };
  })();

  return codeJarPromise;
}

// ---------------------------------------------------------------- 语法高亮

const KEYWORDS = {
  js: [
    'import', 'from', 'export', 'default', 'const', 'let', 'var', 'function',
    'return', 'if', 'else', 'for', 'while', 'do', 'break', 'continue', 'try',
    'catch', 'finally', 'throw', 'new', 'class', 'extends', 'super', 'this',
    'typeof', 'instanceof', 'in', 'of', 'async', 'await', 'yield', 'delete',
    'void', 'null', 'undefined', 'true', 'false',
  ],
  py: [
    'import', 'from', 'as', 'def', 'return', 'if', 'elif', 'else', 'for',
    'while', 'break', 'continue', 'try', 'except', 'finally', 'raise', 'with',
    'class', 'lambda', 'yield', 'global', 'nonlocal', 'assert', 'del', 'pass',
    'and', 'or', 'not', 'in', 'is', 'None', 'True', 'False',
  ],
};

function escapeHtml(s) {
  return s
    .replace(/&/g, '&amp;')
    .replace(/</g, '&lt;')
    .replace(/>/g, '&gt;');
}

/**
 * 把源码转成带 <span class="tok-*"> 的 HTML。
 *
 * 做法是「一趟扫描」而不是连续 replace：注释 / 字符串 / 数字 / 关键字放在同一个
 * 正则的四个捕获组里按出现顺序匹配，这样 **字符串里的关键字不会被误高亮** ——
 * 连续 replace 的写法一定会踩这个坑。
 *
 * @param {string} code
 * @param {'js'|'py'} lang
 */
export function highlight(code, lang = 'js') {
  const words = KEYWORDS[lang] || KEYWORDS.js;
  const kw = words.map((w) => w.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')).join('|');

  const re =
    lang === 'py'
      ? new RegExp(`(#[^\\n]*)|('(?:\\\\.|[^'\\\\])*'|"(?:\\\\.|[^"\\\\])*")|\\b(\\d+\\.?\\d*)\\b|\\b(${kw})\\b`, 'g')
      : new RegExp(
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

// ---------------------------------------------------------------- 编辑器实例

/**
 * 在容器里创建编辑器。
 *
 * @param {HTMLElement} host   编辑器容器（会被清空后重建内容）
 * @param {{value?: string, language?: 'js'|'py', onUpdate?: (v: string) => void}} [opts]
 * @returns {Promise<{getValue(): string, setValue(v: string): void, setLanguage(l: string): void, focus(): void, isFallback: boolean, error: string|null}>}
 */
export async function createEditor(host, opts = {}) {
  const { value = '', language = 'js', onUpdate } = opts;
  const { CodeJar, error } = await loadCodeJar();

  host.textContent = '';
  let lang = language;

  // ---- 降级路径：原生 textarea（没有高亮，但功能完整）----
  if (!CodeJar) {
    const ta = document.createElement('textarea');
    ta.className = 'editor-fallback';
    ta.spellcheck = false;
    ta.value = value;
    ta.setAttribute('aria-label', '代码编辑器');
    if (onUpdate) ta.addEventListener('input', () => onUpdate(ta.value));
    host.appendChild(ta);
    return {
      getValue: () => ta.value,
      setValue: (v) => {
        ta.value = v;
      },
      setLanguage: (l) => {
        lang = l;
      },
      focus: () => ta.focus(),
      isFallback: true,
      error,
    };
  }

  // ---- 正常路径：CodeJar ----
  const el = document.createElement('div');
  el.className = 'editor';
  el.contentEditable = 'true';
  el.spellcheck = false;
  el.setAttribute('role', 'textbox');
  el.setAttribute('aria-multiline', 'true');
  el.setAttribute('aria-label', '代码编辑器');
  host.appendChild(el);

  const jar = CodeJar(el, (editor) => highlight(editor.textContent, lang), {
    tab: '  ',
    indentOn: /[([{]$/,
    addClosing: true,
    history: true,
  });
  jar.updateCode(value);
  if (onUpdate) jar.onUpdate((code) => onUpdate(code));

  return {
    getValue: () => jar.toString(),
    setValue: (v) => jar.updateCode(v),
    // 换语言要重画一遍：CodeJar 只在自己触发更新时才调 highlight
    setLanguage: (l) => {
      lang = l;
      jar.updateCode(jar.toString());
    },
    focus: () => el.focus(),
    isFallback: false,
    error: null,
  };
}
