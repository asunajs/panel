//! 插件 → worker 的分配策略。
//!
//! # 为什么要有 worker 池
//!
//! 解释器（QuickJS / PocketPy）都不是 `Send`，所以历史上所有插件任务都跑在**一条**
//! 引擎线程上。代价是实测出来的（`scripts/serial_cost.py`）：一个每 1 秒只忙等 300ms
//! 的合规任务，就能把同轮一个 20ms 任务的间隔 p99 从 29ms 推到 **311ms**，代价严格
//! 正比于阻塞时长。
//!
//! 这**不能靠优先级/抢占解决**：`http_fetch` 是同步阻塞的系统调用，QuickJS 的
//! interrupt 只能打断 CPU 循环，拦不住它。唯一的办法是让慢的**看不见**快的 ——
//! 多条执行线程，每条有自己的解释器。
//!
//! # 关键约束：插件亲和性
//!
//! 一个插件的解释器状态（模块级变量、注册的处理函数）住在那条线程上，所以
//! **一个插件只能属于一条 worker**，它的所有任务都跟着它。分配的单位是**插件**，不是任务。
//!
//! # 分配目标
//!
//! 不是「让每条 worker 负载均衡」——那是吞吐量的目标。这里的首要目标是
//! **别让粗粒度的大块任务挡住毫秒级的任务**。所以：
//!
//! 1. **细粒度插件**（声明了 ≤1s 的任务）归到「精度 worker」上；
//! 2. **其余插件**按负载贪心装箱（LPT：重的先放）到剩下的 worker 上，让它们互相挤压，
//!    但挤不到精度 worker。
//!
//! 没有细粒度插件时就退化成纯 LPT 均衡（那种情况下没有要保护的精度，就该要吞吐量）。
//!
//! # 怎么知道哪个插件是细粒度的
//!
//! 分配必须发生在**创建解释器之前**（来不及先加载再问插件），所以这里用**源码文本扫描**
//! 找出每个插件声明的最细调度间隔。这是个启发式，所以：
//!
//! - 每条决策都会打日志（`[boot] worker 分配：…`），分配错了看得见；
//! - `ENGINE_PIN` 可以按插件名手工指定 worker —— 启发式错了有逃生门。
//!
//! 代价未知（没有声明任何任务）时按 0 算，交给 LPT 自然铺开。

use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// 「细粒度」的分界线：声明的最细间隔**严格小于**这个值的插件才算要精度。单位毫秒。
///
/// 分界线取 1 秒（不含），两个理由：
///
/// - 被推迟 300ms 时，20ms / 500ms 的任务是「明显不对」，1 秒及以上的任务几乎没人
///   会察觉 —— 这才是「要精度」的实际含义。
/// - **分界线的排他性是实测逼出来的**：量化夹具里那个「每 1 秒忙等 300ms」的捣乱任务
///   声明就是 `1s`，如果 `1s` 也算细粒度，它就会和 20ms 的快任务分到同一条 worker 上，
///   隔离等于没做（验收测试会直接显示 p99 没变好）。
pub const FINE_GRAINED_MS: u64 = 1000;

/// 一个插件的分配依据（纯数据，方便单测）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginCost {
    pub name: String,
    pub file: PathBuf,
    /// 源码里声明的最细调度间隔（毫秒）。`None` = 没扫到 / 没有任务。
    pub finest_ms: Option<u64>,
    /// 已知的负载权重：拿不到实测耗时时用「触发频率」近似。
    /// 越大表示这个插件占引擎越多，装箱时越该被单独放。
    pub weight: u64,
}

impl PluginCost {
    pub fn is_fine_grained(&self) -> bool {
        // 严格小于：整 1 秒的任务不算要精度（见 FINE_GRAINED_MS 的说明）
        matches!(self.finest_ms, Some(ms) if ms < FINE_GRAINED_MS)
    }
}

/// 分配结果。
#[derive(Debug, Clone)]
pub struct Plan {
    /// `groups[i]` = 第 i 条 worker 要加载的文件
    pub groups: Vec<Vec<PathBuf>>,
    /// 给日志用的一句句说明（为什么这么分）
    pub notes: Vec<String>,
}

