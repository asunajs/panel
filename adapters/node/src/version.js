// Node 版本闸门（issue #6）。
//
// 为什么卡在 24 而不是 22.5：`node:sqlite` 在 22.x 是实验性的，实测（22.11.0）
// `conn.prepare(sql).run(...args)` 直接报 `column index out of range`，6 个用例挂；
// 而且子进程还得额外传 `--experimental-sqlite`。24.22.1 上裸跑全绿。
//
// 与其让用户在 22 上撞一堆看不懂的错，不如在**加载时**说清楚要升到哪个版本。
// 注意：本文件必须被 `index.js` 的**第一个** import 引入 —— ES 模块按 import 顺序求值，
// 这样它才能在 `sqlite.js`（静态 import `node:sqlite`）之前跑起来。

const major = Number(String(process.versions.node).split('.')[0]);

if (!Number.isFinite(major) || major < 24) {
  throw new Error(
    `@plugin-panel/host-api 需要 Node 24+（当前 ${process.versions.node}）。` +
      'Node 22 的 node:sqlite 还是实验性的：绑定参数会报 `column index out of range`，' +
      '子进程还需要额外传 --experimental-sqlite。请升级到 Node 24 LTS。'
  );
}

export const NODE_MAJOR = major;