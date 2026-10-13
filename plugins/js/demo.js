// plugins/js/demo.js —— 示例插件：自带配置页 + 自己的 API + 一条定时任务。
//
// 这个文件演示三件事，也是「插件自有 UI」的最小完整形态：
//
//   1. **自己的配置存在自己的表里**（`demo_config`）—— 主面板不碰它
//   2. **自己的 API**：注册 `/plugins/demo/api/*` 三条路由，供自己的配置页调用
//   3. **自己的定时任务**：`collect` 会出现在主面板的「任务列表」里
//
// 边界（这一轮的重点）：主面板负责导航、任务定义与调度、测试运行、插件管理；
// 插件负责**它自己那份配置**的界面与接口。所以这里没有「读主面板的任务」这种代码 ——
// 插件不需要知道主面板怎么组织它的界面。
//
// 配置页在 plugins/demo/ 下（与插件同名），运行时可访问 /plugins/demo/。
// 静态文件由 Rust 侧从插件目录读（见 src/web.rs 的 plugin_asset）。

import { log, warn, register_cron, register_route, sqlite_exec, sqlite_query, http_fetch } from 'host-api';

const PLUGIN = 'demo';
const API = '/plugins/demo/api';

// ---------------------------------------------------------------- 配置存取

sqlite_exec(
    'CREATE TABLE IF NOT EXISTS demo_config (k TEXT PRIMARY KEY, v TEXT NOT NULL)'
);

/** 没存过配置时的默认值。 */
const DEFAULTS = {
    interval: '30s',
    target: 'http://127.0.0.1:8080/metrics',
    threshold: '0.85',
    enabled: 1,
};

function readConfig() {
    const cfg = Object.assign({}, DEFAULTS);
    const rows = sqlite_query('SELECT k, v FROM demo_config');
    for (const r of rows) cfg[r.k] = r.v;
    cfg.enabled = String(cfg.enabled) === '1' || cfg.enabled === 1 ? 1 : 0;
    return cfg;
}

function writeConfig(cfg) {
    for (const k of Object.keys(DEFAULTS)) {
        if (cfg[k] === undefined) continue;
        sqlite_exec(
            'INSERT INTO demo_config (k, v) VALUES (?1, ?2) ' +
                'ON CONFLICT(k) DO UPDATE SET v = excluded.v',
            [k, String(cfg[k])]
        );
    }
}

/** 校验放在**后端**（插件侧），不在前端重复一遍 —— 前端只负责把错误原因显示出来。 */
function validate(cfg) {
    if (!/^\d+(ms|s|m|h)$/.test(cfg.interval)) {
        return '采集间隔要写成 500ms / 30s / 5m / 1h 这种形式';
    }
    const th = Number(cfg.threshold);
    if (!Number.isFinite(th) || th <= 0 || th > 1) {
        return '阈值要是 0 到 1 之间的小数，例如 0.85';
    }
    if (cfg.target && !/^https?:\/\//.test(cfg.target)) {
        return '目标地址要以 http:// 或 https:// 开头';
    }
    return null;
}

// ---------------------------------------------------------------- 采集

/** 合成一份样本。离线（拉不到目标）时的兜底，保证示例随时可跑。 */
function syntheticSamples(threshold) {
    const t = Date.now() / 1000;
    const names = ['cpu', 'mem', 'latency_ms'];
    const out = [];
    for (let i = 0; i < names.length; i++) {
        // 用 sin 造一条平滑但有波动的曲线，比 Math.random 更好看趋势
        const raw = 0.5 + 0.35 * Math.sin(t / 7 + i * 1.7);
        const value = names[i] === 'latency_ms' ? Math.round(raw * 200) : Math.round(raw * 100) / 100;
        out.push({
            name: names[i],
            value: value,
            abnormal: names[i] === 'latency_ms' ? value / 200 > threshold : value > threshold,
        });
    }
    return out;
}

/** 从目标地址拉真实样本；拉不到就返回 null（由调用方决定怎么办）。 */
function fetchSamples(target, threshold) {
    try {
        const res = http_fetch(target, { timeout_ms: 1500 });
        if (res.status !== 200) return { error: '目标返回 HTTP ' + res.status };
        const body = JSON.parse(res.body);
        const out = [];
        for (const k of Object.keys(body)) {
            const v = Number(body[k]);
            if (!Number.isFinite(v)) continue;
            out.push({ name: k, value: v, abnormal: v > threshold });
        }
        return out.length ? { samples: out } : { error: '目标返回的 JSON 里没有数值字段' };
    } catch (e) {
        return { error: String(e && e.message ? e.message : e) };
    }
}

