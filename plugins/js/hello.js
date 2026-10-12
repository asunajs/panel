// plugins/hello.js —— 最小插件示例
//
// 宿主在加载本文件之前，已经往这个插件的全局作用域注入了：
//   host.log(msg) / host.warn(msg) / host.error(msg)   → Rust 侧 __host_log_raw → 控制台 + 面板
//   console.log / warn / error / info                   → 同样进宿主日志
//
// 每个插件跑在**自己的 QuickJS Context** 里，所以这里的顶层 const/let
// 不会和其他插件重名冲突（counter.js 里也有同名的 PLUGIN_NAME）。

const PLUGIN_NAME = 'hello'; // 顶层 const 只存在于本插件 Context
let tickCount = 0;

host.log('hello.js 已加载，等待 on_tick');

// 宿主每秒调用一次，参数是 JSON 可序列化的对象 { seq, ts }
function on_tick(payload) {
    tickCount++;

    // 每 5 秒打一条，避免刷屏
    if (tickCount % 5 === 0) {
        host.log(`第 ${tickCount} 次 tick | 宿主序号 seq=${payload.seq} | 时间 ${payload.ts}`);
    }

    // 验证「任意 JS 值 → Rust 侧字符串」的格式化链路
    if (payload.seq === 3) {
        host.log('对象也能直接打：', { plugin: PLUGIN_NAME, tickCount, nested: { ok: true } });
        host.warn('这是一条 warn 级别的日志');
        console.log('console.log 同样会被转发到宿主');
    }

    // 返回值会被宿主忽略（宿主用 call::<_, ()>），返回什么都行
    return { ok: true, plugin: PLUGIN_NAME };
}
