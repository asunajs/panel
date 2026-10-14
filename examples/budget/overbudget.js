// 第三种情况（①-6）：声明了长预算，但**没有**声明 `worker_hint('exclusive')`。
//
// 这一条是「加载期警告」的判据。为什么值得单独一个夹具：
// 预算原本存在的理由就是「一个跑飞的插件不能把**同一条线程上的**别的任务拖住」。
// 声明长预算却不独占线程，等于把这个保护绕开了 —— 而症状出现在**别的插件**身上
// （它们的任务忽然变慢），没人会想到是刚装的那个插件。
//
// 所以加载期必须说出来。不说，这个错误就只能靠实机排查。

import { register_cron } from 'host-api';

function spin(ms) {
    const t = Date.now();
    while (Date.now() - t < ms) {
        // 故意空转
    }
}

register_cron(
    'long',
    '@every 30s',
    function () {
        spin(3000);
    },
    { budget_ms: 8000 },
);