/**
 * 本地时间戳。
 *
 * 不用 `toISOString()`：它给的是 UTC，面板上显示出来会差 8 小时，而
 * 嵌入式 QuickJS 没有 ICU，`toLocaleString` 也拿不到时区 —— 只能自己拼。
 * 这类「本地运行时的缺口」是写插件时最容易踩的坑之一。
 */
function localStamp() {
    const d = new Date();
    const p = (n) => (n < 10 ? '0' + n : String(n));
    return (
        d.getFullYear() + '-' + p(d.getMonth() + 1) + '-' + p(d.getDate()) +
        ' ' + p(d.getHours()) + ':' + p(d.getMinutes()) + ':' + p(d.getSeconds())
    );
}

function collect() {
    const cfg = readConfig();
    const threshold = Number(cfg.threshold);
    const at = localStamp();

    if (!cfg.enabled) {
        return { at: at, target: cfg.target, samples: [], abnormal: 0, note: '插件已停用，跳过采集' };
    }

    let samples = null;
    let note = '';
    if (cfg.target) {
        const r = fetchSamples(cfg.target, threshold);
        if (r.samples) samples = r.samples;
        else note = '拉取目标失败（' + r.error + '），本次用合成样本';
    }
    if (!samples) samples = syntheticSamples(threshold);

    let abnormal = 0;
    for (const s of samples) if (s.abnormal) abnormal++;

    return { at: at, target: cfg.target, samples: samples, abnormal: abnormal, threshold: threshold, note: note };
}

// ---------------------------------------------------------------- 自己的 API

/** 统一的 JSON 回包。 */
function json(status, obj) {
    return {
        status: status,
        headers: { 'content-type': 'application/json; charset=utf-8' },
        body: JSON.stringify(obj),
    };
}

register_route(API + '/config', 'GET', function () {
    return json(200, { config: readConfig() });
});

register_route(API + '/config', 'POST', function (req) {
    let body;
    try {
        body = JSON.parse(req.body || '{}');
    } catch (e) {
        return json(400, { error: '请求体不是合法 JSON：' + e.message });
    }

    const cfg = {
        interval: body.interval === undefined ? DEFAULTS.interval : String(body.interval).trim(),
        target: body.target === undefined ? '' : String(body.target).trim(),
        threshold: body.threshold === undefined ? DEFAULTS.threshold : String(body.threshold).trim(),
        enabled: body.enabled ? 1 : 0,
    };

    const bad = validate(cfg);
    if (bad) return json(400, { error: bad });

    writeConfig(cfg);
    log(`${PLUGIN}: 配置已更新 interval=${cfg.interval} threshold=${cfg.threshold} enabled=${cfg.enabled}`);

    // 间隔改了要让调度跟着变：重注册同名任务会报错，所以这里只提示，不动调度。
    const note = '调度间隔改动会在插件热更新后生效（任务表达式由 register_cron 声明）';
    return json(200, { ok: true, config: readConfig(), note: note });
});

register_route(API + '/run', 'POST', function () {
    const out = collect();
    if (out.note && out.samples.length === 0) warn(`${PLUGIN}: ${out.note}`);
    return json(200, out);
});

// ---------------------------------------------------------------- 自己的定时任务

// 这条任务会出现在主面板的「任务列表」里（来源标为「插件代码」）。
// 它的配置只在插件自己的配置页上改 —— 这就是主面板与插件 UI 的分工。
register_cron('collect', '@every 30s', function (p) {
    const out = collect();
    if (out.note) {
        warn(`${PLUGIN}: ${out.note}`);
        return;
    }
    log(
        `${PLUGIN}: 第 ${p.runs} 次采集 ${out.samples.length} 个样本` +
            (out.abnormal ? `，${out.abnormal} 个超过阈值` : '，全部正常')
    );
});

log(`${PLUGIN}: 已加载（配置页 /plugins/demo/，API ${API}/*）`);
