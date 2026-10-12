// adapters/node/examples/hello.js —— 最小插件：只用宿主全局函数（不 import）。
// 与仓库 plugins/hello.js 的写法一致，用来验证「QuickJS 宿主里的脚本原样跑在 Node 上」。

host.log('hello from a global-style plugin');

host.every('tick-500ms', '500ms', function (p) {
  host.log(`tick 第 ${p.runs} 次，延迟 ${p.lateMs}ms`);
});

host.cron('every-3s', '*/3 * * * * *', function (p) {
  host.log(`cron 第 ${p.runs} 次，expr=${p.expr}，plugin=${p.plugin}`);
});
