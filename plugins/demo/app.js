/*
  plugins/demo/app.js —— 示例插件自己的前端逻辑。

  它只调**这个插件自己的** API（/plugins/demo/api/*），不碰主面板的接口 ——
  这就是主面板与插件自有 UI 的边界：主面板管任务与调度，插件管自己那份配置。

  约定：
  - GET  /plugins/demo/api/config  → 读配置
  - POST /plugins/demo/api/config  → 写配置
  - POST /plugins/demo/api/run     → 用当前配置立刻采一次样
  这三条路由由 plugins/js/demo.js 用 register_route 注册（Rust 侧引擎线程执行）。

  所有错误分支都必须有明确出口：网络失败、后端返回非 JSON、后端返回 {ok:false}。
*/

const API = '/plugins/demo/api';

/** 主面板用 iframe 嵌进来时会带 ?embed=1。 */
const EMBEDDED = new URLSearchParams(location.search).get('embed') === '1';
if (EMBEDDED) document.body.classList.add('embedded');

const $ = (id) => document.getElementById(id);

/**
 * 统一的请求封装：**不抛异常**，返回 { ok, ...数据 } 或 { ok:false, error }。
 * 跟主面板的 web/js/api.js 同一套约定 —— 插件页面也用得上，值得照抄这个形状。
 */
async function req(path, opts = {}) {
  const { method = 'GET', body } = opts;
  const init = { method, headers: {}, credentials: 'same-origin' };
  if (body !== undefined) {
    init.headers['content-type'] = 'application/json';
    init.body = JSON.stringify(body);
  }

  let res;
  try {
    res = await fetch(path, init);
  } catch (e) {
    return { ok: false, error: '连不上后端：' + (e.message || e) };
  }

  if (res.status === 401) {
    // 主面板开了鉴权时，插件页面同样要登录。直接送回登录页。
    location.href = '/login?next=' + encodeURIComponent(location.pathname);
    return { ok: false, error: '未登录' };
  }

  const text = await res.text();
  let data;
  try {
    data = text ? JSON.parse(text) : {};
  } catch {
    return { ok: false, error: `HTTP ${res.status}（响应不是 JSON）：${text.slice(0, 160)}` };
  }
  if (!res.ok) return { ok: false, error: data.error || `HTTP ${res.status}` };
  return Object.assign({ ok: true }, data);
}

// ---------------------------------------------------------------- 配置

function fillForm(cfg) {
  $('cfg-interval').value = cfg.interval ?? '';
  $('cfg-target').value = cfg.target ?? '';
  $('cfg-threshold').value = cfg.threshold ?? '';
  $('cfg-enabled').value = String(cfg.enabled ?? 1);
}

function readForm() {
  return {
    interval: $('cfg-interval').value.trim(),
    target: $('cfg-target').value.trim(),
    threshold: $('cfg-threshold').value.trim(),
    enabled: $('cfg-enabled').value === '1' ? 1 : 0,
  };
}

async function loadConfig() {
  const conn = $('conn');
  conn.className = 'badge idle';
  conn.textContent = '读取配置中…';

  const res = await req(`${API}/config`);
  if (!res.ok) {
    conn.className = 'badge failed';
    conn.textContent = '读取失败';
    $('save-hint').className = 'err-text';
    $('save-hint').textContent = res.error;
    return;
  }
  fillForm(res.config || {});
  conn.className = 'badge success';
  conn.textContent = '已连接';
}

async function saveConfig() {
  const btn = $('save-btn');
  const hint = $('save-hint');
  btn.disabled = true;
  hint.className = 'hint';
  hint.textContent = '保存中…';

  const res = await req(`${API}/config`, { method: 'POST', body: readForm() });

  btn.disabled = false;
  if (res.ok) {
    hint.className = 'hint';
    hint.textContent = '已保存' + (res.note ? '（' + res.note + '）' : '');
  } else {
    // 后端会做校验（间隔格式、阈值范围），失败原因原样显示 —— 不在前端重复实现一遍校验
    hint.className = 'err-text';
    hint.textContent = res.error;
  }
}

// ---------------------------------------------------------------- 试跑

async function runOnce() {
  const btn = $('run-btn');
  const out = $('out');
  btn.disabled = true;
  btn.innerHTML = '<span class="spinner"></span> 采集中…';

  const res = await req(`${API}/run`, { method: 'POST', body: {} });

  btn.disabled = false;
  btn.textContent = '立即采集一次';
  out.innerHTML = '';

  const line = (text, cls = 'stdout') => {
    const span = document.createElement('span');
    span.className = 'console-line ' + cls;
    span.textContent = text + '\n';
    out.appendChild(span);
  };

  if (!res.ok) {
    line('采集失败：' + res.error, 'error');
    return;
  }
  line(`采样时间 ${res.at} · 目标 ${res.target}`, 'meta');
  for (const s of res.samples || []) {
    line(`  ${s.name}: ${s.value}${s.abnormal ? '  ← 超过阈值 ' + res.threshold : ''}`, s.abnormal ? 'stderr' : 'stdout');
  }
  if (res.abnormal) {
    line(`⚠ ${res.abnormal} 个样本超过阈值`, 'error');
  } else {
    line('全部样本正常', 'stdout');
  }
}

// ---------------------------------------------------------------- 启动

$('save-btn').addEventListener('click', saveConfig);
$('reload-btn').addEventListener('click', loadConfig);
$('run-btn').addEventListener('click', runOnce);

loadConfig();
