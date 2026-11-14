// plugins/js/fs-demo.js —— 在面板内写文件的跨运行时写法（`node:fs` / `node:path`）。
//
// 这个文件同时是**文档**和**加载期回归**：它 import 了 fs-demo 需要的所有名字，
// 任何一处接线断了，插件在加载期就炸 —— 不需要另写一个脚本去验。
//
// ## 同一个写法，两个宿主
//
// ```js
// import fs from 'node:fs';
// import path from 'node:path';
// ```
//
// - **面板外**（`node fs-demo.js`）：Node 原生的 `fs` / `path`。
// - **面板内**：同样是原生模块 —— 面板把子进程的 CWD 设成插件数据目录，不做路径围栏。
//
// ## 「插件自己的数据目录」是什么
//
// 面板给每个插件一个数据根：`<PLUGIN_DATA_DIR>/<插件名>/`，插件进程的 CWD 就是它。
// 所以 `fs.writeFileSync('state.json', …)` 落到的真实位置是
// `<PLUGIN_DATA_DIR>/fs-demo/state.json`：
//
// - 用 `process.cwd()` 能问出来（排查「我到底写哪去了」时很省事）
// - **不是**插件源码所在的目录 —— 那个目录会被面板当静态目录服务出去，
//   往里写的东西（比如 token）会出现在网页上
// - 没有围栏：插件能读写当前用户能读写的一切。只用相对路径是**可移植**建议，
//   不是安全边界（见 docs/plugin-fs.md §23）
//
// ## 跨宿主的写法
//
// 只用相对路径、把 `path` 的结果原样交回 `fs` —— 面板内、面板外、adapters/node
// 都落在各自的当前目录下。不要依赖已删除的 `fs.rootDir()`。

import fs from 'node:fs';
import path from 'node:path';
import { log, register_handler, register_cron } from 'host-api';

const PLUGIN = 'fs-demo';
const STATE = path.join('data', 'state.json');

// 数据目录在哪？
//
// 跨宿主可移植的写法是**只用相对路径**（下面的 STATE 就是这样）：两边都落在
// 自己的"当前目录"下，谁也不用知道对方的规矩。这里把 cwd 打出来只为排查。
const DATA_ROOT = typeof process !== 'undefined' ? process.cwd() : '(未知)';

log(`${PLUGIN}: 已加载，数据根 = ${DATA_ROOT}`);

/// 读上次的状态。**第一次跑时文件还不存在** —— 这就是 `existsSync` 的用处，
/// 不要靠 catch 当流程控制。
function readState() {
    if (!fs.existsSync(STATE)) return { runs: 0, lastAt: null };
    try {
        return JSON.parse(fs.readFileSync(STATE, 'utf8'));
    } catch (e) {
        // 文件坏了（比如上次写到一半被 SIGKILL）：说出来并重新开始，
        // 别让一个坏文件把插件永久卡死在这一行
        log(`${PLUGIN}: 状态文件读不动（${e.message}），重新开始`);
        return { runs: 0, lastAt: null };
    }
}

function writeState(state) {
    fs.mkdirSync(path.dirname(STATE), { recursive: true });
    fs.writeFileSync(STATE, JSON.stringify(state, null, 2));
}

/// 跑一次：累加计数并落盘，然后读回来验证（写没写进去只有读回来才知道）。
export function run() {
    const state = readState();
    state.runs += 1;
    state.lastAt = new Date().toISOString();
    writeState(state);

    const back = JSON.parse(fs.readFileSync(STATE, 'utf8'));
    log(`${PLUGIN}: 第 ${back.runs} 次；文件 ${fs.statSync(STATE).size} 字节；目录里有 ${fs.readdirSync('data').join('、')}`);
}

// 面板上的「新建任务」可以直接挂这个处理函数（`register_handler` 只登记不调度）。
register_handler('fs-demo', run);

// 也可以自己声明一条调度（30 秒一次，够看出状态在累加，又不至于刷屏）。
register_cron('fs-demo-tick', '@every 30s', run);