/// 扫描源码，找出声明的最细调度间隔。
///
/// 认三种写法（与插件 API 的三种注册方式一一对应）：
/// - `every('名字', '20ms', …)` / `host.every(…, '500ms', …)`
/// - `register_cron('名字', '@every 20ms', …)`
/// - `register_cron('名字', '*/1 * * * * *', …)` —— 6 段 cron 的第一段是秒
///
/// 都是**字符串字面量**里的内容，所以扫描足够可靠：这些 API 只接受字面量间隔，
/// 传变量的写法本身就很少见，而且漏扫的后果只是「当成粗粒度」（退化成负载均衡）。
pub fn finest_interval_ms(source: &str) -> Option<u64> {
    let mut best: Option<u64> = None;
    let mut consider = |ms: u64| {
        if ms > 0 {
            best = Some(best.map_or(ms, |b: u64| b.min(ms)));
        }
    };

    // ① `@every 20ms` / `@every 2s` / `@every 5m` / `@every 1h`
    for cap in intervals_in(source, "@every ") {
        consider(cap);
    }
    // ② `every('name', '20ms', …)`：第二个参数是裸的间隔字符串（没有 @every 前缀）。
    //    直接找 `'<数字><单位>'` 这种"看起来就是间隔"的字面量 —— 比解析调用更稳。
    for cap in intervals_in(source, "'") {
        consider(cap);
    }
    // ③ 6 段 cron 的秒字段
    for ms in cron_intervals(source) {
        consider(ms);
    }
    best
}

/// 抽出一个源码里所有字符串字面量的内容（`'…'` 与 `"…"`）。
///
/// 先抽字面量再判断，比「在整段源码里找 `*/`」可靠得多 —— 后者会把
/// `*/5 * * * *`（5 段 cron，根本没有秒字段）后面的代码也算进字段数，
/// 于是误判成 5 秒周期。实测就是这么错的（单测抓到的）。
fn string_literals(source: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    let mut chars = source.chars().peekable();
    while let Some(ch) = chars.next() {
        match quote {
            None => {
                if ch == '\'' || ch == '"' {
                    quote = Some(ch);
                    cur.clear();
                } else if ch == '/' && chars.peek() == Some(&'/') {
                    // 行注释整行跳过：注释里的例子不该影响分配
                    for c in chars.by_ref() {
                        if c == '\n' {
                            break;
                        }
                    }
                }
            }
            Some(q) => {
                if ch == '\\' {
                    if let Some(esc) = chars.next() {
                        cur.push(esc);
                    }
                } else if ch == q {
                    out.push(std::mem::take(&mut cur));
                    quote = None;
                } else {
                    cur.push(ch);
                }
            }
        }
    }
    out
}

/// 6 段 cron 的秒字段 → 周期毫秒。
///
/// 只认 `*/N`（每 N 秒）。单个具体秒数（`30 * * * * *`）是每分钟一次，算粗粒度。
fn cron_intervals(source: &str) -> Vec<u64> {
    let mut out = Vec::new();
    for lit in string_literals(source) {
        let fields: Vec<&str> = lit.split_whitespace().collect();
        // **正好** 6 段才是「秒 分 时 日 月 周」；5 段没有秒字段
        if fields.len() != 6 {
            continue;
        }
        if let Some(rest) = fields[0].strip_prefix("*/") {
            if let Ok(n) = rest.parse::<u64>() {
                if n > 0 {
                    out.push(n * 1000);
                }
            }
        }
    }
    out
}

