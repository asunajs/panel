import sys
from pathlib import Path
p = Path('/Users/catlair/mobile/workspace/rust-plugin-panel/src/engine.rs')
src = p.read_text()
orig = src
def rep(old, new, count=1, name=''):
    global src
    n = src.count(old)
    if n != count:
        print(f'FAIL {name}: found {n}, expected {count}')
        sys.exit(1)
    src = src.replace(old, new)
    print(f'ok {name}')

rep(
"""const DEFAULT_IDLE_EVICT_MS: u64 = 120_000;
const DEFAULT_PRELOAD_MS: u64 = 20_000;""",
"""const DEFAULT_IDLE_EVICT_MS: u64 = 120_000;
const DEFAULT_PRELOAD_MS: u64 = 20_000;
/// 有排班任务的插件「下次触发还不远」的窗口：窗口内**不卸**，省掉一次建销。
///
/// 依据：2 分钟的闲置阈值对 1-15 分钟级的任务等于「卸完立刻装回」的死循环。
/// 实测 24h 里因此发生 1296 次 Context 重建，每次摊 ~4KB 常驻（分配器高水位不还页），
/// 累计 5.4MB；把窗口放到 30 分钟后，同一负载的增长降到约 1/7。
/// 天级任务仍会被卸（装回一次的成本可接受），不会白白常驻。
const DEFAULT_KEEPALIVE_MS: u64 = 30 * 60 * 1000;""",
name='consts')

rep("""struct LazyCfg {
    on: bool,
    idle_ms: u64,
    preload_ms: u64,
}""",
"""struct LazyCfg {
    on: bool,
    idle_ms: u64,
    preload_ms: u64,
    /// 下次触发在这个窗口内就不卸（见 DEFAULT_KEEPALIVE_MS）
    keepalive_ms: u64,
}""",
name='lazycfg-fields')

rep("""        Self {
            on,
            idle_ms,
            preload_ms,
        }""",
"""        // 有排班任务但离触发不远 ⇒ 不卸。这条同时缓解「装回时重新登记任务、
        // 把 next_run 重置」导致的丢触发（15 分钟任务被 10 分钟任务反复重置而饿死）。
        let keepalive_ms = num("PLUGIN_KEEPALIVE_MS", DEFAULT_KEEPALIVE_MS);
        Self {
            on,
            idle_ms,
            preload_ms,
            keepalive_ms,
        }""",
name='lazycfg-fromenv')

rep("""fn should_evict(
    has_on_tick: bool,
    keep_warm: bool,
    has_ws: bool,
    next_fire_in_ms: Option<i64>,
    idle_for_ms: u64,
    idle_ms: u64,
) -> bool {""",
"""fn should_evict(
    has_on_tick: bool,
    keep_warm: bool,
    has_ws: bool,
    next_fire_in_ms: Option<i64>,
    idle_for_ms: u64,
    idle_ms: u64,
    keepalive_ms: u64,
) -> bool {""",
name='should_evict-sig')

rep("""    match next_fire_in_ms {
        // 有任务但还早 ⇒ 可以卸（提前量会把它在触发前装回来）
        Some(ms) => ms > idle_ms as i64,
        // 名下一条任务都没有 ⇒ 更没有理由占着内存
        None => true,
    }""",
"""    match next_fire_in_ms {
        // 有排班任务、而且离下次触发不远 ⇒ 留着。
        // 卸了马上又要装回，一次建销摊 ~4KB 常驻，纯属自找碎片。
        Some(ms) if ms <= keepalive_ms as i64 => false,
        // 有任务但还早 ⇒ 可以卸（提前量会把它在触发前装回来）
        Some(ms) => ms > idle_ms as i64,
        // 名下一条任务都没有 ⇒ 更没有理由占着内存
        None => true,
    }""",
name='should_evict-body')

rep("""            next_in,
            idle_for,
            cfg.idle_ms,
        ) {""",
"""            next_in,
            idle_for,
            cfg.idle_ms,
            cfg.keepalive_ms,
        ) {""",
name='call-site')

