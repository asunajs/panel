// 界面逻辑：**一个读数接口画主体**，把六个区块画出来。
//
// # 三条设计约束（与 index.html 的文件头是同一件事，这里说的是怎么落实）
//
// ① **一次调用画全部**。`/plugins/mcloud/api/readings` 一次给全六个区块的主体读数
//    （AI豆 / 领奖 这两格是"进这一页才自己取"的，见各自那一节的模块头）。
//    旧版是一个区块一个接口，于是每块各有各的加载/错误状态 —— 用户看到
//    「有的块有数、有的块空着、有的块报错」，而没人知道该看哪个。
//    现在只有两种状态：**有读数** 或者 **一次都没读到**（一个错误信息）。
//
// ② **区块第一行回答用户的问题**，不是复述接口字段。
//    「登录了吗」「今天跑了吗」「下一场几点」—— 这些是用户会问的；
//    「session.expireAt」不是。
//
// ③ **动作按钮只有一个语义**：跑一次 / 领取 / 保存。
//    没有「推进一步」—— 任务是一次跑完的（见 docs/redesign-plan.md 阶段 ②）。
//
// # 关于 `x-panel: 1`
//
// 面板的**写**接口要求这个头（防跨站请求伪造）。插件自己的路由同样适用
// （见面板的 `guard_write`）—— 所以下面每个 POST 都带它。
// 忘了的表现是 403 加一句「写接口要求请求头 x-panel: 1」，不是静默失败。

'use strict';

// 页面版本号：和 style.css 里的 --style-ver 一起人工递增。
// 「样式自检」（boot.js 的 ?diag=1）会读它 —— 用来区分「CSS 是新的、JS 还是旧的」这种
// 半新半旧状态（第 155 轮：样式表是 v155，但 DOM 里根本没有 .ex-group-row 这种行）。
document.documentElement.dataset.appVer = '155';

/** 插件自己的接口。 */
const API = '/plugins/mcloud/api';

/** 三个任务所在的插件与名字（面板的任务接口按这两段定位）。 */
const TASK = {
  daily: ['mcloud-daily', '日常'],
  exchange: ['mcloud-exchange', '兑换'],
  live: ['mcloud-live', '直播'],
};

const $ = (id) => document.getElementById(id);

/** 最近一次读数。 */
let last = null;

// ──────────────────────────────────────────────────────────── 基础

/** 调一个接口。`write` 时带 `x-panel: 1`（面板的写保护）。 */
async function call(url, { method = 'GET', body, write = false } = {}) {
  const headers = {};
  if (write) headers['x-panel'] = '1';
  if (body !== undefined) headers['content-type'] = 'application/json';
  const res = await fetch(url, {
    method,
    headers,
    credentials: 'same-origin',
    body: body === undefined ? undefined : JSON.stringify(body),
  });
  const text = await res.text();
  let data = null;
  try {
    data = text ? JSON.parse(text) : null;
  } catch {
    // 不是 JSON ⇒ 面板或反代在中间挡了一道（403/502 那种会回纯文本）。
    // 这时候把原文带上，否则用户只看到「失败」两个字。
    throw new Error(`${res.status} ${text.slice(0, 200)}`);
  }
  if (!res.ok || (data && data.error)) {
    const err = new Error((data && data.error) || `${res.status} ${text.slice(0, 200)}`);
    // ⚠ 把**整个响应体**挂到错误上。
    //
    // 只抛 `message` 的话，调用方拿不到后端除 `error` 之外说的话 ——
    // 而有些接口要用别的字段驱动界面（比如 `/accounts` 的 `needPcPlatform`
    // 决定"要不要把 win/mac 那个选择露出来"）。丢掉它就只能靠字符串比对，
    // 而字符串比对会在改文案时静默失效。
    if (data && typeof data === 'object') err.data = data;
    throw err;
  }
  return data;
}

const api = (path, opts) => call(API + path, opts);

/** 跑一条任务（面板的任务接口）。 */

// ══════════ 跑一次（触发 → **等它真跑完** → 说结果） ══════════
//
// 面板那个 `…/run` 接口是**异步触发**：它回 `{ok:true}` 只表示"已经交给它了"，
// 任务还要跑几十秒。原来这里立刻弹「跑完了」并重画读数 —— 那两句都是假的：
// 用户看到"跑完了"、读数却还是上一次的。
//
// 所以：触发之后**盯着面板的运行记录**（`/api/runs?plugin=&task=&limit=1`），
// 直到这一次的 `finished_at` 有了值；期间按钮上写着秒数（**证明它还活着**），
// 而日志面板那边本来就是一行一行往下走的。
const RUN_WAIT_MAX_MS = 180_000;

/** 这一次运行完了吗？返回那条记录，或 `null`（还没完 / 记录还没写）。 */
async function runRecordOf(plugin, task, sinceMs) {
  try {
    const r = await fetch(
      `/api/runs?plugin=${encodeURIComponent(plugin)}&task=${encodeURIComponent(task)}&limit=1`,
      { headers: { 'x-panel': '1' } },
    );
    if (!r.ok) return null;
    const j = await r.json();
    const one = (j.runs || [])[0];
    if (!one || !one.finished_at) return null;
    // 只认**这一次**（上一次的记录会一直躺在那儿，不判时间就会立刻"完成"）
    if (Date.parse(String(one.started_at).replace(' ', 'T')) < sinceMs - 2000) return null;
    return one;
  } catch {
    return null;
  }
}

/** 等这一次跑完。`onTick` 每秒叫一次（按钮拿它写秒数）。
 *
 *  返回那条运行记录；超时返回 `null`（**别假装成功**）。 */
async function waitRun(plugin, task, sinceMs, onTick) {
  for (;;) {
    const ms = Date.now() - sinceMs;
    if (onTick) onTick(ms);
    const one = await runRecordOf(plugin, task, sinceMs);
    if (one !== null) return one;
    if (ms > RUN_WAIT_MAX_MS) return null;
    await new Promise((r) => setTimeout(r, 1000));
  }
}

/** 触发一条任务，等它跑完，再把真结果说出来。 */
async function runTaskAndWait(key, btn) {
  const pair = TASK[key];
  // ⚠ 筛在「提醒以上」时，一次跑完界面上一行都没有 —— 用户的原话就是
  //   「没看到任何日志」。**提示要跟在结果那句里**：单发一条 toast 会被
  //   0.4 秒后的「跑完了」直接覆盖掉，等于没说。
  // ⚠ `level` 在 `el()` **里面**（`TERMS[k].level` 是 undefined，踩过）
  const lv = TERMS[key] ? TERMS[key].el().level.value : 'info';
  const only = lv === 'info' ? '' : lv === 'warn' ? '提醒以上' : '只看错误';
  const hidden = only === '' ? '' : ` · 日志只显示「${only}」，这趟的行大多被挡掉了`;
  const label = btn.textContent;
  const stopBtn = $(`${key}-stop`);
  btn.disabled = true;
  // 触发前先把终端流连上：否则前几行会落在连接之前
  if (TERMS[key]) termHandle(key).open();
  if (stopBtn) {
    stopBtn.hidden = false;
    stopBtn.disabled = false;
    stopBtn.textContent = '停止';
  }
  const t0 = Date.now();
  try {
    // `?async=1`：面板只负责入队，立刻回 `{queued:true}`。
    // 旧面板没有这个参数，会一直挂到任务结束 —— 所以它必须和新面板一起用；
    // 这样按钮上的秒数、停止按钮和日志流才能真正同时工作。
    await call(
      `/api/tasks/${encodeURIComponent(pair[0])}/${encodeURIComponent(pair[1])}/run?async=1`,
      { method: 'POST', write: true },
    );
    const one = await waitRun(pair[0], pair[1], t0, (ms) => {
      btn.textContent = `跑着呢 ${Math.round(ms / 1000)}s`;
    });
    if (one === null) {
      toast('还没跑完 —— 日志里能看到它走到哪了', true);
      return;
    }
    const secs = one.duration_ms ? `${(one.duration_ms / 1000).toFixed(1)}s` : '';
    const stopped = /interrupted|停止/i.test(String(one.error || ''));
    if (one.ok === true) toast(`跑完了${secs === '' ? '' : ` · 用时 ${secs}`}${hidden}`);
    else if (stopped) toast(`已停止${secs === '' ? '' : ` · 用时 ${secs}`} —— 当前那一步已经收尾`, true);
    else toast(`跑完了，但有失败${secs === '' ? '' : ` · 用时 ${secs}`} —— 看日志里是哪几步${hidden}`, true);
  } catch (e) {
    toast(String(e && e.message ? e.message : e), true);
  } finally {
    btn.disabled = false;
    btn.textContent = label;
    if (stopBtn) {
      stopBtn.hidden = true;
      stopBtn.disabled = false;
      stopBtn.textContent = '停止';
    }
    await load();
  }
}

/// 停止当前任务：面板侧会打断 QuickJS；已经卡在宿主调用里的部分只能等它返回。
async function stopRunning(key, btn) {
  const pair = TASK[key];
  btn.disabled = true;
  btn.textContent = '停止中…';
  try {
    await call(`/api/tasks/${encodeURIComponent(pair[0])}/${encodeURIComponent(pair[1])}/stop`, {
      method: 'POST',
      write: true,
    });
    toast('已请求停止；当前那一步返回后会停');
  } catch (e) {
    btn.disabled = false;
    btn.textContent = '停止';
    toast(String(e && e.message ? e.message : e), true);
  }
}

function runTask(key) {
  const pair = TASK[key];
  return call(
    `/api/tasks/${encodeURIComponent(pair[0])}/${encodeURIComponent(pair[1])}/run`,
    { method: 'POST', write: true },
  );
}

function toast(msg, bad) {
  const el = $('toast');
  // 没有字就别画：一个空的白卡片比不提示更让人困惑（它看起来像「加载中」）。
  if (!msg) {
    el.hidden = true;
    return;
  }
  el.textContent = msg;
  // 用主面板的提示条组件（`.toast` / `.toast.bad`），不是自己画一个：
  // 同一种"出事了"的提示在全站应该长一样。
  el.className = 'toast' + (bad ? ' bad' : '');
  el.hidden = false;
  clearTimeout(toast._t);
  toast._t = setTimeout(
    () => {
      el.hidden = true;
    },
    bad ? 9000 : 3500,
  );
}

/** 把一次动作包起来：按钮禁用 → 跑 → 提示 → 重画。
 *
 *  为什么统一包一层：旧版每个按钮各写一遍 try/catch + 提示 + 刷新，
 *  于是有的忘了刷新（点了没反应）、有的忘了提示（不知道成没成）、
 *  有的错误只 `console.log`（用户看不见）。 */
async function act(btn, fn, okMsg) {
  // ⚠ 被重发倒计时**接管**的按钮（见 `startResend` 的 `dataset.resend`）连
  //   "转圈"都不该画：倒计时的文字是它自己的状态，`act` 插进去只会闪一下 `…`。
  const owned = btn.dataset.resend === '1';
  const old = btn.textContent;
  btn.disabled = true;
  if (!owned) btn.textContent = '…';
  try {
    const r = await fn();
    // ⚠ 空串 = 「这次别提示」（比如切到「数据」页时那次自动取豆）。
    // 以前这里无条件 toast，于是 `act(..., () => '')` 会在页面顶上一个**没有字的空卡片**
    // —— 用户看到的就是「啥也没写的提示条」（第 145 轮实测：切 tab 就冒一下）。
    const msg = typeof okMsg === 'function' ? okMsg(r) : okMsg;
    if (msg) toast(msg);
    await load();
  } catch (e) {
    toast(String(e && e.message ? e.message : e), true);
  } finally {
    // ⚠ 同理，收尾也不还原它：否则会把「重新获取（N 秒）」改回去、还会把它重新点亮。
    if (btn.dataset.resend !== '1') {
      btn.disabled = false;
      btn.textContent = old;
    }
  }
}

// ──────────────────────────────────────────────────────────── 小工具

