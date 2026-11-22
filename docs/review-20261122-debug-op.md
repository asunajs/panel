# 变更 Review：`host.debug` 第四级日志 op（2026-11-22）

review 基准：工作树未提交改动（基点 `756d4b7`），涉及 `Cargo.toml` / `Cargo.lock` / `src/hostapi.rs`；
相邻参照已提交的 `1608831`（panel-host.d.ts 补 debug 类型）与 `756d4b7`（装插件过版本闸门）。
原则同 [review-20261115.md](review-20261115.md)——**每条发现带证据（文件:行号）**，按严重度给建议。

改动本身很小：版本 0.8.1 → 0.8.2，`OP_NAMES` 32 → 33，`ops()` 加 `op_debug`。
但 **op 实际到不了插件手里**——见 F1。

---

## 发现

| # | 发现 | 证据 | 严重度 | 建议 |
|---|---|---|---|---|
| F1 | **`debug` 没进 WRAP，插件侧拿到 `undefined`，op 全链路是断的** | 见下节展开 | **高（发版必须修）** | WRAP 补一行 `debug: raw.debug,` |
| F2 | 注释宣称「debug 默认不落库」，实际落库——注释与实现相反 | `src/hostapi.rs:327` vs `src/log/mod.rs:184-205` | 中 | 注释改成与实现一致（落库；debug 单独一档可查） |
| F3 | `docs/host-api.md` 没跟上：日志 op 表仍是四档，而代码注释引用它当依据 | `docs/host-api.md:28`、`:686` | 中 | 表格补 debug；probe 覆盖了它再更新 `:686` |
| F4 | `CHANGELOG.md` 还没有 0.8.2 段，版本号已先行 bump | `CHANGELOG.md` 最新段是 0.8.1 | 低 | 发版前补：插件 `host.debug` 上线一条；`docs/verification.md` 按惯例补发版读数段 |

---

## F1 展开：WRAP 漏了 debug（本轮唯一的功能性 bug）

导出路径是三段式的：

1. `ops()` 造 raw 对象——`debug` 在（`src/hostapi.rs:329`）；
2. `WRAP` 把 raw 包成 host 对象——**返回的对象字面量只列了 `log / info / warn / error`，没有 `debug`**（`src/hostapi.rs:123`）；
3. `HostModule::evaluate` 对每个 `OP_NAMES` 条目从 **wrap 后的 host** 取值导出（`src/hostapi.rs:313-315`）。

第 3 步对缺属性不报错：rquickjs-core 0.14.0 的 `Object::get` 就是 `JS_GetProperty`
（`~/.cargo/registry/.../rquickjs-core-0.14.0/src/value/object.rs:41`），QuickJS 对缺失属性返回
`undefined`、无异常。所以模块**静默导出 `debug: undefined`**：

- `host.debug("...")` → 运行期 `TypeError: host.debug is not a function`
- 具名导出 `import { debug } from 'panel:host'` 同样是 `undefined`

而 TS 侧已经先行承诺了这个函数（`tools/plugin-shared/panel-host.d.ts:120` `debug(msg, extra?)`、
`:148` 具名导出、`:194` 全局 `log.debug`）——类型说有、运行时没有。

**为什么测试没拦住**：probe 只打 log/info/warn/error（`docs/host-api.md:686`
「四级日志（log/info/warn/error）都进库」），仓库里没有任何测试断言「OP_NAMES 的每一项
导出后是 function」。这类枚举漏一项的错，只有枚举型测试能拦。

**修复**：`WRAP` 的日志行补上 `debug`：

```js
log: raw.log, debug: raw.debug, info: raw.info, warn: raw.warn, error: raw.error,
```

**建议顺手加一条引擎测试**（engine_tests）：遍历 `OP_NAMES` 断言
`typeof host[name] === 'function'`（或 probe 加一句 `host.debug` 并断言库里 `level=debug`）。
一行修复 + 一条枚举测试，这个坑就封死了。

---

## F2 展开：「默认不落库」与实现相反

`src/hostapi.rs:327` 注释写「debug：默认不落库（见 docs/host-api.md 四级日志）」。实际：

