// 「抢兑」的替身 —— 用来验证 `worker_hint('exclusive')` 真的把它隔离了。
//
// # 为什么表达式是 1 秒而不是 `0 10 * * *`
//
// 真实形态是 `0 10 * * *`（一天一次），可一天跑一次没法测量。
// 1 秒正好卡在 `FINE_GRAINED_MS` 的分界线上：`is_fine_grained()` 是
// **严格小于** 1000ms，所以 1 秒的任务会被启发式判成**粗粒度** ——
// 这正是要复现的那个洞：一个「错过就废」的任务，光看间隔是看不出来的。
//
// # 这一行就是被测量的东西
//
// 下面那个 `worker_hint('exclusive')` 会被 `scripts/worker_hint.py` 删掉跑一次对照。
// 两次运行的**唯一差别**就是这个声明（`ENGINE_WORKERS` 都是 1）。

import { register_cron, register_route, worker_hint } from 'host-api';

worker_hint('exclusive'); // STRIP-ME —— 对照运行会把这一行删掉

function json(status, obj) {
    return {
        status: status,
        headers: { 'content-type': 'application/json; charset=utf-8' },
        body: JSON.stringify(obj),
    };
}

const lates = []; // 宿主给的 lateMs：预定时刻到现在差了多少
let count = 0;
let first = 0;

register_route('/plugins/exchange/stats', 'GET', function () {
    const sorted = [...lates].sort((a, b) => a - b);
    const q = (p) =>
        sorted.length === 0 ? null : sorted[Math.min(sorted.length - 1, Math.floor(sorted.length * p))];
    return json(200, {
        count,
        late_p50: q(0.5),
        late_p99: q(0.99),
        late_max: sorted.length ? sorted[sorted.length - 1] : null,
        // 验收判据就是它：被拖住过几次
        late_over_5: lates.filter((x) => x > 5).length,
        late_over_100: lates.filter((x) => x > 100).length,
        elapsed_ms: first ? Date.now() - first : null,
    });
});

register_route('/plugins/exchange/reset', 'POST', function () {
    lates.length = 0;
    count = 0;
    first = 0;
    return json(200, { ok: true });
});

// `*/1 * * * * *` = 每秒一次（6 段 cron，第一段是秒）。
// 用 cron 而不是 `every('t','1s')` 是刻意的：真实表达式就是 cron，
// 而且这样源码里的 `*/1` 会被 `finest_interval_ms` 扫成 1000ms（判成粗粒度）。
register_cron('exchange', '*/1 * * * * *', function (p) {
    if (!first) first = Date.now();
    if (lates.length < 20000) lates.push(p.lateMs);
    count++;
});