function esc(s) {
  return String(s === undefined || s === null ? '' : s)
    .replace(/&/g, '&amp;')
    .replace(/</g, '&lt;')
    .replace(/>/g, '&gt;')
    .replace(/"/g, '&quot;');
}

/** 把 ISO 时刻说成人话。读数是给用户看的，「2026-10-15T00:12:03.000Z」不是人话。 */
function when(iso) {
  if (!iso) return '';
  const t = Date.parse(iso);
  if (!Number.isFinite(t)) return String(iso);
  const d = new Date(t);
  const now = new Date();
  const hm =
    String(d.getHours()).padStart(2, '0') + ':' + String(d.getMinutes()).padStart(2, '0');
  const sameDay = (a, b) =>
    a.getFullYear() === b.getFullYear() &&
    a.getMonth() === b.getMonth() &&
    a.getDate() === b.getDate();
  if (sameDay(d, now)) return `今天 ${hm}`;
  if (sameDay(d, new Date(now.getTime() - 86400000))) return `昨天 ${hm}`;
  return `${d.getMonth() + 1}-${d.getDate()} ${hm}`;
}

/** 建一个元素。`el('div', 'row')` —— 比到处 `createElement` + `className` 短得多。 */
function el(tag, cls) {
  const e = document.createElement(tag);
  if (cls) e.className = cls;
  return e;
}

/** 调插件的**写**路由（要带 `x-panel: 1`，`call` 里已经处理了）。 */
const post = (path, body) => api(`/${path}`, { method: 'POST', body, write: true });

/** 把一段毫秒说成人话（「1 分 20 秒」而不是「80000」）。 */
function fmtMs(ms) {
  const s = Math.max(0, Math.round(ms / 1000));
  if (s < 60) return `${s} 秒`;
  if (s < 3600) return `${Math.floor(s / 60)} 分 ${s % 60} 秒`;
  // ⚠ 一小时后要进位：`1278 分 34 秒` 没人算得出那是多久
  const h = Math.floor(s / 3600);
  const m = Math.floor((s % 3600) / 60);
  return m === 0 ? `${h} 小时` : `${h} 小时 ${m} 分`;
}

const setText = (id, v) => {
  const el = $(id);
  if (el) el.textContent = v === undefined || v === null ? '' : String(v);
};

/** 画一个列表：每项 `[主文案, 副文案, 右侧 HTML]`。 */
function paintList(id, rows, emptyText) {
  const ul = $(id);
  if (!ul) return;
  if (!rows || rows.length === 0) {
    ul.innerHTML = `<li class="empty">${esc(emptyText)}</li>`;
    return;
  }
  ul.innerHTML = rows
    .map(
      (r) =>
        `<li class="item"><div class="item-main"><span class="item-t">${esc(r[0])}</span>` +
        (r[1] ? `<span class="item-s">${esc(r[1])}</span>` : '') +
        `</div>${r[2] ? `<div class="item-r">${r[2]}</div>` : ''}</li>`,
    )
    .join('');
}

// ──────────────────────────────────────────────────────────── 六个区块

/** 登录那一节。三种方式各自的状态由**路由**推，界面只负责显示与触发。 */
function renderLogin(l) {
  const qr = l.qr;
  const sms = l.sms;

  // ⚠ 这里**不再直接改 pane 的 hidden** —— 弹窗里页签的显隐由 `showAddTab` 统一管
  //   （原来改的是 `pane-qr`/`pane-sms`，那两个 id 现在不存在了）。
  //   这里的职责变成"**把页签跳到有事的那一个**"：
  if (qr) {
    if ($('add-dialog').hidden === false) showAddTab('qr');
    if (qr.expired) {
      // ⚠ 过期的码**不许画得像还活着**：它扫了也白扫（手机上会回"二维码已失效"），
      //   而"等手机扫码 · 剩 0 秒"看着完全像还能扫 —— 用户会一直对着它试。
      //   所以把码撤掉、直说下一步，并停掉轮询（再轮也只是问一个已经死掉的 sID）。
      $('qr-box').hidden = true;
      $('qr-box').innerHTML = '';
      setText('qr-note', '二维码过期了 —— 点「生成二维码」重新出码');
    } else {
      $('qr-box').hidden = false;
      // 二维码是**插件这一侧**渲染好的 SVG（浏览器里没有二维码库）
      $('qr-box').innerHTML = qr.svg || '';
      const sec = Math.ceil(qr.expiresInMs / 1000);
      // ⚠ 相位不同文案要不同：`scanned` 之后**该等的是手机**，不是"再扫一次"。
      setText(
        'qr-note',
        qr.scanned ? `已扫码，请在手机上确认 · 剩 ${sec} 秒` : `${qr.note} · 剩 ${sec} 秒`,
      );
    }
    // 刷新页面 / 重开弹窗之后**接着轮**（会话是落盘的，手机扫的时候我们得在问上游）
    syncQrPoll(qr);
  } else {
    stopQrPoll();
    $('qr-box').hidden = true;
    $('qr-box').innerHTML = '';
  }

  // ⚠ 滑块**不画在界面上**（F-073）：坐标是这一侧从 `puzzleLeft` 解出来的
  //   （`sliderAnswer`），发码那一路会自动取题、解坐标、带答案重发 ——
  //   人只负责等短信。摆一张图反而像在要人肉眼看缺口。
  if (sms) {
    if ($('add-dialog').hidden === false) showAddTab('sms');
    paintCodePane('sms', sms);
  } else {
    paintCodePane('sms', null);
  }

  // 手机端那一格（形状与 `sms` 一样，见 `loginReading` 里那段说明）
  const app = l.app;
  if (app) {
    if ($('add-dialog').hidden === false) showAddTab('app');
    paintCodePane('app', app);
  } else {
    paintCodePane('app', null);
  }
}

// ──────────────────────────────────────────────────────────── 手机号 + 验证码：一屏

/** 手机号打码：`13800000000` → `138****0000`。
 *
 *  ⚠ 只用在**说明文字**里（"验证码已发到 138****0000"）：输入框本身就是号码的
 *  显示位置，而它会一直留在屏幕上。 */
function maskPhone(p) {
  const s = String(p || '');
  return /^\d{11}$/.test(s) ? `${s.slice(0, 3)}****${s.slice(7)}` : s;
}

/** 主按钮的可用性：**只按"还差几个字符"**，不问服务端。
 *
 *  ⚠ 这是提前告知，不是替用户做判断 —— 点了才报错的写法，用户要先失败一次
 *  才知道规则（而这里唯一的规则就是位数）。
 *  ⚠ 被重发倒计时接管的按钮**不由位数决定**（它由倒计时决定）：每次动作之后都会
 *  `load()` → 重画 → 走到这里，不排掉的话「重新获取（N 秒）」会被重新点亮，
 *  用户就能在倒计时里再点一次（连点正是要防的那件事）。 */
function gate(btnId, value, need) {
  const el = $(btnId);
  if (el && el.dataset.resend !== '1') {
    el.disabled = String(value || '').trim().length !== need;
  }
}

/** 两个页签各一份倒计时状态（发码之后 60 秒内不许再点）。 */
const resendTimers = { sms: null, app: null };

/** 停掉某个页签的重发倒计时，按钮回到「获取验证码」。 */
function stopResend(kind) {
  if (resendTimers[kind] !== null) {
    clearInterval(resendTimers[kind]);
    resendTimers[kind] = null;
  }
  const btn = $(`${kind}-send`);
  if (btn) {
    delete btn.dataset.resend;
    btn.textContent = '获取验证码';
    btn.disabled = false;
  }
}

/** 发码成功之后开始倒计时。**这是界面节奏，不是权威** ——
 *  能不能发、还剩几次，最终是服务端说了算（`remainGetTimes`）。
 *  它的用处是别让用户连点：连点只会喂限流。 */
function startResend(kind, seconds = 60) {
  const btn = $(`${kind}-send`);
  let left = seconds;
  if (resendTimers[kind] !== null) clearInterval(resendTimers[kind]);
  // ⚠ `dataset.resend` 是给 `act` 看的：倒计时接管了这个按钮之后，
  //   `act` 的收尾**不许**把它改回「获取验证码」、也不许把它重新点亮。
  if (btn) btn.dataset.resend = '1';
  const tick = () => {
    if (left <= 0) {
      stopResend(kind);
      return;
    }
    if (btn) {
      btn.textContent = `重新获取（${left} 秒）`;
      btn.disabled = true;
    }
    left -= 1;
  };
  tick();
  resendTimers[kind] = setInterval(tick, 1000);
}

/** 画这一屏的状态（两条路共用）。`s === null` ⇒ 还没开始这一趟。
 *
 *  ⚠ **一屏**，排布照参考实现自己的登录页：手机号（右端嵌「获取验证码」）→ 验证码
 *  → 整宽「登录」。第一版我做成了"两步"（下一步 → 登录），用户把参考发过来之后
 *  改成了一屏 —— 别让用户先点一次「下一步」才看到验证码那一格。 */
function paintCodePane(kind, s) {
  const phone = $(`${kind}-phone`);
  if (s) {
    if (phone && document.activeElement !== phone) phone.value = s.phone;
    // 一句人话回答"现在该干什么"：没发码 ⇒ 点获取；发了 ⇒ 去看短信
    const left = Math.max(0, Math.round(Number(s.expiresInMs) / 60000));
    setText(
      `${kind}-note`,
      s.sent
        ? `验证码已发到 ${maskPhone(s.phone)}${left > 0 ? ` · ${left} 分钟内有效` : ''}。没收到就点「重新获取」。`
        : '点「获取验证码」，码会发到上面这个号。',
    );
  }
  gate(`${kind}-send`, phone.value, 11);
  gate(`${kind}-submit`, $(`${kind}-code`).value, 6);
}

/** 输入框的实时反应：只留数字、按位数开关按钮、回车等于点那个主按钮。
 *
 *  ⚠ 两个输入框**当参数传进来**（而不是在函数里用 `${kind}-phone` 拼）：
 *  界面测试按字面 id 检查"每个控件都真的被取用过"，拼出来的字符串它看不见。 */
function wireCodeForm(kind, phone, code) {
  phone.addEventListener('input', () => {
    phone.value = phone.value.replace(/\D/g, '').slice(0, 11);
    gate(`${kind}-send`, phone.value, 11);
  });
  phone.addEventListener('keydown', (ev) => {
    if (ev.key === 'Enter' && !$(`${kind}-send`).disabled) $(`${kind}-send`).click();
  });
  code.addEventListener('input', () => {
    code.value = code.value.replace(/\D/g, '').slice(0, 6);
    gate(`${kind}-submit`, code.value, 6);
    // 填满了就把焦点交给「登录」：手机上少按一次（**不自动提交** —— 点了才算）
    if (code.value.length === 6) $(`${kind}-submit`).focus();
  });
  code.addEventListener('keydown', (ev) => {
    if (ev.key === 'Enter' && !$(`${kind}-submit`).disabled) $(`${kind}-submit`).click();
  });
}

/** 点「获取验证码」：**先把会话开起来再发**。
 *
 *  ⚠ 参考实现是一屏（手机号旁边就是「获取验证码」），而这一侧的后端要先有会话
 *  才肯发码（`/login/sms/start` 校验号码、并把"用哪种客户端形态"冻进会话）。
 *  所以这里**串一下**：先 start（纯本地，不发请求），再 send。用户看到的仍然是一屏。
 *  ⚠ 每次点都重新 start：形态可能刚被改过，而"界面上的选择"必须就是这一趟用的档案。 */
async function requestCode(kind) {
  const phone = $(`${kind}-phone`).value.trim();
  if (kind === 'sms') {
    await api('/login/sms/start', { method: 'POST', write: true, body: { phone, pcPlatform: addPc } });
    return api('/login/sms/send', { method: 'POST', write: true });
  }
  await api('/login/app/start', { method: 'POST', write: true, body: { phone } });
  return api('/login/app/send', { method: 'POST', write: true });
}

function renderAccount(a, login) {
  const chip = $('acct-chip');
  const list = a.accounts || [];
  const n = list.length;

  if (n === 0) {
    // ⚠ 「有会话、没账号」是**一种正常状态**，不能一律说成「还没登录」。
    //
    // 贴一条会话串（凭据页那条路）不产生账号，但会话本身能用 —— 靠它的任务照跑。
    // 原来这里不管这一层，于是"能用的东西"在界面上长得和"什么都没配"一模一样，
    // 用户会去重新登录一个本来好好的账号，而他没有任何线索知道自己多虑了。
    const ses0 = a.session || {};
    setText('acct-hero', '还没登录账号');
    if (ses0.present) {
      const age = Math.max(0, Math.round(Number(ses0.ageHours) || 0));
      const when = age < 1 ? '刚存下' : `${age} 小时前存下`;
      setText(
        'acct-sub',
        ses0.lastCheckOk
          ? `你贴进来的会话还能用（${when}）。`
          : `你贴进来的会话不通了（${ses0.lastCheckNote || '验证没过'}）—— 重新贴一条，或者扫一次码。`,
      );
      chip.textContent = ses0.lastCheckOk ? '会话可用' : '会话不通';
      chip.className = ses0.lastCheckOk ? 'badge' : 'badge warning';
    } else {
      setText('acct-sub', '登录之后才能签到、领豆、领小红花。');
      chip.textContent = '还没登录';
      chip.className = 'badge idle';
    }
  } else {
    // ⚠ 原来这里挑「当前账号」那一个报名字（`list.find((x) => x.current)`）。
    //   删徽章（F-106）时这一处一起改：多个号时按那个指针点名 = 在瞎指，
    //   而它会被兑换任务停在**最后遍历的那个号**上。
    const one = list[0];
    setText('acct-hero', n > 1 ? `已登录 ${n} 个账号` : `已登录 ${one.label || one.masked}`);
    setText('acct-sub', n > 1 ? `一共 ${n} 个账号，任务会挨个跑。` : '任务会用这个账号跑。');
    chip.textContent = n > 1 ? `${n} 个账号` : one.label || one.masked;
    chip.className = 'badge success';
  }

  // 会话体检。这一条回答「我的登录还有效吗」—— 票过期时**所有**任务一起失败，
  // 而那个现象看起来像「账号被踢了」，其实是票到期。
  const ses = a.session || {};
  if (n > 0 && ses.note) setText('acct-sub', ses.note);

  // 账号卡 —— 照 mcloud：昵称 + 平台徽章 + `设备 … · 凭据 N 天后` + 五个动作。
  //
  // ⚠ 用 `innerHTML` 直接搭（原来是 `paintList` 的三列网格，形状对不上卡片）。
  const box = $('acct-list');
  box.innerHTML =
    n === 0
      ? '<li class="hint">还没有账号。点上面那个「添加账号」。</li>'
      : list
          .map((x) => {
            // 平台徽章：手机端 / macOS / Windows。认不出就**如实说**，不猜。
            //
            // ⚠ 用 `.badge lang`（**不带状态圆点**）—— 平台是"这是台什么设备"的
            //   **描述**，不是状态。面板的圆点（`.badge::before`）是"状态"的视觉语言：
            //   带圆点的「Windows」看起来像"Windows 处于某种状态"，
            //   而旁边真状态徽章（「已停用」）反而被混淆了。
            //   `.badge.lang` 在面板里就是这个用途（语言标签），只去掉圆点，底色不变。
            const badge =
              x.platform === 'mobile'
                ? '<span class="badge lang">手机端</span>'
                : x.platform === 'pc'
                  ? `<span class="badge lang">${x.pcPlatform === 'mac' ? 'macOS' : 'Windows'}</span>`
                  : '<span class="badge warning">平台未知</span>';
            // `凭据 N 天后`：`null` 是"这条凭据里没带到期时间"，**不能画成 0**
            //（那看起来像"已过期"，用户会去重新登录一个本来好好的账号）。
            // ⚠ 这里**故意不提**票的到期时刻。
            //   那是插件自己的内部机制（票很短命、任务会自己续），
            //   用户既管不了也不需要知道 —— 摆出来只会让人以为要动手做什么。
            //   真续不上时，跑单那句会说「重新导入会话」，那时才需要他。
            const exp =
              x.expireDays === null
                ? '不知道什么时候过期'
                : x.expireDays >= 0
                  ? `${x.expireDays} 天后过期`
                  : `已经过期 ${-x.expireDays} 天`;
            const dev = x.deviceId ? `设备 ${esc(String(x.deviceId).slice(0, 10))}… · ` : '';
            // 卡片用主面板那套（`.card` + `.card-head` + `.card-title` +
            // `.card-actions`），与面板的插件卡、任务卡同源。
            return `
              <li class="card acct-card">
                <div class="card-head">
                  <div class="card-head-main">
                    <span class="card-title${x.disabled ? ' is-off' : ''}">${esc(x.label || x.masked)}</span>
                    ${badge}
                    ${x.disabled ? '<span class="badge idle">已停用</span>' : ''}
                  </div>
                  <div class="card-actions">
                    <!-- ⚠ 这里原来有「切到这个」，卡片标题旁有徽章「当前账号」
                         （+ 一圈 accent 描边）。两个都删了 —— 它们说的是后端那个
                         指针 mcloud.account.current，而它在界面上**没有主人**：
                         切账号是任务页的事（F-062），兑换任务跑完还会把那个指针
                         停在最后遍历的那个号上。留着就是"看起来有个号被选中了"，
                         而任务实际用哪些号只在任务页那三个勾选里。
                         指针本身仍要留（按账号的键靠它兜底），只是不再摆给用户看。 -->
                    <button class="btn" data-acc-refresh="${esc(x.id)}">刷新</button>
                    <button class="btn" data-acc-rename="${esc(x.id)}">编辑</button>
                    <button class="btn" data-acc-settings="${esc(x.id)}">设置</button>
                    <button class="btn" data-acc-disable="${esc(x.id)}" data-on="${x.disabled ? '1' : ''}">${x.disabled ? '启用' : '停用'}</button>
                    <button class="btn danger" data-acc-del="${esc(x.id)}">移除</button>
                  </div>
                </div>
                <div class="card-sub">${dev}${exp}</div>
              </li>`;
          })
          .join('');

  // 三条任务的账号选择器
  for (const cfg of PICKERS) renderPicker(cfg);

  // 本机设备指纹：**只读显示**（它不是用户填的东西 —— 见 index.html 那段说明）。
  setText('device-id-show', login.deviceId || '（还没生成）');

  // ⚠ 这几行原来往 `login-note` 写"扫码/短信/账密都能用…" —— 那个 id 现在是
  //   弹窗里扫码页签的提示，写法也不对了（贴的是**凭据**不是"会话串"）。删掉。
  renderLogin(login || { qr: null, sms: null });
}


// ══════════ 任务排期（来自**面板**，不是写死的文案） ══════════
//
// 界面上那句「它自己什么时候跑」必须是**真的**：原来写死成「白天每 30 分钟跑一次」，
// 而排期早改成 `0 8,20 * * *`（一天两次）—— 用户看到的就是一句错话。
// 真值在面板的 `GET /api/state` 里（`jobs[].schedule` / `next_run` / `paused`）。
//
// ⚠ 这是**面板**的接口（同源、同一套 cookie），不是插件的 —— 插件的路由不该
//   替面板转述排期（那样两份数据迟早不一致）。
let JOBS = [];

/** 取某条任务的 job（`plugin` + 任务名）。 */
function jobOf(plugin, name) {
  return JOBS.find((j) => j.plugin === plugin && j.name === name) || null;
}

/** 把面板里的排期串说成人话。
 *
 *  只认我们真用到的这几种形状；**认不出就说"按面板里设的时间跑"** ——
 *  把一串星号糊到用户脸上比说不知道更糟，而编一句"每天几点"出来更糟。 */
function scheduleText(job) {
  const expr = String(job.schedule || '').trim();
  const next = job.next_run ? when(String(job.next_run).replace(' ', 'T')) : '';
  const tail = next === '' ? '' : `，下次 ${next}`;
  const at = (h, m) => `${String(h).padStart(2, '0')}:${String(m).padStart(2, '0')}`;
  let m = /^(\d{1,2})\s+(\d{1,2}(?:,\d{1,2})*)\s+\*\s+\*\s+\*$/.exec(expr);
  if (m !== null) {
    const mins = Number(m[1]);
    const hours = m[2].split(',').map((h) => at(Number(h), mins)).join('、');
    return `每天 ${hours} 各跑一次${tail}`;
  }
  m = /^(\d{1,2})\s+\*\s+\*\s+\*\s+\*$/.exec(expr);
  if (m !== null) return `每小时的第 ${Number(m[1])} 分跑一次${tail}`;
  m = /^\*\/(\d{1,2})\s+\*\s+\*\s+\*\s+\*$/.exec(expr);
  if (m !== null) return `每 ${Number(m[1])} 分钟跑一次${tail}`;
  return `按面板里设的时间跑${tail}`;
}

/** 拉一次面板的任务表（失败就安静：那只是第二行文案，不该弹错误）。 */
async function loadJobs() {
  try {
    const r = await fetch('/api/state', { headers: { 'x-panel': '1' } });
    if (!r.ok) return;
    const j = await r.json();
    JOBS = Array.isArray(j.jobs) ? j.jobs.filter((x) => x.deleted !== true) : [];
  } catch {
    JOBS = [];
  }
}

function renderDaily(d) {
  const run = d.last;
  // ⚠ 这里**不能**写「今天已完成」—— 那读起来像"今天不会再跑了"。
  //   一天两趟都是**照跑**的：第二趟是兜底（插件没法证明所有小任务都到了预期，
  //   见 `src/entries/daily.ts` 里那段）。所以这里说"跑过几趟"。
  const localDay = (iso) => {
    const t = new Date(String(iso || ''));
    return Number.isNaN(t.getTime()) ? '' : t.toLocaleDateString('sv');
  };
  const todayKey = new Date().toLocaleDateString('sv');
  const todayRuns = (d.recent || []).filter((x) => localDay(x.at) === todayKey).length;
  if (!run) {
    setText('daily-hero', '还没跑过');
  } else if (!run.ok) {
    setText('daily-hero', '上次有失败');
  } else if (todayRuns > 0) {
    setText('daily-hero', `今天跑过 ${todayRuns} 趟`);
  } else {
    setText('daily-hero', '今天还没跑完');
  }
  // 第二行只放**两件事**：它什么时候自己跑（来自面板那条任务的排期）+
  // 上一次的结果摘要。细节在下面的日志里 —— 用户的原话：
  // 「这一块没必要了，用户看不懂」（指的是那一长串"未登录：state 里没有会话"）。
  const bits = [];
  const job = jobOf('mcloud-daily', '日常');
  if (job) {
    bits.push(scheduleText(job));
    if (job.paused) bits.push('现在**暂停着**（去面板的任务页恢复）');
  }
  if (run) {
    const cnt = [];
    if (run.badCount > 0) cnt.push(`${run.badCount} 步失败`);
    if (run.pendingCount > 0) cnt.push(`${run.pendingCount} 项等服务端确认`);
    if (run.okCount > 0) cnt.push(`${run.okCount} 步完成`);
    if (run.idleCount > 0) cnt.push(`${run.idleCount} 步跳过`);
    bits.push(`上次 ${when(run.at)}${cnt.length ? `：${cnt.join('、')}` : ''}`);
  } else {
    bits.push('点「现在跑一次」看看它怎么走');
  }
  setText('daily-sub', bits.join(' · '));
  renderAcctChips('daily-accounts', 'daily-account-detail', d.recent);
}

/** 上一次**每个号**的结果，一行 chips；点一个展开那个号的每一步。
 *
 *  ⚠ 数据源是读数里那些"一次一条"的运行记录：同一次触发写下的几条**共用一个 `at`**
 *   （id 里带账号，所以 id 各不相同 —— 见 `runLog.ts` 的 `RunRecord.id`）。
 *  ⚠ 名字用记录里的 `who`；只有旧记录没那一格时才去 `note` 的「名字：」里取。
 *  ⚠ 每一步的明细**点开才拉**（`/runs?id=`）—— 一次 16 步 × 3 个号全塞进读数
 *   就是几百 KB 的首屏。 */
function renderAcctChips(boxId, detailId, recent) {
  const box = $(boxId);
  if (!box) return;
  const detail = $(detailId);
  box.textContent = '';
  if (detail) detail.innerHTML = '';
  const list = recent || [];
  if (list.length === 0) return;
  const runAt = list[0].at;
  const sameRun = list.filter((x) => x.at === runAt);
  // ⚠ 三个账号说**同一句话**的时候，三枚一模一样的 chip 只是噪声 ——
  //   而且会让人以为"每个号各出了一件事"。合成一枚：「3 个账号」+ 那句话。
  //   （现场：领小红花没开时，三个号都是「领小红花没开（要先在服务端那边开启）」。）
  const restOf = (one) => {
    const note = String(one.note || '');
    const cut = note.indexOf('：');
    return (cut > 0 ? note.slice(cut + 1) : note) || (one.ok ? '都好了' : '有没成的');
  };
  if (sameRun.length > 1 && new Set(sameRun.map(restOf)).size === 1) {
    const one = sameRun[0];
    const chip = el('button', 'acct-chip' + (one.ok ? ' is-good' : ' is-bad'));
    chip.type = 'button';
    chip.dataset.runId = one.id;
    const name = el('b');
    name.textContent = `${sameRun.length} 个账号`;
    const rest = el('span');
    rest.textContent = restOf(one);
    chip.append(name, rest);
    if (detail) chip.addEventListener('click', () => toggleRunDetail(detail, one.id, chip));
    box.appendChild(chip);
    return;
  }
  for (const one of sameRun) {
    const note = String(one.note || '');
    const cut = note.indexOf('：');
    const who = one.who || (cut > 0 ? note.slice(0, cut) : '这个号');
    const chip = el('button', 'acct-chip' + (one.ok ? ' is-good' : ' is-bad'));
    chip.type = 'button';
    chip.dataset.runId = one.id;
    const name = el('b');
    name.textContent = who;
    const rest = el('span');
    rest.textContent =
      (cut > 0 ? note.slice(cut + 1) : note) || (one.ok ? '都好了' : '有没成的');
    chip.append(name, rest);
    if (detail) chip.addEventListener('click', () => toggleRunDetail(detail, one.id, chip));
    box.appendChild(chip);
  }
}

/** 一行一个用户认得的任务 —— 与后端 `tasks/stepLog.ts::userLine` 同一套口径。
 *
 *  用户只要两件事：成了没有、没成是什么原因。所以：
 *   `签到：成了` / `签到：没成 —— <原因>` / `领取AI豆：今天没有可领的`
 *  内部管道步骤（后端在运行记录里标了 `plumb`）成功时不出现（`null`）——
 *  界面这侧**不维护名字表**：两份名字迟早不一致，而且那些词本身就是黑话。
 */
function stepLine(step) {
  const name = String(step.name || '');
  if (step.plumb === true && step.ok) return null;
  let why = String(step.note || '');
  for (const n of [name, name.split('（')[0] || name]) {
    if (why.startsWith(n + '：')) {
      why = why.slice(n.length + 1);
      break;
    }
  }
  why = why.replace(/^(第 )?\d+ 步(失败|走完) —— /, '');
  // 同后端 `trimNote`：`note` 的前缀是名字的一部分（`消息推送：…` vs `消息推送奖励（问状态）`）
  const lead = /^([^：]{2,12})：/.exec(why);
  if (lead !== null && name.includes(lead[1])) why = why.slice(lead[0].length);
  const raw = /^code=(-?\d+)(?: msg=(.*))?$/.exec(why.trim());
  if (raw !== null) {
    const msg = String(raw[2] || '').trim();
    why = msg === '' ? `服务端没给原因（编号 ${raw[1]}）` : `服务端说：${msg}`;
  }
  why = why.replace(/（服务端回 \{[^）]*\}）/g, '（服务端确认了）').trim();
  // ⚠ 与后端 `userLine` 同一套：不写"成了/成功/跳过"这种抽象词，
  //   直接把这一步**发生了什么**说出来（用户的原话：「成了是啥啊」）。
  if (step.pending === true) return `${name}：${why === '' ? '等服务端确认' : why}`;
  if (!step.ok) return `${name}：没成${why === '' ? '' : ` —— ${why}`}`;
  return `${name}：${why === '' ? '好了' : why}`;
}

/** 展开/收起某个号那一次的每一步。 */
async function toggleRunDetail(box, runId, chip) {
  const open = box.dataset.run === runId;
  box.innerHTML = '';
  for (const c of box.parentElement.querySelectorAll('.acct-chip')) c.classList.remove('is-open');
  if (open) {
    delete box.dataset.run;
    return;
  }
  box.dataset.run = runId;
  chip.classList.add('is-open');
  const li = el('li', 'hint');
  li.textContent = '正在读那一次的每一步…';
  box.appendChild(li);
  try {
    const r = await api('/runs?id=' + encodeURIComponent(runId));
    box.innerHTML = '';
    // 失败的在前面：点开就是为了看它
    const steps = (r.steps || []).slice().sort((a, b) => Number(a.ok) - Number(b.ok));
    let shown = 0;
    for (const step of steps) {
      const line = stepLine(step);
      if (line === null) continue; // 管道步骤成了就不提
      const row = el('li', step.pending === true ? 'is-idle' : step.ok ? '' : 'is-bad');
      row.textContent = line;
      box.appendChild(row);
      shown += 1;
    }
    if (shown === 0) {
      const none = el('li', 'hint');
      none.textContent = steps.length === 0 ? '这一次没有记下每一步。' : '这一次没有要报的事。';
      box.appendChild(none);
    }
  } catch (e) {
    box.innerHTML = '';
    const bad = el('li', 'hint');
    bad.textContent = `读不到那一次：${e && e.message ? e.message : e}`;
    box.appendChild(bad);
  }
}


// ══════════════ 兑换页（对齐原 Rust mcloud 的兑换页）══════════════
//
// 原页面有：关注、按组看清单、状态/库存、二次确认、立即兑换、定时。
// 插件这一侧保留自己的多账号任务模型；下面这些是**能搬过来的行为**：
//   · 关注（按账号存，见 exchangeList.ts 的 STATE_SUBS）
//   · 按 groupId 分组、搜名字/编号、只看可兑
//   · 每件显示价格 / 池子 / 今日份额 / 判据，被挡住的不给「立即兑换」
//   · 「立即兑换」先确认（可在页面关掉），然后逐步推进后端 order 状态机
//   · 「定时」写定时链读的那一格（不直接下单；真正几点抢在下面选）
const EX_CONFIRM_KEY = 'mcloud-exchange-confirm';
const EX_LOCAL_JUDGE_KEY = 'mcloud-exchange-local-judge';
let exBusy = false;

function exConfirmFirst() {
  try {
    return localStorage.getItem(EX_CONFIRM_KEY) !== '0';
  } catch {
    return true;
  }
}

function exSetConfirmFirst(on) {
  try {
    localStorage.setItem(EX_CONFIRM_KEY, on ? '1' : '0');
  } catch {
    /* 存不进去只影响下次记不记得，不影响这次 */
  }
}

/** 本地判定：开着就按判据挡住；关掉 = 被挡的档也真发单（原页面同一个语义）。 */
function exLocalJudge() {
  try {
    return localStorage.getItem(EX_LOCAL_JUDGE_KEY) !== '0';
  } catch {
    return true;
  }
}

function exSetLocalJudge(on) {
  try {
    localStorage.setItem(EX_LOCAL_JUDGE_KEY, on ? '1' : '0');
  } catch {
    /* 同上 */
  }
}

/** 判据 → 人话。表头/行内共用一份（`e.verdictText` 由后端给）。 */
function exSay(v) {
  const t = last && last.exchange && last.exchange.verdictText;
  return (t && t[v]) || v || '';
}

function exSubs() {
  const v = last && last.exchange && last.exchange.subs;
  return Array.isArray(v) ? v : [];
}

function exSamePrize(a, b) {
  if (a && b && a.id !== undefined && b.id !== undefined) return a.id === b.id;
  return !!a && !!b && a.name === b.name;
}

function exIsSub(p) {
  return exSubs().some((s) => exSamePrize(s, p));
}

/** 清单缓存：表格 / 关注 / 定时弹窗共用一份。 */
let exPicks = [];

function exFindPick(name, id) {
  return exPicks.find((p) => (id !== undefined && p.id === id) || p.name === name);
}

/** 价格 / 池子 / 今日份额 —— 各自标清，不混成一句。 */
function exStockText(p) {
  const bits = [];
  if (p.count !== undefined) {
    bits.push(p.totalCount !== undefined ? `现量 ${p.count} / 共 ${p.totalCount}` : `现量 ${p.count}`);
  }
  if (p.dailyRemainderCount !== undefined) {
    bits.push(
      p.dailyCount !== undefined
        ? `今日 ${p.dailyRemainderCount}/${p.dailyCount}`
        : `今日还剩 ${p.dailyRemainderCount}`,
    );
  }
  if (p.monthLimit !== undefined) bits.push(`本月 ${p.received ?? 0}/${p.monthLimit}`);
  return bits.join(' · ');
}

/** 这一件值不值得「定时」：时间会改变结论的档才给（对齐原页面的 `canSchedule`）。 */
function exCanSchedule(p) {
  if (p.verdict === 'ok') return true;
  return (
    p.verdict === 'tomorrow' ||
    p.verdict === 'min-remainder-zero' ||
    p.verdict === 'sold-out' ||
    p.verdict === 'daily-empty'
  );
}

function exGroupId(p) {
  return p.groupId === undefined || p.groupId === null ? 'none' : String(p.groupId);
}

// 组字典（上游静态文件 `sign_in_3/prizeGroup.json`）里的组名。
// 上游清单只给 groupId，不给组名；这里按 2026-10-18 实拍把常用组名钉下来。
// 没列到的组不硬编数字：显示「其他奖品」，宁可少一点信息也不要再出现「第 N 组」。
const EX_GROUP_TITLES = {
  '0': '其他权益奖品',
  '1': '视频类会员',
  '2': '音乐类会员',
  '3': '山西专属流量奖品',
  '4': '限时抢兑',
  '5': '外卖美食权益',
  '6': '交通出行类',
  '7': '快递寄件券',
  '8': '云盘转存券',
  '9': '实用工具类',
  '11': '美食饮品权益',
  '12': '西藏用户专属',
  '13': '咖啡饮品权益',
  '14': '游戏礼包权益',
  '15': '限时福利',
};

// 组规则也来自同一份 prizeGroup.json 的 subTitle；空串 = 这一组没有额外规则。
const EX_GROUP_RULES = {
  '1': '(月卡每月多选一限兑)',
  '2': '(月卡每月多选一限兑)',
  '3': '(每月多选一限兑)',
  '5': '(每月多选一限兑)',
  '6': '（每月多选一限兑）',
  '7': '（每月多选一限兑）',
  '11': '(每月多选一限兑)',
  '12': '(每月多选一限兑)',
  '13': '(每月多选一限兑)',
  '14': '(每月多选一限兑)',
};

function exGroupTitle(key) {
  if (key === 'none') return '未分组';
  return EX_GROUP_TITLES[key] || '其他奖品';
}

function exGroupRule(key) {
  return EX_GROUP_RULES[key] || '';
}

/** 展开中的组。默认空 = 全部折叠；点组头才展开，重绘后仍保持。 */
const exOpenGroups = new Set();

function exToggleGroup(key) {
  if (exOpenGroups.has(key)) exOpenGroups.delete(key);
  else exOpenGroups.add(key);
  repaintExchange();
}

function exGroupWindow(items) {
  const s = items.find((p) => p.startTime)?.startTime;
  const e = items.find((p) => p.endTime)?.endTime;
  if (!s && !e) return '';
  const fmt = (v) => (typeof shortWhen === 'function' ? shortWhen(v) : v);
  return `${s ? fmt(s) : '?'} ~ ${e ? fmt(e) : '?'}`;
}

/** 当前账号下拉。切账号会改「当前账号」，任务也会跟着用这个号。 */
function renderExchangeAccount() {
  const sel = $('ex-account');
  if (!sel) return;
  const acc = (last && last.account) || {};
  const list = Array.isArray(acc.accounts) ? acc.accounts : [];
  if (document.activeElement === sel) return;
  sel.innerHTML = list.length
    ? list
        .map(
          (a) =>
            `<option value="${esc(a.id)}"${a.current ? ' selected' : ''}>${esc(a.label || a.id)}</option>`,
        )
        .join('')
    : '<option value="">还没有账号</option>';
  sel.disabled = list.length === 0;
}

/** 我关注的：固定在上面，和下面清单里的星星是同一份数据。 */
function renderExchangeSubs() {
  const box = $('ex-subs');
  const subLine = $('ex-subs-sub');
  if (!box) return;
  const subs = exSubs();
  if (subLine) subLine.textContent = subs.length ? `${subs.length} 件` : '';
  if (!subs.length) {
    box.innerHTML =
      '<p class="hint">在下面清单里点 ☆ 关注一件，它会固定到这一块 —— 以后不用每次翻组找它。</p>';
    return;
  }
  box.innerHTML = subs
    .map((s) => {
      const live = exFindPick(s.name, s.id);
      const status = live ? exSay(live.verdict) : '不在今天的清单里';
      const stock = live ? exStockText(live) : '';
      const price = live && live.price !== undefined ? `${live.price} 豆` : '';
      const bits = [price, stock].filter(Boolean).join(' · ');
      return (
        `<div class="ex-sub-row">` +
        `<span class="ex-sub-name">${esc(s.name)}</span>` +
        `<span class="badge ${live && live.verdict === 'ok' ? 'success' : 'idle'}">${esc(status)}</span>` +
        `<span class="hint grow">${esc(bits)}</span>` +
        `<button class="btn btn-sm ex-star is-on" data-unsub-name="${esc(s.name)}" title="取消关注">★</button>` +
        (live
          ? (exCanSchedule(live)
              ? `<button class="btn btn-sm" data-plan-name="${esc(s.name)}" title="打开定时抢兑弹窗">定时</button>`
              : '') +
            (live.verdict === 'ok' || !exLocalJudge()
              ? `<button class="btn btn-sm primary" data-buy-name="${esc(s.name)}"${exBusy ? ' disabled' : ''}>立即兑换</button>`
              : '')
          : '') +
        `</div>`
      );
    })
    .join('');
}

/** 清单表：按 groupId 分组；被挡住的件不给「立即兑换」，时间会变的仍给「定时」。 */
function renderExchangeTable(e, say) {
  const tbl = $('ex-tbl');
  if (!tbl) return;
  const q = (($('ex-filter') && $('ex-filter').value) || '').trim().toLowerCase();
  const only = ($('ex-verdict') && $('ex-verdict').value) || '';
  const showBlocked = !!($('ex-show-blocked') && $('ex-show-blocked').checked);
  const blocked = exPicks.filter((p) => p.verdict !== 'ok').length;
  const blockedCount = $('ex-blocked-count');
  if (blockedCount) blockedCount.textContent = String(blocked);
  const shown = exPicks.filter((p) => {
    if (q && !(p.name.toLowerCase().includes(q) || String(p.id ?? '').includes(q))) return false;
    if (only === 'ok' && p.verdict !== 'ok') return false;
    if (only === 'sub' && !exIsSub(p)) return false;
    // 原页面把"不可兑"折进一行；这里默认也折起来 —— 一份 50 件全判不了的清单
    // 逐件铺 50 个"判不了"的徽章，读的人只会看到一堵墙。
    if (!showBlocked && p.verdict !== 'ok') return false;
    return true;
  });
  const count = $('ex-count');
  if (count) {
    count.textContent = exPicks.length
      ? `显示 ${shown.length} / ${exPicks.length} 件 · 可兑 ${exPicks.filter((p) => p.verdict === 'ok').length} 件 · 不可兑 ${blocked} 件`
      : '';
  }
  if (!exPicks.length) {
    tbl.innerHTML = '<tr><td class="empty">还没有清单 —— 点旁边那颗「读一次」。</td></tr>';
    return;
  }
  if (!shown.length) {
    const why = [
      ...new Set(exPicks.filter((p) => p.verdict !== 'ok').map((p) => say(p.verdict))),
    ].join('、');
    tbl.innerHTML =
      '<tr><td class="empty">' +
      (blocked > 0 && !showBlocked
        ? `现在没有可兑的；另有 ${blocked} 件不可兑${why ? `（${why}）` : ''}。勾上面的「显示不可兑」查看每一件。`
        : '没有匹配的奖品。') +
      '</td></tr>';
    return;
  }
  const groups = new Map();
  for (const p of shown) {
    const k = exGroupId(p);
    if (!groups.has(k)) groups.set(k, []);
    groups.get(k).push(p);
  }
  const sorted = [...groups.entries()].sort((a, b) => {
    const ao = a[1].some((p) => p.verdict === 'ok') ? 0 : 1;
    const bo = b[1].some((p) => p.verdict === 'ok') ? 0 : 1;
    if (ao !== bo) return ao - bo;
    return (Number(a[0]) || 0) - (Number(b[0]) || 0);
  });
  const rows = [
    '<thead><tr><th>奖品</th><th>编号</th><th>价格</th><th>池子 / 今日</th><th>能不能换</th><th>操作</th></tr></thead><tbody>',
  ];
  for (const [key, items] of sorted) {
    const win = exGroupWindow(items);
    const ok = items.filter((p) => p.verdict === 'ok').length;
    const open = exOpenGroups.has(key);
    rows.push(
      `<tr class="ex-group-row"><td colspan="6">` +
        `<button type="button" class="ex-group-toggle" data-group-toggle="${esc(key)}" aria-expanded="${open}">` +
        `<span class="ex-group-caret" aria-hidden="true"></span>` +
        `<span class="ex-group-name">${esc(exGroupTitle(key))}</span>` +
        (exGroupRule(key) ? `<span class="ex-group-rule">${esc(exGroupRule(key))}</span>` : '') +
        `<span class="hint"> · ${items.length} 件 · 可兑 ${ok} 件${win ? ` · ${esc(win)}` : ''}</span>` +
        `</button></td></tr>`,
    );
    for (const p of items) {
      const idx = exPicks.indexOf(p);
      const isSub = exIsSub(p);
      const stock = exStockText(p) || '——';
      rows.push(
        `<tr data-group-key="${esc(key)}"${open ? '' : ' hidden'}>` +
          `<td data-label="奖品">${esc(p.name)}</td>` +
          `<td data-label="编号" class="mono dim">${esc(p.id ?? '——')}</td>` +
          `<td data-label="价格">${p.price === undefined ? '——' : `${esc(p.price)} 豆`}</td>` +
          `<td data-label="池子 / 今日" class="hint">${esc(stock)}</td>` +
          `<td data-label="能不能换"><span class="badge ${p.verdict === 'ok' ? 'success' : p.verdict === 'unknown' ? 'warning' : 'idle'}">${esc(say(p.verdict))}</span></td>` +
          `<td data-label="操作"><span class="ex-row-actions">` +
          `<button class="btn btn-sm ex-star ${isSub ? 'is-on' : ''}" data-sub-idx="${idx}" title="${isSub ? '取消关注' : '关注'}">${isSub ? '★' : '☆'}</button>` +
          (exCanSchedule(p)
            ? `<button class="btn btn-sm" data-plan-idx="${idx}" title="打开定时抢兑弹窗">定时</button>`
            : '') +
          (p.verdict === 'ok' || !exLocalJudge()
            ? `<button class="btn btn-sm primary" data-buy-idx="${idx}"${exBusy ? ' disabled' : ''}>立即兑换</button>`
            : '') +
          `</span></td>` +
          `</tr>`,
      );
    }
  }
  rows.push('</tbody>');
  tbl.innerHTML = rows.join('');
}

/** 结果区：一次手动兑换的每一步。 */
function exResult(text, ok = true) {
  const box = $('ex-result');
  if (!box) return;
  const p = el('p', ok ? 'hint' : 'hint is-bad');
  p.textContent = text;
  box.appendChild(p);
}

/** 定时弹窗当前给哪一件排定时（不是手动兑换）。 */
let planDialogQuery = '';
/** 定时目标的 **prizeId**（稳定）；老数据只有名字时是 null。 */
let planDialogId = null;
/** 正在编辑的计划 id；空 = 新建。 */
let planEditId = '';

/** 把读数灌进定时弹窗（打开时 / 轮询重绘时）。 */
function syncPlanFields(e, plan) {
  const setVal = (id, v) => {
    const el = $(id);
    if (el && document.activeElement !== el) el.value = v;
  };
  const cron = plan
    ? plan.cron
    : e.scheduleCron ||
      (e.targetHour === null || e.targetHour === undefined ? '0 * * * *' : `0 ${e.targetHour} * * *`);
  setVal('ex-plan-cron', cron);
  const until = plan ? plan.until || '' : e.scheduleUntil || '';
  setVal('ex-plan-until', /^\d{4}-\d{2}-\d{2}$/.test(until) ? `${until}T23:59` : until);
  setVal('ex-plan-lead-sec', String(plan ? plan.leadSec : e.leadSec ?? 120));
  setVal('ex-plan-max', String(plan ? plan.maxRuns : e.scheduleMaxRuns ?? 0));
  if (plan) setVal('ex-plan-account', plan.accountId);
  setText(
    'ex-plan-note',
    plan
      ? `这条计划已经跑过 ${plan.runs ?? 0} 次`
      : e.scheduleRuns
        ? `当前账号这一版定时已经跑过 ${e.scheduleRuns} 次`
        : '',
  );
  updatePlanPreview();
}

function updatePlanPreview() {
  const cron = ($('ex-plan-cron') && $('ex-plan-cron').value.trim()) || '（未填 cron）';
  const l = ($('ex-plan-lead') && $('ex-plan-lead').value.trim()) || '10';
  const leadSec = ($('ex-plan-lead-sec') && $('ex-plan-lead-sec').value.trim()) || '120';
  const max = ($('ex-plan-max') && $('ex-plan-max').value.trim()) || '0';
  const until = ($('ex-plan-until') && $('ex-plan-until').value) || '';
  setText(
    'ex-plan-preview',
    `cron ${cron} · 提前 ${leadSec}s 启动 · 每号 ${max === '0' ? '不限' : max} 次` +
      (until ? ` · 截止 ${until.replace('T', ' ')}` : '') +
      ` · 整点前 ${l}ms 发`,
  );
}

/** 填账号下拉：编辑时保留原账号（即使已停用），新建时默认当前账号。 */
function fillPlanAccounts(selected) {
  const sel = $('ex-plan-account');
  if (!sel) return;
  const all = (last && last.account && last.account.accounts) || [];
  const usable = all.filter((a) => !a.disabled || a.id === selected);
  sel.innerHTML = usable
    .map(
      (a) =>
        `<option value="${esc(a.id)}">${esc(a.label || a.id)}${a.disabled ? '（已停用）' : ''}</option>`,
    )
    .join('');
  const cur = all.find((a) => a.current);
  sel.value = selected || (cur && cur.id) || (usable[0] && usable[0].id) || '';
}

/** 填目标下拉：优先用清单缓存；编辑的目标不在缓存里时补一条（保留 prizeId）。 */
function fillPlanTargets(name, id) {
  const sel = $('ex-plan-target');
  if (!sel) return;
  const items = exPicks || [];
  const wantId = id === null || id === undefined ? '' : String(id);
  const match = (p) => (wantId !== '' ? String(p.id) === wantId : !!name && p.name === name);
  const opts = items.map((p) => {
    const pid = p.id === undefined ? '' : String(p.id);
    return (
      `<option value="${esc(p.name)}" data-id="${esc(pid)}"${match(p) ? ' selected' : ''}>` +
      `${esc(p.name)}${p.id === undefined ? '' : ` · #${p.id}`}</option>`
    );
  });
  if (name && !items.some(match)) {
    opts.unshift(
      `<option value="${esc(name)}" data-id="${esc(wantId)}" selected>${esc(name)}${wantId === '' ? '' : ` · #${wantId}`}</option>`,
    );
  }
  sel.innerHTML = opts.join('') || '<option value="">先点「读一次」拿清单</option>';
}

/** 打开「定时抢兑」弹窗。`p` 是清单/关注里点的那一件；`plan` 是编辑已有计划。 */
function openPlanDialog(p, plan) {
  const e = (last && last.exchange) || {};
  planEditId = plan ? plan.id : '';
  if (plan) {
    planDialogQuery = plan.prizeName || '';
    planDialogId = plan.prizeId === undefined || plan.prizeId === null ? null : plan.prizeId;
  } else {
    planDialogQuery = (p && p.name) || '';
    planDialogId = p && Number.isInteger(p.id) ? p.id : null;
  }
  fillPlanAccounts(plan ? plan.accountId : '');
  fillPlanTargets(planDialogQuery, planDialogId);
  setText('ex-plan-title', plan ? '编辑定时计划' : '新建定时计划');
  setText('ex-plan-prize', planDialogQuery || '还没选目标');
  setText('ex-plan-price', p && p.price !== undefined ? `（${p.price} 豆）` : '');
  const lead = $('ex-plan-lead');
  if (lead) lead.value = String(e.leadMs ?? 10);
  syncPlanFields(e, plan || null);
  $('ex-plan-dialog').hidden = false;
  $('ex-plan-cron').focus();
}

/** 保存一条计划：写计划表 + 目标/时刻/提前量（不 startsOrder、不发请求）。 */
async function savePlanDialog() {
  // 目标以下拉为准（编辑时可能是缓存里没有的旧目标）。
  const t = $('ex-plan-target');
  if (t && t.selectedOptions[0] && t.value !== '') {
    planDialogQuery = t.value;
    planDialogId = t.selectedOptions[0].dataset.id ? Number(t.selectedOptions[0].dataset.id) : null;
  }
  const query = planDialogQuery.trim();
  if (query === '') throw new Error('先从清单里选一件，再保存计划');
  const accountId = ($('ex-plan-account') && $('ex-plan-account').value) || '';
  if (accountId === '') throw new Error('先选一个账号');
  const cron = $('ex-plan-cron').value.trim();
  if (cron === '') throw new Error('cron 表达式不能为空');
  const lead = $('ex-plan-lead').value.trim() || '10';
  const until = $('ex-plan-until').value.trim();
  const leadSec = $('ex-plan-lead-sec').value.trim() || '120';
  const maxRuns = $('ex-plan-max').value.trim() || '0';
  await post('config', { leadMs: lead });
  const r = await api('/exchange/plans', {
    method: 'POST',
    write: true,
    body: {
      action: 'save',
      ...(planEditId === '' ? {} : { id: planEditId }),
      accountId,
      prizeId: planDialogId === null ? undefined : planDialogId,
      prizeName: query,
      cron,
      until,
      maxRuns: Number(maxRuns),
      leadSec: Number(leadSec),
    },
  });
  if (!r.ok) throw new Error(r.note || '计划没存上');
  // 面板那条「兑换」任务由服务端同步（cron 取所有启用计划整点的并集）。
  $('ex-plan-dialog').hidden = true;
  await load();
  return r;
}

/** 立即兑换：先确认，再逐步推进后端 order 状态机（一次一个请求）。 */
async function exBuyNow(p, btn) {
  if (exBusy) {
    toast('上一单还在走，等它结束', true);
    return;
  }
  const price = p.price === undefined ? '' : `价格 ${p.price} 豆；`;
  const stock = exStockText(p);
  const blocked = p.verdict === 'ok' ? '' : `当前状态：${exSay(p.verdict)}；`;
  if (
    exConfirmFirst() &&
    !confirm(
      `立即兑换「${p.name}」？\n\n${blocked}${price}${stock ? stock + '；' : ''}\n` +
        '这是真下单，会扣 AI 豆；同组名额会被占掉。',
    )
  ) {
    return;
  }
  exBusy = true;
  for (const b of document.querySelectorAll('#ex-tbl [data-buy-idx], #ex-subs [data-buy-name]')) {
    b.disabled = true;
    b.textContent = '兑换中…';
  }
  const box = $('ex-result');
  if (box) box.innerHTML = '';
  exResult(`开始兑换「${p.name}」…`, true);
  try {
    if (!(await ensureH5Session())) {
      throw new Error('网页会话没续上 —— 登录凭据可能过期了，去「账号」页重新登录一次');
    }
    let r = await api('/exchange/order', {
      method: 'POST',
      write: true,
      body: { query: p.name },
    });
    exResult(r.note || '已起单', !!r.ok);
    if (!r.ok) throw new Error(r.note || '起单失败');
    for (let i = 0; i < 12; i += 1) {
      r = await api('/exchange/order', {
        method: 'POST',
        write: true,
        body: { action: 'step' },
      });
      exResult(r.note || '继续下一步', !!r.ok);
      if (r.done || r.idle || !r.ok) break;
    }
    toast(r && r.ok ? '兑换流程结束' : '兑换没成，看结果', r && r.ok ? '' : true);
  } catch (e) {
    exResult(String(e && e.message ? e.message : e), false);
    toast(String(e && e.message ? e.message : e), true);
  } finally {
    exBusy = false;
    await load();
  }
}

async function exToggleSub(p, on, btn) {
  await act(
    btn,
    () =>
      api('/exchange/subs', {
        method: 'POST',
        write: true,
        body: { id: p.id, name: p.name, on },
      }),
    (r) => (r && r.note) || (on ? '已关注' : '已取消关注'),
  );
}

/** 定时抢兑的状态卡：启用/暂停 + 目标/时间/提前量一句话说清。 */
function renderScheduleCard(e) {
  const plans = Array.isArray(e.plans) ? e.plans : [];
  const on = plans.filter((p) => p.enabled).length;
  const state = $('ex-sched-state');
  if (state) {
    state.textContent = on > 0 ? '已启用' : plans.length > 0 ? '全部暂停' : '未启用';
    state.className = on > 0 ? 'badge success' : 'badge idle';
  }
  setText(
    'ex-sched-summary',
    plans.length === 0
      ? '还没设定时计划 —— 点「添加计划」选一个账号和一件奖品'
      : `${on} 条启用 / 共 ${plans.length} 条计划`,
  );
  const list = $('ex-sched-list');
  if (!list) return;
  list.innerHTML = plans
    .map((p) => {
      const extra =
        (p.until ? ` · 截止 ${esc(p.until)}` : '') +
        (p.maxRuns > 0 ? ` · 最多 ${p.maxRuns} 次` : '') +
        (p.runs > 0 ? ` · 已跑 ${p.runs} 次` : '') +
        (p.accountOk === false ? ' · 账号不可用' : '');
      return (
        `<div class="plan-row${p.enabled ? '' : ' is-off'}">` +
        `<div class="plan-main"><strong>${esc(p.prizeName)}` +
        `${p.prizeId === null || p.prizeId === undefined ? '' : ` <span class="hint">#${p.prizeId}</span>`}</strong>` +
        `<span class="hint">${esc(p.accountLabel)} · ${esc(p.cron)} · 提前 ${p.leadSec}s${extra}</span></div>` +
        `<div class="row end">` +
        `<button class="btn btn-sm" data-plan-toggle="${esc(p.id)}">${p.enabled ? '暂停' : '启用'}</button>` +
        `<button class="btn btn-sm" data-plan-edit="${esc(p.id)}">编辑</button>` +
        `<button class="btn btn-sm" data-plan-del="${esc(p.id)}">删除</button>` +
        `</div></div>`
      );
    })
    .join('');
}

function renderExchange(e) {
  setText('ex-hero', '兑换');
  renderScheduleCard(e);
  renderAcctChips('exchange-accounts', 'exchange-account-detail', e.recent);
  // 定时弹窗回填当前值（**它们不是秘密**，可以回填）
  const leadEl = $('ex-plan-lead');
  if (leadEl && document.activeElement !== leadEl && $('ex-plan-dialog').hidden) {
    leadEl.value = String(e.leadMs ?? 10);
  }
  if ($('ex-plan-dialog').hidden) syncPlanFields(e);

  setText('ex-meta', e.reading && e.reading.at ? `清单读取于 ${when(e.reading.at)}` : '还没读过清单');
  // ⚠ `jwtExpired` 是**两小时那张票**过期 —— 它由 `ensureH5Session()` 无感续，
  //   不是"要用户处理"的事。只有**原生登录凭据**（auth）过期才需要人重新登录。
  const warn = $('ex-session-warn');
  if (warn) {
    const cur = ((last && last.account && last.account.accounts) || []).find((a) => a.current);
    const authExpired =
      !!cur && typeof cur.expireDays === 'number' && Number.isFinite(cur.expireDays) && cur.expireDays <= 0;
    warn.hidden = !authExpired;
    warn.textContent = authExpired
      ? `当前账号「${cur.label || cur.id}」的登录凭据已过期（不是两小时那张票）—— 先刷新登录凭据，或重新登录。`
      : '';
    const refresh = $('ex-session-refresh');
    if (refresh) refresh.hidden = !authExpired;
  }

  // 清单表读**缓存**（`picks`，全量、按天作废），不是摘要读数。
  exPicks = (e.picks && e.picks.items) || [];
  const can = exPicks.filter((p) => p.verdict === 'ok').length;
  // ⚠ 不报"哪天那份"：缓存**只有今天那份**能读回来（过期即清），
  //   所以这一格恒等于"今天"，写上只会有两个坏处 —— 多一串日期要读，
  //   而且它像一个"可能不是今天"的暗示。
  setText(
    'ex-note',
    exPicks.length === 0
      ? '还没读到清单 —— 点旁边那颗「读一次」读一遍。'
      : can > 0
        ? `今天读到 ${exPicks.length} 件，其中 ${can} 件现在能换`
        : `今天读到 ${exPicks.length} 件，现在都换不了`,
  );
  // 每一行的"能不能换"要走那张表翻成人话（原来直接印 `ok`/`tomorrow` 这种码）
  const say = (v) => (e.verdictText && e.verdictText[v]) || v || '';
  // 原 Rust mcloud 的兑换页：关注在最上面，清单按组展开。
  // 这里沿用同一份 `picks`，只是把「表格」换成「按组 + 行内动作」。
  renderExchangeAccount();
  renderExchangeSubs();
  renderExchangeTable(e, say);
  // 二次确认的偏好跟着开关变：关掉之后还写"点下去会先问"就是骗人。
  const cf = $('ex-confirm-first');
  if (cf && document.activeElement !== cf) cf.checked = exConfirmFirst();
  const lj = $('ex-local-judge');
  if (lj && document.activeElement !== lj) lj.checked = exLocalJudge();
  setText(
    'ex-intro',
    (exConfirmFirst()
      ? '挑一件兑换。下单会真的扣 AI 豆，所以点下去要再确认一次；抢不到的可以先「定时」等那一场。'
      : '挑一件兑换。下单会真的扣 AI 豆 —— 你已经关掉二次确认，点「立即兑换」就直接扣。') +
      (exLocalJudge() ? '' : ' 本地判定已关：抢光/未开始/已兑过也会真发单。'),
  );
}

/** 两小时那张 `jwttoken` 过期时**无感续**：调 `/h5/step` 把四步续票走完。
 *
 *  参考 Rust mcloud 的 `H5Client`：票过期不该让用户看到"未登录"，
 *  只有**原生登录凭据**（auth）过期才需要人重新登录。
 *  `force` 用于移动云盘明确回 `90001`（票没被认）时强制续一轮。 */
let h5Refreshing = null;
async function ensureH5Session(force = false) {
  const cur = ((last && last.account && last.account.accounts) || []).find((a) => a.current);
  if (!force && !(cur && cur.jwtExpired)) return true;
  if (h5Refreshing) return h5Refreshing;
  h5Refreshing = (async () => {
    for (let i = 0; i < 6; i += 1) {
      const r = await post('h5/step', {});
      if (r.done) return r.ok !== false;
    }
    return false;
  })().finally(() => {
    h5Refreshing = null;
  });
  return h5Refreshing;
}

/** 读一次清单。**先保证票新鲜**（到期主动换），再只发清单那一枪。 */
async function refreshPicks() {
  if (!(await ensureH5Session())) {
    throw new Error('网页会话没续上 —— 登录凭据可能过期了，去「账号」页重新登录一次');
  }
  let r = await api('/exchange/picks', { method: 'POST', write: true });
  // 票没过期、但被移动云盘拒了（90001）：强制续一轮再重试**一次**。
  if (r && r.ok === false && /90001|未登录|票没被认|会话/.test(String(r.note || ''))) {
    await ensureH5Session(true);
    r = await api('/exchange/picks', { method: 'POST', write: true });
  }
  return r;
}

/** 点开这一屏时补一枪，**只补一次**。
 *
 *  ⚠ 判据是"今天这份缓存有没有"，不是"我有没有点过"：昨天那份已经被服务端
 *  作废清掉了，所以这里读到空 ⇒ 才去打。已经有今天的 ⇒ 一个字都不发。 */
let exPicksAutoFetched = false;
function autoFetchPicksOnce() {
  if (exPicksAutoFetched) return;
  exPicksAutoFetched = true;
  const e = (last && last.exchange) || {};
  const cur = ((last && last.account && last.account.accounts) || []).find((a) => a.current);
  const jwtExpired = !!(cur && cur.jwtExpired);
  const have = ((e.picks && e.picks.items) || []).length > 0;
  // 有今天的缓存、且两小时那张票还没过期 ⇒ 一个字都不发。
  // 票过期则**自动续票并重读**（mcloud 的 H5Client 语义：票过期无感，不是让用户点）。
  if (have && !jwtExpired) return;
  // `act` 自己 toast 失败原因、也自己 `load()`（见 `autoFetchBeansOnce` 同一条理由）
  void act($('ex-read'), refreshPicks, (r) => (r && r.note) || '清单已刷新');
}

/** `2026-10-20 14:30:00` → `10-20 14:30`；今天/明天就直说。 */
function shortWhen(v) {
  const m = /^(\d{4})-(\d{2})-(\d{2})[ T](\d{2}):(\d{2})/.exec(String(v || ''));
  if (m === null) return String(v || '');
  const now = new Date();
  const day = `${m[2]}-${m[3]}`;
  const today = `${String(now.getMonth() + 1).padStart(2, '0')}-${String(now.getDate()).padStart(2, '0')}`;
  const tmr = new Date(now.getTime() + 86400000);
  const tomorrow = `${String(tmr.getMonth() + 1).padStart(2, '0')}-${String(tmr.getDate()).padStart(2, '0')}`;
  const hm = `${m[4]}:${m[5]}`;
  if (day === today) return `今天 ${hm}`;
  if (day === tomorrow) return `明天 ${hm}`;
  return `${day} ${hm}`;
}

function renderLive(l) {
  const pool = l.pool || {};
  const codes = (l.codes || []).filter((c) => c && c.code);
  const valid = pool.validCount || 0;

  // ⚠ 第一行要回答"它现在在干什么"，不是"池子里有几条口令"（用户的原话：
  //   这两件事接不上）。顺序：正在听 > 下一场几点 > 还没探到。
  const lr = (l.listen && l.listen.reading) || null;
  const listenWindowMs = Math.max(1, Number(l.listenSeconds || 600)) * 1000;
  // ⚠ 不能只看 `step === 'listen'`：窗口到点后如果没有下一拍来收尾，状态会
  //   一直停在 listen，界面就会在几小时后仍说「正在听」。用窗口起点 + 时长判活。
  const listening = !!(
    lr &&
    lr.step === 'listen' &&
    !lr.finishedAt &&
    Number(lr.windowStartMs || 0) + listenWindowMs > Date.now()
  );
  const next = shortWhen(pool.nextStart || (l.auto && l.auto.nextStart) || '');
  const gateNow = l.gate || {};
  const liveNow = gateNow.active === true;
  setText(
    'live-hero',
    listening
      ? `正在听${next ? ` ${next} 那场` : ''}`
      : liveNow
        ? `「${String(gateNow.target || '目标主播')}」正在直播`
        : next
          ? `下一场 ${next}`
          : '还没探到下一场',
  );
  // 第二行只回答两件事：**下次什么时候跑**、**现在在不在听（为什么）**。
  //
  // ⚠ 原来这里还有一段「还没有能用的口令（主播发的时候要人贴进来）」。三个毛病：
  //   ① 它是"功能说明"，不是状态 —— 状态行该报"现在怎么样"；
  //   ② 和刚定的口径打架：**没在播就不听、不看小红书**，那就不该在同一行里
  //      提醒用户"记得贴口令"（等于暗示"我们会去拿"）；
  //   ③ 位置错：口令有几条是**口令池**那一块的属性，那一块就在下面。
  //   所以条数移到口令池那行去说，这里只留"在不在听 + 为什么"。
  const bits = [];
  const job = jobOf('mcloud-live', '直播');
  if (job) bits.push(scheduleText(job));
  if (listening) {
    bits.push(next ? `正在听 ${next} 那场` : '正在听');
  } else {
    const g = gateNow;
    const target = String(g.target || '目标主播');
    const why =
      g.at === undefined || g.at === null
        ? ''
        : g.active === true
          ? `「${target}」正在播`
          : (g.liveCount || 0) === 0
            ? '谁都没开播'
            : `有 ${g.liveCount} 间在播，但没有「${target}」的房间`;
    bits.push(why === '' ? '现在没在听' : `现在没在听：${why}`);
  }
  setText('live-sub', bits.join(' · ') || '还没有日程 —— 它探到下一场就会自己开始听。');
  // 听弹幕时长：任务级参数（照 mcloud Rust 的「听弹幕时长」）。
  // 正在输入时不要被刷新覆盖。
  const secs = Number(l.listenSeconds ?? (last.settings && last.settings.listenSeconds));
  const secsBox = $('live-listen-seconds');
  if (secsBox && document.activeElement !== secsBox) {
    secsBox.value = Number.isFinite(secs) && secs > 0 ? String(secs) : '';
  }
  // ⚠ 「领小红花没开」是**全局开关**的状态，不是某个账号跑出来的结果。
  //   口令池那一行已经把它说全了（还带"这里的开关只是允许插件去领"），
  //   再在账号 chip 上重复一次就是同一句话说两遍 —— 而且会让人以为每个号各出了一件事。
  //   一个控件只留一处说法。
  const liveRecent = (l.recent || []).filter(
    (x) => !/领小红花没开/.test(String(x.note || '')),
  );
  renderAcctChips('live-accounts', 'live-account-detail', liveRecent);

  // 领小红花的结论（四栏 + 进账）。这是「直播」这一块第二重要的读数 ——
  // 它回答「上一轮到底领到没有、没领到是为什么」。
  const draw = l.draw || {};
  const rep = draw.report;
  // 口令池那行先说"手里有几条"（条数是这一块的属性，不是上面那行的）
  const poolHead = valid > 0 ? `手里有 ${valid} 条能用的口令` : '还没有能用的口令';
  if (rep) {
    const bits2 = [];
    if (rep.expired.length > 0) bits2.push(`${rep.expired.length} 条本场已结束（不是口令抄错）`);
    if (rep.invalid.length > 0) bits2.push(`${rep.invalid.length} 条口令无效`);
    if (rep.alreadyDrawn.length > 0) bits2.push(`${rep.alreadyDrawn.length} 条这个账号已经领过`);
    if (rep.other.length + rep.unparsable.length > 0) {
      bits2.push(`${rep.other.length + rep.unparsable.length} 条回了没见过的结果`);
    }
    setText('live-pool-line', `${poolHead} · ${rep.line}${bits2.length > 0 ? ' · ' + bits2.join(' · ') : ''}`);
  } else {
    setText('live-pool-line', `${poolHead} · ${draw.line || pool.line || pool.note || ''}`);
  }
  paintList(
    'live-codes',
    codes.map((c) => {
      const claimed =
        Array.isArray(c.claimedBy) && c.claimedBy.length > 0
          ? ` · 已领过：${c.claimedBy.join('、')}`
          : '';
      return [
        c.code,
        `${c.source || ''}${c.state ? ` · ${c.state}` : ''}${c.reason ? ` · ${c.reason}` : ''}${claimed}`,
        c.kind ? `<span class="badge">${esc(c.kind)}</span>` : '',
      ];
    }),
    '还没有口令。从直播弹幕里抄一条贴在上面。',
  );
}

/** 市场的名字。界面上不摆 `sign_in_3` 这种内部 id —— 那是给排查看的。 */
const MARKET_LABEL = { sign_in_3: '签到', newsign_139mail: '邮箱' };

/** 一条 App 任务的**桶**：判据与写的那半（`judgeItem`）**必须是同一条** ——
 *  `FINISH` ⇒ done；`enable === 1` ⇒ todo；其余 ⇒ skip。
 *  ⚠ 它只管**排序与折叠**，不管徽章上那个词（词见 `appTaskView`）。 */
function appTaskBucket(it) {
  if (it.state === 'FINISH') return 'done';
  if (it.enable === 1) return 'todo';
  return 'skip';
}

/** 徽章上那句话 —— **照抄移动云盘给这一条、这一阶段写的那一句**（`buttonOutText` 那一族）。
 *
 *  ⚠ 不许一刀切印成「待做」：这是 mcloud 犯过又改掉的错（`694e35f`，用户原话
 *  「提示只有已完成和待做，这个**待做容易误导**，请与原接口文案一致」）。
 *  真机实拍（抓包仓 `app-task-run-account-samples.txt` §12.10 / §12.11）：
 *  同一趟里 `state=WAIT` 的两条，1029 是「**去完成**」、1021 是「**开启中**」；
 *  549 是「**下月再来**」（这个月压根做不了）、`FINISH` 的 607 是「**次月可领**」。
 *  ⇒ 一刀切那个词把"移动云盘已经给了具体处境"这件事盖掉了。
 *
 *  ⚠ 我们只走 **V3**，而 V3 把**当前阶段那一档**直接写进 `text` ✓ ——
 *  所以 `buttonOutText` 就是 App 上那个词。`textMiddle`/`textFinish` 与
 *  `actionMiddle` 那套三档判断是 V2 形状的兜底；而 `actionMiddle` 判阶段
 *  **被实拍证伪过一次**（549 它是空串 ⇒ 会显示「去开启」✗，App 是「下月再来」✓），
 *  所以这里只拿它决定"两份文案先看哪一个"，**不当**"能不能做"的判据。
 *  读不出移动云盘那边的文案就回落状态词（已完成 / 未完成），**不猜**。 */
function appTaskView(it) {
  const pick = (...vs) => {
    for (const v of vs) {
      const t = String(v ?? '').trim();
      if (t !== '') return t;
    }
    return '';
  };
  if (appTaskBucket(it) === 'skip') return { label: '不参与', badge: 'idle' };
  const state = String(it.state ?? '').trim();
  const middle = String(it.buttonActionMiddle ?? '').trim();
  if (state === 'FINISH') {
    return { label: pick(it.buttonTextFinish, it.buttonOutText) || '已完成', badge: 'success' };
  }
  if (state === 'DOING') return { label: '进行中', badge: 'running' };
  if (state === 'WAIT') {
    const t =
      middle === ''
        ? pick(it.buttonOutText, it.buttonTextMiddle)
        : pick(it.buttonTextMiddle, it.buttonOutText);
    return { label: t || '未完成', badge: 'warning' };
  }
  // 认不出的状态：有那边的文案就照抄，没有就**把那一格原样摆出来**（不另造词）
  return { label: pick(it.buttonOutText) || state || '未完成', badge: 'idle' };
}

/** 面板的徽章只有 idle / running / success / failed / warning 这几档 —— 写别的
 *  会落到"没有修饰"的基础款（就是一个小灰点），看起来像没做完的样式。 */

/** 任务名里会带**字面 HTML**（真机实拍：`分享文件领好礼<span id='share_title'>7/7</span>`）。
 *  那是移动云盘给 App 内嵌文本用的，直接画出来就是一堆尖括号。
 *  这里只做"剥标签 + 还原几个常见实体"，不做 DOM 解析：我们**不执行**它，
 *  也不把 HTML 拼进页面（一律走文本节点/`esc`），剥干净就够了。 */
function plainText(raw) {
  return String(raw ?? '')
    .replace(/<[^>]*>/g, ' ')
    .replace(/&nbsp;/g, ' ')
    .replace(/&lt;/g, '<')
    .replace(/&gt;/g, '>')
    .replace(/&amp;/g, '&')
    .replace(/\s+/g, ' ')
    .trim();
}

/** App 任务：**逐条列出来**，不是只给"已完成 12 · 可做 4"。
 *
 *  ⚠ 只给计数的话，"可做 4 条"到底是哪 4 条、叫什么、奖励是什么，用户一个都看不见 ——
 *  那一格就只能看个热闹（用户原话：「app任务看不了具体事项」）。
 *  所以按市场分两段，段里逐条：名字 + 奖励文案 + 状态。
 *  ⚠ 顺序是**没做完的在前**：这一页是读数页，用户来这儿是想知道"还有什么没做"。 */
function paintAppTasks(groups, local) {
  const ul = $('matrix-list');
  if (!ul) return;
  const rows = [];
  for (const g of groups || []) {
    const r = g.reading;
    if (!r) {
      rows.push(
        `<li class="item"><div class="item-main"><span class="item-t">${esc(MARKET_LABEL[g.group] || g.group)}</span>` +
          `<span class="item-s">还没读到（日常跑一次就有了）</span></div></li>`,
      );
      continue;
    }
    const items = Array.isArray(r.items) ? r.items.slice() : [];
    const order = { todo: 0, done: 1, skip: 2 };
    items.sort((a, b) => order[appTaskBucket(a)] - order[appTaskBucket(b)]);
    rows.push(
      `<li class="item"><div class="item-main"><span class="item-t">${esc(MARKET_LABEL[g.group] || g.group)}</span>` +
        `<span class="item-s">已完成 ${r.done} · 未完成 ${r.todo} · 不参与 ${r.skip} · ${when(r.at)}</span></div></li>`,
    );
    // 已完成的那些**默认收起来**：这一页是给"还有什么没做"看的，
    // 十几条"已完成"铺在前面会把真正的重点挤到屏幕外。点一下还能看全。
    //
    // ⚠ 折叠行摆在**没做完那几条之后**：它是"下面这一坨"的标题，
    //   放在最前面的话，展开之后它就把最该先看的几条挤到下面去了。
    const line = (it, foldedRow) => {
      const view = appTaskView(it);
      const name = plainText(it.name) || plainText(it.description) || `#${it.taskId ?? '?'}`;
      const desc = plainText(it.description);
      // 名字与描述常常是"任务 + 奖励"两截（实拍：`永葆童心 领38云朵`）——
      // 名字里已经含了描述就不重复画。
      const sub = desc && !name.includes(desc) ? desc : '';
      // 照 mcloud：每条都带 taskId；没做完的再带 `currStep`（通知 / 备份这类
      // 分阶段任务，光看「去开启」不知道走到哪一步了）。
      const id = it.taskId === undefined || it.taskId === null ? '' : `#${it.taskId}`;
      const bits = [id];
      // 照 mcloud：`#106 · 第 1 步`。`currStep` 不是"完成了几步"，
      // 它可以是 22 这种任务专属步号，所以**不写分母**。
      if (appTaskBucket(it) === 'todo') {
        bits.push(`第 ${typeof it.currStep === 'number' ? it.currStep : 0} 步`);
        const day = String(it.buttonOutDay ?? '').trim();
        if (day !== '') {
          const total = String(it.buttonOutTotal ?? '').trim();
          const guess = total === '' ? (/连续(\d+)天/.exec(name)?.[1] ?? '') : total;
          bits.push(guess === '' ? `完成进度 ${day}` : `完成进度 (${day}/${guess})`);
        }
        // 本机动作证据：106 这种"传了、点了，但服务端还是 WAIT"（mcloud 有同一条真机记录）
        const upName = String((local && local.uploadName) || '');
        if (local && local.uploadDone && (upName === '' || upName.startsWith(name))) {
          bits.push('本机已上传');
        }
        if (local && Array.isArray(local.unconfirmed) && local.unconfirmed.includes(it.taskId)) {
          bits.push('本机已请求（服务端未确认）');
        } else if (local && Array.isArray(local.clicked) && local.clicked.includes(it.taskId)) {
          bits.push('本机已点过');
        }
      }
      // **累计进度**（`process`）—— 只对"要攒够 N 个"的那几条有（实拍 522=67/100）。
      // ⚠ 不放进上面那个"只对未完成"的分支：攒满之后它照样有意义（"这个月传了 100 个"）。
      const proc = typeof it.process === 'number' && Number.isFinite(it.process) ? it.process : undefined;
      // ⚠ `process` 每条都有（其它任务是 0）—— 只有"名字里带满 N 个"或"真的攒了"才画，
      //   否则每行都会多出一句「已累计 0」。
      const cap = /满(\d+)/.exec(name)?.[1] ?? '';
      if (proc !== undefined && (cap !== '' || proc > 0)) {
        bits.push(cap === '' ? `已累计 ${proc}` : `已累计 ${proc}/${cap}`);
      }
      const meta = [sub, bits.filter(Boolean).join(' · ')].filter(Boolean).join(' · ');
      // ⚠ 徽章上那个词是**上游给的**，照样要 `esc`（它进过我们的 DOM 字符串）
      return (
        `<li class="item is-sub"${foldedRow ? ` hidden data-folded="1" data-g="${esc(g.group)}"` : ''}>` +
        `<div class="item-main"><span class="item-t">${esc(name)}</span>` +
        (meta ? `<span class="item-s">${esc(meta)}</span>` : '') +
        `</div><div class="item-r"><span class="badge ${view.badge}">${esc(view.label)}</span></div></li>`
      );
    };
    const todoItems = items.filter((it) => appTaskBucket(it) === 'todo');
    const restItems = items.filter((it) => appTaskBucket(it) !== 'todo');
    for (const it of todoItems) rows.push(line(it, false));
    if (restItems.length > 0) {
      rows.push(
        `<li class="item is-sub is-more" data-more="1" data-g="${esc(g.group)}" data-n="${restItems.length}"><div class="item-main">` +
          `<span class="item-t">其余 ${restItems.length} 条（已完成 / 不参与）—— 点一下展开</span>` +
          `</div></li>`,
      );
      for (const it of restItems) rows.push(line(it, true));
    }
    if (items.length === 0) {
      rows.push(
        `<li class="item is-sub"><div class="item-main"><span class="item-s">这一组没有条目（移动云盘这次没给）</span></div></li>`,
      );
    }
  }
  if (rows.length === 0) {
    ul.innerHTML = '<li class="empty">还没有读数（日常跑一次就有了）。</li>';
    return;
  }
  ul.innerHTML = rows.join('');
}

/** 数据页「App 任务」当前看的是哪个账号（空 = 跟当前账号走）。 */
let matrixAccountId = '';

function matrixLabel(accounts, id) {
  const hit = (accounts || []).find((a) => a.id === id);
  return hit ? hit.label || hit.id : id;
}

function fillMatrixAccounts(accounts, current) {
  const sel = $('matrix-account');
  if (!sel) return;
  const list = accounts || [];
  if (matrixAccountId === '' || !list.some((a) => a.id === matrixAccountId)) {
    matrixAccountId = current || (list[0] ? list[0].id : '');
  }
  sel.innerHTML = list
    .map(
      (a) =>
        `<option value="${esc(a.id)}"${a.id === matrixAccountId ? ' selected' : ''}>${esc(a.label || a.id)}</option>`,
    )
    .join('');
  if (list.length === 0) sel.innerHTML = '<option value="">还没有账号</option>';
}

const BACKUP_STEP_TEXT = {
  read: '正在读取',
  upload: '正在上传',
  'snapshot-create': '正在建快照',
  'snapshot-put': '正在写快照',
  'snapshot-complete': '正在完成快照',
  wait: '等移动云盘记账',
  recheck: '正在复查',
  cleanup: '正在清理',
  done: '已完成',
  // ⚠ 这两档**不是**完成：`done` 只在复查**读到**备份已开启时成立
  //   （见 tasks/taskExpansion.ts 的 verdictOf）。写成"已完成"就是那句
  //   「本月备份：未开启（任务拓展：已完成）」自相矛盾的来源。
  blocked: '没成',
  unknown: '判不了',
};

/** 一句话截短（这一行是"看的是哪个号"的副标题，不是运行记录的详情）。 */
function shortWhy(text, max) {
  const t = String(text || '').trim();
  return t.length > max ? `${t.slice(0, max)}…` : t;
}

function paintMatrixLine(label, groups, backup) {
  const read = (groups || []).filter((g) => g.reading).length;
  const b =
    backup && typeof backup.enabled === 'boolean'
      ? ` · 本月备份：${backup.enabled ? '已开启' : '未开启'}${
          backup.step
            ? `（任务拓展：${BACKUP_STEP_TEXT[backup.step] || backup.step}${
                backup.why ? ` —— ${shortWhy(backup.why, 60)}` : ''
              }）`
            : ''
        }`
      : '';
  setText(
    'matrix-line',
    (read === 0
      ? `看的是「${label}」—— 还没有读数（日常跑一次就有了）。`
      : `看的是「${label}」· 只读本地缓存，不发新请求。`) + b,
  );
}

/** 切账号只读**本地矩阵缓存**（`GET /matrix` 不发请求）；默认不主动请求。 */
async function loadMatrix(account, refresh = false) {
  const url = `/matrix?account=${encodeURIComponent(account)}${refresh ? '&refresh=1' : ''}`;
  const r = await api(url);
  matrixAccountId = account;
  paintAppTasks(r.groups || [], r.local);
  paintMatrixLine(r.label || account, r.groups || [], r.backup);
  return r;
}

function renderData(d, account) {
  const m = d.matrix || {};
  const current = (account && account.current) || '';
  const accounts = (account && account.accounts) || [];
  fillMatrixAccounts(accounts, current);
  const selected = matrixAccountId || current;
  setText('data-hero', `读了 ${m.read || 0}/${m.total || 0} 个市场`);
  // ⚠ 别再写"这一页没有任何动作"：豆那一格现在是**进来就取一次**的（见 autoFetchBeansOnce）
  setText('data-sub', '读数都在这儿 —— 豆那一格进这一页就取一次，要新的点它右上角「刷新」。');
  if (selected !== '' && selected !== current) {
    // 用户之前切到了别的号：只在**本地**读那个号的缓存。
    void loadMatrix(selected).catch((e) => {
      setText('matrix-line', `读不到这个账号的任务状态 —— ${e.message || String(e)}`);
    });
    return;
  }
  paintAppTasks(m.groups, m.local);
  paintMatrixLine(matrixLabel(accounts, selected), m.groups, m.backup);
}

$('matrix-account').addEventListener('change', (ev) => {
  const id = ev.currentTarget.value;
  if (!id) return;
  matrixAccountId = id;
  void loadMatrix(id).catch((e) => toast(`读不到任务状态：${e.message || String(e)}`, true));
});

$('matrix-refresh').addEventListener('click', (ev) => {
  const id = matrixAccountId || $('matrix-account').value;
  if (!id) return;
  const btn = ev.currentTarget;
  btn.disabled = true;
  void loadMatrix(id, true)
    .then((r) => {
      const errs = Array.isArray(r.refreshErrors) ? r.refreshErrors : [];
      if (errs.length > 0) toast(`刷新有失败：${errs[0]}`, true);
      else toast('已向移动云盘重新读取任务状态');
    })
    .catch((e) => toast(`读不到任务状态：${e.message || String(e)}`, true))
    .finally(() => {
      btn.disabled = false;
    });
});

// ══════════════ 今日AI豆（**点了才取**，照参考实现 `upstream-page.tsx::CloudCell`）══════════════
//
// # 为什么不在 `/readings` 里
//
// 这一格每次点都要打上游 `cloudRecordV2`（而且**要翻到短页为止**，不是"读本地状态"）。
// 把它塞首屏聚合等于**替每个账号发一枪**才能打开页面 —— 而"豆是每账号一本账、
// 只读、点了才取"这三条一起是参考实现那一格的口径。这里照做。
//
// # 数据流
//
//   ·「取一次」 → `GET /beans/today` → 每账号一行（净/收/支/笔 + 「查看明细」）
//   · 行右侧「查看明细」 → 打开弹窗 + `GET /beans/records?id=&days=1`
//   · 弹窗里换窗口（1/7/30/0）→ 重取（照参考：档位与拉取**一次点完**）

/** `beans/today` 那一趟的读数（null = 还没取）。 */
let beansToday = null;
/** 弹窗当前打开的账号 id（换窗口时要用它重取）。 */
let beansDialogId = '';

/** 把 `beansToday` 画进 `#beans-list`：成功行在左，失败行也照样占一行。 */
function paintBeansList() {
  const ul = $('beans-list');
  if (!ul) return;
  if (beansToday === null) {
    // 打开这一页就会自己取一次，所以这里不是"快去点一下"，而是"还没到/没取到"
    ul.innerHTML =
      '<li class="empty">正在取…（取不到的话点右上角「刷新」再试一次）</li>';
    return;
  }
  const ok = beansToday.accounts || [];
  const bad = beansToday.failed || [];
  if (ok.length === 0 && bad.length === 0) {
    ul.innerHTML = '<li class="empty">这个插件里还没有账号（或都停用了）。</li>';
    return;
  }
  const rows = [];
  for (const a of ok) {
    const sign = a.net > 0 ? `+${a.net}` : String(a.net);
    const bal = typeof a.balance === 'number' ? `${a.balance}` : '？';
    const today = `今日 ${sign}（收 ${a.gained} · 支 ${a.spent} · ${a.count} 笔）`;
    const sub =
      typeof a.balance === 'number'
        ? today
        : `当前余额读不到${a.balanceReason ? `（${a.balanceReason}）` : ''} · ${today}`;
    rows.push(
      `<li class="item"><div class="item-main">` +
        `<span class="item-t">${esc(a.label)} · <b>当前 ${esc(bal)} 豆</b></span>` +
        `<span class="item-s">${esc(sub)}</span>` +
        `</div><div class="item-r">` +
        `<button class="btn ghost" type="button" data-beans-detail="${esc(a.id)}">查看明细</button>` +
        `</div></li>`,
    );
  }
  for (const f of bad) {
    // 失败那一行：**不给「查看明细」** —— 点开也没数据可给；
    // 那句"读不到"就是全部信息（照 F-062 那条"失败要说清是哪一个"的口径）。
    rows.push(
      `<li class="item"><div class="item-main">` +
        `<span class="item-t">${esc(f.label)}</span>` +
        `<span class="item-s">读不到 —— ${esc(f.reason)}</span>` +
        `</div></li>`,
    );
  }
  ul.innerHTML = rows.join('');
}

/** 把"取于 …"那句写进 `#beans-line`。 */
function paintBeansLine() {
  if (beansToday === null) return;
  const iso = new Date(beansToday.nowMs).toISOString();
  const cache = beansToday.cached === true ? ' · 来自缓存（2 小时内不重取，刷新可强取）' : '';
  setText(
    'beans-line',
    `取于 ${when(iso)}${cache} · 每个账号各一本账，不给跨账号合计（豆不能跨账号花）`,
  );
}

/** 取一次今日AI豆（画完就返回）。**自动取**与「刷新」共用这一条路。 */
async function fetchBeans(force = false) {
  beansToday = await api(`/beans/today${force ? '?refresh=1' : ''}`);
  paintBeansList();
  paintBeansLine();
}

$('beans-refresh').addEventListener('click', (ev) =>
  act(ev.currentTarget, () => fetchBeans(true), () => ''),
);

/** 这一页**打开时自己取**，不用先点一下。
 *
 *  ⚠ 只在这一份页面**第一次**进「数据」时取：再点回来不重取（`cloudRecordV2`
 *  是按账号一枪的，来回切标签就重打一轮属于白花配额）。要新的就点「刷新」。
 *  ⚠ 失败了也不拦着用户 —— 把错误画在那一格下面，他点「刷新」能重试。 */
let beansAutoFetched = false;
function autoFetchBeansOnce() {
  if (beansAutoFetched) return;
  beansAutoFetched = true;
  // ⚠ `act` 自己会 toast 失败原因（它不 rethrow），所以这里不用再接一层 catch；
  //   它还会顺手 `load()` 一把，把这一页其余读数一起刷新 —— 正好。
  void act($('beans-refresh'), () => fetchBeans(false), () => '').then(() => {
    // 取不到时说清"下一步点哪里"，别留一句"正在取…"在那儿装作还在跑
    if (beansToday === null) {
      const ul = $('beans-list');
      if (ul) ul.innerHTML = '<li class="empty">还没取到 —— 点右上角「刷新」再试一次。</li>';
    }
  });
}

/** 打开/切换"明细"弹窗。**每次都真去移动云盘重取**（照参考实现那一格）。
 *
 *  ⚠ `days` 不在 `1/7/30/0` 里的值，后端会当"全部"（`beansRead::normalizeDays`）。
 *  ⚠ 窗口 + 拉取**一次点完** —— 换档位本来就要重取。 */
async function openBeansDetail(id, days) {
  beansDialogId = id;
  $('beans-dialog').hidden = false;
  for (const b of $('beans-windows').querySelectorAll('[data-days]')) {
    b.classList.toggle('is-on', Number(b.dataset.days) === days);
  }
  const known =
    beansToday !== null
      ? [...(beansToday.accounts || []), ...(beansToday.failed || [])].find((x) => x.id === id)
      : undefined;
  setText('beans-title', known ? known.label : id);
  setText('beans-sub', '正在取…');
  $('beans-detail').innerHTML = '';
  try {
    const r = await api(`/beans/records?id=${encodeURIComponent(id)}&days=${days}`);
    if (r.error !== undefined) {
      // 后端**没用非 200 报失败**（那一格参考实现就是"红了但不炸别账号"的语义）：
      // 拿 `error` 字段判读不到。
      setText('beans-sub', `读不到 —— ${r.error}`);
      return;
    }
    const bits = [`共 ${(r.records || []).length} 笔`];
    if (r.hasMore === true) bits.push('这一份不全（撞了翻页或条数上限）');
    if (r.undated > 0) bits.push(`其中 ${r.undated} 笔时间读不出`);
    bits.push(`取于 ${when(new Date(r.nowMs).toISOString())}`);
    setText('beans-sub', bits.join(' · '));
    const ul = $('beans-detail');
    if ((r.records || []).length === 0) {
      ul.innerHTML = '<li class="empty">这个窗口里没有流水。</li>';
      return;
    }
    ul.innerHTML = (r.records || [])
      .map((x) => {
        const sign = x.num > 0 ? `+${x.num}` : String(x.num);
        const time =
          typeof x.atMs === 'number'
            ? when(new Date(x.atMs).toISOString())
            : x.rawTime || '时间读不出';
        const mark = x.mark ? ` · ${esc(x.mark)}` : '';
        return (
          `<li class="item"><div class="item-main">` +
          `<span class="item-t">${esc(x.summary || '')}</span>` +
          `<span class="item-s">${esc(time)}${mark}</span>` +
          `</div><div class="item-r"><b>${esc(sign)}</b></div></li>`
        );
      })
      .join('');
  } catch (e) {
    setText('beans-sub', `读不到 —— ${e.message || String(e)}`);
  }
}

$('beans-close').addEventListener('click', () => {
  $('beans-dialog').hidden = true;
});
$('beans-dialog').addEventListener('click', (ev) => {
  if (ev.target === $('beans-dialog')) $('beans-dialog').hidden = true;
});
$('beans-windows').addEventListener('click', (ev) => {
  const b = ev.target.closest('[data-days]');
  if (b === null) return;
  void openBeansDetail(beansDialogId, Number(b.dataset.days));
});

// App 任务的「其余 N 条」那一行：点开/收起同一组里被折叠的行。
// ⚠ 委派（这一格每次 load 都重画）。
$('matrix-list').addEventListener('click', (ev) => {
  const more = ev.target.closest('[data-more]');
  if (!more) return;
  const open = more.dataset.open === '1';
  more.dataset.open = open ? '0' : '1';
  const label = more.querySelector('.item-t');
  // ⚠ 按**组**找，不是按父节点：两个市场共用一个 `<ul>`，
  //   用 `parentElement` 一收就把另一组的行也收掉了（点「签到」的展开，「邮箱」跟着动）。
  const g = more.dataset.g || '';
  for (const li of $('matrix-list').querySelectorAll('[data-folded]')) {
    if ((li.dataset.g || '') !== g) continue;
    li.hidden = open;
  }
  if (label) {
    // ⚠ 条数从 `data-n` 拿，**不要**从当前文字里 re 出来：展开之后那句话是
    //   「收起这些 —— 点一下」，里面没有数字，re 出来就是「其余 几条」。
    const n = more.dataset.n || '';
    label.textContent = open
      ? `其余 ${n} 条（已完成 / 不参与）—— 点一下展开`
      : '收起这些 —— 点一下';
  }
});

// 「查看明细」按钮在 `#beans-list` 里，是每次点「取一次」重新画的 —— 所以用委派。
$('beans-list').addEventListener('click', (ev) => {
  const det = ev.target.closest('[data-beans-detail]');
  if (det === null) return;
  void openBeansDetail(det.dataset.beansDetail, 1);
});

// ══════════════ 领奖（待领取 → 领取 → 短信验证码） ══════════════
//
// # 三步，一下一个动作
//
//   ① 选账号 → `GET /prizes`（进这一块**自己读一次**，跟「数据」那一格同一条理由：
//      待领取是"每账号一本账、只读"，塞进 `/readings` 等于开页面就先打一圈）
//   ② 点「领取」 → 打开弹窗 → **自动** `POST /prizes/claim`（发短信）
//   ③ 填验证码 → `POST /prizes/confirm`
//
// ⚠ 界面上**没有**滑块图，也**没有**「输入偏移量 / 像素」这种格子 —— 那几条像素
//   人眼分不出来，让用户填等于让他猜。滑块是后端自己解的，前端只等短信。
//
// ⚠ 三个接口的失败形状不一样，别只看 HTTP 状态：
//   · `GET /prizes` 读失败是 **200 + `failed`** ⇒ 照常渲染，写「读不到 —— 原因」；
//   · `claim` / `confirm` 业务失败是 **200 + `{ok:false, note}`** ⇒ 不 throw；
//   · 只有"账号不存在 / 没先发短信"才是 400 + `{error}` ⇒ `call()` 会 throw。

/** `GET /prizes` 那一趟的读数（null = 还没取）。 */
let prizesNow = null;
/** 当前看的账号（'' = 还没选，第一趟交给后端用当前账号）。 */
let prizesAccountId = '';
/** 取数序号：切账号时上一趟可能回来得更晚，不许它盖掉新的。 */
let prizeFetchSeq = 0;
/** 弹窗里正在领的那一件。 */
let prizeDialogOid = '';
let prizeDialogName = '';
/** 每开/关一次弹窗就 +1：晚回来的请求不许再动界面（用户可能已经关了或换了号）。 */
let prizeDialogSeq = 0;
/** 「重新发送」的倒计时。关弹窗 / 切账号必须清掉（否则它会在后台一直跑）。 */
let prizeResendTimer = null;

/** 这个号叫啥（接口给的 `label` 已经脱敏，直接显示）。 */
function prizeLabelOf(id) {
  const sel = $('prize-account');
  if (sel) {
    for (const opt of Array.from(sel.options)) {
      if (opt.value === id) return opt.textContent || id;
    }
  }
  // 下拉还没画出来的那一瞬间（或者这个号不在列表里）：退回读数里那一份。
  const r = prizesNow;
  if (r && r.account && r.account.id === id) return r.account.label || id;
  return id || '这个号';
}

/** 过期时间说人话：今年就写「过期 10-31 23:59」，跨年才带年份；没有就直说。 */
function prizeExpireText(iso) {
  if (!iso) return '没过期时间';
  const t = Date.parse(iso);
  if (!Number.isFinite(t)) return '没过期时间';
  const d = new Date(t);
  const hm = `${String(d.getHours()).padStart(2, '0')}:${String(d.getMinutes()).padStart(2, '0')}`;
  const md = `${d.getMonth() + 1}-${d.getDate()}`;
  const day = d.getFullYear() === new Date().getFullYear() ? md : `${d.getFullYear()}-${md}`;
  return `过期 ${day} ${hm}`;
}

/** 账号下拉：列表与选中项都以接口回来的 `accounts` 为准（它是唯一来源）。
 *
 *  ⚠ 还没取到读数时（`prizesNow === null`）**不许**写「还没有账号」：那时
 *  "有没有账号"根本还不知道，写了会让刚切过来的用户以为自己的号没了。
 *  那时下拉禁用 + 写「正在读…」；「还没有账号」只在取到了读数、确实空着时才写。 */
function fillPrizeAccounts(accounts, current) {
  const sel = $('prize-account');
  if (!sel) return;
  if (prizesNow === null) {
    sel.disabled = true;
    sel.innerHTML = '<option value="">正在读…</option>';
    return;
  }
  const list = accounts || [];
  if (prizesAccountId === '' || !list.some((a) => a.id === prizesAccountId)) {
    prizesAccountId = current || (list[0] ? list[0].id : '');
  }
  sel.disabled = list.length === 0;
  sel.innerHTML =
    list.length === 0
      ? '<option value="">还没有账号</option>'
      : list
          .map(
            (a) =>
              `<option value="${esc(a.id)}"${a.id === prizesAccountId ? ' selected' : ''}>${esc(a.label || a.id)}</option>`,
          )
          .join('');
}

/** 第一行回答"有几件要领"。
 *
 *  ⚠ `failed` 可能**与 items 同时出现**（后面几页没读成、第一页已经有货）：
 *  这时按 items 本来有几件说，别把已有的货一起说成"没读到"。 */
function paintPrizeHero() {
  const r = prizesNow;
  if (r === null) {
    setText('prize-hero', '…');
    return;
  }
  const items = r.items || [];
  // 一件都没读到、又确实读挂了 ⇒ 直说不知道有几件。
  if (r.failed && items.length === 0) {
    setText('prize-hero', '没读到有几件要领');
    return;
  }
  const accounts = r.accounts || [];
  if (r.noAccount === true || accounts.length === 0) {
    setText('prize-hero', '还没选账号');
    return;
  }
  const n = r.failed
    ? items.length
    : typeof r.pending === 'number'
      ? r.pending
      : items.length;
  setText('prize-hero', n === 0 ? '没有待领取的' : `${n} 件待领取`);
}

/** 第二行：这一步会往哪个号发短信。 */
function paintPrizeSub() {
  const r = prizesNow;
  if (r === null) {
    setText('prize-sub', '点「领取」会发一条短信验证码到这个号上，收到后填进来就能领到。');
    return;
  }
  const accounts = r.accounts || [];
  // ⚠ 读挂了不等于"没有账号"：那时不写「还没有账号」（同 `fillPrizeAccounts` 那条理由）。
  if (accounts.length === 0 && !r.failed) {
    setText('prize-sub', '还没有账号 —— 先去「账号」页加一个，回来这里就能领。');
    return;
  }
  const label = prizesAccountId
    ? prizeLabelOf(prizesAccountId)
    : (r.account && (r.account.label || r.account.id)) || '';
  setText(
    'prize-sub',
    label
      ? `点「领取」会发一条短信验证码到 ${label} 上，收到后填进来就能领到。`
      : '点「领取」会发一条短信验证码到这个号上，收到后填进来就能领到。',
  );
}

/** 卡片标题下面那一行：读到时间 / 缓存 / 共扫了多少条。
 *
 *  ⚠ `failed` 与 `items` 同时出现时（后面几页没读成）这句**照常写**，只在末尾补一句
 *  「后几页没读到」—— 把已有的读数整句换成"读不到"等于把读成功的半份也丢了。 */
function paintPrizeLine() {
  const r = prizesNow;
  if (r === null) {
    setText('prize-line', '正在读…');
    return;
  }
  if (r.failed && (r.items || []).length === 0) {
    setText('prize-line', `读不到 —— ${r.failed}（点「刷新」再试）`);
    return;
  }
  const bits = [];
  if (typeof r.nowMs === 'number') bits.push(`读到 ${when(new Date(r.nowMs).toISOString())}`);
  if (typeof r.pending === 'number') bits.push(`待领取 ${r.pending} 件`);
  if (typeof r.scanned === 'number') bits.push(`共扫了 ${r.scanned} 条`);
  bits.push(r.cached === true ? '来自缓存（点「刷新」强取一次）' : '刚从移动云盘读的');
  // 撞了翻页/条数上限：说清"这一份不全"，别让上面的件数看着像全部
  // （与「AI豆」明细那一句同一个说法）。
  if (r.hasMore === true) bits.push('这一份不全（撞了翻页或条数上限）');
  let line = bits.join(' · ');
  if (r.failed) line += `（后几页没读到：${r.failed}）`;
  setText('prize-line', line);
}

/** 列表：一行一件，右边一个「领取」。 */
function paintPrizeList() {
  const ul = $('prize-list');
  if (!ul) return;
  const r = prizesNow;
  if (r === null) {
    ul.innerHTML = '<li class="empty">正在读…</li>';
    return;
  }
  const items = r.items || [];
  // ⚠ `failed` 与 `items` 可能同时出现（后面几页没读成、第一页已经有货）：
  //   有货就**照常列出来**，只在上面那行说明后几页没读到；一件都没有时才写"读不到"。
  if (items.length === 0) {
    if (r.failed) {
      ul.innerHTML = `<li class="empty">读不到 —— ${esc(r.failed)}（点「刷新」再试）</li>`;
      return;
    }
    const accounts = r.accounts || [];
    if (r.noAccount === true || accounts.length === 0) {
      ul.innerHTML = '<li class="empty">还没有账号 —— 去「账号」页加一个</li>';
      return;
    }
    ul.innerHTML = '<li class="empty">没有待领取的东西</li>';
    return;
  }
  ul.innerHTML = items
    .map((x) => {
      const sub = [x.market, prizeExpireText(x.expireAt), x.phone]
        .filter((v) => v !== undefined && v !== null && v !== '')
        .join(' · ');
      // 没有单号（oId）的那一条领不了：按钮禁用并说清原因，别让用户点了才被拒
      const can = x.claimable !== false && x.oid !== '';
      const btn = can
        ? `<button class="btn primary" type="button" data-prize-oid="${esc(x.oid)}">领取</button>`
        : '<button class="btn" type="button" disabled title="这一条没有单号">领不了</button>';
      return (
        `<li class="item"><div class="item-main">` +
        `<span class="item-t"><b>${esc(x.name || '这件奖品')}</b></span>` +
        `<span class="item-s">${esc(sub)}</span>` +
        `</div><div class="item-r">` +
        btn +
        `</div></li>`
      );
    })
    .join('');
}

/** 整张卡一起画。 */
function paintPrizeAll() {
  const r = prizesNow;
  fillPrizeAccounts((r && r.accounts) || [], (r && r.account && r.account.id) || '');
  paintPrizeHero();
  paintPrizeSub();
  paintPrizeLine();
  paintPrizeList();
}

/** 取一次待领取（只取，不画）。读失败也**不抛** —— 落到 `failed` 上让界面说人话。 */
async function fetchPrizes(force = false) {
  const id = prizesAccountId || ($('prize-account') ? $('prize-account').value : '');
  const q = [];
  if (id) q.push(`account=${encodeURIComponent(id)}`);
  if (force) q.push('refresh=1');
  try {
    return await api(`/prizes${q.length > 0 ? `?${q.join('&')}` : ''}`);
  } catch (e) {
    const prev = prizesNow || {};
    return {
      ok: false,
      accounts: prev.accounts || [],
      account: prev.account || null,
      items: [],
      failed: String(e && e.message ? e.message : e),
    };
  }
}

/** 取一次并画出来（进这一块自动取、点「刷新」、切账号都走这一条）。 */
async function loadPrizes(force = false, btn = null) {
  const seq = ++prizeFetchSeq;
  const target = btn || $('prize-refresh');
  const sel = $('prize-account');
  if (target) {
    target.disabled = true;
    target.textContent = '正在读…';
  }
  if (sel) sel.disabled = true;
  if (prizesNow === null) {
    paintPrizeLine();
    paintPrizeList();
  }
  const r = await fetchPrizes(force);
  // 有更新的一趟在跑 ⇒ 这一趟的结果作废（别让慢的那一趟盖掉新的账号）。
  if (seq !== prizeFetchSeq) return r;
  prizesNow = r;
  if (r.account && r.account.id) prizesAccountId = r.account.id;
  paintPrizeAll();
  if (target) {
    target.disabled = false;
    target.textContent = '刷新';
  }
  return r;
}

/** 切账号的那一下：先把卡片换成"正在读"，别让上一个号的奖品看着像这个号的。 */
function paintPrizeSwitching() {
  setText('prize-hero', '…');
  setText('prize-line', '正在读…');
  const ul = $('prize-list');
  if (ul) ul.innerHTML = '<li class="empty">正在读…</li>';
}

/** 这一块**第一次**进来时自己读一次（之后要新的点「刷新」；切账号才重取）。
 *
 *  ⚠ 不轮询：待领取不是"看着它变"的东西，而这一页在 iframe 里。
 *  ⚠ 上一次读完是"没有账号 / 没读成"时，再进这一块**再试一次** —— 用户很可能
 *    刚去「账号」页加了一个号；不重读的话下拉还停在"还没有账号"，看着像坏了。
 *    这仍然是一次进入一次请求，不是轮询。
 *  ⚠ 读失败不拦着用户 —— 那句话已经画在卡片里，他点「刷新」能重试。 */
let prizesAutoFetched = false;
function autoFetchPrizesOnce() {
  const unusable =
    prizesNow !== null && ((prizesNow.accounts || []).length === 0 || !!prizesNow.failed);
  if (prizesAutoFetched && !unusable) return;
  prizesAutoFetched = true;
  void loadPrizes(false);
}

// ── 领取弹窗 ──

/** 清掉倒计时，「重新发送」回到还没发过的样子（关弹窗 / 切账号都走这里）。 */
function resetPrizeResend() {
  if (prizeResendTimer !== null) {
    clearInterval(prizeResendTimer);
    prizeResendTimer = null;
  }
  const b = $('prize-resend');
  if (!b) return;
  b.hidden = true;
  b.disabled = false;
  b.textContent = '重新发送（60 秒）';
}

/** 发码成功之后开始倒计时：60 秒内不许再点（连点只会让后端限流）。 */
function startPrizeResend(seconds = 60) {
  const b = $('prize-resend');
  if (!b) return;
  if (prizeResendTimer !== null) clearInterval(prizeResendTimer);
  b.hidden = false;
  b.disabled = true;
  let left = seconds;
  const tick = () => {
    if (left <= 0) {
      if (prizeResendTimer !== null) clearInterval(prizeResendTimer);
      prizeResendTimer = null;
      b.disabled = false;
      b.textContent = '重新发送';
      return;
    }
    b.textContent = `重新发送（${left} 秒）`;
    left -= 1;
  };
  tick();
  prizeResendTimer = setInterval(tick, 1000);
}

/** 把弹窗里的临时状态清干净（关弹窗 / 切账号 / 打开新的一条都走这里）。 */
function resetPrizeDialog() {
  resetPrizeResend();
  const code = $('prize-code');
  if (code) {
    code.value = '';
    code.disabled = false;
  }
  const confirm = $('prize-confirm');
  if (confirm) {
    confirm.disabled = true;
    confirm.textContent = '确认领取';
  }
  const send = $('prize-send');
  if (send) {
    send.hidden = false;
    send.disabled = false;
    send.textContent = '发送验证码';
  }
  const row = $('prize-code-row');
  if (row) row.hidden = true;
  setText('prize-note', '');
}

function closePrizeDialog() {
  const back = $('prize-dialog');
  if (back) back.hidden = true;
  // 关掉就是新的一轮：还在路上的发码 / 确认请求回来之后**不许**再改界面。
  prizeDialogSeq += 1;
  prizeDialogOid = '';
  prizeDialogName = '';
  resetPrizeDialog();
}

/** 发一条验证码。打开弹窗时**自动**跑一次；「再试一次」/「重新发送」也走这里。
 *
 *  ⚠ 发不出去是 200 + `{ok:false, note}`（可能是滑块没解出来、这个号刚发过、
 *    被限流），所以**不能**只看 HTTP 状态；只有 400 才 throw。两种都说成人话。 */
async function sendPrizeCode() {
  const oid = prizeDialogOid;
  if (!oid) return;
  // 记下这一轮的序号：用户在等短信时关了弹窗 / 换了号，这一趟回来就作废。
  const session = prizeDialogSeq;
  const account = prizesAccountId || ($('prize-account') ? $('prize-account').value : '');
  const send = $('prize-send');
  const codeRow = $('prize-code-row');
  const confirm = $('prize-confirm');
  const code = $('prize-code');
  // 新一轮发码：上一轮的输入框与倒计时都先收起来（用户手上还没有新码）。
  resetPrizeResend();
  if (codeRow) codeRow.hidden = true;
  if (confirm) confirm.disabled = true;
  if (code) {
    code.value = '';
    code.disabled = false;
  }
  if (send) {
    send.hidden = false;
    send.disabled = true;
    send.textContent = '正在发验证码…';
  }
  setText('prize-note', '正在发验证码…（要等几秒，短信到了就填在下面）');
  try {
    const r = await post('prizes/claim', { account, oid });
    if (session !== prizeDialogSeq) return;
    if (r && r.ok === true) {
      if (send) send.hidden = true;
      if (codeRow) codeRow.hidden = false;
      setText(
        'prize-note',
        `验证码已发到 ${r.phone || prizeLabelOf(account)}，填进来就能领到。`,
      );
      startPrizeResend(60);
      if (code) code.focus();
      return;
    }
    // `unknown` = **没拿到回应**（不是"没发出去"）：短信可能已经发了。
    // ⇒ 把验证码格露出来（收到就能填），并给 60 秒倒计时（没收到就等一分钟再发）。
    if (r && r.unknown === true) {
      if (send) send.hidden = true;
      if (codeRow) codeRow.hidden = false;
      setText('prize-note', r.note || '没拿到回应 —— 短信可能已经发出去，收到就填进来。');
      startPrizeResend(60);
      if (code) code.focus();
      return;
    }
    // 业务失败：原因**留在弹窗里**（不只 toast），并给一个重发的按钮。
    if (send) {
      send.hidden = false;
      send.disabled = false;
      send.textContent = '再试一次';
    }
    setText('prize-note', `没发出去 —— ${(r && r.note) || '原因不明'}（点「再试一次」重新发）`);
  } catch (e) {
    if (session !== prizeDialogSeq) return;
    if (send) {
      send.hidden = false;
      send.disabled = false;
      send.textContent = '再试一次';
    }
    // 这条是 400（账号不对 / 缓存里没有这一件之类）—— 后端那句话本身就是下一步，
    // 不再补「点再试一次」，那种情况重发也没用。
    setText('prize-note', `没发出去 —— ${e.message || String(e)}`);
  }
}

/** 确认领取。验证码错是最正常的失败 ⇒ **留在弹窗里**让他重输。
 *
 *  ⚠ 成功之后只重新 GET 一次列表（**不带** `refresh=1`）：后端已经把这件从缓存里
 *    摘掉了，再去打一次移动云盘是白花一次请求。 */
async function confirmPrize() {
  const oid = prizeDialogOid;
  if (!oid) return;
  const session = prizeDialogSeq;
  const input = $('prize-code');
  const btn = $('prize-confirm');
  const code = String((input && input.value) || '').trim();
  if (code.length < 4) {
    setText('prize-note', '验证码是短信里那串数字，至少 4 位。');
    return;
  }
  const account = prizesAccountId || ($('prize-account') ? $('prize-account').value : '');
  if (input) input.disabled = true;
  if (btn) {
    btn.disabled = true;
    btn.textContent = '正在领…';
  }
  setText('prize-note', '正在领…');
  try {
    const r = await post('prizes/confirm', { account, oid, code });
    if (r && r.ok === true) {
      // 成功了就照实说 + 重读列表（弹窗可能已经关了，也不影响这两件事）。
      toast(typeof r.note === 'string' && r.note !== '' ? r.note : `已领取：${prizeDialogName}`);
      closePrizeDialog();
      await loadPrizes(false);
      return;
    }
    if (session !== prizeDialogSeq) return;
    // `unknown` = 这一单可能已经领到了（写请求没拿到回应）：
    // ⇒ **不许原地再撞**，关掉弹窗去刷新列表看它还在不在。
    if (r && r.unknown === true) {
      toast(r.note || '没拿到回应 —— 先点「刷新」看它还在不在', true);
      closePrizeDialog();
      await loadPrizes(false);
      return;
    }
    if (input) {
      input.disabled = false;
      input.focus();
    }
    if (btn) {
      btn.disabled = false;
      btn.textContent = '确认领取';
    }
    // `code:1000` 那种"要去页面里领"的会给一个地址；原样显示出来（不吞掉）
    const why = (r && r.note) || '原因不明';
    const link = r && typeof r.link === 'string' && r.link !== '' ? `（地址：${r.link}）` : '';
    setText('prize-note', `没领到 —— ${why}${link}`);
  } catch (e) {
    if (session !== prizeDialogSeq) return;
    // 400 这类是"面板明确拒了"（没写出去）：留在弹窗里说清。
    // 网络层失败**不知道面板收没收到** ⇒ 当结果未知，别原地重试，去刷新列表。
    const known = e && e.data && typeof e.data === 'object';
    if (!known) {
      toast(`没拿到回应 —— 先点「刷新」看它还在不在：${e.message || String(e)}`, true);
      closePrizeDialog();
      await loadPrizes(false);
      return;
    }
    if (input) {
      input.disabled = false;
      input.focus();
    }
    if (btn) {
      btn.disabled = false;
      btn.textContent = '确认领取';
    }
    setText('prize-note', `没领到 —— ${(e.data && e.data.error) || e.message || String(e)}`);
  }
}

/** 打开「领取」弹窗。**打开就发码** —— 用户点「领取」本身就是明确动作。 */
async function openPrizeDialog(item) {
  // 新的一条 = 新的一轮：上一次还没回来的请求从此作废。
  prizeDialogSeq += 1;
  resetPrizeDialog();
  prizeDialogOid = String(item.oid);
  prizeDialogName = item.name || '这件奖品';
  setText('prize-title', prizeDialogName);
  $('prize-dialog').hidden = false;
  await sendPrizeCode();
}

// 「领取」按钮是每次重画列表时新生成的 —— 所以用委派。
$('prize-list').addEventListener('click', (ev) => {
  const b = ev.target.closest('[data-prize-oid]');
  if (b === null || b.disabled) return;
  const oid = b.dataset.prizeOid;
  const item =
    ((prizesNow && prizesNow.items) || []).find((x) => String(x.oid) === String(oid)) || null;
  void openPrizeDialog(item || { oid, name: '' });
});

$('prize-refresh').addEventListener('click', (ev) => {
  void loadPrizes(true, ev.currentTarget);
});

// 切账号：弹窗里那一条属于旧号，先关掉（顺带清掉倒计时），再用 `?account=` 取这个号。
$('prize-account').addEventListener('change', (ev) => {
  closePrizeDialog();
  prizesAccountId = ev.currentTarget.value;
  paintPrizeSwitching();
  void loadPrizes(false);
});

$('prize-send').addEventListener('click', () => {
  void sendPrizeCode();
});
$('prize-resend').addEventListener('click', () => {
  void sendPrizeCode();
});
$('prize-confirm').addEventListener('click', () => {
  void confirmPrize();
});
$('prize-close').addEventListener('click', () => {
  closePrizeDialog();
});
$('prize-dialog').addEventListener('click', (ev) => {
  if (ev.target === $('prize-dialog')) closePrizeDialog();
});

// 验证码只收数字；满 4 位才点亮「确认领取」（这是提前告知，不是替用户判对错）。
$('prize-code').addEventListener('input', () => {
  const input = $('prize-code');
  const digits = String(input.value || '').replace(/\D/g, '').slice(0, 8);
  if (digits !== input.value) input.value = digits;
  const confirm = $('prize-confirm');
  if (confirm) confirm.disabled = digits.length < 4;
});
// 回车 = 点「确认领取」（键盘上输完码不用再去够鼠标）。
$('prize-code').addEventListener('keydown', (ev) => {
  if (ev.key !== 'Enter') return;
  ev.preventDefault();
  const confirm = $('prize-confirm');
  if (confirm && !confirm.disabled) void confirmPrize();
});

// ── 运行记录（三块共用） ──
//
// # 为什么是「一次运行一行」而不是「一条日志一行」
//
// 面板的环形日志混着所有插件、而且只有 200 行 —— 用它回答
// 「昨天那次兑换为什么没成」是做不到的。所以插件自己记：
// **一次运行 = 一行**，点开才看那一次的步骤。
//
// 列表只给**摘要**（成功几步 / 失败几步 / 没做事几步），步骤按需再取 ——
// 20 次 × 40 步全塞进一次读数就是几百 KB 的 JSON，而这是 iframe 里的页面。

/** 画一个任务的「最近几次」。`boxId` / `stepsId` 是那一块的容器。
 *
 *  ⚠ **最近一次默认展开**。全折起来虽然"干净"，但页面一打开什么都没有 ——
 *  用户得先点一下才知道跑成什么样，而"要点哪个"正是这一版要解决的问题。 */

/** 取某一次运行的步骤并画出来。
 *
 *  ⚠ 取不到（记录已经被新记录挤掉）时说清是**过期**，不是"出错了" ——
 *  运行记录只留最近 N 次，用户手上那个 id 确实可能已经没了。 */
async function loadRunDetail(stepsId, id) {
  const ul = $(stepsId);
  if (!ul) return;
  ul.innerHTML = '';
  // 记下现在展开的是哪一次 —— 按钮靠它判断"再点一次是收起"
  ul.dataset.run = id;
  try {
    const r = await api(`/runs?id=${encodeURIComponent(id)}`);
    for (const s of r.steps || []) {
      const li = el('li', s.pending === true ? 'is-idle' : s.ok ? (s.idle ? 'is-idle' : 'is-ok') : 'is-bad');
      const n = el('span', 'step-name');
      n.textContent = s.name;
      const t = el('span', 'step-note');
      t.textContent = s.note;
      li.appendChild(n);
      li.appendChild(t);
      ul.appendChild(li);
    }
    if ((r.steps || []).length === 0) {
      const li = el('li', 'is-idle');
      li.textContent = '这一次没有步骤';
      ul.appendChild(li);
    }
  } catch (e) {
    const li = el('li', 'is-idle');
    li.textContent = `这一次的记录已经过期了（只留最近几次）：${e.message}`;
    ul.appendChild(li);
  }
}

// ⚠ 这里**故意没有**"整份配置的缓存"：界面拿不到已存的值（只有掩码），
//   所以保存走的是 `{set, remove}` 增量形状。曾经想缓存一份再整份发回去，
//   但那要么要求界面持有秘密、要么在保存时把别的渠道**清空**。

// ⚠ **「设置」那一页整个删掉了**（`renderSettings` 与那六个地址口子一起）。
//
// 为什么删：那一页是**给搭测试环境的人看的**（云盘地址 / 口令主页 / 直播日程主机 /
// 弹幕 WebSocket / 进房间主机 / 登录主机 / 自检），而它出现在**给用户看的界面**上。
// 用户的判断很直接：「这些 mock 内容压根不能交给用户使用，用户看不懂，还会导致
// 一堆问题」—— 而且真的导致了：面板的「登录主机」被指到 mock 上，扫码登录永远
// 走不完（F-067 的那条尾巴）。
//
// 口子本身**没有删**：`/config` 那条路由还在（e2e 脚本靠它把基址指到 mock，
// 见 `scripts/e2e-mock.sh`），只是**不再从界面上暴露**。开发要改就调接口。

function render(r) {
  // 版本号：`/readings` 给什么印什么（没有就留空，不写死一个数在界面里）
  setText('plugin-ver', r.version ? `v${String(r.version)}` : '');
  renderAccount(r.account || {}, r.login || {});
  renderDaily(r.daily || {});
  renderExchange(r.exchange || {});
  renderLive(r.live || {});
  renderData(r.data || {}, r.account || {});
}

// ──────────────────────────────────────────────────────────── 加载

async function load() {
  try {
    // 排期与读数**串行**拉（第 141 轮）：面板里一个插件只有**一个** JS 执行位，
    // 同时发两个请求时第二个会立刻拿到 502「正忙」—— 这不是任务在跑，是自己撞自己。
    // 代价是这两个请求不再并行（各几十毫秒，界面感知不到）。
    const r = await api('/readings');
    await loadJobs();
    last = r;
    render(last);
  } catch (e) {
    // 读不到 = 界面全是空的。这时候必须**说出来**，否则用户看到的是
    // 「插件坏了」而不是「没读到」。
    toast(`读数没取到：${e.message}`, true);
  }
}

// ──────────────────────────────────────────────────────────── 区块切换

/** 切到某个区块（顶部菜单与账号页工具栏共用）。
 *
 *  ⚠ 提取出来的理由：工具栏那三个按钮（账号配置 / 上游状态）要跳到别的区块，
 *  而原来这套逻辑**内联在菜单的事件处理里** —— 复用只能靠复制一份，
 *  复制的那份迟早和菜单不一致。 */

// ══════════ 运行日志（终端样式，SSE） ══════════
//
// 数据源是**面板自己的日志流**：`GET /api/logs/stream?channel=plugin&plugin=<名字>`。
// 插件里每一句 `log(...)` 都在那儿，带时间、级别、插件名。
//
// ⚠ 为什么不用插件自己那套"运行记录"画终端：那是**一次一条**的摘要
//   （`mcloud.run.log`：时间 / 成败 / 每步结果），适合「最近几次」那种列表；
//   而"看着它跑"需要的是**行**。
// ⚠ 只在**这一块显示时**才连（`showBlock` 里开关）：三块各连一条流没必要，
//   而且浏览器对同域 SSE 的并发连接有上限（HTTP/1.1 是 6 条）。
// ⚠ 级别切换要**重连**：过滤发生在服务端（1MB 缓冲里有几 MB 文本，
//   全量推给前端再丢是白烧带宽 —— 面板的注释原话）。
const TERMS = {
  daily: {
    plugin: 'mcloud-daily',
    el: () => ({
      body: $('daily-term-body'),
      meta: $('daily-term-meta'),
      follow: $('daily-term-follow'),
      level: $('daily-term-level'),
      clear: $('daily-term-clear'),
    }),
  },
  exchange: {
    plugin: 'mcloud-exchange',
    el: () => ({
      body: $('exchange-term-body'),
      meta: $('exchange-term-meta'),
      follow: $('exchange-term-follow'),
      level: $('exchange-term-level'),
      clear: $('exchange-term-clear'),
    }),
  },
  live: {
    plugin: 'mcloud-live',
    el: () => ({
      body: $('live-term-body'),
      meta: $('live-term-meta'),
      follow: $('live-term-follow'),
      level: $('live-term-level'),
      clear: $('live-term-clear'),
    }),
  },
};

/** 级别下拉**每次进页面都回「全部」**。
 *
 *  ⚠ 浏览器会"恢复"上次的 `<select>` 值（刷新/前进后退都算）—— 于是用户
 *  可能在某次刷新后停在「提醒以上」，而一次跑完界面上一行都没有：
 *  那是 F-091 那张截图（「没看到任何日志」）。
 *  `autocomplete="off"` 在 HTML 里不是好办法（MDN 明确不建议，项目里也有用例禁它），
 *  所以在这里**显式写回默认值**：加载时一次，`pageshow` 再一次（恢复发生在它之前）。
 */
function resetTermLevels() {
  for (const k of Object.keys(TERMS)) {
    const el = TERMS[k].el();
    if (el && el.level) el.level.value = 'info';
  }
}
resetTermLevels();
window.addEventListener('pageshow', resetTermLevels);

/** 每块最多留多少行。再多也不给人看，只会让滚动条变细。 */
const TERM_MAX_LINES = 400;

/** 建一个终端面板实例。`key` 是 DOM 前缀（`daily-term-body` 那套）。 */
function makeTerm(plugin, el) {
  const { body, meta, follow, level, clear } = el;
  let es = null;
  let lines = 0;
  /** **屏上摆着几行**（只数 `.term-line`，不含分界线与空态那句）。
   *  页头那个数报的是它 —— 跟着级别筛选、也跟着 400 行上限。 */
  let shown = 0;

  /** 把一行日志画上去。⚠ 一律走 `textContent`（日志里有用户可控的字符串）。 */
  function paint(ev) {
    let d;
    try {
      d = JSON.parse(ev.data);
    } catch {
      return;
    }
    // SSE 的 `type` 才是语义类型（start/success/fail）；`level` 只是过滤档位。
    // 历史（/log/tail）没有 `type`，它把类型放在 `level` 里。
    const lv = d.type || d.level || 'info';
    if (!levelPasses(lv)) return;
    const empty = body.querySelector('.term-empty');
    if (empty) empty.remove();
    const el = document.createElement('span');
    el.className = `term-line term-line-${lv}`;
    const ts = document.createElement('span');
    ts.className = 'term-ts';
    // 面板给的是 `2026-10-17 10:50:42.498`；我们铺历史时给的是已经切好的 `10:50:42.498`。
    // 两种都要能吃（`slice(11)` 对后者会把时分秒切掉）。
    const raw = String(d.ts || '');
    ts.textContent = (/^\d{4}-\d{2}-\d{2}/.test(raw) ? raw.slice(11) : raw) + ' ';
    const lvEl = document.createElement('span');
    lvEl.className = `term-lv term-lv-${lv}`;
    lvEl.textContent = lv.padEnd(7);
    const msg = document.createElement('span');
    msg.className = 'term-msg';
    msg.textContent = String(d.msg || d.line || '');
    el.append(ts, lvEl, msg);
    body.appendChild(el);
    lines += 1;
    shown += 1;
    // 超了就从**上面**砍（终端只看最近的）
    while (lines > TERM_MAX_LINES && body.firstElementChild) {
      const cut = body.firstElementChild;
      if (cut.classList.contains('term-line')) shown -= 1;
      cut.remove();
      lines -= 1;
    }
    setMeta('');
    if (follow.checked) body.scrollTop = body.scrollHeight;
  }

  /** 页头那句：`插件名 · 显示 N 行 · （状态） · 只显示「筛选」`。
   *
   *  ⚠ 那个数原来是**服务端环**的行数（`notice` 里的 `p.lines`），叫「缓冲」。
   *  筛在「提醒以上」时它照旧报全量 ⇒ 页头与屏幕对不上（用户圈的同一类毛病）。
   *  现在报屏上的行数（跟着筛选走），环的口径只在**真丢了东西**时出场：
   *  「环里淘汰了 N 行」—— 那才是"日志少了一段"需要的解释。 */
  function setMeta(tail) {
    if (!meta) return;
    const only = levelNote();
    const parts = [plugin, `显示 ${shown} 行`];
    if (tail !== '' && tail !== undefined && tail !== null) parts.push(String(tail));
    if (only !== '') parts.push(`只显示「${only}」`);
    const text = parts.join(' · ');
    // 每画一行都会走到这里（`paint` 末尾）—— 文本没变就不碰 DOM
    if (meta.textContent !== text) meta.textContent = text;
  }

  /** 这一行的级别过不过当前那道门槛。口径与下拉那三个值一致：
   *  `info`=全部、`warn`=提醒以上、`error`=只看错误。
   *  `start/success/debug` 是过程/着色，归最低档；`fail` 是根因失败，
   *  要跟 `warn` 一起出现在「提醒以上」里（否则票过期时反而看不到红字）。
   *  认不出的级别 ⇒ 当作最低档，**不猜它是高的**。 */
  const LEVEL_RANK = { debug: 0, info: 0, start: 0, success: 0, warn: 1, fail: 2, error: 2, fatal: 3 };
  function levelPasses(lv) {
    const want = LEVEL_RANK[level.value] ?? 0;
    return (LEVEL_RANK[lv] ?? 0) >= want;
  }

  /** 先铺历史：插件自己留了一份（`/log/tail`），**面板重启之后也还在**。
   *
   *  ⚠ 历史要**在这里自己滤一遍**：`level` 那个下拉是发给服务端的参数，
   *  而服务端只滤**实时流** —— 历史是本地从 `/log/tail` 铺的，不经那道闸。
   *  不滤的话，选着「只看错误」下面照样一排 `info`，而同一行页头还写着
   *  「只显示「只看错误」」（用户圈的正是这个自相矛盾）。 */
  async function seed() {
    try {
      const r = await api('/log/tail?limit=' + TERM_MAX_LINES);
      const history = r.lines || [];
      if (history.length === 0) return;
      // ⚠ 实时流已经画了行就别再清屏：`open()` 现在是**先连流、后补历史**，
      //   这一枪可能因为任务占着插件运行时而被拖到任务结束才回来；此时屏上
      //   已经是新的实时日志了，再清一次会把它整段抹掉。
      if (shown > 0) return;
      body.textContent = '';
      lines = 0;
      shown = 0;
      let painted = 0;
      for (const one of history) {
        if (!levelPasses(one.l)) continue;
        // ⚠ 环里存的是 ISO（UTC）；面板日志流给的是**本地**时间。
        //   直接 slice 会让历史显示成凌晨 —— 两边必须同一种时间。
        const d = new Date(one.t);
        const pad = (n, w) => String(n).padStart(w, '0');
        const ts = Number.isNaN(d.getTime())
          ? ''
          : `${pad(d.getHours(), 2)}:${pad(d.getMinutes(), 2)}:${pad(d.getSeconds(), 2)}.${pad(d.getMilliseconds(), 3)}`;
        paint({ data: JSON.stringify({ ts, level: one.l, msg: one.m }) });
        painted += 1;
      }
      // 一行都没滤出来：别写那句分界（它上面是空的），也**别留白屏** ——
      // "界面上一行都没有"就是 F-091 那张截图，当时查出来是被筛选挡掉的。
      if (painted === 0) {
        const only = levelNote();
        const gap = document.createElement('span');
        gap.className = 'term-empty';
        gap.textContent =
          `（上次留下的 ${history.length} 行里没有过这道门槛的` +
          (only === '' ? '' : ` —— 现在只显示「${only}」，想看全部就把上面那个下拉切回「全部」`) +
          `）`;
        body.appendChild(gap);
        lines += 1;
        setMeta('');
        return;
      }
      // ⚠ 不能复用 `.term-empty`：`paint()` 每画一行都会把 `.term-empty` 删掉
      //   （那是"还没有日志"的占位），分界线用同一个类就会被第一行新日志吃掉。
      const gap = document.createElement('span');
      gap.className = 'term-gap';
      gap.textContent = '—— 上面是上次留下的，下面是新的 ——';
      body.appendChild(gap);
      lines += 1;
      setMeta('');
    } catch {
      // 读不到就当没有历史（那只是首屏少几行，不该弹错误）
    }
  }

  function open() {
    if (es !== null || typeof EventSource === 'undefined') return;
    // 服务端永远给 `debug` 全量，级别筛选**在本地做**：`fail` 在面板那边
    // 归 info 档，若把下拉值直接发过去，选「提醒以上」会把红字滤掉。
    const q = new URLSearchParams({ channel: 'plugin', plugin, level: 'debug' });
    setMeta('连接中…');
    // ⚠ **先连流，再补历史**。`/log/tail` 是插件自己的路由；任务正在跑时
    //   它和任务抢同一个插件运行时，请求会被排到任务结束才返回。旧顺序是
    //   `seed()`（等 `/log/tail`）完成后才 `connect()` —— 于是整场任务期间
    //   SSE 根本没连上，日志只能等任务跑完一次性出现。
    //   历史回来时如果实时流已经画了行，`seed()` 会自己放弃清屏（见上）。
    connect(q);
    void seed();
  }

  function connect(q) {
    es = new EventSource('/api/logs/stream?' + q.toString());
    es.addEventListener('notice', (ev) => {
      // 面板会先发一条概况（环里多少行 / 淘汰了多少）。⚠ 这里**只取"淘汰"那一格**：
      // 环的容量是全量口径，报在页头上就和筛选后的屏面对不上（F-109 末段）。
      try {
        const d = JSON.parse(ev.data);
        const p = d.plugin || {};
        setMeta(p.dropped ? `环里淘汰了 ${p.dropped} 行` : '已连上');
      } catch {
        setMeta('');
      }
    });
    es.addEventListener('log', paint);
    es.onerror = () => setMeta('断了 —— 正在重连（浏览器会自己接）');
    es.onopen = () => setMeta('已连上');
  }

  function close() {
    if (es !== null) {
      es.close();
      es = null;
    }
  }

  /** 级别筛选的**人话**（`info` = 全部，不提示）。 */
  function levelNote() {
    const v = level && level.value;
    if (!v || v === 'info') return '';
    return v === 'warn' ? '提醒以上' : '只看错误';
  }

  /** 清空（只清屏，不断流 —— 断了就看不到接下来那几行了）。
   *
   *  ⚠ 空态必须**把级别筛选说出来**：筛在「提醒以上」时，一次跑完
   *  界面上一行都没有 —— 用户的原话就是「没看到任何日志」。
   *  那不是没日志，是**被自己设的筛选挡掉了**，而屏幕上没有任何线索。 */
  function clearNow() {
    body.textContent = '';
    lines = 0;
    shown = 0;
    const el = document.createElement('span');
    el.className = 'term-empty';
    const only = levelNote();
    el.textContent =
      only === ''
        ? '（已清空）'
        : `（已清空 · 只显示「${only}」—— 这次跑的行大多是 info，想看全部就把上面那个下拉切回「全部」）`;
    body.appendChild(el);
    setMeta('');
  }

  /** 换级别 = **重连**（过滤在服务端，见 `TERMS` 那段说明）。 */
  function reload() {
    const wasOpen = es !== null;
    close();
    body.textContent = '';
    lines = 0;
    shown = 0;
    setMeta('');
    if (wasOpen) open();
  }

  if (body && body.childElementCount === 0) {
    const el = document.createElement('span');
    el.className = 'term-empty';
    const only = levelNote();
    el.textContent =
      only === ''
        ? '（还没有日志。点「现在跑一次」之后这里会一行一行往下走）'
        : `（还没有日志 · 现在只显示「${only}」—— 想看全部就把上面那个下拉切回「全部」）`;
    body.appendChild(el);
  }
  return { open, close, clearNow, reload, get isOpen() { return es !== null; } };
}

// 三个终端的「清空」「级别」—— 写成**字面量**（`$('…')`），
// 因为界面测试会逐个核对"HTML 里的按钮都真的挂了处理器"。
$('daily-term-clear').addEventListener('click', () => termHandle('daily').clearNow());
$('daily-term-level').addEventListener('change', () => termHandle('daily').reload());
$('exchange-term-clear').addEventListener('click', () => termHandle('exchange').clearNow());
$('exchange-term-level').addEventListener('change', () => termHandle('exchange').reload());
$('live-term-clear').addEventListener('click', () => termHandle('live').clearNow());
$('live-term-level').addEventListener('change', () => termHandle('live').reload());

/** 三块各自的终端实例（懒建：第一次显示那一块时才建）。 */
const termOf = {};
/** 取（必要时先建）某一块的终端实例。 */
function termHandle(key) {
  if (termOf[key] === undefined) termOf[key] = makeTerm(TERMS[key].plugin, TERMS[key].el());
  return termOf[key];
}

function syncTerms(blockName) {
  for (const key of Object.keys(TERMS)) {
    if (key === blockName) termHandle(key).open();
    else termHandle(key).close();
  }
}

function showBlock(name) {
  for (const b of document.querySelectorAll('.seg-item[data-block]')) {
    const on = b.dataset.block === name;
    // 面板的分段控件用 `aria-pressed` 表达选中态（不是自定义的 `is-on` 类）
    b.setAttribute('aria-pressed', on ? 'true' : 'false');
    // 屏幕阅读器：再给一个 aria-current 说"现在在哪一块"
    if (on) b.setAttribute('aria-current', 'page');
    else b.removeAttribute('aria-current');
  }
  for (const s of document.querySelectorAll('.block')) {
    s.classList.toggle('is-on', s.id === 'block-' + name);
  }
  // 日志流跟着"这一块是不是显示着"开关（见 TERMS 那段说明）
  syncTerms(name);
  // 「数据」页的第一格是**主动请求**的：开这一页就去取，不用先点一下。
  if (name === 'data') autoFetchBeansOnce();
  // 兑换页同理：今天还没读到清单就补一枪（清单是下拉的唯一来源）。
  if (name === 'exchange') autoFetchPicksOnce();
  // 「领奖」页也是：第一次进就读一次未领取（之后点「刷新」/切账号才重取）。
  if (name === 'prizes') autoFetchPrizesOnce();
}

$('menu').addEventListener('click', (ev) => {
  const btn = ev.target.closest('.seg-item[data-block]');
  if (!btn) return;
  showBlock(btn.dataset.block);
});

// ──────────────────────────────────────────────────────────── 按钮

$('refresh').addEventListener('click', (e) => act(e.currentTarget, load, '已刷新'));

// 账号：三种登录
//  ⚠ 推进的触发者是**用户点按钮**（状态机是用户本人：出码 → 用户扫 → 用户确认）。
// ══════════ 添加账号弹窗（复刻 mcloud） ══════════
//
// 参考实现：五个页签 + **客户端形态在最上面**。
// 这里那个"选中的形态"是**弹窗级**状态：扫码/短信/账密/凭据四种方式都按它来。
// ⚠ **「手机端」页签下那一块整块收起来** —— 那一路走 App 自己的身份，没有
//   Windows / macOS 之分。用户的问法很直白：「手机端哪来的 macOS 和 Windows」。
let addTabNow = 'qr';
/** 弹窗里选的形态。⚠ 只对 PC 端有意义 —— 手机端（App）不存在 win/mac 之分。
 *
 *  它是**唯一**的形态入口（设置页那个下拉框已删）。值用参考实现的词表
 *  （`win` / `mac`），后端收口时归一成 `windows` / `macos`。 */
let addPc = 'win';

function paintAddPc() {
  $('add-win').classList.toggle('is-on', addPc === 'win');
  $('add-mac').classList.toggle('is-on', addPc === 'mac');
}

/** 把 `addPc` 摆到"跟正在进行的这一趟一致"的位置。
 *
 *  ⚠ 进行中的会话是**出码那一刻冻住**的（`QrSession.platform`）：用户开弹窗时
 *  看到的按钮必须与屏幕上那张码一致 —— 否则他会看到"选了 macOS、码却是
 *  Windows 的"界面，而两个都不算错，只是没人说清楚哪个算数。 */
function syncAddPcFromReading() {
  const live = (last && last.login && (last.login.qr || last.login.sms)) || null;
  const from = live ? live.platform : last && last.settings ? last.settings.loginPlatform : '';
  if (from === 'macos') addPc = 'mac';
  else if (from === 'windows') addPc = 'win';
}

/** 换形态。**三件事一起做**，少一件就出现"界面说一套、请求发另一套"：
 *  ① 记住这次选择（下次开弹窗预填，`/config` 那格只是预填值，不是身份来源）；
 *  ② 进行中的会话**作废** —— 码里的版本号与轮询请求体必须出自同一套档案；
 *  ③ 重画按钮。 */
async function setAddPc(v) {
  if (addPc === v) return { ok: true, note: '没变' };
  addPc = v;
  paintAddPc();
  await api('/config', {
    method: 'POST',
    write: true,
    body: { loginPlatform: v === 'mac' ? 'macos' : 'windows' },
  });
  const qrShowing = !$('add-dialog').hidden && !$('qr-box').hidden;
  const smsRunning = !!(last && last.login && last.login.sms && last.login.sms.active);
  if (qrShowing || smsRunning) {
    stopQrPoll();
    await api('/login/cancel', { method: 'POST', write: true }).catch(() => undefined);
    $('qr-box').innerHTML = '';
    $('qr-box').hidden = true;
    setText('qr-note', '形态换了 —— 点「生成二维码」重新出码');
    return { ok: true, note: '形态换了，重新出一次码' };
  }
  return { ok: true, note: v === 'mac' ? '已切成 macOS' : '已切成 Windows' };
}

function showAddTab(name) {
  addTabNow = name;
  for (const b of $('add-tabs').querySelectorAll('.tab')) {
    b.classList.toggle('is-on', b.dataset.tab === name);
  }
  for (const pane of $('add-dialog').querySelectorAll('[data-pane]')) {
    pane.hidden = pane.dataset.pane !== name;
  }
  // ⚠ 「手机端」这一路走的是 App 自己的身份，**没有** Windows / macOS 之分 ——
  //   所以形态那一块在这个页签下收起来，而不是摆着让人问"手机端哪来的这两个按钮"。
  //   （用户就是这么问的。）其余四个页签照旧在顶部，它是弹窗级的。
  $('add-form').hidden = name === 'app';
}

/** 登录成功之后**收尾**：把「添加账号」弹窗关掉（用户的原话：「登录成功弹窗不自动关闭」）。
 *
 *  ⚠ 四条进账号的路（扫码 / 短信 / 手机端 / 贴凭据）都必须走这一个。原来只有扫码
 *  与贴凭据顺手关了，短信与手机端登进去之后弹窗**还杵在那儿** —— 用户只能自己点「关闭」，
 *  而那一刻他已经在等别的东西了。
 *
 *  ⚠ 关之前把定时器与轮询停掉：弹窗看不见了，后台还在问接口是最难查的一类问题。
 *  ⚠ 两个验证码框清空：里面的码属于刚才那一趟会话，留着只会让人以为没登进去。 */
function closeAddDialog() {
  stopQrPoll();
  stopResend('sms');
  stopResend('app');
  for (const id of ['sms-code', 'app-code']) {
    const el = $(id);
    if (el) el.value = '';
  }
  $('add-dialog').hidden = true;
}

$('acct-add').addEventListener('click', () => {
  // 开弹窗先跟读数对齐：进行中的会话优先，其次才是"上次用的那种"
  syncAddPcFromReading();
  $('add-dialog').hidden = false;
  showAddTab(addTabNow);
  paintAddPc();
  // 关弹窗时轮询停了（不留偷偷打接口的定时器）⇒ 重开时如果那张码还活着，接着轮
  syncQrPoll(last && last.login ? last.login.qr : null);
});
$('add-close').addEventListener('click', () => {
  stopQrPoll();
  $('add-dialog').hidden = true;
});
// 点遮罩关闭（点盒子里面不关）
$('add-dialog').addEventListener('click', (ev) => {
  if (ev.target === $('add-dialog')) $('add-dialog').hidden = true;
});
$('add-tabs').addEventListener('click', (ev) => {
  const b = ev.target.closest('.tab');
  if (!b) return;
  // 换页签要把上一路"进行中"的会话清掉（否则两边同时进行，界面不知道显示哪个）
  stopQrPoll();
  void api('/login/cancel', { method: 'POST', write: true }).catch(() => undefined);
  showAddTab(b.dataset.tab);
});
$('add-win').addEventListener('click', (e) =>
  act(e.currentTarget, () => setAddPc('win'), (r) => (r && r.note) || '已切成 Windows'),
);
$('add-mac').addEventListener('click', (e) =>
  act(e.currentTarget, () => setAddPc('mac'), (r) => (r && r.note) || '已切成 macOS'),
);

// 工具栏剩下的两个（「上游状态」是跳读数，另一个是真动作）。
// ⚠ 「账号配置」那个跳「设置」页的按钮跟着那一页一起删了 —— 见 index.html 那段说明。
$('acct-upstream').addEventListener('click', () => showBlock('data'));
$('acct-refresh-all').addEventListener('click', (e) =>
  act(e.currentTarget, () => api('/auth/refresh', { method: 'POST', write: true }), (r) => r.note || '已刷新'),
);

// ══════════ 账号选择器（多选 + 全部） ══════════
//
// 参考实现为这个控件单独写了一个组件，理由有两条：
//  · 原生 `<select multiple>` 要靠"按住 + 点选"，**移动端几乎不可用**；
//  · 展开面板**不能进文档流**（那会把卡片撑高、把下面的内容顶下去）。
// 所以这里是**浮层**，而且点外部要能关。
//
// ⚠ 三条任务共用**同一个控制器**（下面那张表驱动）—— 参考实现的原话：
//   「任务页与直播页各有一张'选账号再运行'的表单，两处必须是**同一套控件、
//    同一套口径**，否则迟早一处改了、另一处没改」。
const PICKERS = [
  // 每个任务两份：auto 给定时，manual 只给手动。不要合成一份（见 taskAccounts.ts）。
  //
  // ⚠ `label` 必须**逐个写死**，不要用 `${key}-acct-label` 拼：
  //   auto 那两个是 `daily-acct-label` / `live-acct-label`，而 manual 那两个
  //   在 HTML 里叫 `daily-manual-label` / `live-manual-label`（没有 `-acct-`）。
  //   拼出来的 id 找不到元素，`setText` 静默不写 ⇒ 按钮永远印着 HTML 里的默认
  //   「跟随自动」，而展开面板里勾的其实是「测试」—— 用户看不出这一趟要跑谁
  //   （现场 2026-11-01 截图：本次运行「跟随自动」但只有「测试」打勾）。
  { task: '日常', mode: 'auto', key: 'daily', label: 'daily-acct-label', legacy: 'daily-acct-legacy', btn: 'daily-acct-btn', panel: 'daily-acct-panel', all: 'daily-acct-all', list: 'daily-acct-list' },
  { task: '日常', mode: 'manual', key: 'daily-manual', label: 'daily-manual-label', btn: 'daily-manual-btn', panel: 'daily-manual-panel', all: 'daily-manual-all', list: 'daily-manual-list', follow: 'daily-manual-follow' },
  { task: '直播', mode: 'auto', key: 'live', label: 'live-acct-label', legacy: 'live-acct-legacy', btn: 'live-acct-btn', panel: 'live-acct-panel', all: 'live-acct-all', list: 'live-acct-list' },
  { task: '直播', mode: 'manual', key: 'live-manual', label: 'live-manual-label', btn: 'live-manual-btn', panel: 'live-manual-panel', all: 'live-manual-all', list: 'live-manual-list', follow: 'live-manual-follow' },
];

function pickerLabel(v, follow) {
  if (follow === true) return '跟随自动';
  if (!v) return '…';
  if (v.all) return '全部账号';
  const picked = v.ids || v.picked || [];
  const n = picked.length;
  if (n === 0) return '没选账号';
  if (n === 1) {
    const a = (v.accounts || []).find((x) => x.id === picked[0]);
    return (a && a.label) || picked[0];
  }
  return `${n} 个账号`;
}

/** 触发按钮上的「选中了什么」——用**标签**说（TDesign 多选的样式），不用一句话。
 *
 *  ⚠ 原来按钮上只印一行字（「全部账号」/「2 个账号」）：用户点开面板才知道具体是谁，
 *  收起来又忘了。标签能一眼看出「这趟跑哪几个号」，多了折成 +N。
 */
function pickerTags(v, follow, accounts) {
  const tag = (text, cls) =>
    '<span class="ms-tag' + (cls ? ' ' + cls : '') + '">' + esc(text) + '</span>';
  if (follow === true) return tag('跟随自动', 'is-polar');
  if (!v) return tag('…', 'is-polar');
  if (v.all === true) return tag('全部账号') + tag('新号也算', 'is-polar');
  const ids = v.ids || v.picked || [];
  if (ids.length === 0) return tag('没选账号', 'is-polar');
  const labels = ids.map((id) => {
    const a = (accounts || []).find((x) => x.id === id);
    return (a && a.label) || id;
  });
  const head = labels.slice(0, 2).map((t) => tag(t)).join('');
  const more = labels.length > 2 ? tag('+' + (labels.length - 2), 'is-more') : '';
  return head + more;
}

function renderPicker(cfg) {
  const all = (last && last.taskAccounts && last.taskAccounts[cfg.task]) || null;
  const v = cfg.mode === 'manual'
    ? ((all && all.manual) || { follow: true, all: false, ids: [] })
    : ((all && all.auto) || { all: true, ids: [] });
  const accounts = (all && all.accounts) || [];
  const follow = cfg.mode === 'manual' && v.follow === true;
  // ⚠ 按钮上只说**跑哪些号**，别印内部状态。
  //   这里以前印的是「全部账号（旧版设置）」——「旧版」是仓库里的说法
  //   （`legacyAuto` = 这一版还没单独设过、读的是旧键），用户看到它只会猜
  //   "这设置过期了？"或"还有一份新版设置我没找到"。所以按钮保持干净，
  //   "这份是从你以前的设置沿用的"挪进展开面板，那儿有地方说清楚。
  // 触发按钮：标签（TDesign 多选的样子）；收起来也能看出跑哪几个号
  const labelEl = $(cfg.label);
  if (labelEl) {
    labelEl.innerHTML = pickerTags({ ...v, accounts }, follow, accounts);
    labelEl.title = pickerLabel({ ...v, accounts }, follow);
  }
  // ⚠ 同理：`${cfg.key}-acct-legacy` 拼出来的是 `daily-manual-acct-legacy`，
  //   HTML 里根本没有这个 id（只有 `daily-acct-legacy`）⇒ 拼出来的写法在 manual
  //   那两条上永远写不到（评审 2026-11-01）。改成表里写死。
  const note = cfg.legacy ? $(cfg.legacy) : null;
  if (note) note.hidden = !(cfg.mode === 'auto' && all && all.legacyAuto === true);
  if (cfg.follow) $(cfg.follow).checked = follow;
  $(cfg.all).checked = follow ? false : v.all === true;
  $(cfg.all).disabled = follow;

  // 名单。⚠ 「全部」勾着时下面那些**不禁用** —— 用户可以先挑几个，
  //   取消「全部」时就带着这份名单继续，不用从头勾一遍。
  const list = $(cfg.list);
  list.innerHTML = accounts
    .map((a) => {
      const on = !follow && (v.all || (v.ids || []).includes(a.id));
      return `<label class="picker-row${a.disabled ? ' is-off' : ''}">
        <input type="checkbox" data-pick="${esc(a.id)}"${on ? ' checked' : ''}${a.disabled || follow ? ' disabled' : ''} />
        <span>${esc(a.label || a.id)}${a.disabled ? '（已停用）' : ''}</span>
      </label>`;
    })
    .join('') || '<p class="hint">还没有已登录的账号。先去「账号」页加一个。</p>';
}

/** 把当前勾选存下来。manual 只写 .manual，绝不改 .auto。 */
async function savePicker(cfg, all, ids, follow) {
  const r = await api('/task/accounts', {
    method: 'POST',
    write: true,
    body: { task: cfg.task, mode: cfg.mode, all, ids, follow: follow === true },
  });
  toast(r.note || '已存下');
  await load();
}

function pickerToggle(cfg, ev) {
  ev.stopPropagation();
  const panel = $(cfg.panel);
  const open = panel.hidden;
  for (const o of PICKERS) $(o.panel).hidden = true;
  panel.hidden = !open;
  $(cfg.btn).setAttribute('aria-expanded', String(open));
}

function pickerToggleAll(cfg, checked) {
  const all = (last && last.taskAccounts && last.taskAccounts[cfg.task]) || {};
  const cur = cfg.mode === 'manual' ? (all.manual || {}) : (all.auto || {});
  void savePicker(cfg, checked, cur.ids || [], false).catch((e) =>
    toast(e.message || '没存上', true),
  );
}

function pickerFollow(cfg, checked) {
  const all = (last && last.taskAccounts && last.taskAccounts[cfg.task]) || {};
  const cur = all.manual || {};
  void savePicker(cfg, checked ? false : cur.all === true, checked ? [] : (cur.ids || []), checked).catch(
    (e) => toast(e.message || '没存上', true),
  );
}

function pickerPick(cfg, ev) {
  const box = ev.target.closest('[data-pick]');
  if (!box) return;
  const panel = $(cfg.panel);
  const ids = [...panel.querySelectorAll('input[data-pick]')]
    .filter((b) => b.checked && !b.disabled)
    .map((b) => b.dataset.pick);
  // 手动动一格 = 取消「全部」；manual 还要取消「跟随自动」。
  void savePicker(cfg, false, ids, false).catch((e) => toast(e.message || '没存上', true));
}

/** 「清空」：一个号都不勾（= 这趟不跑），并把「全部 / 跟随」一起取消。
 *  这是 TDesign 多选浮层页脚那个「清空」的等价物 —— 手点三个框才回到空，太啰嗦。 */
function pickerClear(cfg) {
  void savePicker(cfg, false, [], false).catch((e) => toast(e.message || '没存上', true));
}

// ── 绑定：每个任务两份，字面 id（静态检查能看出有没有绑）──
$('daily-acct-btn').addEventListener('click', (ev) => pickerToggle(PICKERS[0], ev));
$('daily-acct-all').addEventListener('change', (ev) => pickerToggleAll(PICKERS[0], ev.currentTarget.checked));
$('daily-acct-list').addEventListener('change', (ev) => pickerPick(PICKERS[0], ev));

$('daily-acct-clear').addEventListener('click', () => pickerClear(PICKERS[0]));
$('daily-manual-btn').addEventListener('click', (ev) => pickerToggle(PICKERS[1], ev));
$('daily-manual-follow').addEventListener('change', (ev) => pickerFollow(PICKERS[1], ev.currentTarget.checked));
$('daily-manual-all').addEventListener('change', (ev) => pickerToggleAll(PICKERS[1], ev.currentTarget.checked));
$('daily-manual-list').addEventListener('change', (ev) => pickerPick(PICKERS[1], ev));

$('daily-manual-clear').addEventListener('click', () => pickerClear(PICKERS[1]));
$('live-acct-btn').addEventListener('click', (ev) => pickerToggle(PICKERS[2], ev));
$('live-acct-all').addEventListener('change', (ev) => pickerToggleAll(PICKERS[2], ev.currentTarget.checked));
$('live-acct-list').addEventListener('change', (ev) => pickerPick(PICKERS[2], ev));

$('live-acct-clear').addEventListener('click', () => pickerClear(PICKERS[2]));
$('live-manual-btn').addEventListener('click', (ev) => pickerToggle(PICKERS[3], ev));
$('live-manual-follow').addEventListener('change', (ev) => pickerFollow(PICKERS[3], ev.currentTarget.checked));
$('live-manual-all').addEventListener('change', (ev) => pickerToggleAll(PICKERS[3], ev.currentTarget.checked));
$('live-manual-list').addEventListener('change', (ev) => pickerPick(PICKERS[3], ev));

$('live-manual-clear').addEventListener('click', () => pickerClear(PICKERS[3]));

document.addEventListener('click', (ev) => {
  if (ev.target.closest('.picker')) return;
  for (const o of PICKERS) $(o.panel).hidden = true;
});

// ══════════ 每账号设置（复刻 mcloud 的「设置」） ══════════
//
// ⚠ 这两个字段（听窗口长度 / 只听这间房）**是按账号的键**，而它们原来挂在
//   **全局**的设置区块里 —— 也就是说"改了哪个账号"取决于当前选中，而界面
//   一个字都不说。用户看到的现象会是"我明明改了，怎么另一个号也跟着变了"。
//   搬到这里之后，"改的是哪一条"由**打开弹窗时点的那条**决定，不再靠猜。
let acctSettingsId = '';

function openAcctSettings(id) {
  const list = (last && last.account && last.account.accounts) || [];
  const a = list.find((x) => x.id === id);
  if (!a) return;
  acctSettingsId = id;
  setText('acct-cfg-title', a.label || a.masked || id);
  $('acct-cfg-seconds').value = Number.isFinite(a.listenSeconds) ? String(a.listenSeconds) : '';
  $('acct-cfg-room').value = Number.isFinite(a.listenRoom) && a.listenRoom > 0 ? String(a.listenRoom) : '';
  $('acct-cfg-heartbeat').value = a.wsHeartbeat || '';
  setText('acct-cfg-note', '');
  $('acct-cfg-dialog').hidden = false;
}

$('acct-cfg-close').addEventListener('click', () => {
  $('acct-cfg-dialog').hidden = true;
});
$('acct-cfg-dialog').addEventListener('click', (ev) => {
  if (ev.target === $('acct-cfg-dialog')) $('acct-cfg-dialog').hidden = true;
});
$('acct-cfg-save').addEventListener('click', (e) =>
  act(
    e.currentTarget,
    async () => {
      const secs = $('acct-cfg-seconds').value.trim();
      const room = $('acct-cfg-room').value.trim();
      const body = { op: 'set-settings', id: acctSettingsId };
      // 空串 = **这一项不改**（不是"设成 0"）—— 留空就是"保持原样"。
      if (secs !== '') body.listenSeconds = Number(secs);
      if (room !== '') body.listenRoom = Number(room);
      body.wsHeartbeat = $('acct-cfg-heartbeat').value.trim();
      const r = await api('/accounts', { method: 'POST', write: true, body });
      $('acct-cfg-dialog').hidden = true;
      return r;
    },
    (r) => r.note || '设置已存下',
  ),
);

// ══════════ 扫码：自动轮询（**没有**"我扫完了"按钮） ══════════
//
// 参考实现：出码 → `setInterval(pollOnce, 服务端给的 poll_interval_ms)` →
// `scanned` 时显示「已扫码，请在手机上确认」（确认动作在**手机上**）→
// `logged_in` 时收尾。所以推进的触发者是**轮询**，不是用户点按钮。
//
// ⚠⚠ F-067：这一段从写下来那天起**就没人调用过** —— `startQrPoll` 定义了、
// 注释也写了"出码后 setInterval"，但没有任何一处真的调它。后果是：
// 出码之后面板**从来不问上游**，手机扫了、在手机上点了确认，而这边永远不知道 ——
// 用户看到的就是「这二维码扫不了」。所以下面两个调用点是这一段的**关键部分**，
// 不是可选装饰：① 出码之后；② 读数里还有一张活的码时（刷新页面 / 重开弹窗）。
let qrTimer = null;

/** 当前这一轮用的间隔（毫秒）。0 = 还没起过。 */
let qrPollMs = 0;

/** 间隔的上限：失败退避最多退到这儿。 */
const QR_POLL_MAX_MS = 6000;

function stopQrPoll() {
  if (qrTimer !== null) {
    clearInterval(qrTimer);
    qrTimer = null;
  }
}

function startQrPoll(ms) {
  stopQrPoll();
  qrPollMs = ms;
  qrTimer = setInterval(() => void qrPollOnce(), ms);
}

/** 轮询**没问成**之后：先**立刻重问一次**，连着两次没问成才退一步。
 *
 *  ⚠ 为什么第一次不退避（F-071）：扫码成功那一枪**只出现一次**（拿到就没了）。
 *  真机上就撞见过——`200059548 已扫码` 之后紧接着一次空响应，然后就是
 *  `200059541 未找到设备`：那次空响应很可能正是要交付的成功响应。此时等 4 秒
 *  再问，等于把机会让掉。退避只用来对付"真的连不上"（连着两次没问成）。
 *
 *  ⚠ 间隔上限 6 秒：退到用户以为卡死之前就该停住。 */
function backoffQrPoll(base) {
  startQrPoll(Math.min((qrPollMs || base || 2000) * 2, QR_POLL_MAX_MS));
}

/** 问成了 ⇒ 回到服务端给的间隔（退避只是"问不到时"的事）。 */
function resetQrPoll(base) {
  if (base && qrPollMs !== base) startQrPoll(base);
}

/** 连着几次没问成。成功一次就清零。 */
let qrFailStreak = 0;

/** 立刻重问一次（不走定时器）。 */
function retryQrNow() {
  stopQrPoll();
  setTimeout(() => void qrPollOnce(), 0);
}

/** 让轮询跟着"现在屏幕上有没有一张**活的**码"走。
 *
 *  - 没有码 / 码过期 / 弹窗关着 ⇒ 停（不留一个偷偷打接口的定时器）；
 *  - 有活的码而定时器没跑 ⇒ 起（页面刷新、面板重启之后接着轮：
 *    会话是**落盘**的，手机扫的时候我们得在问服务端）。
 *
 *  ⚠ 间隔用读数给的那个（`qr.pollMs`）—— 界面不许自己写死一个数。 */
function syncQrPoll(qr) {
  const live = !!qr && !qr.expired && !$('add-dialog').hidden;
  if (!live) {
    stopQrPoll();
    return;
  }
  if (qrTimer === null) startQrPoll(qr.pollMs);
}

/** 轮询回来那一行字：**带倒计时**（剩余毫秒是服务端给的）。
 *
 *  ⚠ 没有倒计时的话，第一次轮询之后那句「· 剩 N 秒」就消失了 —— 而那一行是
 *  用户判断"这张码还活着吗"的唯一依据（画码时是 `renderLogin` 拼的同一句话，
 *  两处必须长得一样）。 */
function qrNoteText(r) {
  const note = r.note || '等手机扫码';
  if (typeof r.expiresInMs !== 'number') return note;
  return `${note} · 剩 ${Math.ceil(r.expiresInMs / 1000)} 秒`;
}

async function qrPollOnce() {
  // 弹窗关了就一次都不该再问（挂着的"立刻重问"也走这条）
  if ($('add-dialog').hidden) {
    stopQrPoll();
    return;
  }
  try {
    const r = await api('/login/qr/poll', { method: 'POST', write: true });
    if (r.note || typeof r.expiresInMs === 'number') setText('qr-note', qrNoteText(r));
    // ⚠ 只有 `stop` 才停。「还没扫到」是轮询期间**绝大多数**的响应，不是错误。
    if (r.stop) {
      stopQrPoll();
      if (r.ok && !r.account && r.note === undefined) return;
      if (r.ok) {
        closeAddDialog();
        toast('登录成功');
      } else {
        toast(r.note || '扫码登录失败', true);
      }
      await load();
      return;
    }
    // 没问成（空响应 / 网络）：第一次立刻重问，连着两次才退避；
    // 问成了 ⇒ 清零 + 回到服务端给的间隔
    if (r.retry) {
      qrFailStreak += 1;
      if (qrFailStreak === 1) retryQrNow();
      else backoffQrPoll(r.pollMs);
    } else {
      qrFailStreak = 0;
      resetQrPoll(r.pollMs);
    }
  } catch (err) {
    stopQrPoll();
    toast(err.message || '轮询失败', true);
  }
}

$('login-qr').addEventListener('click', (e) =>
  act(
    e.currentTarget,
    async () => {
      // 换方式要把另一种的会话清掉（否则两边同时"进行中"，界面不知道该显示哪个）
      await api('/login/cancel', { method: 'POST', write: true });
      // ⚠ 形态**必须跟着这一次出码走**：它进码里的 `ver`，也决定后面轮询的请求头。
      //   后端把它冻进会话，所以之后改界面上的选择动不到这张码（要换就重新出码）。
      const r = await api('/login/qr/start', {
        method: 'POST',
        write: true,
        body: { pcPlatform: addPc },
      });
      // 出码是纯本地拼接（不发上游），所以立刻把码画出来
      // ⚠ 弹窗里页签的显隐统一由 `showAddTab` 管（原来这里是直接改
      //   `pane-qr/pane-sms/pane-pwd` 的 `hidden` —— 那三个 id 现在不存在了）。
      showAddTab('qr');
      $('qr-box').innerHTML = r.svg || '';
      $('qr-box').hidden = false;
      setText('qr-note', r.note);
      // ⚠ 出码之后**必须开始轮询**：手机扫码 → 在手机上确认 → 上游才把凭据给出来，
      //   而"上游给出来了没有"只能靠我们自己去问。少了这一行，扫了也白扫（F-067）。
      //   间隔用这一侧给的那个（`pollMs`），界面不写死。
      startQrPoll(r.pollMs);
      return r;
    },
    '出码了 —— 用手机 App 扫',
  ),
);

// ⚠ 「获取验证码」这一个按钮干两件事：**开会话**（`/login/sms/start`，纯本地）
//   然后**发码**。用户看到的是一屏一个动作 —— 见 `requestCode` 的说明。
$('sms-send').addEventListener('click', (e) =>
  act(
    e.currentTarget,
    async () => {
      const r = await requestCode('sms');
      // ⚠ 滑块由**后端**自动过（取新题 → 解坐标 → 带答案重发，见 `autoPassSlider`）：
      //   界面这一层只可能拿到 `sent` 或一句人话，没有"再点一次"这种中间态。
      if (r.sent) {
        // 发出去了 ⇒ 开始重发倒计时（**界面节奏**：连点只会喂限流）
        startResend('sms');
        return r;
      }
      throw new Error(r.note || '发码没成');
    },
    '验证码已发出',
  ),
);

$('sms-submit').addEventListener('click', (e) =>
  act(
    e.currentTarget,
    async () => {
      const r = await api('/login/sms/submit', {
        method: 'POST',
        write: true,
        body: { code: $('sms-code').value.trim() },
      });
      if (r.account) {
        // ⚠ 先关弹窗再提示：弹窗还开着的话，用户会以为"这一步还要我做什么"
        closeAddDialog();
        toast(`登录成功：${r.account.label}`);
        return r;
      }
      throw new Error(r.note || '登录没成');
    },
    '登录成功',
  ),
);

$('paste-submit').addEventListener('click', (e) =>
  act(
    e.currentTarget,
    async () => {
      const raw = $('paste-body').value.trim();
      if (!raw) throw new Error('先把凭据贴进来');
      const body = { op: 'add', auth: raw };
      // PC 端才带上形态（手机端带了后端也会丢掉）
      // ⚠ 平台是**弹窗级**的（参考实现把它放在最上面，对全部页签生效）——
      //   所以这里不再看"凭据那一栏有没有露出形态选择"，直接带上。
      //   ⚠ 手机端（App）不存在 win/mac 之分，后端会按凭据里的前缀丢掉它。
      body.pcPlatform = addPc;
      let r;
      try {
        r = await api('/accounts', { method: 'POST', write: true, body });
      } catch (err) {
        // ⚠ 形态选择现在是**弹窗级**的（最上面那一排），所以这里的
        //   `needPcPlatform` 不再是"把选择露出来"，而是**提示用户去上面选**。
        if (err && err.data && err.data.needPcPlatform) {
          $('paste-note').textContent =
            '这条是 PC 端 —— 去上面「客户端形态」选一种（Windows / macOS），再点一次「存下」';
          return { ok: true, note: '先去上面选形态' };
        }
        throw err;
      }
      $('paste-body').value = '';
      $('paste-note').textContent = '';
      closeAddDialog();
      return r;
    },
    '凭据已存下',
  ),
);

// ────────── 手机端（App 链路）──────────
//
// ⚠ 这一路的滑块与 PC **不是同一件事**（参考实现的注释）：
//   两端 `NeedSlider` 名字一样，但 PC 是"取图 → 本地识别 → 重发"，
//   而 App 真机是"开 WebView，短信由那个 H5 发"。
//   我们这边两端都走"取图自己解"（参考实现的 App 那条也是），但**成功载荷的
//   形状不同**：App 的 `data` 是密文字符串、要用**另一把钥匙**解。
//   所以这里是三条独立的路由，不是把 PC 那套换个头。

$('app-send').addEventListener('click', (e) =>
  act(
    e.currentTarget,
    async () => {
      const r = await requestCode('app');
      // 同 PC：滑块由后端自动过，界面只认 `sent` 或一句人话
      if (r.sent) {
        startResend('app');
        return r;
      }
      throw new Error(r.note || '发码没成');
    },
    '验证码已发出',
  ),
);

$('app-submit').addEventListener('click', (e) =>
  act(
    e.currentTarget,
    async () => {
      const r = await api('/login/app/submit', {
        method: 'POST',
        write: true,
        body: { code: $('app-code').value.trim() },
      });
      if (r.account) {
        closeAddDialog();
        toast(`登录成功：${r.account.label}`);
        return r;
      }
      throw new Error(r.note || '登录没成');
    },
    '登录成功',
  ),
);

// ⚠ 两个页签的输入框要接上"实时反应"（只留数字 / 按位数开关主按钮 / 回车提交）。
//   放在处理器都挂完之后：`wireCodeForm` 里会 `$()` 那几个按钮。
wireCodeForm('sms', $('sms-phone'), $('sms-code'));
wireCodeForm('app', $('app-phone'), $('app-code'));

// ────────── 补回被误删的四个处理器（切片时把它们一起切掉了）──────────

$('qr-cancel').addEventListener('click', (e) =>
  act(
    e.currentTarget,
    async () => {
      // ⚠ 轮询必须**先停** —— 否则取消之后还在轮，界面会一边"已取消"一边被旧会话改掉。
      stopQrPoll();
      const r = await api('/login/cancel', { method: 'POST', write: true });
      $('qr-box').innerHTML = '';
      $('qr-box').hidden = true;
      return r;
    },
    '已取消',
  ),
);

$('pwd-submit').addEventListener('click', (e) =>
  act(
    e.currentTarget,
    () =>
      api('/login/pwd', {
        method: 'POST',
        write: true,
        body: {
          phone: $('pwd-phone').value.trim(),
          password: $('pwd-pass').value,
          pcPlatform: addPc,
        },
      }),
    (r) => r.note || '登录成功',
  ),
);

// 「换一台设备」—— 本机设备指纹那一块**唯一**的动作（参考实现也只有一个）。
//
// ⚠ 这里原来还绑了第二个按钮「我的码一直扫不出来」（同一个动作、按症状命名）。
//   理由是"用户的念头是症状、而动作叫「换一台设备」，名字对不上会来回翻"。
//   用户圈出来之后删了：**复刻就是复刻** —— 多一个按钮就多一处"点哪个"的犹豫，
//   而那正是这一版要消灭的东西。
const resetDevice = (e) =>
  act(
    e.currentTarget,
    () => api('/login/device/reset', { method: 'POST', write: true }),
    (r) => r.note,
  );
$('device-reset').addEventListener('click', resetDevice);

// 账号列表里的切换 / 删除（按钮是画出来的，用事件代理）
$('acct-list').addEventListener('click', async (ev) => {
  const pick = (name) => ev.target.closest(`[data-acc-${name}]`);
  const ref = pick('refresh');
  const ren = pick('rename');
  const cfg = pick('settings');
  const dis = pick('disable');
  const del = pick('del');
  if (!ref && !ren && !cfg && !dis && !del) return;
  const post = (body) => api('/accounts', { method: 'POST', write: true, body });
  try {
    if (ref) {
      // ⚠ 这一条**不改当前选中的账号**：后端用 `scoped(store, id)` 作用在
      //   指定账号上。老做法只能"先切过去再刷"，那会让用户莫名其妙的换号。
      const r = await post({ op: 'refresh', id: ref.dataset.accRefresh });
      toast(r.note || '凭据已刷新');
    } else if (ren) {
      const id = ren.dataset.accRename;
      const cur = (last && last.account && last.account.accounts) || [];
      const a = cur.find((x) => x.id === id) || {};
      const label = prompt('这个账号叫什么？留空就回落成手机号。', a.label || '');
      if (label === null) return;
      const r = await post({ op: 'edit', id, label });
      toast(r.note || '已改名');
    } else if (cfg) {
      openAcctSettings(cfg.dataset.accSettings);
      return; // 弹窗自己会 load()，不走下面那一趟
    } else if (dis) {
      const on = dis.dataset.on !== '1'; // 当前是启用 ⇒ 这次要停用
      const r = await post({ op: 'disable', id: dis.dataset.accDisable, disabled: on });
      toast(r.note || (on ? '已停用' : '已启用'));
    } else {
      const id = del.dataset.accDel;
      if (!confirm('移除这个账号？\n⚠ 它名下的票、进度与"这一小时已下单"都会一起清掉。')) return;
      const r = await post({ op: 'remove', id });
      toast(r.note || '已移除');
    }
    await load();
  } catch (err) {
    toast(err.message || '失败了', true);
  }
});

// 日常
// ⚠ 不走 `act`：它会在触发后**立刻**说"跑完了"（面板那个接口是异步的）
$('daily-run').addEventListener('click', (e) => runTaskAndWait('daily', e.currentTarget));
$('daily-stop').addEventListener('click', (e) => stopRunning('daily', e.currentTarget));

// 兑换
// 当前账号（手动兑换用）。切号会改全局「当前账号」—— 原页面也是这个语义。
$('ex-account').addEventListener('change', async (ev) => {
  const id = ev.target.value;
  if (!id) return;
  try {
    const r = await api('/accounts', { method: 'POST', write: true, body: { op: 'select', id } });
    toast(r.note || '已切换账号');
    await load();
  } catch (e) {
    toast(String(e && e.message ? e.message : e), true);
  }
});

// 二次确认偏好（localStorage：这台浏览器的事，不是账号属性）。
$('ex-confirm-first').addEventListener('change', (ev) => {
  exSetConfirmFirst(ev.target.checked);
  setText(
    'ex-intro',
    (ev.target.checked
      ? '挑一件兑换。下单会真的扣 AI 豆，所以点下去要再确认一次；抢不到的可以先「定时」等那一场。'
      : '挑一件兑换。下单会真的扣 AI 豆 —— 你已经关掉二次确认，点「立即兑换」就直接扣。') +
      (exLocalJudge() ? '' : ' 本地判定已关：抢光/未开始/已兑过也会真发单。'),
  );
});

$('ex-local-judge').addEventListener('change', (ev) => {
  exSetLocalJudge(ev.target.checked);
  setText(
    'ex-intro',
    (exConfirmFirst()
      ? '挑一件兑换。下单会真的扣 AI 豆，所以点下去要再确认一次；抢不到的可以先「定时」等那一场。'
      : '挑一件兑换。下单会真的扣 AI 豆 —— 你已经关掉二次确认，点「立即兑换」就直接扣。') +
      (ev.target.checked ? '' : ' 本地判定已关：抢光/未开始/已兑过也会真发单。'),
  );
  repaintExchange();
});

// 清单筛选：名字/编号 + 只看可兑/关注。
const repaintExchange = () => {
  if (last && last.exchange) renderExchangeTable(last.exchange, exSay);
};
$('ex-filter').addEventListener('input', repaintExchange);
$('ex-verdict').addEventListener('change', repaintExchange);
$('ex-show-blocked').addEventListener('change', repaintExchange);
$('ex-session-refresh').addEventListener('click', (e) =>
  act(
    e.currentTarget,
    () => post('auth/refresh', { force: true }),
    (r) => (r && r.note) || '登录凭据已刷新',
  ),
);

/** 表格/关注区里的动作。`data-*-idx` 指 `exPicks` 的下标（清单里的一件）。 */
async function exchangeRowAction(ev) {
  const star = ev.target.closest('[data-sub-idx]');
  const planBtn = ev.target.closest('[data-plan-idx]');
  const buyBtn = ev.target.closest('[data-buy-idx]');
  const unsub = ev.target.closest('[data-unsub-name]');
  const planName = ev.target.closest('[data-plan-name]');
  const buyName = ev.target.closest('[data-buy-name]');
  if (!star && !planBtn && !buyBtn && !unsub && !planName && !buyName) return;
  try {
    if (star) {
      const p = exPicks[Number(star.dataset.subIdx)];
      if (p) await exToggleSub(p, !exIsSub(p), star);
      return;
    }
    if (unsub) {
      const p = exFindPick(unsub.dataset.unsubName, undefined) || { name: unsub.dataset.unsubName };
      await exToggleSub(p, false, unsub);
      return;
    }
    if (planBtn || planName) {
      const p = planBtn ? exPicks[Number(planBtn.dataset.planIdx)] : exFindPick(planName.dataset.planName, undefined);
      if (!p) throw new Error('清单里找不到这一件');
      openPlanDialog(p);
      return;
    }
    const p = buyBtn ? exPicks[Number(buyBtn.dataset.buyIdx)] : exFindPick(buyName.dataset.buyName, undefined);
    if (!p) throw new Error('清单里找不到这一件');
    await exBuyNow(p, buyBtn || buyName);
  } catch (e) {
    toast(String(e && e.message ? e.message : e), true);
  }
}

$('ex-tbl').addEventListener('click', (ev) => {
  const toggle = ev.target.closest('[data-group-toggle]');
  if (toggle) {
    ev.preventDefault();
    exToggleGroup(toggle.dataset.groupToggle);
    return;
  }
  exchangeRowAction(ev);
});
$('ex-subs').addEventListener('click', exchangeRowAction);
// 「读一次」= 上面「刷新清单」**同一枪**（只读清单，不发余额/预热/取题那几枪）
$('ex-read').addEventListener('click', (e) =>
  act(e.currentTarget, refreshPicks, (r) => (r && r.note) || '清单已刷新'),
);

// 直播
$('live-claim').addEventListener('click', (e) =>
  act(
    e.currentTarget,
    async () => {
      const raw = $('live-paste').value.trim();
      if (!raw) throw new Error('先把口令贴进来');
      await api('/live/codes', { method: 'POST', write: true, body: { codes: raw } });
      const t0 = Date.now();
      await runTask('live');
      toast('口令收下了 —— 它正在领');
      await waitRun('mcloud-live', '直播', t0, null);
      // 跑完再清**保存的**口令；输入框留着，用户能看到自己贴了什么。
      // 保存的旧口令不清会让下一次自动运行误判成“手动领取”。
      await api('/live/codes', { method: 'POST', write: true, body: { clear: true } });
      return null;
    },
    '这一轮领完了 —— 结果看下面的日志',
  ),
);
// 同「日常」：这个也是异步触发的任务，要说真话就得等它跑完
$('live-run').addEventListener('click', (e) => runTaskAndWait('live', e.currentTarget));
$('live-stop').addEventListener('click', (e) => stopRunning('live', e.currentTarget));

$('live-listen-seconds').addEventListener('change', async (e) => {
  // 清空就恢复默认，不需要再点保存 —— 否则用户删了 30，跑起来还是 30。
  if (e.currentTarget.value.trim() !== '') return;
  try {
    await api('/config', { method: 'POST', write: true, body: { listenSeconds: 600 } });
    toast('听弹幕时长已恢复默认 600 秒');
    await load();
  } catch (err) {
    toast(err && err.message ? err.message : '没恢复成', true);
  }
});

$('live-listen-save').addEventListener('click', (e) =>
  act(
    e.currentTarget,
    async () => {
      // 空输入 = 恢复默认 600。以前直接 Number('') = 0，校验失败，
      // 保存不了；用户删掉旧值再跑，实际还在用旧值。
      const raw = $('live-listen-seconds').value.trim();
      const n = raw === '' ? 600 : Number(raw);
      if (!Number.isInteger(n) || n < 5 || n > 1800) {
        throw new Error('听弹幕时长要给 5–1800 之间的整数（秒）');
      }
      return api('/config', {
        method: 'POST',
        write: true,
        body: { listenSeconds: n },
      });
    },
    (r) => r.note || `听弹幕时长已设为 ${r.listenSeconds} 秒`,
  ),
);

// ⚠ 「设置」那一页的保存按钮（`cfg-save`）与「跑自检」（`selftest-run`）跟着那一页
//   一起删了。自检那条**路由**还在（`/selftest`，e2e 与 `scripts/split_check.py`
//   都在用它），只是不再有界面入口 —— 那是装机器的人该看的，不是用户。

// ── 兑换的两个时间参数 ──
/** 数字框左右加减：空值不猜（点减号保持空），点加号才从默认值开始。 */
function stepNumber(btn) {
  const input = $(btn.dataset.stepFor);
  if (!input) return;
  const min = input.min === '' ? 0 : Number(input.min);
  const max = input.max === '' ? Number.POSITIVE_INFINITY : Number(input.max);
  const delta = Number(btn.dataset.delta) || 0;
  let v = input.value.trim();
  if (v === '') {
    if (delta < 0) return;
    v = input.dataset.default || String(min);
  }
  const n = Number(v);
  if (!Number.isFinite(n)) return;
  input.value = String(Math.min(max, Math.max(min, n + delta)));
  input.dispatchEvent(new Event('input', { bubbles: true }));
  input.focus();
}

for (const b of document.querySelectorAll('[data-step-for]')) {
  b.addEventListener('click', () => stepNumber(b));
}

// 定时抢兑：计划列表（添加 / 编辑 / 暂停 / 删除）。
$('ex-sched-add').addEventListener('click', () => openPlanDialog(null));
$('ex-sched-list').addEventListener('click', (ev) => {
  const btn = ev.target.closest('button[data-plan-toggle],button[data-plan-edit],button[data-plan-del]');
  if (!btn) return;
  const plans = (last && last.exchange && last.exchange.plans) || [];
  const id = btn.dataset.planToggle || btn.dataset.planEdit || btn.dataset.planDel;
  const plan = plans.find((p) => p.id === id);
  if (!plan) return;
  if (btn.dataset.planEdit !== undefined) {
    openPlanDialog(null, plan);
    return;
  }
  if (btn.dataset.planDel !== undefined) {
    if (!confirm(`删除计划「${plan.prizeName}」？`)) return;
    return act(
      btn,
      async () => {
        const r = await api('/exchange/plans', {
          method: 'POST',
          write: true,
          body: { action: 'delete', id },
        });
        if (!r.ok) throw new Error(r.note || '没删成');
        await load();
        return r;
      },
      (r) => (r && r.note) || '计划已删除',
    );
  }
  const on = !plan.enabled;
  return act(
    btn,
    async () => {
      const r = await api('/exchange/plans', {
        method: 'POST',
        write: true,
        body: { action: 'toggle', id, enabled: on },
      });
      if (!r.ok) throw new Error(r.note || '没改成功');
      await load();
      return r;
    },
    (r) => (r && r.note) || (on ? '计划已启用' : '计划已暂停'),
  );
});

// 定时抢兑弹窗：入口是清单/关注行的「定时」。
$('ex-plan-target').addEventListener('change', () => {
  const opt = $('ex-plan-target').selectedOptions[0];
  planDialogQuery = opt ? opt.value : '';
  planDialogId = opt && opt.dataset.id ? Number(opt.dataset.id) : null;
  setText('ex-plan-prize', planDialogQuery || '还没选目标');
  setText('ex-plan-price', '');
});
$('ex-plan-close').addEventListener('click', () => {
  $('ex-plan-dialog').hidden = true;
});
$('ex-plan-cancel').addEventListener('click', () => {
  $('ex-plan-dialog').hidden = true;
});
$('ex-plan-dialog').addEventListener('click', (ev) => {
  if (ev.target === $('ex-plan-dialog')) $('ex-plan-dialog').hidden = true;
});
$('ex-plan-submit').addEventListener('click', (ev) =>
  act(ev.currentTarget, savePlanDialog, (r) => (r && r.note) || '定时已保存'),
);
for (const id of ['ex-plan-cron', 'ex-plan-lead', 'ex-plan-lead-sec', 'ex-plan-max', 'ex-plan-until']) {
  $(id).addEventListener('input', updatePlanPreview);
}

// ──────────────────────────────────────────────────────────── 起手

// 打开页面先画一次。⚠ **不自动轮询** —— 读数只在动作之后刷新。
// 自动轮询会让「我点了一下」和「它自己变了」分不清，而且这个页面在 iframe 里，
// 没人看的时候不该一直打接口。
//
// 「今日AI豆」那一格**不塞进 `load()`** —— 它每次点都要打移动云盘，见上面 `renderData`
// 之后那一节的模块头。这里只画"还没取"那一行占位。
paintBeansList();
// 领奖那一格同理：进这一块才取（见 `autoFetchPrizesOnce`），这里只画占位。
paintPrizeAll();
load();
