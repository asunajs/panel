// 对照组（①-6）：**没有**声明预算的 4 秒任务，应当被全局预算（默认 2 秒）掐断。
//
// 这一条是「保护还在」的证据 —— 光证明长任务能跑完不够，还得证明
// 没声明预算的跑飞任务依然会被拦下。否则这个能力就等于把超时保护取消了。

import { log, register_cron } from 'host-api';

function spin(ms) {
    const t = Date.now();
    while (Date.now() - t < ms) {
        // 故意空转
    }
}

register_cron('runaway', '@every 5s', function () {
    spin(4000);
    log('runaway：不该跑到这里（应当被预算掐断）');
});
