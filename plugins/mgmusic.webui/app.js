// 咪咕音乐插件页的界面逻辑（面板 0.4.x）。
//
// # 这份和老面板（0.3.x）的 app.js 是什么关系
//
// 流程、文案、信息密度**照抄**：账号卡片、短信登录（发码 → 60 秒重发倒计时 → 校验）、
// 四个动作（daily / 签到 / 评论 / 补报）、状态查询、设置（含折叠的高级项）、输出区。
// 变的是三件事，都在契约里写着（/tmp/mgmusic-port-contract.md）：
//
//   ① **接口路径与载荷一个字节没改**：全部还是 /plugins/mgmusic/api/<name>，
//      POST 带 content-type: application/json、body 就是原来的 JSON。插件侧按 evt.rel 分发。
//   ② **「立即跑一次」打面板的任务接口**，不再打插件的 /run：
//      面板给插件界面的请求预算是 30 秒（WEBUI_BUDGET_MS），daily 要跑一两分钟；
//      而且任务在跑时插件界面请求会被 502「正忙」挡掉 —— 同步跑必然失败。
//      现在：POST /api/tasks/mgmusic/daily/run（立即回 run_id），再轮询
//      GET /api/runs?plugin=mgmusic&task=daily&limit=1 看这一趟真跑完没有。
//      插件的 /run 路由还在，只作兼容保留，界面不碰它。
//   ③ 控件换成面板的组件（<p-button> <p-field> <p-number> <p-select> <p-dialog> <p-empty>），
//      颜色一律走 token —— 两个主题自动跟着面板走。
//
// # 三条老约定（保留，别破坏）
//
//   · 账号不让用户手填 devices/uid/ce 的原始 JSON，只走短信登录；
//   · 删账号前要问一句，而且说清「只删本面板里的凭证，不注销咪咕账号」；
//   · 每个动作都要有「进行中 / 成功 / 失败」三种回话 —— 老版靠 withBusy + setState，
//     这里一样，只是失败文案里带上了「为什么」（超时 / 502 正忙 / 插件没加载）。

'use strict';

// 页面版本号：和 style.css 里的 --style-ver 一起人工递增。
// ?diag=1 的自检块读它 —— 用来分辨「CSS 是新的、JS 还是旧的」这种半新半旧状态。
document.documentElement.dataset.appVer = '1';

/** 插件自己的接口（和老面板一字不差）。 */
const API = '/plugins/mgmusic/api';

/** 面板任务接口里的两段：插件名 + 任务名（插件 register_cron('daily', …) 的那个名字）。 */
const PLUGIN = 'mgmusic';
const TASK_DAILY = 'daily';

/** 插件界面请求的面板预算（src/web.rs 的 WEBUI_BUDGET_MS = 30s）。留 2 秒给网络。 */
const WEBUI_TIMEOUT_MS = 32000;

/** 「立即跑一次」最多等多久（等不到就说「还没跑完」，绝不假装成功）。 */
const RUN_WAIT_MAX_MS = 120000;

const $ = (id) => document.getElementById(id);

const S = {
  config: null,
  editing: -1,
  sending: false,
  sendTimer: null,
  sendPhone: '',
  running: false,
};

// ──────────────────────────────────────────────────────────── 小工具

function clone(v) {
  return JSON.parse(JSON.stringify(v));
}

function clampInt(v, min, max, fallback) {
  const n = Math.floor(Number(v));
  if (!Number.isFinite(n)) return fallback;
  return Math.max(min, Math.min(max, n));
}

function mask(v, head, tail) {
  const s = String(v || '');
  if (!s) return '';
  if (s.length <= head + tail) return s;
  return s.slice(0, head) + '***' + s.slice(-tail);
}

/**
 * 顶部那个状态徽章。tone 用面板的徽章变体：idle / ok / warn / err / accent。
 * （老版写的是 badge success / failed —— 0.4.0 里叫 ok / err，两套都认，用新的。）
 */
function setState(tone, text) {
  const el = $('state');
  if (!el) return;
  el.className = 'badge' + (tone ? ' ' + tone : '');
  el.textContent = text;
}

/** 运行区的那个小徽章（「空闲 / 执行中 / 完成」），和顶栏状态是两件事。 */
function setRunBadge(tone, text) {
  const el = $('run-badge');
  if (!el) return;
  el.className = 'badge' + (tone ? ' ' + tone : '');
  el.textContent = text;
}

