// examples/hostile-plugins/alive.js
//
// 对照组：一个完全正常的任务。
//
// 如果执行预算生效，即使同目录下的 runaway.js 每次触发都死循环，
// 这个任务的 runs 也应该持续增长 —— 说明引擎没有被单个插件拖死。
host.every('alive', '1s', function (p) {
    if (p.runs % 3 === 0) {
        host.log(`alive 任务仍在运行（第 ${p.runs} 次）`);
    }
});
