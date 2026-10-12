// plugins/counter.js —— 第二个插件。
//
// 它的存在是为了证明一件事：多个插件各有独立 Context，
// 这里的 `const PLUGIN_NAME` 和 hello.js 里的**同名**，两者互不影响。
// 如果所有插件共用同一个 Context，下面这行会直接抛
// `SyntaxError: redeclaration of 'PLUGIN_NAME'`。

const PLUGIN_NAME = 'counter';
const STATE = { total: 0, lastSeq: 0, startedAt: null };

host.log('counter.js 已加载');

function on_tick(payload) {
    STATE.total += 1;
    STATE.lastSeq = payload.seq;
    if (STATE.startedAt === null) {
        STATE.startedAt = payload.ts;
    }

    if (payload.seq === 2) {
        host.log('插件内部状态:', STATE);
    }
    if (payload.seq % 10 === 0) {
        host.log(`已运行 ${payload.seq} 秒 | 本插件累计 ${STATE.total} 次 tick`);
    }
}