function show(text) {
  const el = $('out');
  if (el) el.textContent = text;
}

function showLines(lines) {
  show(Array.isArray(lines) ? lines.join('\n') : String(lines || ''));
}

/** 行内提示：空就是空（.hint 有 min-height，不会跳）；错误加 .hint-err。 */
function setHint(id, kind, text) {
  const el = $(id);
  if (!el) return;
  el.className = 'hint' + (kind === 'error' ? ' hint-err' : '');
  el.textContent = text || '';
}

/**
 * <p-button> 把文字包进了里面那个真 <button>（web/ui.js 的 PButton）。
 * 所以读/写按钮文字要落到内层，直接写宿主元素是看不见的。**改按钮文案必须走这两个。**
 */
function btnText(btn) {
  const b = btn && (btn._btn || btn);
  return b ? b.textContent : '';
}

function setBtnText(btn, text) {
  const b = btn && (btn._btn || btn);
  if (b) b.textContent = text;
}

/** 禁用/启用：<p-button> 和 <button> 都认 .disabled = true。 */
function setBtnOff(btn, off) {
  if (btn) btn.disabled = !!off;
}

// ──────────────────────────────────────────────────────────── 网络

/**
 * 一次请求。三件事在这里统一做完，别在每个动作里各写一遍：
 *
 *   ① **超时**：插件界面的请求面板最多给 30 秒，超时后浏览器那头会一直挂着
 *      （看起来就是「点了没反应」）。这里用 AbortController 在 32 秒时自己断掉。
 *   ② **非 JSON 的响应**：502「正忙」、403 少了 x-panel 头，回来的都是纯文本；
 *      不解析就给用户看，否则只剩一句「失败」。
 *   ③ **插件自己的错误形状**：{ok:false, error, lines} 里的 lines 是给人看的逐条结果，
 *      挂到 error 上带给 showLines（老版就是这么用的）。
 */
async function request(url, opts) {
  const options = opts || {};
  const method = options.method || 'GET';
  const headers = {};
  // 面板的**写**接口要求 x-panel: 1（防跨站请求伪造）。⚠ 插件自己的 /plugins/<name>/api/*
  // **不校验**这个头（宿主把请求整段交给插件分发器，见 web.rs 的 plugin_webui_any），
  // 但这里照样发：同一份 request() 两处都用，且将来宿主收紧时不用回头改前端。
  if (options.write) headers['x-panel'] = '1';
  if (options.body !== undefined) headers['content-type'] = 'application/json';

  const timeout = options.timeout === undefined ? WEBUI_TIMEOUT_MS : options.timeout;
  const ac = typeof AbortController === 'function' ? new AbortController() : null;
  const timer = ac ? setTimeout(() => ac.abort(), timeout) : null;

  let res;
  try {
    res = await fetch(url, {
      method: method,
      headers: headers,
      credentials: 'same-origin',
      body: options.body === undefined ? undefined : JSON.stringify(options.body),
      signal: ac ? ac.signal : undefined,
    });
  } catch (e) {
    if (e && e.name === 'AbortError') {
      throw new Error('等了 ' + Math.round(timeout / 1000) + ' 秒还没回来。'
        + '面板给插件界面的请求上限是 30 秒，长动作请用「立即跑一次」（走面板任务）。');
    }
    throw new Error('请求发不出去：' + (e && e.message ? e.message : String(e)));
  } finally {
    if (timer) clearTimeout(timer);
  }

  const text = await res.text();
  let data = {};
  try {
    data = text ? JSON.parse(text) : {};
  } catch (e) {
    // 默认文案不够用时，原文比「失败」两个字有用一百倍。
    throw new Error('HTTP ' + res.status + '（响应不是 JSON）：' + text.slice(0, 200));
  }

  // 502 是面板说的「插件正忙 / 这一趟超时」，不是用户做错了什么 —— 单独说清。
  if (res.status === 502) {
    throw new Error('插件正忙（502）：有任务正在跑，或者这一趟超过 30 秒。'
      + '等它跑完再点，或者用「立即跑一次」让面板去跑。');
  }
  if (!res.ok) throw new Error(data.error || ('HTTP ' + res.status));
  if (data && data.ok === false) {
    const err = new Error(data.error || '操作失败');
    if (data.lines) err.lines = data.lines;
    throw err;
  }
  return data;
}

