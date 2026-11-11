import host from "panel:host";

// 每天 9 点跑：拉一次数据，写进 sqlite。
await host.info("开始同步");
const cfg = await host.config();
await host.sqlite_exec("CREATE TABLE IF NOT EXISTS daily(id INTEGER PRIMARY KEY, at INTEGER)");
await host.sqlite_exec("INSERT INTO daily(at) VALUES(?1)", [Date.now()]);
await host.info("同步完成 cfg keys=" + Object.keys(cfg).length);
