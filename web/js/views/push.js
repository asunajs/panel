/*
  web/js/views/push.js —— 面板级推送（#/push）。

  任务跑完 / 熔断时，面板把结果发到用户配好的渠道。这一页只管三件事：
  开不开、发到哪儿、刚才到底发出去没有。

  ---- 两条必须守住的边界 ----

  1. **已存的值不回填**。`GET /api/push` 的 `fields` 里是掩码
     （`abc…xyz（共 32 字符）`），不是明文 —— 界面拿不到，也不该拿到。所以保存走
     `{set, remove}` 的增量形状：`set` 里只放这次真填了值的字段，留空 = 不改。
     想"整份发回去"就必然要把秘密送进浏览器，而秘密本来就不该到这儿来。
  2. **设置与渠道分开保存**。总开关 / 触发条件是一组，每个渠道的凭据是另一组。
     混成一个保存按钮的话，改一次总开关就得把十个渠道的掩码一起发回去 ——
     而掩码是不能回传的（同第 1 条）。

  ---- 排版上的取舍 ----

  10 个渠道 × 每个 1~7 个字段 = 三十几个输入框，全铺出来就是一屏表单。所以渠道
  一行一个、点「填 / 改」才展开那一个（同时只展开一个）。渠道的**顺序照后端给的
  `channels` 数组**，前端不写死 —— 顺序是配置的一部分，抄一份就会漂。
*/

import { getPush, savePush, savePushTask, testPush } from '../api.js';