- `op_debug` → `emit`（`src/hostapi.rs:384`）→ `LogHub::emit` → `emit_src`
  （`src/log/mod.rs:184-205`）：**无条件 SSE 广播 + 入库**，全链路没有按 level 过滤的点；
- `retention.rs` 不分级别清理；
- 消费侧全部按「落库可查」设计：API `level` 白名单认 debug（`docs/api.md:47`，
  `src/web_tests.rs:3111` 有测试）、界面有 debug 分段按钮（`web/index.html:253`）——
  这些都是 0.8.0 给面板侧 debug 日志（空闲卸载那批）铺的，插件侧理应保持同一语义。

所以这条注释两处失实：行为描述错（落库）、文档引用错（`docs/host-api.md` 里没有
「debug 不落库」的四级日志说明）。建议把注释改成与实现一致，例如：

```rust
// debug：第五个日志 op？不，第四级——面板侧 0.8.0 起就有（空闲卸载走它）。
// 落库、可查，界面单独 debug 档；插件用它记「正常但没人想逐条看」的内部流程，
// 比如「票不认，换一张重试」——那不是用户的问题，不该占 info。
```

若真想要「不落库」语义，那是 `LogHub` 的行为变更，需要另行设计——不建议：
面板侧 debug 已落库，插件侧走同一管道，语义分裂反而难排查。

---

## 验证过的部分（没有问题）

- **`cargo check` 通过**（exit 0）；`cargo test` 全量结果见文末补记。
- **版本闸门侧无需改动**：`panelversion::current()` 唯一真源是 `CARGO_PKG_VERSION`
  （`src/panelversion.rs:26-29`），Cargo.toml bump 自动带到 `panel_version()` /
  `panel_at_least()` / manifest 比对；`min_panel` 是插件清单声明、加载期比对
  （`src/manifest.rs:540-544`），面板不扫插件源码找 op，所以 debug 不需要闸门登记。
- **消费侧 0.8.0 已就位**：API 白名单（含测试）、UI debug 档、`docs/api.md` 的
  `level` 语义都齐——本轮只需要把 op 接上。
- **TS 类型与本轮 Rust 侧对应**：签名 `(msg, extra?)` 与 log/info 一致（extra 面板侧
  忽略是既有约定，d.ts 注释里写明了），具名导出与 default 两套都声明了。
- **数量对账**：`OP_NAMES` 33 项 = `ops()` 33 项（除 WRAP 缺 debug 外，声明/装配一致）。

---

## 结论

改动意图清楚、消费侧早已备好，但 **F1 让 op 在插件侧完全不可用**——当前状态不能发 0.8.2。
一行修复（WRAP 补 debug）+ 注释/文档三处跟进（F2/F3/F4）后即可收口。

---

## 补记

- `cargo test`（修复前）：**546 全绿**（0 failed / 0 ignored，4.94s）——全量套件对 WRAP 缺
  debug 无感，反证了 F1 的「测试盲区」：现有 546 条里没有一条覆盖「OP_NAMES 每项导出后可调用」。

### 修复落点（同日已落，工作树）

- **F1**：WRAP 补 `debug: raw.debug,`（`src/hostapi.rs:123`）；
  并新增枚举测试 `every_host_op_is_a_function_on_both_export_paths`
  （`src/engine_tests.rs`）——清单从 `OP_NAMES` 生成（已改 `pub(crate)`），default 与具名
  两条路都查。**回归验证**：临时撤掉 WRAP 那行重跑，测试立刻 FAILED；恢复后
  `cargo test` **547 全绿**。这个坑封死了。
- **F2**：注释改为与实现一致（照常落库、可查，指向 docs/api.md 的 level 白名单与界面 debug 档）。
- **F3**：`docs/host-api.md` 最小集表补 `debug`（注明 0.8.2 起与语义）；`:686`/`:754` 的
  probe 记录行没动（那是当时发版验证的历史读数，probe 本身还没打 debug）。
- **F4 未动**：CHANGELOG 的 0.8.2 段按惯例留到发版时写（届时记「插件 `host.debug` 上线」）。
