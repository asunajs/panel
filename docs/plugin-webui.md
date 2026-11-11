# 插件 WebUI

插件可以带自己的界面。**不做模板、不做组件库**：插件想怎么画就怎么画，
宿主只负责把文件送出去、把请求转进来。

## 1. 放哪

```
plugins/
  hello.js              # 插件代码（已有）
  webui-demo.js         # 插件代码
  webui-demo.webui/     # 这个插件的界面（目录名 = <插件名>.webui）
    index.html          # 必需：/plugins/webui-demo/ 返回它
    style.css
    app.js
```

目录名是 `<插件名>.webui`，和 `<插件名>.js` 并排。这样插件删了界面跟着删，
也不会和「插件名」抢命名空间（插件名里不允许出现两个连续的点，见 `safe_name`）。

## 2. 路由（已定）

| 路由 | 行为 |
|---|---|
| `GET /plugins/<name>/` | 读 `index.html`；没有就是 404 |
| `GET /plugins/<name>/<rel>` | 静态文件；MIME 按扩展名（html/js/css/json/svg/png/webp/ico/woff2/wasm/txt），`cache-control: no-store`，`nosniff` |
| `POST /plugins/<name>/api/<handler>` | 交给插件注册的 handler，返回它的**返回值** |

路径安全：只允许普通路径段。`..`、绝对路径、盘符一律 400 —— 界面的路径来自 URL，
不能拿它直接拼文件名。

## 3. 动态请求长什么样

插件侧：

```js
import host from 'panel:host';

await host.register_handler('hello', async (req) => {
  // req = { body, query, path, handler }
  const who = (req.body && req.body.who) || "世界";
  return { msg: '你好，' + who };
});
```

浏览器侧：

```js
const r = await fetch('./api/hello', {
  method: 'POST',
  headers: { 'content-type': 'application/json' },
  body: JSON.stringify({ who: 'WebUI' }),
});
const { ok, value } = await r.json();   // value 就是 handler 的返回值
```

请求体（JSON）进 `evt.body`，query 字符串进 `evt.query`。handler 抛异常 → 502 + 纯文本错误
（错误来自引擎，可能带栈，所以不包装成 JSON 骗人）。

## 4. 为什么复用 handler 机制

插件已经在用 `register_handler` 给 cron 派发用了，WebUI 的请求就是**另一种触发**。
`Plugin::call_json` 和 `Plugin::dispatch` 是同一条代码路径，只多把返回值带出来 ——
预算死线、中断、`catch_engine_panic` 边界、日志归属全都直接继承，不需要第二套机制。

代价是 WebUI 请求和 cron 任务**共用同一个 busy 锁**：插件有任务在跑时，界面请求会拿到
502「正忙」。这是对的 —— 一个 Context 不能并发派发（`__result` 是全局的）。

## 5. 鉴权与嵌入（已定）

- `/plugins/*` 和面板其它路由**同一个令牌门**（`require_token`）。
- 新标签页里的页面加不了自定义请求头，所以**只对 `/plugins/*`** 额外接受 `?t=<token>`；
  面板自己的页面永远走 `x-panel-token` 头。代价是令牌会进浏览器历史 —— 本机自用可接受，
  共享机器应该干脆不启令牌（那时只允许绑回环）。
- `x-frame-options: DENY` + `frame-ancestors 'none'` 对所有响应生效，所以插件界面
  **不能被任何页面嵌**（包括面板自己）。入口就是「新标签打开」。
  想内嵌就得放宽这两条，收益只是省一个标签页，暂不做。

## 6. 读数（真面板 8787）

| 项 | 结果 |
|---|---|
| `GET /plugins/webui-demo/` | `200 text/html; charset=utf-8` |
| `GET /plugins/webui-demo/style.css` | `200 text/css; charset=utf-8` |
| `GET /plugins/webui-demo/app.js` | `200 text/javascript; charset=utf-8` |
| `POST /plugins/webui-demo/api/hello` | `{"ok":true,"value":{"msg":"你好，WebUI","at":…,"query":""}}` |
| `GET /plugins/webui-demo/../../data/config.json` | `400`（路径穿不出去） |
| `GET /plugins/webui-demo/nope.js` | `404` |
| `POST /plugins/nobody/api/hello` | `502 插件未加载: nobody` |
| `GET /api/plugins/files` | 每个文件带 `webui: true/false` |

## 7. 不做

- **iframe 内嵌**：见 §5，要放宽 XFO/CSP，收益不值。
- **上传 zip / 多文件包**：单文件插件用「新建 + 编辑源码」已经够，界面文件直接放目录里。
- **插件页面之间的路由表**：静态文件 + 一个 `api/<handler>` 已经能表达所有需求，
  再做一层路由分发就是把 Express 抄一遍。