function esc(s) {
  return String(s ?? '').replace(/[&<>"']/g, (c) =>
    ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' })[c],
  );
}

/**
 * 触发条件：键名与 `settings` 里的一一对应。第三个元素是给用户看的理由
 * （挂在 title 上）—— 只写开关名的话，用户不知道"熔断"和"失败"为什么要分开。
 */
const TRIGGERS = [
  ['on_failure', '失败时通知', '任务抛错时发一条。默认开。'],
  ['on_success', '成功时通知', '正常跑完也发。默认关：每天都成功的任务会把消息淹掉。'],
  ['on_breaker', '熔断时通知', '连续失败被面板停掉时发一条 —— 这一条最该开着。'],
];

/** 投递结果三档（`results[].outcome`）。认不出来的原样显示，不猜。 */
const OUTCOME = {
  sent: { cls: 'success', text: '已发送' },
  skipped: { cls: 'warning', text: '跳过' },
  failed: { cls: 'failed', text: '失败' },
};

/** `recent[].kind` → 中文。只挂在 title 上，不占列宽（列是时间/任务/渠道/结果/note）。 */
const KIND = { run: '任务跑完', breaker: '熔断', test: '手动测试' };

/**
 * customPost 的这两个字段是 **JSON 文本**：用 textarea，保存前解析成对象。
 * 键名按契约写死在这里 —— 它们是这个渠道的数据形状，不是展示文案。
 */
const JSON_KEYS = new Set(['headers', 'body']);

/**
 * 掩码是给人看的短串（`abc…xyz（共 32 字符）`）。但自定义请求在后端是**数组**，
 * 它那份"已配"的摘要可能是结构化值 —— 直接插进模板会印出 [object Object]，
 * 用户看到的就是"这行坏了"。所以非字符串先 JSON 化，再截断。
 */
function maskText(v) {
  const s = typeof v === 'string' || typeof v === 'number' ? String(v) : JSON.stringify(v);
  return s && s.length > 120 ? `${s.slice(0, 120)}…` : s || '';
}

/**
 * @param {HTMLElement} root
 * @param {{toast: Function}} ctx
 */
export async function renderPush(root, ctx) {
  let data = null;
  let loadError = '';
  let editing = ''; // 展开着的是哪个渠道（同时只展开一个）
  let testResult = null;
  let testError = '';
  let settingsError = '';

  const configured = () => (data && Array.isArray(data.configured) ? data.configured : []);
  const channels = () => (data && Array.isArray(data.channels) ? data.channels : []);
  const masks = () => (data && data.fields) || {};
  const recent = () => (data && Array.isArray(data.recent) ? data.recent : []);
  const findChan = (id) => channels().find((c) => c.id === id) || null;

  function fail(errEl, msg) {
    if (errEl) errEl.textContent = msg;
    ctx.toast(msg, 'bad');
  }

  // ---------------------------------------------------------------- 各区块

  /** 顶部一行：现在会不会推。这是这一页唯一必须一眼看到的事。 */
  function heroHtml() {
    const on = !!data.enabled;
    const n = configured().length;
    const today = Number(data.today) || 0;
    return `<div class="card">
      <div class="row" style="align-items:center;gap:var(--sp-2)">
        <span class="badge ${on ? 'success' : 'idle'}">${on ? '已开启' : '未开启'}</span>
        <span>${on ? `配了 ${n} 个渠道 · 今天已发 ${today} 条` : '任务跑完不会发推送'}</span>
      </div>
      ${
        // 「开着但一个渠道都没配」要单独说破：光看「已开启」会以为没问题
        on && n === 0
          ? '<p class="err-text" style="margin-top:var(--sp-2)">开着，但一个渠道都没配 —— 现在不会真的发出任何消息。</p>'
          : ''
      }
      <p class="hint" style="margin-top:var(--sp-2)">
        任务跑完 / 熔断时，面板把结果发到下面配好的渠道。触发条件与总开关在「触发设置」里。
      </p>
    </div>`;
  }

  /** 一个渠道一行：名字 + 说明 + 掩码 + 状态 + 按钮。 */
  function channelRow(ch) {
    const on = configured().includes(ch.id);
    const mask = on ? masks()[ch.id] : null;
    const maskLine =
      mask && typeof mask === 'object'
        ? `<span class="hint mono">${Object.entries(mask)
            .map(([k, v]) => `${esc(k)}=${esc(maskText(v))}`)
            .join('　')}</span>`
        : '';
    const expanded = editing === ch.id;
    return `<div class="push-chan" data-chan="${esc(ch.id)}">
      <div class="push-chan-main">
        <span class="field-label">${esc(ch.label || ch.id)}</span>
        <span class="hint">${esc(ch.hint || '')}</span>
        ${maskLine}
      </div>
      <span class="badge ${on ? 'success' : 'idle'}">${on ? '已配' : '未配'}</span>
      <div class="card-actions">
        <button class="btn ${on ? 'ghost' : ''}" data-edit="${esc(ch.id)}"
          aria-expanded="${expanded}">${expanded ? '收起' : on ? '改' : '填'}</button>
        ${on ? `<button class="btn ghost" data-remove="${esc(ch.id)}">不用这个</button>` : ''}
      </div>
    </div>
    ${expanded ? fieldsBlock(ch) : ''}`;
  }

  /** 展开的字段区：只画这一个渠道的 fields（规格由后端给，前端不抄）。 */
  function fieldsBlock(ch) {
    const inputs = (ch.fields || [])
      .map((f) => {
        const id = `pf-${ch.id}-${f.key}`;
        const common = `id="${id}" data-field="${esc(f.key)}" data-numeric="${f.numeric ? '1' : '0'}"`;
        const label = `<label class="field-label" for="${id}">${esc(f.label || f.key)}${
          f.secret ? '（留空 = 不改）' : ''
        }</label>`;
        let control;
        if (JSON_KEYS.has(f.key)) {
          control = `<textarea class="textarea mono" rows="4" ${common} spellcheck="false"
            placeholder="（JSON 对象，例如 {}）"></textarea>`;
        } else {
          // secret 用口令框：既是肩窥防护，也让浏览器别去自动填充
          const type = f.secret ? 'password' : f.numeric ? 'number' : 'text';
          // 非 secret 字段不给 placeholder：标签已经写在上面了，
          // 再放一遍同样的字只是噪音（secret 的"留空 = 不改"是有用的信息）
          control = `<input class="input ${f.numeric ? 'mono' : ''}" type="${type}" ${common}
            placeholder="${f.secret ? '留空 = 不改' : ''}"
            autocomplete="off" spellcheck="false">`;
        }
        return `<div class="field ${JSON_KEYS.has(f.key) ? 'push-wide' : ''}">${label}${control}</div>`;
      })
      .join('');

    return `<div class="push-fields" data-fields="${esc(ch.id)}">
      ${inputs}
      <p class="hint push-wide">已经填过的项留空 = 不改（界面拿不到已存的值，只有掩码）。${
        ch.id === 'customPost'
          ? '自定义请求整条替换：改的时候把 url / method / headers / body 一起填全。'
          : ''
      }</p>
      <p class="err-text push-wide" data-chan-err></p>
      <div class="card-actions push-wide">
        <button class="btn primary" data-save-chan="${esc(ch.id)}">保存 ${esc(ch.label || ch.id)}</button>
      </div>
    </div>`;
  }

  /** 触发设置：总开关 + 三个触发条件 + 冷却 / 上限 / 标题。 */
  function settingsBlock() {
    const s = data.settings || {};
    const chips = TRIGGERS.map(
      ([key, label, why]) =>
        `<label class="chip" title="${esc(why)}">
          <input type="checkbox" data-set="${key}" ${s[key] ? 'checked' : ''}>${esc(label)}
        </label>`,
    ).join('');
    return `<div class="row" style="align-items:center;gap:var(--sp-2)">
        <label class="chip"><input type="checkbox" data-set="enabled" ${
          data.enabled ? 'checked' : ''
        }>开启推送</label>
        <span class="hint">总开关。关掉之后什么都不发，配置留着。</span>
      </div>
      <div class="divider"></div>
      <div class="row" style="gap:var(--sp-2)">${chips}</div>
      <p class="hint" style="margin:var(--sp-2) 0 var(--sp-3)">
        失败 / 熔断默认开，成功默认关 —— 一个每天都成功的任务，成功通知只会把有用的消息淹掉。
      </p>
      <div class="grid cols-2">
        <div class="field">
          <label class="field-label" for="pf-title">标题</label>
          <input class="input" id="pf-title" data-set="title" value="${esc(data.title || '面板任务')}"
            placeholder="面板任务">
          <span class="hint">消息标题，默认「面板任务」。</span>
        </div>
        <div class="field">
          <label class="field-label" for="pf-cool">冷却秒数</label>
          <input class="input mono" id="pf-cool" type="number" min="0" data-set="cooldown_secs"
            value="${esc(s.cooldown_secs ?? 600)}">
          <span class="hint">同一条任务在这个时间内只发一条；0 = 不冷却。</span>
        </div>
        <div class="field">
          <label class="field-label" for="pf-cap">每日上限</label>
          <input class="input mono" id="pf-cap" type="number" min="0" data-set="daily_cap"
            value="${esc(s.daily_cap ?? 100)}">
          <span class="hint">一天最多发几条（各渠道合计）；0 = 不限。</span>
        </div>
      </div>
      <p class="err-text" style="margin-top:var(--sp-2)" data-settings-err>${esc(settingsError)}</p>`;
  }

  /** 「发一条试试」的结果：逐条显示成功 / 跳过 / 失败 + 原因。 */
  // ---- 按任务 ----

  function tasks() {
    return Array.isArray(data && data.tasks) ? data.tasks : [];
  }

  function modes() {
    return Array.isArray(data && data.modes) && data.modes.length
      ? data.modes
      : [{ id: 'inherit', label: '跟随全局' }];
  }

  function ruleKey(t) {
    return t.plugin + '::' + t.name;
  }

  function ruleOf(t) {
    const r = (data && data.task_rules && data.task_rules[ruleKey(t)]) || {};
    return {
      mode: r.mode || 'inherit',
      channels: Array.isArray(r.channels) ? r.channels : [],
    };
  }

  function taskLabel(t) {
    return t.plugin === '__script__' ? '脚本任务 / ' + t.name : t.plugin + ' / ' + t.name;
  }

  /** 每条任务一行：模式下拉 + 渠道 chips。改一项就存一项，不设「保存」按钮。 */
  function tasksBlock() {
    const list = tasks();
    if (!list.length) {
      return '<p class="hint">还没有任务。任务建好（或脚本目录里有文件）之后，这里会出现它。</p>';
    }
    const conf = configured();
    return list
      .map((t) => {
        const key = ruleKey(t);
        const r = ruleOf(t);
        const modeOptions = modes()
          .map(
            (m) =>
              '<option value="' +
              esc(m.id) +
              '"' +
              (m.id === r.mode ? ' selected' : '') +
              '>' +
              esc(m.label) +
              '</option>',
          )
          .join('');
        const chips = conf.length
          ? '<div class="push-chips">' +
            conf
              .map((cid) => {
                const meta = channels().find((c) => c.id === cid);
                const on = r.channels.includes(cid);
                return (
                  '<label class="chip"><input type="checkbox" data-task-chan="' +
                  esc(key) +
                  '" data-channel="' +
                  esc(cid) +
                  '"' +
                  (on ? ' checked' : '') +
                  '>' +
                  esc((meta && (meta.label || meta.id)) || cid) +
                  '</label>'
                );
              })
              .join('') +
            '</div>'
          : '';
        const dead = r.channels.length > 0 && !r.channels.some((c) => conf.includes(c));
        const badge = t.status === 'tripped' ? '<span class="badge failed">已熔断</span>' : '';
        return (
          '<div class="push-task">' +
          '<div class="push-task-main">' +
          '<span class="field-label">' +
          esc(taskLabel(t)) +
          '</span>' +
          '<span class="hint">' +
          (t.plugin === '__script__' ? '脚本任务' : '插件任务') +
          '</span>' +
          badge +
          '</div>' +
          '<select class="input push-task-mode" data-task-mode="' +
          esc(key) +
          '" aria-label="推送模式">' +
          modeOptions +
          '</select>' +
          chips +
          (dead
            ? '<span class="err-text">指定的渠道都没配了 —— 这条任务现在推不出去</span>'
            : '') +
          '</div>'
        );
      })
      .join('');
  }

  /** 改一项存一项。成功后只更新本地快照，不重画：连续勾渠道时页面不跳。 */
  async function saveTaskRule(t, mode, channels) {
    const key = ruleKey(t);
    const res = await savePushTask({ plugin: t.plugin, task: t.name, mode, channels });
    if (!res || !res.ok) {
      ctx.toast('没保存：' + ((res && res.error) || '未知错误'), 'bad');
      return;
    }
    if (!data.task_rules) data.task_rules = {};
    if (res.removed) delete data.task_rules[key];
    else
      data.task_rules[key] = {
        mode: res.mode || mode,
        channels: Array.isArray(res.channels) ? res.channels : channels,
      };
    ctx.toast('已保存 ' + taskLabel(t), 'ok');
  }

  function testBlock() {
    if (testError) return `<p class="err-text">没发出去：${esc(testError)}</p>`;
    if (!testResult) {
      return `<p class="hint">还没有试过。点「发一条试试」发一条 —— 收到了就说明渠道配对了；
        这一步把「配置错了」和「任务没跑到发送那一步」分开。</p>`;
    }
    const rows = Array.isArray(testResult.results) ? testResult.results : [];
    const head = `<p class="hint">${esc(testResult.line || '')}</p>`;
    if (!rows.length) return head;
    return `${head}<div class="stack">${rows
      .map((r) => {
        const o = OUTCOME[r.outcome] || { cls: 'idle', text: String(r.outcome ?? '未知') };
        return `<div class="push-delivery">
          <span class="mono">${esc(r.label || r.channel || '')}</span>
          <span class="badge ${o.cls}">${esc(o.text)}</span>
          <span class="${r.outcome === 'failed' ? 'err-text' : 'hint'}"
            style="flex:1 1 10rem;min-width:0">${esc(r.note || '（没有说明）')}</span>
        </div>`;
      })
      .join('')}</div>`;
  }

  /** 最近投递：时间 / 任务 / 渠道 / 结果 / note，最多 20 条。 */
  function recentBlock() {
    const rows = recent().slice(0, 20);
    if (!rows.length) {
      return `<p class="empty">还没有投递记录。任务跑完 / 熔断，或者点「发一条试试」之后，
        这里会记下每一次投递。</p>`;
    }
    return rows
      .map((r) => {
        return `<div class="push-delivery">
          <span class="mono dim" style="flex:none">${esc(r.at || '—')}</span>
          <span class="mono" style="flex:1 1 12rem;min-width:0">${esc(r.plugin || '—')} / ${esc(
            r.task || '—',
          )}</span>
          <span class="badge idle">${esc(r.channel || '—')}</span>
          <span class="badge ${r.ok ? 'success' : 'failed'}"
            title="${esc(KIND[r.kind] || '')}">${r.ok ? '送达' : '失败'}</span>
          <span class="${r.ok ? 'hint' : 'err-text'}" style="flex:1 1 10rem;min-width:0">${esc(
            r.note || '',
          )}</span>
        </div>`;
      })
      .join('');
  }

  // ---------------------------------------------------------------- 画

  // 一屏铺完 10 个渠道 + 触发设置 + 按任务 + 试发 + 最近投递要滚好几屏，
  // 用户找不到「保存设置」在哪。拆成页签，每个只回答一个问题：发到哪 /
  // 什么时候发 / 哪条任务例外 / 刚才发出去没有。
  //
  // 实现上是**只藏不删**：paint() 仍把五块都画出来，页签只切 hidden。
  // 这样从「渠道」切到「触发设置」再切回来，没保存的输入还在 —— 比每次重画
  // 更不容易让用户白填一遍。
  const TABS = [
    ['channels', '渠道'],
    ['settings', '触发设置'],
    ['tasks', '按任务'],
    ['test', '试发与记录'],
  ];
  let tab = 'channels';

  function tabLabel(id, label) {
    if (id !== 'channels') return label;
    return label + ' ' + configured().length + '/' + channels().length;
  }

  function tabButtonHtml(id, label) {
    const active = tab === id;
    const controls =
      id === 'channels'
        ? 'push-sec-1'
        : id === 'settings'
          ? 'push-sec-2'
          : id === 'tasks'
            ? 'push-sec-3'
            : 'push-sec-4 push-sec-5';
    return (
      '<button class="btn ' +
      (active ? 'primary' : 'ghost') +
      '" type="button" role="tab" data-tab="' +
      id +
      '" aria-selected="' +
      active +
      '" aria-controls="' +
      controls +
      '">' +
      esc(tabLabel(id, label)) +
      '</button>'
    );
  }

  /** 把页签条挂到标题区末尾，并给各块补上 tabpanel 语义。每次 paint 都是新 DOM，挂一次即可。 */
  function mountTabs() {
    const first = root.querySelector(':scope > .section');
    if (!first) return;
    const bar = document.createElement('div');
    bar.className = 'push-tabs';
    bar.setAttribute('role', 'tablist');
    bar.setAttribute('aria-label', '推送设置');
    bar.innerHTML = TABS.map(([id, label]) => tabButtonHtml(id, label)).join('');
    first.appendChild(bar);
    [...root.querySelectorAll(':scope > .section')].forEach((s, i) => {
      if (i > 0) {
        s.id = 'push-sec-' + i;
        s.setAttribute('role', 'tabpanel');
      }
    });
    applyTab();
  }

  /** 只切显隐与选中态：不重画，所以其它页签里没保存的输入不会丢。 */
  function applyTab() {
    const show =
      tab === 'settings' ? [2] : tab === 'tasks' ? [3] : tab === 'test' ? [4, 5] : [1];
    [...root.querySelectorAll(':scope > .section')].forEach((s, i) => {
      if (i > 0) s.hidden = !show.includes(i);
    });
    for (const b of root.querySelectorAll('[data-tab]')) {
      const active = b.dataset.tab === tab;
      b.classList.toggle('primary', active);
      b.classList.toggle('ghost', !active);
      b.setAttribute('aria-selected', String(active));
    }
  }

  function paint() {
    if (!data) return;
    root.innerHTML = `
      <div class="section">
        <div class="section-head">
          <span class="section-title">推送</span>
          <div class="card-actions"><button class="btn ghost" data-refresh>刷新</button></div>
        </div>
        ${heroHtml()}
      </div>

      <div class="section">
        <div class="section-head">
          <span class="section-title">渠道</span>
        </div>
        <p class="hint" style="margin:0 0 var(--sp-2)">
          一行一个渠道，点「填 / 改」展开这一个（同时只展开一个）。保存只发这一个渠道的字段。
        </p>
        <div class="card">
          ${
            channels().length
              ? channels().map(channelRow).join('')
              : '<p class="hint">后端没有给出渠道清单。</p>'
          }
        </div>
      </div>

      <div class="section">
        <div class="section-head">
          <span class="section-title">触发设置</span>
          <div class="card-actions">
            <button class="btn primary" data-settings-save>保存设置</button>
          </div>
        </div>
        <div class="card" data-settings>${settingsBlock()}</div>
      </div>

      <div class="section">
        <div class="section-head">
          <span class="section-title">按任务</span>
        </div>
        <p class="hint" style="margin:0 0 var(--sp-2)">
          默认「跟随全局」。可以单独关掉某个话多的任务、只让它推成功，或只发指定渠道。
          渠道都不勾 = 用全部已配渠道。
        </p>
        <div class="card">${tasksBlock()}</div>
      </div>

      <div class="section">
        <div class="section-head">
          <span class="section-title">试一下</span>
          <div class="card-actions"><button class="btn" data-test>发一条试试</button></div>
        </div>
        <div class="card" data-test-box>${testBlock()}</div>
      </div>

      <div class="section">
        <div class="section-head">
          <span class="section-title">最近投递（${recent().length}）</span>
        </div>
        <div class="card">${recentBlock()}</div>
      </div>`;
    mountTabs();
  }

  function paintError() {
    root.innerHTML = `
      <div class="section">
        <div class="section-head">
          <span class="section-title">推送</span>
          <div class="card-actions"><button class="btn" data-refresh>重试</button></div>
        </div>
        <div class="card">
          <span class="badge failed">读不到推送配置</span>
          <p class="hint" style="margin-top:var(--sp-2)">${esc(loadError || '未知错误')}</p>
          <p class="hint">后端没起来，或者这个版本的二进制还没有 /api/push。</p>
        </div>
      </div>`;
  }

  /** 只重画测试结果那块：整页重画会把用户还没保存的设置表单清掉。 */
  function paintTestBox() {
    const box = root.querySelector('[data-test-box]');
    if (box) box.innerHTML = testBlock();
  }

  // ---------------------------------------------------------------- 动作

  /**
   * 读配置。`quiet` = 动作之后的静默刷新：失败了只用 toast 说一声，
   * 不把整页换成错误态（用户刚保存成功，页面却变成"读不到配置"会很莫名其妙）。
   */
  async function load(quiet = false) {
    const res = await getPush();
    if (!root.isConnected) return;
    if (!res || !res.ok) {
      const msg = (res && res.error) || '未知错误';
      if (quiet && data) {
        ctx.toast(`刷新失败：${msg}`, 'bad');
        return;
      }
      data = null;
      loadError = msg;
      paintError();
      return;
    }
    loadError = '';
    data = res;
    paint();
  }

  /** 保存**一个**渠道：只收集这个展开区里的字段，留空的一律不发。 */
  async function saveChannel(id, btn) {
    const ch = findChan(id);
    if (!ch) return;
    const box = [...root.querySelectorAll('[data-fields]')].find((el) => el.dataset.fields === id);
    if (!box) return;
    const errEl = box.querySelector('[data-chan-err]');
    if (errEl) errEl.textContent = '';

    const values = {};
    for (const el of box.querySelectorAll('[data-field]')) {
      const key = el.dataset.field;
      const raw = el.value.trim();
      if (raw === '') continue; // 留空 = 不改这一项
      if (el.dataset.numeric === '1') {
        const n = Number(raw);
        if (!Number.isFinite(n)) {
          fail(errEl, `「${key}」要填数字`);
          return;
        }
        values[key] = n;
      } else {
        values[key] = raw;
      }
    }
    if (Object.keys(values).length === 0) {
      fail(errEl, '一项都没填 —— 留空 = 不改，没有可保存的内容。');
      return;
    }

    // customPost 的两个 JSON 字段：解析失败就**不发请求**。后端只会回 400，
    // 而 400 的原因不会告诉用户是哪个字段、第几个字符写错了。
    if (id === 'customPost') {
      for (const key of ['headers', 'body']) {
        if (!(key in values)) continue;
        try {
          values[key] = JSON.parse(values[key]);
        } catch (e) {
          fail(errEl, `「${key}」不是合法 JSON：${(e && e.message) || '解析失败'}`);
          return;
        }
      }
      if (!values.url) {
        fail(errEl, '自定义请求至少要填 url（整条替换，改的时候要填全）。');
        return;
      }
    }

    btn.disabled = true;
    btn.textContent = '保存中…';
    // 自定义渠道后端存成**数组**：一个渠道就是数组里的一条。
    const payload = id === 'customPost' ? [values] : values;
    const res = await savePush({ set: { [id]: payload } });
    if (!root.isConnected) return;
    btn.disabled = false;
    btn.textContent = `保存 ${ch.label || ch.id}`;
    if (!res || !res.ok) {
      const msg = (res && res.error) || '未知错误';
      fail(errEl, `保存失败：${msg}`);
      return;
    }
    editing = ''; // 存完就收起来：掩码会变，展开着也看不出变化
    ctx.toast(`已保存 ${ch.label || ch.id}`, 'ok');
    await load(true);
  }

  /** 「不用这个」：显式 remove（不是把字段清空 —— 空值等于"不改"）。 */
  async function removeChannel(id, btn) {
    const ch = findChan(id);
    btn.disabled = true;
    btn.textContent = '处理中…';
    const res = await savePush({ remove: [id] });
    if (!root.isConnected) return;
    if (!res || !res.ok) {
      btn.disabled = false;
      btn.textContent = '不用这个';
      ctx.toast(`没删掉：${(res && res.error) || '未知错误'}`, 'bad');
      return;
    }
    editing = '';
    ctx.toast(`已停用 ${(ch && (ch.label || ch.id)) || id}`, 'ok');
    await load(true);
  }

  /** 保存触发设置。**不碰渠道**（渠道各有各的保存）。 */
  async function saveSettings(btn) {
    const box = root.querySelector('[data-settings]');
    if (!box) return;
    const errEl = box.querySelector('[data-settings-err]');
    if (errEl) errEl.textContent = '';
    const get = (key) => box.querySelector(`[data-set="${key}"]`);

    const title = (get('title')?.value || '').trim();
    if (!title) {
      fail(errEl, '标题不能空着（默认是「面板任务」）。');
      return;
    }

    const settings = {};
    for (const [key] of TRIGGERS) settings[key] = !!get(key)?.checked;

    // 冷却 / 上限：**空输入不能当成 0**。这两项上 0 是「不冷却 / 不限」，
    // 手滑清空输入框就变成"不设防"—— 那是最不该静默发生的一种。
    const intField = (key, label, zero) => {
      const raw = String(get(key)?.value ?? '').trim();
      if (raw === '') return { error: `${label}不能空着（${zero}）。` };
      const n = Number(raw);
      if (!Number.isInteger(n) || n < 0) return { error: `${label}要填 0 或正整数。` };
      return { value: n };
    };
    const cool = intField('cooldown_secs', '冷却秒数', '0 表示不冷却');
    if (cool.error) {
      fail(errEl, cool.error);
      return;
    }
    const cap = intField('daily_cap', '每日上限', '0 表示不限');
    if (cap.error) {
      fail(errEl, cap.error);
      return;
    }
    settings.cooldown_secs = cool.value;
    settings.daily_cap = cap.value;

    btn.disabled = true;
    btn.textContent = '保存中…';
    const res = await savePush({
      enabled: !!get('enabled')?.checked,
      title,
      settings,
    });
    if (!root.isConnected) return;
    btn.disabled = false;
    btn.textContent = '保存设置';
    if (!res || !res.ok) {
      fail(errEl, `保存失败：${(res && res.error) || '未知错误'}`);
      return;
    }
    ctx.toast('设置已保存', 'ok');
    await load(true);
  }

  /** 发一条测试消息。结果逐条显示 —— 失败必须看得见原因。 */
  async function runTest(btn) {
    btn.disabled = true;
    btn.textContent = '发送中…';
    const res = await testPush();
    if (!root.isConnected) return;
    btn.disabled = false;
    btn.textContent = '发一条试试';
    if (!res || !res.ok) {
      // 一个渠道都没配时后端也是 ok:false，原因在 line 里（不一定有 error 字段）
      testResult = null;
      testError = (res && (res.error || res.line)) || '未知错误';
      paintTestBox();
      ctx.toast(`没发出去：${testError}`, 'bad');
      return;
    }
    testError = '';
    testResult = res;
    paintTestBox();
    ctx.toast(res.line || '已发送', 'ok');
    // 投递记录会多一条，顺手刷新（静默：失败不该盖掉刚看到的测试结果）
    await load(true);
  }

  // ---------------------------------------------------------------- 绑定

  // 事件委托挂在 root 上（paint() 只换 innerHTML，root 本身不换）：
  // 重画之后按钮仍然能点 —— 逐个按钮绑监听必然漏掉新画出来的那批。
  root.addEventListener('click', (ev) => {
    if (!(ev.target instanceof Element)) return;
    const btn = ev.target.closest(
      'button[data-tab], button[data-refresh], button[data-edit], button[data-remove],' +
        ' button[data-save-chan], button[data-test], button[data-settings-save]',
    );
    if (!btn || btn.disabled) return;
    const d = btn.dataset;
    if (d.tab !== undefined) {
      tab = d.tab;
      applyTab();
      return;
    }
    if (d.refresh !== undefined) {
      void load(false);
      return;
    }
    if (d.edit !== undefined) {
      editing = editing === d.edit ? '' : d.edit;
      paint();
      return;
    }
    if (d.remove !== undefined) {
      void removeChannel(d.remove, btn);
      return;
    }
    if (d.saveChan !== undefined) {
      void saveChannel(d.saveChan, btn);
      return;
    }
    if (d.test !== undefined) {
      void runTest(btn);
      return;
    }
    if (d.settingsSave !== undefined) void saveSettings(btn);
  });

  // 按任务：模式下拉与渠道勾选都是「改一项存一项」，所以走 change 而不是 click。
  root.addEventListener('change', (ev) => {
    const el = ev.target;
    if (!(el instanceof HTMLSelectElement || el instanceof HTMLInputElement)) return;
    const modeKey = el.dataset.taskMode;
    const chanKey = el.dataset.taskChan;
    const key = modeKey || chanKey;
    if (!key) return;
    const t = tasks().find((x) => ruleKey(x) === key);
    if (!t) return;
    const r = ruleOf(t);
    if (modeKey) {
      void saveTaskRule(t, el.value, r.channels);
      return;
    }
    const cid = el.dataset.channel;
    const channels = el.checked
      ? [...new Set([...r.channels, cid])]
      : r.channels.filter((c) => c !== cid);
    void saveTaskRule(t, r.mode, channels);
  });

  await load(false);
  return null;
}
