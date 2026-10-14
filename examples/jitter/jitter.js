// 抖动夹具（第二十三轮）—— 验证随机表达式真的把触发时刻打散了，
// 而且「打散」这件事**没有污染** `lateMs`。
//
// # 为什么需要它
//
// 多个用户/实例用同一个表达式时，会在同一毫秒一起打服务器。抖动把触发时刻
// 随机推开一点，就能把这个尖峰摊平。
//
// # 它同时验证什么
//
// - `@every 1s -200ms` 的实际触发落在理论时刻**之前**，但不超过 200ms
// - payload 里 `lateMs` 是**真实**的调度延迟（几毫秒），不是「-200ms 那种被
//   抖动污染过的数」—— 这一条是重点：污染了的话 `diag` 的 p99 告警就废了
// - `firedAt - scheduledAt == lateMs + jitterMs` 这个恒等式成立
// - 同一个 `PANEL_JITTER_SEED` 给出同一串偏移（可复现，方便排障）

import { register_cron, register_route } from 'host-api';

const records = [];

function json(status, obj) {
    return {
        status: status,
        headers: { 'content-type': 'application/json; charset=utf-8' },
        body: JSON.stringify(obj),
    };
}

register_route('/plugins/jitter/stats', 'GET', function () {
    return json(200, { records: records });
});

register_route('/plugins/jitter/reset', 'POST', function () {
    records.length = 0;
    return json(200, { ok: true });
});

// 上限正好是周期的 20%（1s 的 20% = 200ms）—— 贴着上限写，最能暴露越界
register_cron('jittered', '@every 1s -200ms', function (p) {
    if (records.length < 200) {
        records.push({
            name: p.name,
            scheduledAt: p.scheduledAt,
            firedAt: p.firedAt,
            lateMs: p.lateMs,
            jitterMs: p.jitterMs,
            // ⚠ 这一格**必须记**：`jitterMs == 0` 有两个来源（抖到正好 0 /
            //   落后重对齐），验收脚本要靠它把重对齐那几拍排除掉。
            //   不记的话那条"同种子给出同一偏移"的断言会随机红。
            realigned: p.realigned,
        });
    }
});

// cron 的**理论**时刻是绝对的（整秒），所以「同一种子 → 同一串偏移」在这里
// 才真的成立。`@every` 的理论时刻是从进程启动那一刻起算的，跨重启本来就不同 ——
// 所以可复现性只能用 cron 验。
register_cron('cron-jittered', '*/1 * * * * * ~200ms', function (p) {
    if (records.length < 200) {
        records.push({
            name: p.name,
            scheduledAt: p.scheduledAt,
            firedAt: p.firedAt,
            lateMs: p.lateMs,
            jitterMs: p.jitterMs,
            // ⚠ 这一格**必须记**：`jitterMs == 0` 有两个来源（抖到正好 0 /
            //   落后重对齐），验收脚本要靠它把重对齐那几拍排除掉。
            //   不记的话那条"同种子给出同一偏移"的断言会随机红。
            realigned: p.realigned,
        });
    }
});

// 对照组：没有抖动时 jitterMs 必须恒为 0，lateMs 就是普通的几毫秒
register_cron('plain', '@every 1s', function (p) {
    if (records.length < 200) {
        records.push({
            name: p.name,
            scheduledAt: p.scheduledAt,
            firedAt: p.firedAt,
            lateMs: p.lateMs,
            jitterMs: p.jitterMs,
            // ⚠ 这一格**必须记**：`jitterMs == 0` 有两个来源（抖到正好 0 /
            //   落后重对齐），验收脚本要靠它把重对齐那几拍排除掉。
            //   不记的话那条"同种子给出同一偏移"的断言会随机红。
            realigned: p.realigned,
        });
    }
});
