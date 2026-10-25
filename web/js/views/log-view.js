/*
  web/js/views/log-view.js —— 完整的日志页（终端风格）。

  这页要解决的是「排查时到底发生了什么」这一个问题，所以它比 Dashboard 上那块
  摘要多做了四件事：

    1. **两条通道都能看**（插件日志 / 面板自身日志）。面板自己的日志以前
       根本进不了界面 —— 引擎为什么卡、插件为什么没加载，全都只能去翻 systemd。
    2. **按级别、按插件、按一次执行筛**。毫秒级任务一秒能刷几百行，
       没有过滤的实时日志等于没有。
    3. **关键字即时高亮**（客户端做的，不重连）。
    4. **能带走**：复制 / 下载 `.log` 和 `.jsonl`。

  ## 过滤为什么分两层

  级别 / 插件 / 通道走**服务端**（改了就重连）：1MB 缓冲里有好几 MB 文本，
  全推过来再让浏览器丢掉是白烧带宽和内存。

  关键字走**客户端**：它要即时反馈，每敲一个字重连一次是不可接受的。
  两层各管各的，互不干扰。
*/

import { getState } from '../api.js';
import { fetchJson } from '../api.js';
// 运行结果（成功 / 失败 / 已停止）的判定与文案在 status.js —— 与运行记录弹窗共用一份
import { runOutcome } from '../status.js';
import {
  LEVELS,
  esc,
  lineNode,
  makeRing,
  openLogStream,
  paintHighlight,
} from '../logstream.js';

/// 内部保留的脚本插件名不直接露给用户（和运行记录横幅同一口径）。
function pluginLabel(name) {
  return name === '__script__' ? '脚本任务' : name;
}

/** 行数上限（见 logstream.js 里 makeRing 的说明：服务端按字节限，这里按条数兜）。 */
const MAX_LINES = 2000;

/** 关键字输入的防抖：高亮要即时，但没必要每个字符都重扫 2000 个节点。 */
const HIGHLIGHT_DEBOUNCE_MS = 120;

const CHANNELS = [
  { key: 'all', label: '全部' },
  { key: 'plugin', label: '插件日志' },
  { key: 'panel', label: '面板日志' },
];

function fmtStamp(d = new Date()) {
  const p = (n) => String(n).padStart(2, '0');
  return (
    `${d.getFullYear()}${p(d.getMonth() + 1)}${p(d.getDate())}` +
    `-${p(d.getHours())}${p(d.getMinutes())}${p(d.getSeconds())}`
  );
}

function download(name, text, mime) {
  const blob = new Blob([text], { type: mime });
  const url = URL.createObjectURL(blob);
  const a = document.createElement('a');
  a.href = url;
  a.download = name;
  document.body.appendChild(a);
  a.click();
  a.remove();
  // 立刻 revoke 在部分浏览器上会让下载失败（下载还没开始就把 URL 撤了）
  setTimeout(() => URL.revokeObjectURL(url), 10_000);
}

/**
 * @param {HTMLElement} root
 * @param {{toast: Function, navigate: Function}} ctx
 * @param {{plugin?: string, run?: string}} [params] 从 hash 带进来的初始过滤
 */