/** 调插件自己的接口（/plugins/mgmusic/api/*）。 */
function call(path, opts) {
  return request(API + path, opts);
}

/** 调插件的**写**接口（要带 x-panel: 1）。 */
function post(path, body) {
  return call(path, { method: 'POST', write: true, body: body === undefined ? {} : body });
}

// ──────────────────────────────────────────────────────────── 账号渲染

function devices() {
  return S.config && Array.isArray(S.config.devices) ? S.config.devices : [];
}

function deviceLabel(dev, index) {
  const label = String((dev && dev.label) || '').trim();
  if (label) return label;
  const phone = String((dev && dev.msisdn) || '');
  return phone ? ('migu-' + phone.slice(-4)) : ('账号 ' + (index + 1));
}

function sourceLabel(dev) {
  const source = String((dev && dev.source) || '');
  if (source === 'macos-app' || source === 'app-account.json') return 'App 同步';
  return '短信登录';
}

/** 一个 <p-button>，size="sm"。自定义元素建出来就已经被 /ui.js 注册过了，直接 append。 */
function smallButton(text, variant, onClick) {
  const b = document.createElement('p-button');
  b.setAttribute('size', 'sm');
  if (variant) b.setAttribute('variant', variant);
  b.textContent = text;
  b.addEventListener('click', onClick);
  return b;
}

function renderAccounts() {
  const list = $('account-list');
  const cfg = S.config || {};
  const devs = devices();
  list.textContent = '';

  $('account-summary').textContent = devs.length ? devs.length + ' 个账号' : '还没有账号';
  const empty = $('account-empty');
  if (empty) empty.hidden = devs.length > 0;

  devs.forEach((dev, index) => {
    const li = document.createElement('li');
    li.className = 'acct-item';

    const main = document.createElement('div');
    main.className = 'acct-main';

    const title = document.createElement('div');
    title.className = 'acct-title';
    const name = document.createElement('strong');
    name.textContent = deviceLabel(dev, index);
    // 来源用面板的徽章（.badge.accent），和顶栏的状态徽章是同一个东西
    const source = document.createElement('span');
    source.className = 'badge accent';
    source.textContent = sourceLabel(dev);
    title.appendChild(name);
    title.appendChild(source);
    main.appendChild(title);

    const meta = document.createElement('div');
    meta.className = 'acct-meta';
    meta.textContent = [
      dev.msisdn ? '手机号 ' + mask(dev.msisdn, 3, 4) : '手机号未知',
      dev.uid ? 'uid ' + mask(dev.uid, 6, 4) : 'uid 未解析',
    ].join(' · ');
    main.appendChild(meta);

    const sub = document.createElement('div');
    sub.className = 'acct-sub';
    const topup = Number(dev.topup_min === undefined ? cfg.topup_min : dev.topup_min) || 0;
    sub.textContent = '听歌补报 ' + topup + ' 分钟'
      + (dev.cookie ? ' · 已保存会话 cookie' : ' · 暂无会话 cookie');
    main.appendChild(sub);

    const actions = document.createElement('div');
    actions.className = 'acct-actions';
    const uid = dev.uid || dev.label;
    const sign = smallButton('签到', '', () => doSign(uid, sign));
    const query = smallButton('查询', '', () => doStatus(uid, query));
    const edit = smallButton('设置', '', () => openAccountDialog(index));
    const remove = smallButton('删除', 'danger', () => removeAccount(index));
    actions.appendChild(sign);
    actions.appendChild(query);
    actions.appendChild(edit);
    actions.appendChild(remove);

    li.appendChild(main);
    li.appendChild(actions);
    list.appendChild(li);
  });
}

// ──────────────────────────────────────────────────────────── 设置渲染

function fillSettings() {
  const cfg = S.config || {};
  const ti = cfg.task_interval || {};
  const comment = cfg.comment || {};

  $('set-topup').value = cfg.topup_min === undefined ? 30 : cfg.topup_min;
  $('set-timeout').value = cfg.timeout_ms === undefined ? 15000 : cfg.timeout_ms;
  $('set-report-ms').value = cfg.report_ms === undefined ? 60000 : cfg.report_ms;
  $('set-interval-min').value = ti.min_ms === undefined ? 1000 : ti.min_ms;
  $('set-interval-max').value = ti.max_ms === undefined ? 3000 : ti.max_ms;
  $('set-tls').value = cfg.tls === 'native' ? 'native' : '';
  $('set-cert').value = cfg.cert || '';
  $('set-comment-enabled').checked = !!comment.enabled;
  $('set-comment-song').value = comment.song_id || '';
  $('set-comment-content').value = comment.content || '好听';
}