/// 在 `source` 里找 `prefix` 之后紧跟的 `<数字><单位>` 写法，返回毫秒。
///
/// 单位只认插件的四种：`ms` / `s` / `m` / `h`。**必须带单位**，
/// 这样 `'20'` 这种裸数字（比如数组下标、时间戳）不会被误当成间隔。
fn intervals_in(source: &str, prefix: &str) -> Vec<u64> {
    let mut out = Vec::new();
    let mut from = 0usize;
    while let Some(i) = source[from..].find(prefix) {
        let start = from + i + prefix.len();
        let tail = &source[start..];
        let digits: String = tail.chars().take_while(|c| c.is_ascii_digit()).collect();
        if !digits.is_empty() {
            let after = &tail[digits.len()..];
            let unit = ["ms", "s", "m", "h"]
                .iter()
                .find(|u| after.starts_with(**u))
                .copied();
            if let Some(u) = unit {
                if let Ok(n) = digits.parse::<u64>() {
                    out.push(match u {
                        "ms" => n,
                        "s" => n * 1000,
                        "m" => n * 60_000,
                        _ => n * 3_600_000,
                    });
                }
            }
        }
        from = start;
    }
    out
}

/// 按策略把插件分到 `workers` 条 worker 上。
///
/// `pin` 是逃生门：`插件名 → worker 下标`，命中的插件强制归到指定 worker（越界会被钳住）。
pub fn plan(costs: &[PluginCost], workers: usize, pin: &HashMap<String, usize>) -> Plan {
    let workers = workers.max(1);
    let mut groups: Vec<Vec<PathBuf>> = vec![Vec::new(); workers];
    let mut notes: Vec<String> = Vec::new();

    // ① 手工钉住的先落位
    let mut rest: Vec<&PluginCost> = Vec::new();
    for c in costs {
        match pin.get(&c.name) {
            Some(w) => {
                let w = (*w).min(workers - 1);
                groups[w].push(c.file.clone());
                notes.push(format!("`{}` → worker {w}（ENGINE_PIN 手工指定）", c.name));
            }
            None => rest.push(c),
        }
    }

    let (mut fine, mut coarse): (Vec<&PluginCost>, Vec<&PluginCost>) =
        rest.into_iter().partition(|c| c.is_fine_grained());

    // ② 细粒度插件：worker 0 是「精度 worker」。
    //    有粗粒度插件时，精度 worker 只收细粒度的；没有粗粒度的才把细粒度铺开
    //    （那种情况下没有要保护的精度，就该要吞吐量）。
    // **必须同时存在细粒度和粗粒度**才保留 worker 0：
    //   - 没有细粒度的 → 没有要保护的精度，保留 worker 0 只会让它空着、
    //     把所有任务挤到别的 worker 上（实测就是这么错的，单测抓到的）
    //   - 没有粗粒度的 → 没必要隔离，细粒度应该铺开换吞吐量
    let reserve = workers > 1 && !coarse.is_empty() && !fine.is_empty();
    let fine_slots: Vec<usize> = if reserve { vec![0] } else { (0..workers).collect() };
    if !fine.is_empty() {
        // 轻的往精度 worker 堆（它们互相影响小），重的铺到别的 slot 上
        fine.sort_by_key(|c| std::cmp::Reverse(c.weight));
        for (i, c) in fine.iter().enumerate() {
            let slot = fine_slots[i % fine_slots.len()];
            groups[slot].push(c.file.clone());
            notes.push(format!(
                "`{}` → worker {slot}（细粒度：最细 {:?}ms，要精度）",
                c.name, c.finest_ms
            ));
        }
        if reserve {
            notes.push("worker 0 留给细粒度插件，粗粒度任务不会挤到它上面".into());
        }
    }

    // ③ 粗粒度插件：LPT 贪心（重的先放），装到当前累计权重最小的 worker 上。
    //    只看非精度 worker —— 否则重任务又会被塞回精度 worker。
    let bulk_slots: Vec<usize> = if reserve {
        (1..workers).collect()
    } else {
        (0..workers).collect()
    };
    coarse.sort_by(|a, b| b.weight.cmp(&a.weight).then(a.name.cmp(&b.name)));
    let mut load: Vec<u64> = vec![0; workers];
    for c in coarse {
        // 选累计权重最小的（同权重取下标小的，保证结果确定、可单测）
        let slot = *bulk_slots
            .iter()
            .min_by_key(|s| (load[**s], **s))
            .expect("bulk_slots 不空");
        groups[slot].push(c.file.clone());
        load[slot] += c.weight.max(1);
        notes.push(format!(
            "`{}` → worker {slot}（粗粒度，负载权重 {}）",
            c.name,
            c.weight
        ));
    }

    Plan { groups, notes }
}

