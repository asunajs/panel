// 演示脚本：第三方程序模型（2026-11-14）—— 面板只保管、定时、收 stdout/stderr 和退出码。
//
// 这是一次真实的外部进程（node demo-script.js）：没有 host-api，没有引擎。
// stdout 进运行记录（info），stderr 标 error；退出码非 0 = 这一次运行失败。
// 脚本自己的数据目录在环境变量 PANEL_SCRIPT_DATA（可以往里写状态文件）。

console.log('demo 跑了一次：', new Date().toISOString());
console.log('数据目录：', process.env.PANEL_SCRIPT_DATA ?? '(未设置)');
if (process.env.DEMO_FAIL === '1') {
  console.error('DEMO_FAIL=1，按失败收尾');
  process.exit(3);
}