function renderAll() {
  renderAccounts();
  fillSettings();
}

async function loadAll() {
  setState('idle', '读取中…');
  setHint('settings-hint', '', '');
  try {
    const data = await call('/config');
    S.config = data.config || {};
    renderAll();
    setState('ok', '已连接');
  } catch (e) {
    setState('err', '读取失败');
    setHint('settings-hint', 'error', '读配置失败：' + e.message);
  }
}

/** 写回整份配置。载荷形状和老面板一样：body 就是 config 对象本身（不是 {config: …}）。 */
async function saveConfigObject(cfg) {
  const data = await call('/config', { method: 'POST', write: true, body: cfg });
  S.config = data.config || {};
  renderAll();
  return data.config;
}

/**
 * 把一个动作用「禁用 → 换文字 → 跑 → 还原」包起来，并统一把失败写进输出区。
 * 老版的 withBusy 就是这个；这里多了一步 setRunBadge，让「在跑」看得见。
 */
async function withBusy(btn, busyText, fn) {
  const old = btnText(btn);
  setBtnOff(btn, true);
  setBtnText(btn, busyText);
  try {
    return await fn();
  } catch (e) {
    showLines(e.lines || ('失败：' + e.message));
    setState('err', '有失败');
    setRunBadge('err', '有失败');
    return null;
  } finally {
    setBtnOff(btn, false);
    setBtnText(btn, old);
  }
}

// ──────────────────────────────────────────────────────────── 短信登录

function clearSendTimer() {
  if (S.sendTimer) {
    clearInterval(S.sendTimer);
    S.sendTimer = null;
  }
}

function setSendButton(text, disabled) {
  const btn = $('send-code');
  setBtnText(btn, text);
  setBtnOff(btn, disabled);
}

function resetSendState() {
  clearSendTimer();
  S.sendPhone = '';
  setSendButton('发送验证码', false);
}

function openLoginDialog() {
  // 每次「添加账号」都从干净状态开始：上一位账号的 60 秒倒计时不能带过来。
  clearSendTimer();
  S.sending = false;
  S.sendPhone = '';
  $('login-phone').value = '';
  $('login-code').value = '';
  setSendButton('发送验证码', false);
  setHint('login-hint', '', '');
  $('login-dialog').setAttribute('open', '');
  $('login-phone').focus();
}

function closeLoginDialog() {
  $('login-dialog').removeAttribute('open');
  resetSendState();
}

async function sendCode() {
  if (S.sending) return;
  const phone = ($('login-phone').value || '').trim();
  if (!/^1[3-9]\d{9}$/.test(phone)) {
    setHint('login-hint', 'error', '先填 11 位手机号');
    return;
  }
  S.sending = true;
  setSendButton('发送中…', true);
  setHint('login-hint', '', '正在向 passport 请求验证码…');
  try {
    const data = await post('/login/send', { msisdn: phone });
    setHint('login-hint', '', data.message || '验证码已发送，请查收短信');
    $('login-code').focus();
    S.sendPhone = phone;
    let left = 60;
    setSendButton('重发（' + left + 's）', true);
    clearSendTimer();
    S.sendTimer = setInterval(() => {
      left -= 1;
      if (left <= 0) {
        clearSendTimer();
        S.sendPhone = '';
        setSendButton('发送验证码', false);
      } else {
        setSendButton('重发（' + left + 's）', true);
      }
    }, 1000);
  } catch (e) {
    setSendButton('发送验证码', false);
    setHint('login-hint', 'error', e.message);
  } finally {
    S.sending = false;
  }
}

