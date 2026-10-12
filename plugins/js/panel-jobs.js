// plugins/js/panel-jobs.js —— 第九轮：把「处理函数」和「调度」拆开
//
// 两个注册入口的差别就是任务定义的来源（jobs.source）：
//
//   register_cron(name, expr, fn)   一次声明「处理函数 + 调度」  → source = 'plugin'
//                                   面板上只能暂停，不能删（删了会被 sync_jobs 装回来）
//   register_handler(name, fn)      只登记处理函数，不声明调度  → 由面板创建任务
//                                   面板上新建任务时填的名字就是这里注册的 name
//
// 为什么要有 register_handler：面板上的「新建任务」只能提供调度，不能提供代码 ——
// 代码永远在插件里。所以插件得先把「可以被调度调用的处理函数」登记出来，
// 面板再从里面挑一个挂上表达式。这样「插件的能力只能来自插件代码」这条线没有破。
//
// 本地（Node 适配器）没有面板，等价做法是运行器的 --job：
//   cd adapters/node && node bin/run.mjs ../../plugins/js/panel-jobs.js \
//       --job echo=@every 2s --duration 5

import { log, register_cron, register_handler } from 'host-api';

log('panel-jobs: 已加载（代码声明 1 个任务 + 登记 3 个处理函数）');

// ① 代码声明的任务：source = plugin
register_cron('code-side', '@every 5s', function (p) {
    log(`[代码声明] code-side 第 ${p.runs} 次触发，调度延迟 ${p.lateMs}ms`);
});

// ② 面板任务的三个处理函数 —— 都在面板上「新建任务」时按名字挑
register_handler('echo', function (p) {
    log(`[面板任务] echo 第 ${p.runs} 次触发（${p.expr}），调度延迟 ${p.lateMs}ms`);
});

// handler 复用：同一个处理函数可以被面板上的多条任务挂不同的表达式
register_handler('report', function (p) {
    log(`[面板任务] report 第 ${p.runs} 次触发（${p.expr}），firedAt=${p.firedAt}`);
});

// ③ 故意失败的处理函数：用来看面板上的「执行出错」与熔断
//    （面板任务一样受熔断保护，连续失败到阈值就指数退避冷却）
register_handler('always-fails', function () {
    throw new Error('这个处理函数故意失败（面板任务的熔断演示）');
});
