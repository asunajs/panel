// 侧边栏入口夹具（第二十三轮）—— 验证插件能自己声明一个面板入口。
//
// # 为什么需要它
//
// 面板推导得出「这个插件有没有配置页」（`{name}/index.html` 存在），但推导不出
// 它该**叫什么**、用什么图标。而这两件事恰恰决定了用户能不能一眼认出
// 「这是我的日常任务」—— 面板替它起名叫 `navdemo` 的话，用户看到的是一串
// 插件文件名，那正是要解决的问题。
//
// 所以：名字和图标由插件给，页面位置用面板已有的约定（不给 path 就落在
// `{name}/index.html`）。
//
// 页面在 `examples/nav/navdemo/index.html`。

import { register_nav, register_route } from 'host-api';

// 不给 path —— 走面板的默认约定 `{name}/index.html`
register_nav({ label: '日常任务', icon: '🗓' });

// 页面要用数据就自己开接口（同源，iframe 里直接 fetch 得到）
register_route('/plugins/navdemo/api/summary', 'GET', function () {
    return {
        status: 200,
        headers: { 'content-type': 'application/json; charset=utf-8' },
        body: JSON.stringify({ accounts: 3, next: '08:00:00' }),
    };
});