async function doLogin() {
  const phone = ($('login-phone').value || '').trim();
  const code = ($('login-code').value || '').trim();
  if (!/^1[3-9]\d{9}$/.test(phone)) {
    setHint('login-hint', 'error', '先填 11 位手机号');
    return;
  }
  if (!/^\d{6}$/.test(code)) {
    setHint('login-hint', 'error', '填 6 位短信验证码');
    return;
  }
  const btn = $('login');
  const old = btnText(btn);
  setBtnOff(btn, true);
  setBtnText(btn, '登录中…');
  setHint('login-hint', '', '正在校验验证码并解析 uid…');
  try {
    const data = await post('/login/verify', { msisdn: phone, code: code });
    S.config = data.config || S.config;
    renderAll();
    closeLoginDialog();
    showLines(data.lines);
    setState('ok', '已登录');
    setRunBadge('ok', '账号已保存');
    setHint('settings-hint', '', '账号已保存；建议先点「查询状态」确认 uid 和音符。');
  } catch (e) {
    setHint('login-hint', 'error', e.message);
    if (e.lines) showLines(e.lines);
  } finally {
    setBtnOff(btn, false);
    setBtnText(btn, old);
  }
}

// ──────────────────────────────────────────────────────────── 单账号设置

function openAccountDialog(index) {
  const dev = devices()[index];
  if (!dev) return;
  S.editing = index;
  const dlg = $('account-dialog');
  // <p-dialog> 的标题是建元素时从 title 属性读的，之后要改就改里面那个 span。
  const head = dlg.querySelector('.dialog-head span');
  if (head) head.textContent = '账号设置 · ' + deviceLabel(dev, index);
  $('account-label').value = dev.label || '';
  $('account-topup').value = dev.topup_min === undefined
    ? ((S.config && S.config.topup_min) || 0)
    : dev.topup_min;
  setHint('account-hint', '', '');
  dlg.setAttribute('open', '');
  $('account-label').focus();
}

function closeAccountDialog() {
  $('account-dialog').removeAttribute('open');
  S.editing = -1;
}

async function saveAccount() {
  if (S.editing < 0) return;
  const dev = devices()[S.editing];
  if (!dev) {
    closeAccountDialog();
    return;
  }
  const cfg = clone(S.config);
  const target = cfg.devices[S.editing];
  target.label = ($('account-label').value || '').trim();
  target.topup_min = clampInt($('account-topup').value, 0, 1440, 0);
  const btn = $('account-save');
  setBtnOff(btn, true);
  try {
    await saveConfigObject(cfg);
    closeAccountDialog();
    setHint('settings-hint', '', '账号设置已保存');
  } catch (e) {
    setHint('account-hint', 'error', e.message);
  } finally {
    setBtnOff(btn, false);
  }
}

async function removeAccount(index) {
  const dev = devices()[index];
  if (!dev) return;
  const name = deviceLabel(dev, index);
  if (!window.confirm('删除账号「' + name + '」？只删除本面板里的凭证，不会注销咪咕账号。')) return;
  const cfg = clone(S.config);
  cfg.devices.splice(index, 1);
  try {
    await saveConfigObject(cfg);
    setHint('settings-hint', '', '账号已删除');
  } catch (e) {
    setHint('settings-hint', 'error', e.message);
  }
}

// ──────────────────────────────────────────────────────────── 设置保存

async function saveSettings() {
  const cfg = clone(S.config || {});
  cfg.topup_min = clampInt($('set-topup').value, 0, 1440, 30);
  cfg.timeout_ms = clampInt($('set-timeout').value, 1000, 60000, 15000);
  cfg.report_ms = clampInt($('set-report-ms').value, 1000, 21600000, 60000);
  const tiMin = clampInt($('set-interval-min').value, 0, 15000, 1000);
  const tiMax = clampInt($('set-interval-max').value, 0, 15000, 3000);
  cfg.task_interval = { min_ms: tiMin, max_ms: Math.max(tiMin, tiMax) };
  cfg.tls = $('set-tls').value === 'native' ? 'native' : '';
  cfg.cert = ($('set-cert').value || '').trim();
  cfg.comment = Object.assign({}, cfg.comment, {
    enabled: !!$('set-comment-enabled').checked,
    song_id: ($('set-comment-song').value || '').trim(),
    content: ($('set-comment-content').value || '好听').trim() || '好听',
  });

  const btn = $('save-settings');
  setBtnOff(btn, true);
  setHint('settings-hint', '', '保存中…');
  try {
    await saveConfigObject(cfg);
    setHint('settings-hint', '', '设置已保存');
    setState('ok', '已保存');
  } catch (e) {
    setHint('settings-hint', 'error', e.message);
  } finally {
    setBtnOff(btn, false);
  }
}

