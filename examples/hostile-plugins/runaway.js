// examples/hostile-plugins/runaway.js
//
// 故意写一个会死循环的任务，用来验证宿主**有没有**执行预算机制。
// 这个文件不在默认 plugins/ 目录里，需要显式指定 PLUGINS_DIR 才会加载。
//
// 预期（如果宿主没有防护）：引擎线程被永久占住 —— HTTP 面板还在，但所有任务停止触发。
host.every('runaway', '1s', function () {
    while (true) {
        // 空转，永不返回
    }
});
