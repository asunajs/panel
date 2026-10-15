// plugins/js/log-levels.js —— 八档日志 + 多参的活文档。
//
// 这个插件**故意不注册 cron 任务**：它的作用不是干活，而是两件事 ——
//
//   ① 当文档：八档分别长什么样、什么时候该用哪一档，一眼看完。
//   ② 当**加载期回归**：`import { start, success, fail, fatal } from 'host-api'`
//      只要门面漏掉任何一个具名导出，模块实例化就会失败 —— 每次启动都在验，
//      不需要额外的测试脚本。
//
// 想亲手看一遍输出：面板上给 `log-levels` 处理函数建一条任务，
// 或者用「测试运行」直接跑几行。

import {
  log,
  info,
  debug,
  warn,
  error,
  start,
  success,
  fail,
  fatal,
  register_handler,
} from 'host-api';

const PLUGIN = 'log-levels';

// 八档的语义（`log` 是 `info` 的别名，老写法继续可用）：
//
//   start    开始做一件事        —— 「插件已加载」「开始第 N 轮」
//   info     普通进展（默认档）
//   debug    排查用的细节        —— **默认在面板视图里被过滤掉**（勾 debug 才显示）
//   success  一件事干成了        —— 语义着色，过滤级别仍是 info
//   fail     一件事没干成        —— 语义着色，过滤级别仍是 info
//   warn     有情况但能继续
//   error    出错了，这一步废了
//   fatal    整个插件/进程跑不下去了
//
// ⚠ start / success / fail 是**语义着色**，不是严重程度：一次任务「失败了」在显示上
//   是 fail（红色），它的严重程度由随后的 error 决定。把 fail 当 error 级别会让
//   「只看 error」变得没法用（满屏都是）。
start(`${PLUGIN} 已加载`);
info('info  —— 普通进展');
debug('debug —— 排查细节（默认被面板视图过滤掉，勾上 debug 才看得见）');
warn('warn  —— 有情况，但能继续');
error('error —— 出错了，这一步作废');
start('start —— 开始做一件事');
success('success —— 这件事干成了');
fail('fail  —— 这件事没干成（红色，但过滤级别是 info）');
fatal('fatal —— 跑不下去了（最高级别）');

// 多参：与 Node 的 console.log 一样用空格连接。
// ⚠ 老实现只声明了 `function (msg)` —— 第二个以后的实参**被静默丢掉**。
//   静默丢参数最难查：输出看起来「对」，只是少了半句。
log('多参示例：', { phone: '13600136001' }, 3, [1, 2], '收尾');

// console 逐档映射（与 Node 语义一致）：
//   log / info → info      debug → debug      warn → warn      error → error
console.log('console.log → info');
console.info('console.info → info');
console.debug('console.debug → debug（以前这里被错当成 info）');
console.warn('console.warn → warn');
console.error('console.error → error');

register_handler('log-levels', (payload) => {
  start(`${PLUGIN}：第 ${payload.runs} 次执行`);
  log('账号', '13600136001', '第', payload.runs, '次触发');
  success('这次执行干完了');
});