// ──────────────────────────────────────────────────────────── 动作

/** 查询状态：带 uid 就只查那一个，不带就查全部。 */
async function doStatus(uid, btn) {
  const target = btn || $('status');
  await withBusy(target, '查询中…', async () => {
    const data = uid ? await post('/status', { uid: uid }) : await call('/status');
    showLines(data.lines);
    setRunBadge(data.ok ? 'ok' : 'err', data.ok ? '状态正常' : '有账号失败');
    setState(data.ok ? 'ok' : 'err', data.ok ? '状态正常' : '有账号失败');
  });
}

/**
 * 「立即跑一次 daily」——**不**打插件的 /run，打面板的任务接口。
 *
 * 为什么：daily 要跑一两分钟，而插件界面的请求预算是 30 秒、任务在跑时插件界面请求
 * 还会被 502「正忙」挡掉。面板的任务接口是「入队 + 回 run_id」，跑在插件自己的任务里，
 * 界面这边只轮询运行记录。详见 panel-llrt/docs/mgmusic-migration.md 的「契约对照」一节。
 */
async function doRun() {
  if (S.running) return;
  const btn = $('run');
  const old = btnText(btn);
  S.running = true;
  setBtnOff(btn, true);
  setBtnText(btn, '排入执行…');
  setRunBadge('accent', '执行中…');
  setState('idle', '已排入');
  setHint('run-hint', '', 'daily 由面板的任务去跑，不会卡住这个页面；跑一次通常 1–2 分钟。');

  const t0 = Date.now();
  try {
    const queued = await request(
      '/api/tasks/' + encodeURIComponent(PLUGIN) + '/' + encodeURIComponent(TASK_DAILY) + '/run',
      { method: 'POST', write: true, timeout: 15000 },
    );
    const runId = queued && queued.run_id ? queued.run_id : 0;
    showLines('已排入执行' + (runId ? '（run #' + runId + '）' : '') + '。\n'
      + '这一趟在面板的任务里跑：这里会跟着更新状态，逐条日志在面板的「运行记录」里。');

    const rec = await waitRun(t0, (ms) => {
      setBtnText(btn, '跑着呢 ' + Math.round(ms / 1000) + 's');
    });

    if (rec === null) {
      setRunBadge('warn', '还没跑完');
      setState('warn', '还没跑完');
      showLines('已经排上面板的任务队列，但等了 ' + Math.round(RUN_WAIT_MAX_MS / 1000)
        + ' 秒还没结束 —— 去面板的「运行记录」看这一趟（plugin=mgmusic, task=daily）。\n'
        + '（这里不再等：接着轮询会把页面挂在一次已经跑飞的任务上。）');
      return;
    }

    // 用时只在够长的时候说：一次 0.0s 的失败念出来像在开玩笑
    const spent = Number(rec.ended_at) - Number(rec.started_at);
    const secs = Number.isFinite(spent) && spent >= 500 ? '，用时 ' + (spent / 1000).toFixed(1) + 's' : '';
    if (rec.status === 'ok') {
      setRunBadge('ok', 'daily 完成');
      setState('ok', 'daily 完成');
      showLines('daily 跑完了' + secs + '。\n逐条结果在面板的「运行记录」里（plugin=mgmusic, task=daily）。');
    } else {
      setRunBadge('err', '有失败');
      setState('err', '有失败');
      showLines('daily 跑完了但有失败' + secs + '：\n'
        + (rec.error ? String(rec.error) : '（面板没给错误摘要，去「运行记录」看日志）'));
    }
  } catch (e) {
    setRunBadge('err', '没排上');
    setState('err', '没排上');
    showLines('没能排进执行队列：' + e.message + '\n'
      + '（面板的任务接口是 POST /api/tasks/mgmusic/daily/run；插件没加载、任务名变了、'
      + '或者已经有一趟在跑，都会落到这里。也可以在面板的「任务」页手动运行 daily。）');
  } finally {
    S.running = false;
    setBtnOff(btn, false);
    setBtnText(btn, old);
    // daily 会顺带同步 macOS App 账号，可能改了 devices —— 重读一次
    await loadAll();
  }
}

