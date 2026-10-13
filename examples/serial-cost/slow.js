// 「串行代价」测量夹具 —— 慢任务（那个「捣乱的人」）。
//
// 每 1 秒触发一次，每次**忙等 300ms**（CPU 密集，占满引擎线程）。
//
// 为什么用忙等而不是 http_fetch：忙等是确定性的，不依赖网络；而且两者对引擎的
// 影响完全一样 —— 引擎线程被同步调用占住，期间谁也跑不了。
// 用 sleep 反而测不出问题（插件里没有 sleep，真睡也是占着线程）。
//
// 300ms 是个刻意的数字：它比快任务的 20ms 大一个数量级，但又远小于 2000ms 的
// 执行预算 —— 也就是「合规的、不会被打断的」慢任务，正是最难处理的那类。

import { log } from 'host-api';

const BLOCK_MS = 300;

let runs = 0;

host.every('block', '1s', function () {
    const t = Date.now();
    while (Date.now() - t < BLOCK_MS) {
        // 故意空转：占住引擎线程
    }
    runs++;
    log(`serial-slow 第 ${runs} 次占住引擎 ${BLOCK_MS}ms`);
});
