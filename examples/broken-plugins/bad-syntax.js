// 语法错误：加载阶段就会失败，宿主应该只记 error，不影响其他插件。
function on_tick(payload) {
    // 下面这行缺右括号
    host.log('永远到不了这里'