/**
 * 这一次运行完了吗？返回那条记录，或 null（还没开始写 / 还没完）。
 *
 * ⚠ 两个实测出来的细节（都不许照抄 mcloud 那版）：
 *   ① `/api/runs` **不认 task 查询参数** —— 传了会 400
 *      （「unknown field task, expected one of limit, plugin, run_id, q, sort…」）。
 *      所以只按 plugin 拉最近几条，再在这里按 task 名字与时间自己筛。
 *   ② 记录的字段是 **started_at / ended_at（毫秒数字）** 和 `status: 'ok'|'error'`，
 *      没有 finished_at / duration_ms / ok。判「完成」看 ended_at 有没有值。
 */
async function runRecordSince(sinceMs) {
  try {
    const url = '/api/runs?plugin=' + encodeURIComponent(PLUGIN) + '&limit=8';
    const res = await fetch(url, { headers: { 'x-panel': '1' }, credentials: 'same-origin' });
    if (!res.ok) return null;
    const j = await res.json();
    const rows = j && Array.isArray(j.runs) ? j.runs : [];
    for (const one of rows) {
      if (!one || one.task !== TASK_DAILY) continue;
      if (one.ended_at === undefined || one.ended_at === null) continue;
      // 只认**这一次**：上一次的记录会一直躺在那儿，不判时间就会立刻「完成」
      const started = Number(one.started_at);
      if (Number.isFinite(started) && started < sinceMs - 2000) continue;
      return one;
    }
    return null;
  } catch (e) {
    return null;
  }
}

/** 等这一趟跑完。onTick 每秒叫一次（按钮拿它写秒数）。超时返回 null。 */
async function waitRun(sinceMs, onTick) {
  for (;;) {
    const ms = Date.now() - sinceMs;
    if (onTick) onTick(ms);
    const one = await runRecordSince(sinceMs);
    if (one !== null) return one;
    if (ms > RUN_WAIT_MAX_MS) return null;
    await new Promise((r) => setTimeout(r, 2000));
  }
}

async function doReport() {
  await withBusy($('report'), '补报中…', async () => {
    const ms = clampInt((S.config && S.config.report_ms) || 60000, 1000, 21600000, 60000);
    const data = await post('/report', { ms: ms });
    showLines(data.lines);
    setRunBadge(data.ok ? 'ok' : 'err', data.ok ? '补报完成' : '补报有失败');
    setState(data.ok ? 'ok' : 'err', data.ok ? '补报完成' : '补报有失败');
  });
}

async function doSign(uid, btn) {
  await withBusy(btn, '签到中…', async () => {
    const data = await post('/sign', { uid: uid });
    showLines(data.lines);
    setRunBadge(data.ok ? 'ok' : 'err', data.ok ? '签到完成' : '签到有失败');
    setState(data.ok ? 'ok' : 'err', data.ok ? '签到完成' : '签到有失败');
  });
}

async function doComment() {
  await withBusy($('comment'), '评论中…', async () => {
    const data = await post('/comment', {});
    showLines(data.lines);
    setRunBadge(data.ok ? 'ok' : 'err', data.ok ? '评论完成' : '评论有失败');
    setState(data.ok ? 'ok' : 'err', data.ok ? '评论完成' : '评论有失败');
  });
}

async function doSelftest() {
  const out = $('advanced-out');
  out.hidden = false;
  out.textContent = '执行中…';
  try {
    const data = await call('/selftest');
    out.textContent = JSON.stringify(data, null, 2);
    setRunBadge(data.ok ? 'ok' : 'err', data.ok ? '签名自检通过' : '签名自检失败');
  } catch (e) {
    out.textContent = '失败：' + e.message;
    setRunBadge('err', '签名自检失败');
  }
}

// ──────────────────────────────────────────────────────────── 样式自检（?diag=1）
//
// 判据来自 ~/.dsh/skills/panel-plugin-ui §四：出现「我这边对、你那边不对」时，
// 先要这段输出，别盲改 CSS。打的是**真正生效的东西**：
//   两个版本号（分辨半新半旧）、data-theme、几个关键 computed 值、样式表清单、
//   以及本页的结构性事实（账号卡几个、输出几行）、UA。
// ⚠ 这个块**不能**用内联 style 画（CSP style-src 'self' 会拦掉），样式在 style.css 的 #diag-box。

