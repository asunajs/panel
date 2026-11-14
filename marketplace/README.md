# 官方货架（插件目录）

商店插件（`asunajs/panel-store`，私有仓）默认读的就是这一份：
`https://raw.githubusercontent.com/asunajs/panel/main/marketplace/catalog.json`。
字段全表在 [../docs/store.md](../docs/store.md) §4；这里只讲**怎么加一条**和**为什么现在是空的**。

## 加一条之前：资产必须是 **0.4 形状**

商店装的是「文件」，装完由面板按文件名加载。所以 release 资产 / 单文件必须是：

    <name>.js             ← 顶层，名字就是插件名（mcloud.js / mgcloud-daily.js …）
    <name>.webui/…        ← 可选，界面目录，和 .js 并列
    mcloud.panel.json     ← 可选，清单

**不能**是 0.3 时代的形状（`js/mcloud.js` + `mcloud/index.html`）：那样装进去只会得到
`plugins/js/mcloud.js` 这种文件，加载器按 `plugins/*.js` 扫，一个都不会被加载。

实测（2026-11-14，本机 `unzip -l`）：

```text
mcloud-plugin/dist/mcloud-plugin-0.1.1.zip   → js/mcloud.js、js/mcloud-daily.js、mcloud/index.html …
mgmusic-plugin/dist/mgmusic-plugin-0.3.0.zip → js/mgmusic.js、mgmusic/{boot.js,index.html,style.css}
```

两条都还是 0.3 形状，所以**没有**收进官方目录 —— 收了就是一个「点了装、装完没反应」的陷阱。
等对应插件仓按 R7 发出 0.4 形状的资产（顶层 `<name>.js` + `<name>.webui/`），把条目加进 `catalog.json` 即可。

## 收一条长这样

```json
{
  "id": "mcloud",
  "name": "mcloud 系列",
  "summary": "移动云盘签到、兑换、直播",
  "tags": ["云盘", "签到"],
  "provides": ["mcloud", "mcloud-daily", "mcloud-exchange", "mcloud-live"],
  "type": "release",
  "repo": "asunajs/mcloud-plugin",
  "min_panel": "0.4.0"
}
```

单文件（不需要 release）：

```json
{
  "id": "some-single",
  "name": "某个单文件插件",
  "provides": ["some-single"],
  "type": "file",
  "repo": "asunajs/panel-single-plugins",
  "path": "some-single/some-single.js",
  "ref": "main"
}
```

## 三条硬规矩

1. `provides` 要写**全**（一个包可能带好几个插件）—— 商店靠它判断「已装」；
2. 能给 `sha256` 就给：写了就必须对上，否则一个字节都不写；
3. `type: file` 只认 `.js`。面板不带 TypeScript 转译（老面板内置 OXC，0.4 明确不做），
   `panel-single-plugins/quark/quark.ts` 这种要先在上游仓里构建成 `.js`。

改完这份 JSON 不用发面板版本：商店点「刷新目录」就重取（进程内缓存 15 分钟）。