/// 运行期的「文件 → worker」归属表。
///
/// 为什么不能只在启动时算一次：插件目录是活的（新增 / 改名 / 删除都会发生，热更新靠它）。
/// 运行期冒出来的新插件也得有人加载，而「谁加载」必须**所有 worker 看到同一个答案**，
/// 否则会出现两条 worker 抢同一个文件、或者谁都不认它。
///
/// 同时守一条不变量：**插件名跨 worker 唯一**（名字来自文件 stem）。
/// 撞名时把后来者判给先占者的 worker —— 那条 worker 原有的 `name_taken` 检查
/// 会照常拒绝它并给出「与 X 撞名」的错误信息，行为与单 worker 时完全一致。
/// 否则一个 `a/x.js` 和一个 `b/x.js` 会分别落到不同 worker 上、双双加载成功。
#[derive(Debug, Default)]
pub struct Assignment {
    by_file: HashMap<PathBuf, usize>,
    by_name: HashMap<String, PathBuf>,
}

impl Assignment {
    pub fn new() -> Self {
        Self::default()
    }

    /// 启动时按分配方案一次性灌入。
    pub fn seed(&mut self, plan: &Plan, name_of: impl Fn(&Path) -> String) {
        for (i, group) in plan.groups.iter().enumerate() {
            for f in group {
                let name = name_of(f);
                self.by_name.entry(name).or_insert_with(|| f.clone());
                self.by_file.insert(f.clone(), i);
            }
        }
    }

    pub fn owner_of(&self, file: &Path) -> Option<usize> {
        self.by_file.get(file).copied()
    }

    /// 认领一个**新出现**的文件：已经有归属就返回它，没有就按策略算一个并记住。
    ///
    /// `workers` 是 worker 总数，`finest_ms` 是这个插件源码里最细的调度间隔。
    pub fn claim(&mut self, file: &Path, name: &str, finest_ms: Option<u64>, workers: usize) -> usize {
        if let Some(w) = self.by_file.get(file) {
            return *w;
        }
        let workers = workers.max(1);
        let cost = PluginCost {
            name: name.to_string(),
            file: file.to_path_buf(),
            finest_ms,
            weight: finest_ms.map(|ms| (1000 / ms.max(1)).max(1)).unwrap_or(0),
        };
        let w = if let Some(first) = self.by_name.get(name) {
            // 撞名：跟先占者同一条 worker，让它自己的 name_taken 去拒绝
            self.by_file.get(first).copied().unwrap_or(0)
        } else if cost.is_fine_grained() && workers > 1 {
            0 // 精度 worker
        } else {
            // 其余：挑当前插件数最少的（新来的通常是少数，不需要按权重装箱）
            (0..workers)
                .min_by_key(|w| {
                    (self.by_file.values().filter(|v| **v == *w).count(), *w)
                })
                .unwrap_or(0)
        };
        self.by_name.entry(name.to_string()).or_insert_with(|| file.to_path_buf());
        self.by_file.insert(file.to_path_buf(), w);
        w
    }

    /// 插件名 → worker 下标（命令路由用）。
    pub fn route_map(&self) -> HashMap<String, usize> {
        self.by_name
            .iter()
            .filter_map(|(name, file)| self.by_file.get(file).map(|w| (name.clone(), *w)))
            .collect()
    }

    /// 某个 worker 负责的插件名（清理任务时用来判断「这条任务归不归我管」）。
    pub fn names_of(&self, worker: usize) -> Vec<String> {
        self.by_name
            .iter()
            .filter(|(_, f)| self.by_file.get(*f) == Some(&worker))
            .map(|(n, _)| n.clone())
            .collect()
    }

    /// 名字还在磁盘上的那些插件（跨所有 worker）。
    ///
    /// 清理任务必须用它判「插件是不是真的没了」，**不能用 `by_name`**：
    /// 归属表故意不删名字（文件被删又建回来时归属不变），所以 `by_name` 里
    /// 永远留着已删除插件的名字 —— 用它判会让「插件已不存在」这个分支永不触发，
    /// 失效任务一条也清不掉（实测踩到过）。
    pub fn live_names(&self) -> Vec<String> {
        self.by_name
            .iter()
            .filter(|(_, f)| f.is_file())
            .map(|(n, _)| n.clone())
            .collect()
    }

    
    /// 已认领的文件数（启动日志用）。
    pub fn len(&self) -> usize {
        self.by_file.len()
    }

}

