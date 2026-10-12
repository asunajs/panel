// plugins/scheduler-demo.js —— 第二轮：cron / 毫秒级调度示例
//
// 宿主提供两个注册入口：
//   host.cron(name, expr, handler)      expr 是 5 段或 6 段 cron
//   host.every(name, interval, handler) interval 形如 '500ms' / '2s' / '1m'
//
// handler 收到一个 payload：
//   { name, plugin, expr, runs, scheduledAt, firedAt, lateMs }
//     runs        本次是第几次触发（从 1 开始）
//     scheduledAt 调度器原定的触发时刻
//     firedAt     实际触发时刻
//     lateMs      实际比原定晚了多少毫秒 —— 用来观察调度精度
//
// 任务名在「同一插件内」必须唯一，重名会让插件加载直接失败（不会静默覆盖）。

let fastCount = 0;

// ① 毫秒级：每 500ms 一次
host.every('fast-500ms', '500ms', function (p) {
    fastCount++;
    // 每 4 次打一条（约 2 秒一条），避免刷屏
    if (fastCount % 4 === 0) {
        host.log(`[500ms] 第 ${fastCount} 次 | 调度延迟 ${p.lateMs}ms | ${p.firedAt}`);
    }
});

// ② 秒级 cron：每 3 秒（6 段写法）
host.cron('every-3s', '*/3 * * * * *', function (p) {
    host.log(`[cron */3s] 第 ${p.runs} 次触发，延迟 ${p.lateMs}ms`);
});

// ③ 小时内的固定点：每小时的 0 分 30 秒
host.cron('hourly-30s', '30 0 * * * *', function (p) {
    host.log(`整点后 30 秒的定时任务触发（第 ${p.runs} 次）`);
});

// ④ 工作日 09:30 —— 5 段写法 + 英文星期名（永不触发也没关系，正好验证「待触发」状态）
host.cron('weekday-morning', '30 9 * * MON-FRI', function (p) {
    host.log(`工作日早间任务触发（第 ${p.runs} 次）`);
});

// ⑤ 故意隔次抛异常：验证「单个任务炸掉不影响其他任务」，
//    同时验证错误会落到面板的「执行出错」状态和 jobs 表的 last_error 字段。
let boomCount = 0;
host.every('boom', '4s', function () {
    boomCount++;
    if (boomCount % 2 === 1) {
        throw new Error('这个任务故意失败（第 ' + boomCount + ' 次）');
    }
    host.log(`boom 任务第 ${boomCount} 次正常返回`);
});