export async function render(root, ctx, params = {}) {
  // 插件名的候选值：来自 /api/state。拿不到就算了（只是个 datalist 提示）。
  let pluginNames = [];
  const st = await getState();
  if (st.ok) {
    pluginNames = (st.plugins || []).map((p) => p.name);
  }

  root.innerHTML = `
    <div class="section">
      <div class="section-head">
        <h2 class="section-title">日志</h2>
        <span class="badge running" id="log-conn">连接中…</span>
        <span class="hint" id="log-note"></span>
        <div class="card-actions">
          <button class="btn ghost" id="log-pause">暂停</button>
          <button class="btn ghost" id="log-copy">复制</button>
          <button class="btn ghost" id="log-dl-txt">下载 .log</button>
          <button class="btn ghost" id="log-dl-json">下载 .jsonl</button>
          <button class="btn ghost" id="log-clear">清空视图</button>
        </div>
      </div>

      <div id="run-banner"></div>

      <p class="err-text" id="log-err" role="alert"></p>

      <div class="log-toolbar">
        <label class="tool">通道
          <!-- ⚠ 这些控件必须带上面板自己的类（「select」/「input」）——
               面板的输入框观感挂在类上，没类就拿浏览器默认样式，
               在深色主题下几乎看不出"这里能点/能输入"（用户实测：看不出那是输入框）。
               ⚠ 本段是**模板串里的 HTML 注释**：一个反引号都不能出现，写了会把模板串截断
                 （我这次就是这么把整个日志页弄挂的 —— 页面卡在"连接中…"，控制台里是
                  语法错误。面板的静态检查当时没红，因为那种截断恰好还能被解析）。 -->
          <select class="select" id="f-channel" style="width:auto">
            ${CHANNELS.map((c) => `<option value="${c.key}">${c.label}</option>`).join('')}
          </select>
        </label>

        <span class="tool levels">级别
          <span class="log-levels">
            ${LEVELS.map(
              (l) =>
                `<label class="chip"><input type="checkbox" data-level="${l}"${
                  l === 'debug' ? '' : ' checked'
                }>${l}</label>`,
            ).join('')}
          </span>
        </span>

        <label class="tool">插件
          <!-- 这里是**真 select**，不再是「输入框 + datalist」。
               原来那套的毛病（用户实测）：点一下**什么都不弹**（datalist 只在按方向键、
               或点那个小箭头时才开），看着像 select、用起来不像；弹层还是浏览器原生的，
               跟页面设计不是一套。面板自己的写法就是 select + 「select」类
               （见 views/task-editor.js 的插件选择器），这里照它。
               面板自己的日志没有 plugin 名，是按 scope 分的 —— 所以下面给一组 scope，
               与「通道 = 面板日志」配合用。
               ⚠ 同上：这段里不许出现反引号。 -->
          <select class="select" id="f-plugin" style="width:auto">
            <option value="">全部插件</option>
            ${pluginNames.map((n) => `<option value="${esc(n)}">${esc(pluginLabel(n))}</option>`).join('')}
            <optgroup label="面板日志（按 scope）">
              <option value="engine">engine</option>
              <option value="boot">boot</option>
              <option value="auth">auth</option>
              <option value="tick">tick</option>
              <option value="db">db</option>
            </optgroup>
          </select>
        </label>

        <label class="tool">关键字
          <!-- placeholder 要写**输入什么**（「搜关键字」），不是写这个控件的行为。
               原来写的是「即时高亮，不重连」—— 那是一句说明，被当成了标签，
               再加上没有框，用户根本看不出这里能打字。行为挪到 title（悬停才看）。 -->
          <input class="input" id="f-q" placeholder="搜关键字" title="即时高亮，不重连">
        </label>

        <label class="chip" title="关掉就不会跟着新日志滚动 —— 想翻旧行时很有用">
          <input type="checkbox" id="f-follow" checked>自动贴底
        </label>

        ${
          params.run
            ? `<span class="badge warning" title="只看这一次执行（run 标签）">
                 运行 ${esc(params.run)}
                 <button class="btn ghost" id="f-run-clear" style="margin-left:6px"
                         aria-label="清除运行过滤">×</button>
               </span>`
            : ''
        }
      </div>

      <div class="console console-tall" id="log-box" tabindex="0" aria-label="日志内容"></div>

      <p class="hint" id="log-foot">
        快捷键：<kbd>Ctrl</kbd>+<kbd>F</kbd> 交给浏览器搜页内（也可以直接用上面的关键字框）。
        日志按天落盘（<span class="mono">logs/plugin-YYYY-MM-DD.jsonl</span>），保留 7 天 ——
        面板外的 <span class="mono">jq</span> / <span class="mono">grep</span> 读的也是它。
      </p>
    </div>`;

  const box = root.querySelector('#log-box');
  const note = root.querySelector('#log-note');
  const conn = root.querySelector('#log-conn');
  const followEl = root.querySelector('#f-follow');
  const qEl = root.querySelector('#f-q');
  const pluginEl = root.querySelector('#f-plugin');
  const channelEl = root.querySelector('#f-channel');
  const errEl = root.querySelector('#log-err');

  /** 页面级的错误行（role=alert）。传空串 = 收起（空元素高度 0，不占版面）。 */
  function showErr(msg) {
    if (errEl) errEl.textContent = msg;
  }

  // 深链带进来的插件名不一定在候选里（比如那个插件已经被删了）。
  // 补一个 option 再选中 —— 不补的话 select 会**静默**回到「全部插件」，
  // 而 URL 里还写着 `plugin=xxx`，用户以为筛选生效了、其实没有。
  if (params.plugin) {
    if (![...pluginEl.options].some((o) => o.value === params.plugin)) {
      pluginEl.appendChild(new Option(pluginLabel(params.plugin), params.plugin));
    }
    pluginEl.value = params.plugin;
  }
  if (params.channel) channelEl.value = params.channel;

  /** 当前勾选的级别集合。空集合 = 什么都不显示（有提示，不是静默）。 */
  const checkedLevels = () =>
    [...root.querySelectorAll('input[data-level]:checked')].map((i) => i.dataset.level);

  /** 服务端要的最低级别 = 勾选里的最低那档（先在服务端砍掉一批，省带宽）。 */
  const serverLevel = () => {
    const on = checkedLevels();
    if (!on.length) return 'debug';
    return LEVELS.find((l) => on.includes(l)) || 'info';
  };

  const ring = makeRing(MAX_LINES);
  let stream = null;
  let paused = false;
  let notice = null;
  let dropped = 0;
  let highlightTimer = null;
  // 运行视图：历史 JSONL 与实时 SSE 可能包含同一行（SSE 会回灌内存环），
  // 用这个集合去重。只在 params.run 存在时启用。
  const seen = new Set();
  // 快速切筛选 / 切页面时，晚到的历史响应不能覆盖新一轮的结果。
  let historyGen = 0;
  let disposed = false;

  /** 一行该不该显示：勾选的级别是**精确集合**，不是「最低档」。 */
  const visible = (el) => {
    const on = checkedLevels();
    if (!on.includes(el.dataset.level)) return false;
    const kw = qEl.value.trim().toLowerCase();
    if (kw && !el.dataset.text.includes(kw)) return false;
    return true;
  };

  function applyFilter() {
    let shown = 0;
    for (const el of box.children) {
      const ok = visible(el);
      el.hidden = !ok;
      if (ok) shown += 1;
    }
    updateNote(shown);
  }


  // ---- 运行记录横幅 ----
  //
  // 「日志是日志，运行记录是运行记录」：横幅里的东西（哪条任务、几点跑的、
  // 成没成、花了多久）来自**面板的 runs 表**，不来自日志。日志只是被它关联
  // 出来的那一次执行的输出。
  //
  // 于是"日志过期"是一个**能说清楚**的状态：记录在、日志没了。反过来
  // （记录没了、日志还在）在保留期设计上不会出现 —— 记录留 30 天，日志留 7 天。
  const LOG_KEEP_DAYS = 7;
  let runRecord = null;
  let runFetchDone = false;

  async function renderRunBanner() {
    const el = root.querySelector('#run-banner');
    if (!el) return;
    if (!params.run) {
      el.innerHTML = '';
      return;
    }
    // 同源请求自动带会话 cookie；这里原来塞的是 `window.__PANEL_TOKEN__`，
    // 而那个全局**从来没有定义过**（安全审计 L13）—— 一个永远为空的自定义头
    // 只会让后来的人以为「面板页是拿令牌鉴权的」。
    // fetchJson：401 跳登录、非 JSON 不抛（安全审计 M17）。
    // ⚠ 注意它返回的 ok 语义和 fetch 的 res.ok 一致，但响应体在 data 里。
    const res = await fetchJson(`/api/runs/${encodeURIComponent(params.run)}`);
    runFetchDone = true;
    if (!res.ok) {
      // 记录本身就不在（超过 30 天、或者手动拼的 id）—— 这和"日志过期"
      // 是两句不同的话，不能混
      el.innerHTML = `<div class="card run-banner warn">
        <span class="badge danger">没有这条运行记录</span>
        <span class="hint">运行 id <code>${esc(params.run)}</code> 不在面板的记录里
        （记录保留 30 天）。下面的日志仍按这个 id 过滤。</span>
      </div>`;
      return;
    }
    const { run } = res.data;
    runRecord = run;
    // 成功 / 失败 / 已停止：停止不是错误，横幅不能是红的（判定见 status.js）
    const out = runOutcome(run);
    const dur = run.duration_ms == null ? '—' : `${run.duration_ms} ms`;
    // 脚本任务在 jobs 表里挂的是内部保留插件名 `__script__`：直接印出来
    // 只会让人问「这个插件是什么」。按来源给它一个真名字，并链到脚本页 ——
    // 插件任务与脚本任务已经分页，不能再把脚本任务链到插件任务页。
    const isScript = run.plugin === '__script__';
    const who = isScript ? '脚本' : run.plugin;
    const taskHref = isScript
      ? '#/scripts'
      : `#/tasks?plugin=${encodeURIComponent(run.plugin)}`;
    const taskLabel = isScript ? '看脚本任务' : '看这条任务';
    el.innerHTML = `<div class="card run-banner">
      <span class="badge ${out.cls}">${out.text}</span>
      <span class="run-title">
        <strong>${esc(who)}</strong> / ${esc(run.task)}
        <span class="hint">${esc(run.kind === 'manual' ? '手动触发' : run.kind === 'cron' ? '调度触发' : run.kind)}</span>
      </span>
      <span class="hint">${esc(run.started_at)} · 耗时 ${esc(dur)}</span>
      ${
        out.note
          ? `<div class="${out.cls === 'danger' ? 'run-error' : 'run-note'}">${esc(out.note)}</div>`
          : ''
      }
      <div class="card-actions">
        <a class="btn ghost" href="${taskHref}">${taskLabel}</a>
        <button class="btn ghost" id="run-clear">退出运行视图</button>
      </div>
    </div>`;
    el.querySelector('#run-clear')?.addEventListener('click', () => {
      params.run = '';
      location.hash = `#/logs/plugin/${encodeURIComponent(params.plugin || run.plugin)}`;
      renderRunBanner();
      renderNote2();
    });
  }

  /// 运行视图下：日志一行都没来 → 说清楚是"过期了"还是"这次没打日志"。
  ///
  /// 判据是**运行时间 vs 日志保留期**，不是"文件在不在"：日志按天一个文件、
  /// 留 7 天，所以 7 天前的运行必然过期，7 天内的运行只要没被清理就还在。
  /// 用时间判而不是去问文件，是因为**文件可能被手工删掉**，那时候说"过期"
  /// 也是对的（对用户来说就是没了）。
  function runEmptyNotice() {
    if (!params.run || !runFetchDone || !runRecord) return null;
    if (ring.length > 0) return null;
    const started = Date.parse((runRecord.started_at || '').replace(' ', 'T'));
    if (Number.isNaN(started)) return null;
    const ageDays = (Date.now() - started) / 86_400_000;
    if (ageDays > LOG_KEEP_DAYS) {
      return `这次运行的日志已过期（日志保留 ${LOG_KEEP_DAYS} 天，运行记录保留 30 天）—— 记录还在，日志文件已经清理掉了。`;
    }
    return '这次运行没有产生日志（或者日志已被手工清理）。';
  }

  function renderNote2() {
    const extra = runEmptyNotice();
    if (extra) note.textContent = extra;
  }

  function updateNote(shown) {
    if (shown === undefined) {
      shown = [...box.children].filter((c) => !c.hidden).length;
    }
    const parts = [`显示 ${shown} 行 / 本页缓冲 ${ring.length} 行`];
    if (notice) {
      const b = notice.channel === 'panel' ? notice.panel : notice.plugin;
      if (notice.channel === 'all') {
        parts.push(
          `服务端环形缓冲：插件 ${notice.plugin.lines} 行 · 面板 ${notice.panel.lines} 行`,
        );
        dropped = notice.plugin.dropped + notice.panel.dropped;
      } else if (b) {
        parts.push(`服务端环形缓冲 ${b.lines} 行`);
        dropped = b.dropped;
      }
      if (dropped) {
        // 淘汰必须说出来：让人以为「日志是连续的」比丢日志更糟
        parts.push(`已淘汰 ${dropped} 行（上限 1 MB）`);
      }
      if (notice.lost_lines) {
        parts.push(`断线期间丢了 ${notice.lost_lines} 行`);
      }
    }
    note.textContent = parts.join(' · ');
    // 运行视图下，如果一行都没有，把"为什么没有"写在后面
    const extra = runEmptyNotice();
    if (extra) note.textContent = extra;
  }

  function append(entry, { follow = true } = {}) {
    // 运行视图：历史文件 + SSE 内存回灌可能重复同一条；按「运行 + 时间 + 类型 + 正文」
    // 去重。别的视图不去重 —— 同样的文本在不同毫秒出现是合法的。
    if (params.run) {
      const key = `${entry.run || ''}|${entry.ts}|${entry.type}|${entry.msg}`;
      if (seen.has(key)) return;
      seen.add(key);
    }
    ring.push(entry);
    if (paused) return; // 暂停时只进缓冲，不动 DOM（恢复时整段重画）
    // 批量上屏（安全审计 M19）：以前每来一行就 appendChild + 读 scrollHeight，
    // 后者**强制同步布局** —— 高频日志下每行都要重排一次，日志一多页面就卡。
    // 现在按帧合并：一帧最多一次 DOM 写入、一次布局。
    pending.push(entry);
    scheduleFlush();
  }

  const pending = [];
  let flushScheduled = false;
  function scheduleFlush() {
    if (flushScheduled) return;
    flushScheduled = true;
    requestAnimationFrame(flush);
  }
  function flush() {
    flushScheduled = false;
    if (!pending.length) return;
    const frag = document.createDocumentFragment();
    for (const e of pending) {
      const el = lineNode(e);
      el.hidden = !visible(el);
      if (qEl.value.trim()) paintHighlight(el, qEl.value.trim());
      frag.appendChild(el);
    }
    pending.length = 0;
    box.appendChild(frag);
    // 超上限：从头砍（同时砍 DOM，否则节点会一直涨）
    while (box.children.length > MAX_LINES) box.removeChild(box.firstChild);
    if (follow && followEl.checked) box.scrollTop = box.scrollHeight;
  }

  /** 恢复时按缓冲整段重画（暂停期间新来的行就是这么补上的，不会丢）。 */
  function repaint() {
    const frag = document.createDocumentFragment();
    const items = ring.lines().slice(-MAX_LINES);
    for (const e of items) {
      const el = lineNode(e);
      el.hidden = !visible(el);
      if (qEl.value.trim()) paintHighlight(el, qEl.value.trim());
      frag.appendChild(el);
    }
    box.replaceChildren(frag);
    if (followEl.checked) box.scrollTop = box.scrollHeight;
    applyFilter();
  }

  async function open() {
    const gen = ++historyGen;
    stream?.close();
    stream = null;
    // ⚠ 换筛选条件（通道 / 级别 / 插件）时必须**把屏幕上已有的行清掉**。
    //
    // 服务端的筛选只作用于**新推来的**行，而框里那 300 行是换条件之前渲染的 ——
    // 不清的话，用户看到的是"我选了某个插件，可框里还有一堆别的插件和面板日志"，
    // 结论自然是"这个筛选没用"。实测（把「插件」填成 mcloud-live、等 2.5 秒）：
    // 框里 106 行里仍有 30 行面板日志 + 9 行别的插件，只有新来的行是过滤过的。
    //
    // 只在**换条件**时清：断线重连走的是 EventSource 自己的退避，不经过这里，
    // 所以重连不会把已经看到的行抹掉（那是另一条底线：断线不该丢视野）。
    box.replaceChildren();
    ring.clear();
    seen.clear();
    updateNote(0);
    showErr(''); // 换条件重连 = 新一轮，上一次的报错不再挂着
    conn.className = 'badge running';
    conn.textContent = '连接中…';

    // 运行视图：先回看盘上的 JSONL。SSE 只回灌内存环，进程重启后旧运行的
    // 日志还在文件里、内存里却没有 —— 不读文件就会误报「没有产生日志」。
    if (params.run && channelEl.value !== 'panel') {
      const qs = new URLSearchParams({ run: params.run });
      const plugin = pluginEl.value.trim();
      if (plugin) qs.set('plugin', plugin);
      try {
        const res = await fetchJson(`/api/logs/history?${qs.toString()}`);
        if (gen !== historyGen || disposed) return;
        if (res.ok) {
          const data = res.data;
          for (const entry of data.lines || []) append(entry, { follow: false });
          if (followEl.checked) box.scrollTop = box.scrollHeight;
          updateNote();
        } else {
          // 非 2xx 也是一次失败，别让它静默过去（原来只判 res.ok，失败什么都不说）
          showErr(`读不到历史日志（HTTP ${res.status}），实时日志不受影响`);
        }
      } catch (e) {
        // 历史读不到不挡实时流：至少新日志还能看。但**不能静默** —— 用户看到的
        // 是「这次没有日志」，实际是请求挂了；这两句意思完全不同。写进 role=alert，
        // 屏幕阅读器也听得到。
        showErr(`读不到历史日志（实时日志不受影响）：${(e && e.message) || '请求失败'}`);
      }
      if (gen !== historyGen || disposed) return;
    }

    stream = openLogStream({      channel: channelEl.value,
      level: serverLevel(),
      plugin: pluginEl.value.trim(),
      run: params.run || '',
      onNotice(n) {
        if (n.resumed) {
          note.textContent = `断线期间丢了 ${n.lost_lines} 行（已被 1 MB 缓冲淘汰）`;
          return;
        }
        notice = n;
        updateNote();
      },
      onLine: append,
      onState(s) {
        if (s === 'open') {
          conn.className = 'badge success';
          conn.textContent = '实时';
          conn.title = '服务端主动推送；断线会自动重连，并从断点续传';
        } else {
          // EventSource 自己会退避重连 —— 这里只是把它说出来
          conn.className = 'badge warning';
          conn.textContent = '重连中…';
          conn.title = '连接断了，浏览器正在自动重连（会带上断点，不会漏行）';
        }
      },
    });
  }

  /* ---------------------------------------------------------- 交互 */

  followEl.addEventListener('change', () => {
    if (followEl.checked) box.scrollTop = box.scrollHeight;
  });

  // 用户往上翻 → 自动关掉贴底；翻回底部 → 自动打开。
  // 这是 tail 的惯常行为：手动翻看旧行时被新日志拽走是最讨厌的。
  box.addEventListener('scroll', () => {
    const atBottom = box.scrollTop + box.clientHeight >= box.scrollHeight - 8;
    if (atBottom !== followEl.checked) followEl.checked = atBottom;
  });

  // 级别：勾选变化 → 客户端精确过滤 + 服务端重连（拿更低的档来少传点数据）
  root.querySelectorAll('input[data-level]').forEach((i) =>
    i.addEventListener('change', () => {
      if (!checkedLevels().length) {
        // 一个都不勾 = 什么都看不到，这几乎肯定是误操作。挡住并把这一档还回去。
        i.checked = true;
        ctx.toast('至少勾一个级别', 'bad');
      }
      applyFilter();
      open();
    }),
  );

  channelEl.addEventListener('change', open);

  // 插件名走服务端过滤 → 重连。原来这里是「输入框 + 防抖 400ms」（因为每敲一个字
  // 都重连太狠）；现在它是 select：选一下就是一次明确的意图，不需要防抖。
  pluginEl.addEventListener('change', open);

  // 关键字走客户端：即时，不重连
  qEl.addEventListener('input', () => {
    clearTimeout(highlightTimer);
    highlightTimer = setTimeout(() => {
      const kw = qEl.value.trim();
      for (const el of box.children) {
        el.hidden = !visible(el);
        paintHighlight(el, kw);
      }
      updateNote();
    }, HIGHLIGHT_DEBOUNCE_MS);
  });

  root.querySelector('#f-run-clear')?.addEventListener('click', () => {
    params.run = '';
    ctx.navigate('#/logs');
  });

  root.querySelector('#log-pause').addEventListener('click', (ev) => {
    paused = !paused;
    ev.target.textContent = paused ? '继续' : '暂停';
    if (!paused) repaint();
  });

  // 运行视图：先把"这是哪一次运行"显示出来。**不 await** —— 横幅是附加信息，
  // 等它会把日志流推迟一个往返；先连上流、横幅随后补上。
  renderRunBanner().then(() => updateNote());

  root.querySelector('#log-clear').addEventListener('click', () => {
    // 只清这一页。服务端缓冲（以及已经落盘的文件）不动 —— 说清楚，
    // 否则用户会以为「清空」等于删日志。
    box.replaceChildren();
    ring.clear();
    seen.clear();
    updateNote(0);
    ctx.toast('已清空视图（服务端缓冲与落盘文件不受影响）', 'ok');
  });

  root.querySelector('#log-copy').addEventListener('click', async () => {
    const text = visibleLines().join('\n');
    if (!text) {
      ctx.toast('没有可复制的内容', 'bad');
      return;
    }
    try {
      await navigator.clipboard.writeText(text);
      ctx.toast(`已复制 ${visibleLines().length} 行`, 'ok');
    } catch {
      // http 页面（非安全上下文）下 clipboard API 不可用，退回选中 + 让用户自己按
      selectBox();
      ctx.toast('浏览器不允许自动复制，已帮你全选，按 Ctrl/Cmd+C', 'bad');
    }
  });

  /** 可见行的**原始文本**（与服务端 `line` 逐字节一致 → 和文件里的一样）。 */
  function visibleLines() {
    const out = [];
    for (const el of box.children) {
      if (!el.hidden) out.push(el.dataset.line || el.textContent);
    }
    return out;
  }

  function selectBox() {
    const r = document.createRange();
    r.selectNodeContents(box);
    const sel = window.getSelection();
    sel.removeAllRanges();
    sel.addRange(r);
  }

  root.querySelector('#log-dl-txt').addEventListener('click', () => {
    const lines = visibleLines();
    if (!lines.length) {
      ctx.toast('没有可下载的内容', 'bad');
      return;
    }
    // .log 是纯文本行（与终端/文件里看到的一致）；.jsonl 带全部字段，供 jq 用
    download(`panel-logs-${fmtStamp()}.log`, lines.join('\n') + '\n', 'text/plain;charset=utf-8');
    ctx.toast(`已导出 ${lines.length} 行`, 'ok');
  });

  root.querySelector('#log-dl-json').addEventListener('click', () => {
    const lines = ring.lines();
    if (!lines.length) {
      ctx.toast('没有可下载的内容', 'bad');
      return;
    }
    const body = lines.map((e) => JSON.stringify(e)).join('\n') + '\n';
    download(`panel-logs-${fmtStamp()}.jsonl`, body, 'application/x-ndjson;charset=utf-8');
    ctx.toast(`已导出 ${lines.length} 行（JSONL，可用 jq 筛）`, 'ok');
  });

  open();

  // 统计行的定时刷新。
  //
  // 为什么不挂在每一条新日志上：日志可能一秒来几百条，每条都算一次
  // 「可见行数」就要遍历一遍 DOM（2000 个节点）—— 白烧 CPU。
  // 一秒刷一次，读数足够新，代价可以忽略。
  const noteTimer = setInterval(() => {
    if (!document.body.contains(box)) return;
    updateNote();
  }, 1000);

  // 路由切走时由 app.js 调用：必须把 EventSource 关掉，
  // 否则每切一次页面就多留一条长连接（服务端那边一直挂着）。
  return () => {
    disposed = true;
    historyGen++;
    stream?.close();
    clearInterval(noteTimer);
    clearTimeout(highlightTimer);
  };
}