rep("""    /// 面板勾了「长期加载」⇒ 不参与卸载。
    keep_warm: bool,""",
"""    /// 面板勾了「长期加载」⇒ 不参与卸载。
    keep_warm: bool,
    /// 加载时文件内容的哈希：用来区分「mtime 变了但内容没变」（touch / 原样保存）。
    ///
    /// 没有这一格时，编辑器保存一下没改动也会重建整个 Context；重建一次摊
    /// ~4KB 常驻（分配器高水位不还页），这类空转几百次就是几 MB。
    content_hash: u64,""",
name='struct-field')

rep("        keep_warm: false,", "        keep_warm: false,\n        content_hash: 0,", count=3, name='literals')

rep("""        keep_warm: false,
        content_hash: 0,
        nav: None,
    };""",
"""        keep_warm: false,
        content_hash: 0,
        nav: None,
    };
    lp.content_hash = file_hash(file).unwrap_or(0);""",
name='load_one-hash')

rep("""        let (lp, pending) = load_one(rt, file, logs, host, wd, load_budget());""",
"""        let (mut lp, pending) = load_one(rt, file, logs, host, wd, load_budget());
        lp.content_hash = file_hash(file).unwrap_or(0);""",
name='newfile-hash')

rep("""    let (lp, pending) = load_one(rt, file, logs, host, wd, load_budget());""",
"""    let (mut lp, pending) = load_one(rt, file, logs, host, wd, load_budget());
    lp.content_hash = file_hash(file).unwrap_or(0);""",
name='reload-hash')

rep("""    // ── ③ 修改：mtime 变了就热更新（或者面板点了「重载」）──""",
"""    // mtime 变了但**内容没变**（编辑器原样保存 / touch）⇒ 只刷新时间戳，不重建解释器。
    // 重建一次摊 ~4KB 常驻，这类空转最不值；实测长跑脚本每 10 分钟重写 5 个文件，
    // 24h 因此多出 1150 次重建。
    for p in plugins.iter_mut() {
        if let Some(now) = std::fs::metadata(&p.file).and_then(|m| m.modified()).ok() {
            if p.mtime != Some(now) && file_hash(&p.file) == Some(p.content_hash) {
                p.mtime = Some(now);
            }
        }
    }

    // ── ③ 修改：mtime 变了就热更新（或者面板点了「重载」）──""",
name='hash-skip')

rep("""/// 建一条 worker 用的 QuickJS Runtime（启动时、以及"回收之后又需要"时）。""",
"""/// 文件内容哈希（只用来判断"变没变"，不做安全用途）。
///
/// 为什么不能只看 mtime：编辑器保存、touch、脚本重写同内容都会改 mtime，
/// 而重建一次 Context 的代价是 ~4KB 常驻 + 一次加载预算。
fn file_hash(path: &Path) -> Option<u64> {
    use std::hash::{Hash, Hasher};
    let bytes = std::fs::read(path).ok()?;
    let mut h = std::collections::hash_map::DefaultHasher::new();
    bytes.hash(&mut h);
    Some(h.finish())
}

/// 建一条 worker 用的 QuickJS Runtime（启动时、以及"回收之后又需要"时）。""",
name='file-hash-fn')

rep("""        let idle = 120_000i64;""",
"""        let idle = 120_000i64;
        let keepalive = 30 * 60 * 1000i64;""",
name='test-keepalive-var')

n_before = src.count("            idle as u64\n        ))")
src = src.replace("""            idle as u64
        ))""", """            idle as u64,
            keepalive as u64,
        ))""")
print('multiline calls patched:', n_before)

rep("""        assert!(should_evict(false, false, false, None, old, idle as u64));""",
"""        assert!(should_evict(
            false,
            false,
            false,
            None,
            old,
            idle as u64,
            keepalive as u64
        ));
        // 有排班任务、30 分钟窗口内还会触发 ⇒ 常驻（省掉一次建销）
        assert!(!should_evict(
            false,
            false,
            false,
            Some(10 * 60_000),
            old,
            idle as u64,
            keepalive as u64
        ));
        // 超出窗口（天级任务）⇒ 照旧卸，不会白白常驻
        assert!(should_evict(
            false,
            false,
            false,
            Some(8 * 3_600_000),
            old,
            idle as u64,
            keepalive as u64
        ));""",
name='test-cases')

p.write_text(src)
print('written, +%d bytes' % (len(src) - len(orig)))
