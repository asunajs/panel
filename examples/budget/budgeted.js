// 执行预算夹具（①-6）—— 证明「一个函数从头跑到尾」的长任务不会被误杀。
//
// # 为什么需要它
//
// 面板原本只有一个**全局**执行预算（`EXEC_BUDGET_MS`，默认 2000ms），超了直接
// 报「疑似死循环」掐断。那正是当年被迫把 mcloud 写成 25 条「一次一步」任务的根因。
//
// 现在任务可以自己声明需要多久。这个夹具就是那个能力的最小形态：
// 一个忙等 4 秒的任务（超过默认 2 秒一倍），声明 8 秒预算 ⇒ 应当正常跑完。
//
// 同时声明 `worker_hint('exclusive')`：长预算**必须**配独占线程 ——
// 否则它会把同一条线程上其它插件的任务一起拖住。

import { log, register_cron, register_route, worker_hint } from 'host-api';

worker_hint('exclusive');

/** 忙等 ms 毫秒 —— 模拟「一个函数从头跑到尾」的长任务（多次上游调用）。 */
function spin(ms) {
    const t = Date.now();
    while (Date.now() - t < ms) {
        // 故意空转
    }
}

const json = (status, obj) => ({
    status,
    headers: { 'content-type': 'application/json; charset=utf-8' },
    body: JSON.stringify(obj),
});

register_cron(
    'long',
    '@every 5s',
    function () {
        spin(4000);
        log('long：跑完了（4 秒，预算 8 秒）');
    },
    { budget_ms: 8000 },
);

// 路由也要能声明预算 —— 兑换那条链的手动触发走的是 HTTP 路由，同样会超过 2 秒
register_route(
    '/plugins/budgeted/long',
    'GET',
    function () {
        spin(4000);
        return json(200, { ok: true, ran: 4000 });
    },
    { budget_ms: 8000 },
);

register_route('/plugins/budgeted/quick', 'GET', function () {
    return json(200, { ok: true, ran: 0 });
});
