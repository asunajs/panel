// 「串行代价」测量夹具 —— 快任务。
//
// 每 20ms 触发一次，把**实际间隔**和宿主给的 `lateMs`（预定时刻到现在差多少）
// 都记下来，然后通过一条自带的 HTTP 路由把分位数吐出来。
//
// 为什么在这里算而不是在宿主侧算：`lateMs` 宿主只写进日志（毫秒级任务不能每次
// 触发都刷屏），没有聚合。要让「一个慢任务拖住多少」变成数字，就得自己收集。
//
// 这个插件**故意不碰 SQLite、不做 IO** —— 它自己必须便宜到不构成干扰。

import { every, register_route, log } from 'host-api';

// 插件路由的返回值形状是 {status, headers, body}，而且 body 必须是**字符串** ——
// 直接 return 一个对象的话响应体是空的（这一点文档里没写清，实测踩过）。
function json(status, obj) {
    return {
        status: status,
        headers: { 'content-type': 'application/json; charset=utf-8' },
        body: JSON.stringify(obj),
    };
}

const gaps = []; // 相邻两次触发之间的实际间隔
const lates = []; // 宿主记的调度延迟
let count = 0;
let last = 0;
let first = 0;
let maxGap = 0;

register_route('/plugins/serial-fast/stats', 'GET', function () {
    const sorted = (a) => [...a].sort((x, y) => x - y);
    const q = (arr, p) => (arr.length === 0 ? null : arr[Math.min(arr.length - 1, Math.floor(arr.length * p))]);
    const g = sorted(gaps);
    const l = sorted(lates);
    return json(200, {
        count,
        // 间隔：实际两次触发差了多少（这是用户能感觉到的东西）
        gap_p50: q(g, 0.5),
        gap_p99: q(g, 0.99),
        gap_max: g.length ? g[g.length - 1] : null,
        // 超过一个小阈值就算「丢拍」：20ms 的任务间隔到了 40ms 以上，
        // 说明中间至少有一次没触发
        gap_over_40: g.filter((x) => x > 40).length,
        // 宿主视角的调度延迟
        late_p50: q(l, 0.5),
        late_p99: q(l, 0.99),
        late_max: l.length ? l[l.length - 1] : null,
        late_over_40: l.filter((x) => x > 40).length,
        // 均值与实测窗口：用来对账「触发次数 × 目标间隔」是否等于窗口长度。
        // 对不上说明实际节奏和目标不一致（比四次分位数更能暴露系统性问题）。
        gap_mean: g.length ? g.reduce((a, b) => a + b, 0) / g.length : null,
        elapsed_ms: first ? Date.now() - first : null,
    });
});

// 复位，方便同一个进程里分段测量
register_route('/plugins/serial-fast/reset', 'POST', function () {
    gaps.length = 0;
    lates.length = 0;
    count = 0;
    last = 0;
    first = 0;
    maxGap = 0;
    return json(200, { ok: true });
});

register_route('/plugins/serial-fast/', 'GET', function () {
    return json(200, { plugin: 'serial-fast', interval: '20ms', fires: count });
});

// 注意：register_cron 只吃 cron 表达式（5/6 段或 `@every`）。
// 「毫秒间隔」要用 every() —— 它内部拼成 `@every 20ms`。
every('t', '20ms', function (p) {
    const now = Date.now();
    if (last > 0) {
        const gap = now - last;
        gaps.push(gap);
        if (gap > maxGap) maxGap = gap;
        // 只留最近 4000 个，长跑不涨内存
        if (gaps.length > 4000) gaps.shift();
    }
    if (!first) first = now;
    last = now;
    if (lates.length < 4000) lates.push(p.lateMs);
    count++;
});

log('serial-fast 已加载（20ms 任务，用于测量串行代价）');
