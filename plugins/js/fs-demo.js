// plugins/js/fs-demo.js —— 在面板内写文件的跨运行时写法（`node:fs` / `node:path`）。
//
// 这个文件同时是**文档**和**加载期回归**：它 import 了 fs-demo 需要的所有名字，
// 任何一处接线断了，插件在加载期就炸 —— 不需要另写一个脚本去验。
//
// ## 两份运行时，同一个写法
//
// ```js
// import fs from 'node:fs';
// import path from 'node:path';
// ```
//
// - **面板外**（`node fs-demo.js`）：用 Node 原生的 `fs` / `path`，不经过本面板。
// - **面板内**：由宿主提供同名模块（`src/jshost.rs` 的 NODE_FS_BODY / NODE_PATH_BODY），
//   **限定在插件自己的数据目录里**。
//
// ## 「插件自己的数据目录」是什么
//
// 面板给每个插件一个数据根：`<PLUGIN_DATA_DIR>/<插件名>/`，而它在插件眼里就是 `/`。
// 也就是说 `fs.writeFileSync('state.json', …)` 落到的真实位置是
// `<PLUGIN_DATA_DIR>/fs-demo/state.json`：
//
// - 拿 `fs.rootDir()` 能问出来（排查「我到底写哪去了」时很省事）
// - **不是**插件源码所在的目录 —— 那个目录会被面板当静态目录服务出去，
//   往里写的东西（比如 token）会出现在网页上
// - 越界（`../`、别人的目录、系统文件）一律 `EACCES`，不是"夹回根目录"：
//   静默改写会让同一份代码在面板内外表现不同，那种差异最难查
//
// ## 与 Node 的差异（就这几条，别踩）
//
// | | Node 原生 | 面板内 |
// | --- | --- | --- |
// | `Buffer` | 有 | **没有**；不给 encoding 时给你 `Uint8Array` |
// | 异步 API | `fs.promises` | **没有**（宿主是"一次调用推进一步"，没有真事件循环） |
// | 单次读写上限 | 无 | 8 MiB（返回值要穿过 JSON 桥） |
// | `path.resolve()` 的当前目录 | 进程 cwd | 数据根（结果形如 `/a/b`） |
//
// 只要把 `path` 的结果**原样交回 `fs`**（正常写法都是这样），两边行为一致。

import fs from 'node:fs';
import path from 'node:path';
import { log, register_handler, register_cron } from 'host-api';

const PLUGIN = 'fs-demo';
const STATE = path.join('data', 'state.json');

// 数据根在哪？
//
// ⚠ `fs.rootDir()` 是**面板侧的扩展**，Node 的原生 fs 没有这个函数 ——
//   直接调它会让这个文件在 `node plugin.js` 下**加载期就炸**（实测踩到）。
//   所以跨运行时的插件不要依赖它，只把它当排查用的额外信息：
//     · 面板里 → 那个数据根的真实路径（形如 …/plugin-data/fs-demo）
//     · Node 里 → 相对路径的基准是进程的 cwd，用 process.cwd() 问
//   真正可移植的写法是**只用相对路径**（下面的 STATE 就是这样）：两边都落在
//   自己的"当前目录"下，谁也不用知道对方的规矩。
const DATA_ROOT =
    typeof fs.rootDir === 'function'
        ? fs.rootDir()
        : (typeof process !== 'undefined' ? process.cwd() : '(未知)');

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
