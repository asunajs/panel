/*
  web/js/logstream.js —— 实时日志的 SSE 客户端 + 渲染工具。

  为什么不再轮询：旧版 Dashboard 每 3 秒拉一次 `/api/state`（连整包状态一起，
  只为看那 40 行日志），延迟 3 秒、流量浪费，而且 /api/state 里只有插件日志。
  现在服务端**主动推**（`/api/logs/stream`），面板自己的日志也在里面。

  ## 断线续传是浏览器帮我们做的

  EventSource 会自动重连，并带上 `Last-Event-ID`。服务端那个 id 是
  `"{plugin游标}-{panel游标}"`，于是重连时能精确地从「已经收到哪」接着推 ——
  所以**这里不要自己实现重连**（自己写一个反而会把浏览器那份正确的 id 弄丢）。
  我们只做一件事：把连接状态显示出来，让用户知道现在是「断了在重连」。

  ## 两条通道、一个流

  `channel=all` 时服务端把插件日志和面板日志按时间合流后一起推。
  每条事件里带 `plugin` 字段（面板日志是 null），前端据此区分来源。
*/

/** 八档语义 type → 样式类。与 log/mod.rs 的 `Type` 一一对应。 */
export const TYPE_CLASS = {
  debug: 't-debug',
  info: 't-info',
  start: 't-start',
  success: 't-success',
  fail: 't-fail',
  warn: 't-warn',
  error: 't-error',
  fatal: 't-fatal',
};

/** 五档过滤级别，从低到高。`start/success/fail` 归在 info 档（见 log/mod.rs）。 */
export const LEVELS = ['debug', 'info', 'warn', 'error', 'fatal'];

export function typeClass(type) {
  return TYPE_CLASS[type] || 't-info';
}

/**
 * 打开一条日志流。
 *
 * @param {object} opts
 * @param {'all'|'plugin'|'panel'} [opts.channel]
 * @param {string} [opts.level]   最低级别（服务端过滤；默认 info）
 * @param {string} [opts.plugin]  只看某个插件 / scope
 * @param {string} [opts.run]     只看某一次执行
 * @param {(entry: object) => void} [opts.onLine]
 * @param {(notice: object) => void} [opts.onNotice]
 * @param {(state: 'open'|'error') => void} [opts.onState]
 * @returns {{close: () => void}}
 */
export function openLogStream(opts = {}) {
  const qs = new URLSearchParams({ channel: opts.channel || 'all', level: opts.level || 'info' });
  if (opts.plugin) qs.set('plugin', opts.plugin);
  if (opts.run) qs.set('run', opts.run);

  let es = null;
  let closed = false;

  const connect = () => {
    if (closed) return;
    es = new EventSource(`/api/logs/stream?${qs.toString()}`);

    // 服务端连接先推一条 notice（缓冲概况：留了多少行、淘汰了多少行）。
    // 「日志怎么少了一段」必须有解释 —— 这条就是那个解释。
    es.addEventListener('notice', (e) => {
      try {
        opts.onNotice?.(JSON.parse(e.data));
      } catch {
        /* 坏事件不该让整条流断掉 */
      }
    });

    es.addEventListener('log', (e) => {
      try {
        opts.onLine?.(JSON.parse(e.data));
      } catch {
        /* 同上 */
      }
    });

    es.addEventListener('open', () => opts.onState?.('open'));
    // error 里**不关连接**：EventSource 会自己退避重连，我们只报告状态
    es.addEventListener('error', () => opts.onState?.('error'));
  };

  connect();

  return {
    close() {
      closed = true;
      es?.close();
      es = null;
    },
  };
}

/* ------------------------------------------------------------------ 渲染 */

export function esc(s) {
  return String(s ?? '').replace(/[&<>"']/g, (c) =>
    ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' })[c],
  );
}

/**
 * 一条日志 → DOM 节点。
 *
 * ⚠ 用**结构化字段**拼行（而不是直接用服务端的 `line` 字符串）：
 * 那样才能把时间戳压暗、给 scope 上色。`entry.line` 仍然保留 ——
 * 复制 / 下载走它，于是「面板上复制的文本」与「JSONL / 终端里的一模一样」。
 */
export function lineNode(entry) {
  const el = document.createElement('span');
  el.className = `logline ${typeClass(entry.type)}`;
  el.dataset.type = entry.type;
  el.dataset.level = entry.level;
  // plugin 只有插件通道有；面板通道退化成 scope（boot / engine:0 / auth …）
  el.dataset.scope = entry.scope || '';
  if (entry.plugin) el.dataset.plugin = entry.plugin;
  if (entry.run) el.dataset.run = entry.run;
  // 搜索用的原始文本（小写），避免每次输入都重新拼一遍
  el.dataset.text = String(entry.line || '').toLowerCase();
  // 复制 / 下载走它 —— 于是「面板上导出的文本」与「JSONL / 终端里的一模一样」
  el.dataset.line = entry.line || '';

  el.innerHTML =
    `<span class="log-ts">${esc(entry.ts)}</span> ` +
    `<span class="log-lv">[${esc(entry.type)}]</span> ` +
    `<span class="log-src">[${esc(entry.plugin || entry.scope)}]</span> ` +
    `<span class="log-msg">${esc(entry.msg)}</span>`;
  return el;
}

/** 把关键字在消息文本里高亮出来（只改 msg 那一段的 html）。 */
export function paintHighlight(el, kw) {
  const msgEl = el.querySelector('.log-msg');
  if (!msgEl) return;
  const orig = el.dataset.msg || msgEl.textContent;
  if (!el.dataset.msg) el.dataset.msg = orig;
  if (!kw) {
    msgEl.textContent = orig;
    return;
  }
  // 用 textContent 分片拼，不碰正则 —— 关键字里的 `.` `(` 之类不能被当元字符
  const lower = orig.toLowerCase();
  const k = kw.toLowerCase();
  let out = '';
  let i = 0;
  for (;;) {
    const at = lower.indexOf(k, i);
    if (at < 0) {
      out += esc(orig.slice(i));
      break;
    }
    out += esc(orig.slice(i, at)) + '<mark>' + esc(orig.substr(at, kw.length)) + '</mark>';
    i = at + kw.length;
  }
  msgEl.innerHTML = out;
}

/**
 * 环形缓冲：面板端只保留最近 `cap` 行。
 *
 * 为什么在前端也要限：服务端已经按 1MB 限了，但那是**字节**，
 * 一条超长日志能顶几千条短的；而 DOM 节点的代价是按**条数**算的。
 * 低配盒子上几千个节点就会卡，所以这里再按条数兜一道。
 */
export function makeRing(cap = 2000) {
  const buf = [];
  return {
    push(x) {
      buf.push(x);
      if (buf.length > cap) buf.shift();
      return buf.length > cap;
    },
    get length() {
      return buf.length;
    },
    lines: () => buf,
    clear: () => buf.splice(0, buf.length),
  };
}