function diag() {
  const box = $('diag-box');
  if (!box) return;

  const collect = () => {
    const root = document.documentElement;
    const cs = getComputedStyle(root);
    const out = [];

    out.push('style.css 版本 = ' + (cs.getPropertyValue('--style-ver').trim() || '（读不到：不是我们的 CSS？）'));
    out.push('app.js 版本    = ' + (root.dataset.appVer || '（没标记：旧 app.js）'));
    out.push('data-theme     = ' + (root.dataset.theme || '（没有）') + '   embed = ' + (root.dataset.embed || '0'));
    out.push('--surface      = ' + cs.getPropertyValue('--surface').trim());
    out.push('--surface-2    = ' + cs.getPropertyValue('--surface-2').trim());
    out.push('--line         = ' + cs.getPropertyValue('--line').trim());
    out.push('--fg           = ' + cs.getPropertyValue('--fg').trim());
    out.push('--accent       = ' + cs.getPropertyValue('--accent').trim());
    out.push('body 底色      = ' + getComputedStyle(document.body).backgroundColor);

    // 样式表清单：扩展注入的会以 chrome-extension:// 开头，一眼看得出来
    const sheets = Array.prototype.slice.call(document.styleSheets);
    out.push('样式表 ' + sheets.length + ' 份：');
    sheets.forEach((sh) => {
      let n = -1;
      try {
        n = sh.cssRules.length;
      } catch (e) {
        n = -2;
      }
      out.push('  · ' + (sh.href || '内联') + '（规则 ' + n + '）');
    });

    // 本页的结构性事实：账号卡是 JS 画的，等 DOMContentLoaded 只打一次会得到「0 个」
    const cards = document.querySelectorAll('.acct-item');
    out.push('账号卡片 ' + cards.length + ' 个；输出区 ' + (($('out') ? $('out').textContent : '').split('\n').length) + ' 行');
    const first = cards[0];
    if (first) {
      const st = getComputedStyle(first);
      out.push('第一个账号卡 = ' + first.className + '  bg=' + st.backgroundColor + '  border=' + st.borderTopColor);
    }
    const runBtn = $('run');
    out.push('「立即跑一次」= ' + (runBtn ? runBtn.tagName.toLowerCase()
      + (runBtn._btn ? '（内层 button.btn 已就绪）' : '（内层还没建：/ui.js 没加载？）') : '找不到'));
    const cb = $('set-comment-enabled'); // 页面上剩下的那个勾选框（面板自绘）
    if (cb) {
      const cbs = getComputedStyle(cb);
      out.push('勾选框 = ' + cbs.width + ' × ' + cbs.height + '  bg=' + cbs.backgroundColor
        + (cbs.appearance === 'none' ? '（面板自绘）' : '（原生外观）'));
    }
    out.push('userAgent      = ' + navigator.userAgent);

    box.textContent = out.join('\n');
    box.hidden = false;
  };

  collect();
  setInterval(collect, 2000);
}

// ──────────────────────────────────────────────────────────── 事件接线

$('refresh').addEventListener('click', loadAll);
$('add-account').addEventListener('click', openLoginDialog);
$('login-cancel').addEventListener('click', closeLoginDialog);
$('account-cancel').addEventListener('click', closeAccountDialog);
$('account-save').addEventListener('click', saveAccount);
$('send-code').addEventListener('click', sendCode);
$('login').addEventListener('click', doLogin);
$('save-settings').addEventListener('click', saveSettings);
$('selftest').addEventListener('click', doSelftest);
$('status').addEventListener('click', () => doStatus());
$('run').addEventListener('click', doRun);
$('sign').addEventListener('click', () => doSign('', $('sign')));
$('comment').addEventListener('click', doComment);
$('report').addEventListener('click', doReport);

// 换了手机号就解除上一号码的倒计时，让用户能给另一个账号发验证码。
$('login-phone').addEventListener('input', () => {
  if (S.sendPhone && ($('login-phone').value || '').trim() !== S.sendPhone) {
    resetSendState();
    setHint('login-hint', '', '');
  }
});

// 回车 = 下一步（手机上少点一次键盘）
$('login-phone').addEventListener('keydown', (e) => {
  if (e.key === 'Enter') sendCode();
});
$('login-code').addEventListener('keydown', (e) => {
  if (e.key === 'Enter') doLogin();
});

loadAll();
if (new URLSearchParams(location.search).get('diag') === '1') diag();
