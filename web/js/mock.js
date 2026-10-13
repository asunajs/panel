/*
  web/js/mock.js —— 任务列表演示数据。

  这一份是**示例数据**，存在的意义是让界面在没有后端的情况下也能跑起来
  （直接双击 web/index.html 打开也能看到完整 UI）。

  接真实后端时替换成 `api.js` 的 `listTasks()` 即可 —— 字段名保持一致
  （name / lang / cron / lastRun / status），视图层不用改。
  真实实现里 `status` 的取值是 idle | running | success | failed。
*/

export const TASKS = [
  {
    name: 'collect-metrics',
    lang: 'js',
    cron: '@every 30s',
    lastRun: '2026-10-13 09:12:04',
    status: 'success',
  },
  {
    name: 'flush-buffer',
    lang: 'py',
    cron: '*/5 * * * *',
    lastRun: '2026-10-13 09:10:00',
    status: 'running',
  },
  {
    name: 'rotate-log',
    lang: 'js',
    cron: '0 3 * * *',
    lastRun: '2026-10-13 03:00:00',
    status: 'idle',
  },
  {
    name: 'sync-upstream',
    lang: 'py',
    cron: '@every 5m',
    lastRun: '2026-10-13 09:05:31',
    status: 'failed',
  },
  {
    name: 'healthcheck',
    lang: 'js',
    cron: '*/1 * * * *',
    lastRun: '2026-10-13 09:13:00',
    status: 'success',
  },
];

/** 编辑器里首次打开时给一段能直接跑的示例代码。 */
export const SAMPLE_CODE = {
  js: `import { log, sqlite_query } from 'host-api';

// 面板任务的处理函数由面板挂调度，这里只写"做什么"
export function run(input) {
  const rows = sqlite_query('SELECT COUNT(*) AS n FROM tasks', []);
  log('当前任务数：' + rows[0].n);
  if (input) log('输入：' + input);
  return { ok: true, at: Date.now() };
}
`,
  py: `from host_api import log, sqlite_query


def run(payload):
    rows = sqlite_query("SELECT COUNT(*) AS n FROM tasks", [])
    log("当前任务数：" + str(rows[0]["n"]))
    return {"ok": True}
`,
};
