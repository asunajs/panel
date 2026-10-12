// on_tick 里抛异常：加载成功，但每次调用都炸。
// 宿主应该捕获异常、记 error 日志，并**继续**给其他插件调度。
function on_tick(payload) {
    if (payload.seq % 3 === 0) {
        // 故意引用一个不存在的函数
        this_function_does_not_exist();
    }
    return 'ok';
}
