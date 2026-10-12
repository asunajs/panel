// 没有 on_tick：正常加载，但宿主不应该给它调度。
// 面板上应该显示「已加载 / 无 on_tick」，且 tick 次数恒为 0。
host.log('noop.js 加载完成，但我没有定义 on_tick');
