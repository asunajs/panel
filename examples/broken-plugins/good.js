// 对照组：一个完全正常的插件。
// 如果 bad-tick.js 的异常能把它带崩，说明宿主没做好隔离 —— 本文件就是用来证明它没崩。
let n = 0;

host.log('good.js 已加载');

function on_tick(payload) {
    n++;
    if (payload.seq % 5 === 0) {
        host.log(`good.js 仍然活着，已 tick ${n} 次`);
    }
}