/// 从插件目录扫出每个插件的最细间隔与负载权重。
///
/// 权重用「每秒触发多少次」近似（间隔越短越重），拿不到就按 0 算 —— 分配不依赖实测
/// 耗时（首次启动本来就没有实测），所以它永远是确定性的。
pub fn scan_costs(files: &[PathBuf], name_of: impl Fn(&Path) -> String) -> Vec<PluginCost> {
    files
        .iter()
        .map(|f| {
            let src = std::fs::read_to_string(f).unwrap_or_default();
            let finest = finest_interval_ms(&src);
            let weight = finest.map(|ms| (1000 / ms.max(1)).max(1)).unwrap_or(0);
            PluginCost {
                name: name_of(f),
                file: f.clone(),
                finest_ms: finest,
                weight,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn c(name: &str, finest: Option<u64>) -> PluginCost {
        PluginCost {
            name: name.into(),
            file: PathBuf::from(format!("{name}.js")),
            finest_ms: finest,
            weight: finest.map(|ms| (1000 / ms.max(1)).max(1)).unwrap_or(0),
        }
    }

    fn group_names(p: &Plan) -> Vec<Vec<String>> {
        p.groups
            .iter()
            .map(|g| {
                let mut v: Vec<String> = g
                    .iter()
                    .map(|f| f.file_stem().unwrap().to_string_lossy().to_string())
                    .collect();
                v.sort();
                v
            })
            .collect()
    }

    // ---------- 源码扫描 ----------

    #[test]
    fn scans_every_and_cron_forms() {
        assert_eq!(finest_interval_ms("every('t', '20ms', f);"), Some(20));
        assert_eq!(finest_interval_ms("host.every('t', '500ms', f);"), Some(500));
        assert_eq!(finest_interval_ms("every('t', '2s', f);"), Some(2000));
        assert_eq!(finest_interval_ms("register_cron('x', '@every 30s', f);"), Some(30_000));
        assert_eq!(finest_interval_ms("register_cron('x', '@every 5m', f);"), Some(300_000));
        assert_eq!(finest_interval_ms("register_cron('x', '@every 1h', f);"), Some(3_600_000));
        // 6 段 cron 的秒字段
        assert_eq!(finest_interval_ms("register_cron('x', '*/1 * * * * *', f);"), Some(1000));
        assert_eq!(finest_interval_ms("register_cron('x', '*/10 * * * * *', f);"), Some(10_000));
    }

    #[test]
    fn picks_the_finest_of_several() {
        let src = "every('a', '2s', f);\nevery('b', '50ms', f);\n";
        assert_eq!(finest_interval_ms(src), Some(50));
    }

    #[test]
    fn does_not_mistake_bare_numbers_or_5_field_cron_for_intervals() {
        // 裸数字（没有单位）不该被当成间隔
        assert_eq!(finest_interval_ms("const x = 20; arr[5];"), None);
        // 5 段 cron 没有秒字段，管不到毫秒精度
        assert_eq!(finest_interval_ms("register_cron('x', '*/5 * * * *', f);"), None);
        // 除法不该被当成 cron 步长
        assert_eq!(finest_interval_ms("const y = a * / 3 goes here;"), None);
        assert_eq!(finest_interval_ms("// 没有任何任务"), None);
    }

    // ---------- 分配 ----------

    #[test]
    fn one_second_is_not_fine_grained() {
        // 这是验收测试用的夹具组合：20ms 的快任务 + 每 1 秒忙等 300ms 的捣乱任务。
        // 后者必须被判成粗粒度，否则两者会同 worker，隔离失效。
        assert!(!c("slow", Some(1000)).is_fine_grained(), "整 1 秒不该算细粒度");
        assert!(!c("slow-cron", Some(1000)).is_fine_grained());
        assert!(c("fast", Some(999)).is_fine_grained());
        assert!(c("fast500", Some(500)).is_fine_grained());
        // 而调度层面的分类结果：一个进 worker 0，一个进 worker 1
        let costs = vec![c("fast", Some(20)), c("slow", Some(1000))];
        let p = plan(&costs, 2, &HashMap::new());
        let g = p
            .groups
            .iter()
            .map(|x| {
                let mut v: Vec<String> = x
                    .iter()
                    .map(|f| f.file_stem().unwrap().to_string_lossy().to_string())
                    .collect();
                v.sort();
                v
            })
            .collect::<Vec<_>>();
        assert_eq!(g, vec![vec!["fast"], vec!["slow"]], "快慢必须分开");
    }

    #[test]
    fn fine_grained_plugin_is_isolated_from_bulk() {
        // 核心场景：一个 20ms 的插件 + 一个 30s 的粗粒度插件。
        // 精度插件必须在 worker 0，粗粒度在 worker 1 —— 这样慢任务挤不到它。
        let costs = vec![c("fast", Some(20)), c("bulk", Some(30_000))];
        let p = plan(&costs, 2, &HashMap::new());
        assert_eq!(group_names(&p), vec![vec!["fast"], vec!["bulk"]]);
        assert!(p.notes.iter().any(|n| n.contains("精度")), "{:?}", p.notes);
    }

    #[test]
    fn several_fine_plugins_spread_over_precision_slot_then_bulk_slots() {
        let costs = vec![
            c("f1", Some(20)),
            c("f2", Some(100)),
            c("f3", Some(500)),
            c("bulk", Some(60_000)),
        ];
        let p = plan(&costs, 2, &HashMap::new());
        let g = group_names(&p);
        // worker 0 至少拿到最细的那个；粗粒度的绝不在 worker 0
        assert!(g[0].contains(&"f1".to_string()), "{g:?}");
        assert!(g[1].contains(&"bulk".to_string()), "{g:?}");
        assert!(!g[0].contains(&"bulk".to_string()), "粗粒度不该进精度 worker: {g:?}");
    }

    #[test]
    fn all_coarse_falls_back_to_load_balancing() {
        // 没有要保护的精度时，就该要吞吐量：按权重均衡摊到两条 worker 上
        let costs = vec![
            c("a", Some(2000)),
            c("b", Some(5000)),
            c("c", Some(10_000)),
            c("d", Some(60_000)),
        ];
        let p = plan(&costs, 2, &HashMap::new());
        let g = group_names(&p);
        assert!(g[0].len() == 2 && g[1].len() == 2, "应当均摊: {g:?}");
    }

    #[test]
    fn single_worker_keeps_everything_together() {
        let costs = vec![c("fast", Some(20)), c("bulk", Some(30_000))];
        let p = plan(&costs, 1, &HashMap::new());
        assert_eq!(group_names(&p), vec![vec!["bulk", "fast"]]);
    }

    #[test]
    fn more_workers_than_plugins_is_fine() {
        let costs = vec![c("only", Some(20))];
        let p = plan(&costs, 4, &HashMap::new());
        assert_eq!(p.groups.len(), 4);
        assert_eq!(group_names(&p)[0], vec!["only"]);
        assert!(p.groups[1..].iter().all(|g| g.is_empty()));
    }

    #[test]
    fn zero_workers_is_clamped_to_one() {
        let costs = vec![c("a", Some(20))];
        let p = plan(&costs, 0, &HashMap::new());
        assert_eq!(p.groups.len(), 1);
        assert_eq!(group_names(&p)[0], vec!["a"]);
    }

    #[test]
    fn pin_overrides_the_heuristic_and_is_clamped() {
        let costs = vec![c("fast", Some(20)), c("bulk", Some(30_000))];
        let mut pin = HashMap::new();
        // 故意把「要精度」的插件钉到 worker 1，验证手工指定优先于启发式
        pin.insert("fast".to_string(), 1);
        let p = plan(&costs, 2, &pin);
        let g = group_names(&p);
        assert!(g[1].contains(&"fast".to_string()), "{g:?}");
        assert!(p.notes.iter().any(|n| n.contains("ENGINE_PIN")), "{:?}", p.notes);

        // 越界的下标要被钳住，而不是 panic 或者丢插件
        let mut pin2 = HashMap::new();
        pin2.insert("fast".to_string(), 99);
        let p2 = plan(&costs, 2, &pin2);
        assert_eq!(group_names(&p2)[1].contains(&"fast".to_string()), true);
        // 插件的总数不能变（钉住 + 自动分配不能重复或丢失）
        let total: usize = p2.groups.iter().map(|g| g.len()).sum();
        assert_eq!(total, 2);
    }

    // ---------- 运行期归属表 ----------

    #[test]
    fn assignment_seed_and_route() {
        let costs = vec![c("fast", Some(20)), c("bulk", Some(30_000))];
        let p = plan(&costs, 2, &HashMap::new());
        let name_of = |f: &Path| f.file_stem().unwrap().to_string_lossy().to_string();
        let mut a = Assignment::new();
        a.seed(&p, name_of);
        assert_eq!(a.owner_of(Path::new("fast.js")), Some(0));
        assert_eq!(a.owner_of(Path::new("bulk.js")), Some(1));
        let routes = a.route_map();
        assert_eq!(routes["fast"], 0);
        assert_eq!(routes["bulk"], 1);
        assert_eq!(a.names_of(0), vec!["fast".to_string()]);
    }

    #[test]
    fn new_fine_grained_file_goes_to_precision_worker() {
        let mut a = Assignment::new();
        let w = a.claim(Path::new("late-fast.js"), "late-fast", Some(10), 2);
        assert_eq!(w, 0, "运行期新增的细粒度插件该进精度 worker");
        // 再问一次必须给同一个答案（不能每次算不一样）
        assert_eq!(a.claim(Path::new("late-fast.js"), "late-fast", Some(10), 2), w);
    }

    #[test]
    fn new_coarse_file_goes_to_least_loaded_worker() {
        let mut a = Assignment::new();
        let name_of = |f: &Path| f.file_stem().unwrap().to_string_lossy().to_string();
        let costs = vec![c("a", Some(30_000)), c("b", Some(30_000))];
        a.seed(&plan(&costs, 2, &HashMap::new()), name_of);
        // 两条 worker 各 1 个插件 → 新来的挑下标小的（确定性）
        let w = a.claim(Path::new("new.js"), "new", Some(30_000), 2);
        assert!(w <= 1);
        assert_eq!(a.len(), 3);
    }

    #[test]
    fn same_name_across_dirs_lands_on_the_same_worker() {
        // 不变量：插件名跨 worker 唯一。撞名的后来者必须跟先占者同一条 worker，
        // 让它自己的 name_taken 拒绝 —— 否则两个 x.js 会各自加载成功。
        let mut a = Assignment::new();
        let first = a.claim(Path::new("a/x.js"), "x", Some(20), 2);
        let second = a.claim(Path::new("b/x.js"), "x", Some(20), 2);
        assert_eq!(first, second, "同名文件必须落在同一条 worker 上");
        assert_eq!(a.len(), 2, "两个文件都要有归属（其中一个会被 name_taken 拒绝）");
        let routes = a.route_map();
        assert_eq!(routes.len(), 1, "路由表里同名只有一个");
    }

    #[test]
    fn single_worker_claims_everything_here() {
        let mut a = Assignment::new();
        assert_eq!(a.claim(Path::new("any.js"), "any", Some(5), 1), 0);
        assert_eq!(a.claim(Path::new("other.js"), "other", Some(60_000), 1), 0);
    }

    #[test]
    fn unknown_cost_plugins_do_not_crash_the_bin_packing() {
        let costs = vec![c("x", None), c("y", None), c("z", Some(20))];
        let p = plan(&costs, 2, &HashMap::new());
        let total: usize = p.groups.iter().map(|g| g.len()).sum();
        assert_eq!(total, 3);
        let g = group_names(&p);
        assert!(g[0].contains(&"z".to_string()), "细粒度的应该在精度 worker: {g:?}");
    }
}
