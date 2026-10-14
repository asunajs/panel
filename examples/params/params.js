// 任务参数夹具（第二十三轮）—— 验证「同一个处理函数挂多条任务、各带各的参数」。
//
// # 为什么需要它
//
// 抢兑要求所有账号**并行**发单：串行会错过那几百毫秒的窗口。而面板只能把
// 「一条任务」派给引擎。所以做法是插件注册 N 条任务（名字不同、处理函数相同、
// `params` 里各带一个账号），面板照常调度，每条任务自己带上自己的参数。
//
// 这个夹具就是那个模型的最小形态：三条带参数的任务 + 一条不带参数的。
// 每条都把自己看到的 `params` 记下来，由路由吐出来给脚本断言。
//
// # 它同时验证什么
//
// - 参数**原样**到达 handler（对象就是对象，不用插件自己 JSON.parse）
// - 三条任务各拿各的，不串味
// - 没给参数的拿到 `null`（而不是 `{}` 或 `undefined`）
// - 重启之后参数还在（走的是 `jobs.params` 那一列）

import { register_cron, register_handler, register_route } from 'host-api';

function json(status, obj) {
    return {
        status: status,
        headers: { 'content-type': 'application/json; charset=utf-8' },
        body: JSON.stringify(obj),
    };
}

// task 名 → 每次触发看到的 params（按触发顺序）
const seen = {};

function record(name, params) {
    if (!seen[name]) seen[name] = [];
    if (seen[name].length < 50) seen[name].push(params);
}

register_route('/plugins/params/stats', 'GET', function () {
    return json(200, { seen });
});

register_route('/plugins/params/reset', 'POST', function () {
    for (const k of Object.keys(seen)) delete seen[k];
    return json(200, { ok: true });
});

// 三条**同一个处理函数**的任务，各带各的账号。名字不同 —— 同一个插件内任务名必须唯一。
for (const account of ['alice', 'bob', 'carol']) {
    register_cron(
        'exchange-' + account,
        '*/1 * * * * *',
        function (p) {
            record(p.name, p.params);
        },
        { params: { account: account, slot: 3 } },
    );
}

// 一条**不给参数**的：应该拿到 null，而不是 {} 或 undefined。
register_cron('no-params', '*/1 * * * * *', function (p) {
    record(p.name, p.params);
});

// 参数是任意 JSON，不限于对象 —— 用一条数组参数的任务把这一点钉住。
register_cron(
    'array-params',
    '*/1 * * * * *',
    function (p) {
        record(p.name, p.params);
    },
    { params: ['x', 1, true] },
);

// 只登记处理函数、不声明调度 —— 让**面板**来建任务（并给它带参数）。
// 这一条验的是「面板建的任务也能带参数」，走的是 jobs.params 那一列。
register_handler('panel-task', function (p) {
    record(p.name, p.params);
});
