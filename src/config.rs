//! 面板级只读配置（`host.config()`）：`<PANEL_DATA_DIR>/config.json`。
//!
//! 为什么要有体积上限：这份配置**每个插件各拿一份**，而 `config()` 在热路径上
//! （插件每次派发都可能读）。一个几十 MB 的文件会把内存和每次调用的开销一起放大。
//!
//! 它是**护栏不是安全边界**：能写这个文件的只有运维自己，或插件（`fs` 不受限）。
//! 所以超限的处理是「拒绝启动并报出真实字节数」，不是静默截断 —— 截断会让插件看到
//! 半个配置，属于最难查的一类问题（同基线 §22.2「越界要拒绝不要夹回」）。

use std::path::Path;

use serde_json::Value;

/// 默认上限 **4 MiB**。
///
/// 基线写的是 64 KiB，但那是**任务参数**的上限，不是插件配置
/// （`rust-plugin-panel` 的 `docs/host-api.md:197`）—— 本面板没有任务参数这个面，
/// 64 KiB 对「放账号表 / 映射表」的配置太小，所以这里给到 4 MiB 并允许环境变量调整。
pub const DEFAULT_MAX_BYTES: u64 = 4 * 1024 * 1024;
/// 下限：再小就放不下正常配置了。误配成 0 会表现成「配置莫名其妙坏了」。
const MIN_MAX_BYTES: u64 = 4 * 1024;
/// 覆盖上限的环境变量（运维自己调，不是给插件用的）。
pub const ENV_MAX: &str = "PANEL_CONFIG_MAX_BYTES";
/// 文件名（相对 `PANEL_DATA_DIR`）。
pub const FILE: &str = "config.json";

/// 本次启动生效的上限。环境变量非法就退回默认值（不因为一个笔误拒绝启动）。
pub fn max_bytes() -> u64 {
    match std::env::var(ENV_MAX) {
        Ok(s) => match s.trim().parse::<u64>() {
            Ok(n) if n >= MIN_MAX_BYTES => n,
            _ => DEFAULT_MAX_BYTES,
        },
        Err(_) => DEFAULT_MAX_BYTES,
    }
}

/// 单次运行的**默认预算**：300 秒（5 分钟）。
///
/// 原来是 30 秒。改的理由：面板上跑的多是「拉一次接口、下个文件、转存一下」这类
/// 真实网络活，30 秒经常卡在边界上（用户实测：任务时不时被判超时，其实再给两分钟就完）；
/// 默认值应该是「绝大多数任务不用改」，想收紧的人在任务上写 `budget_ms` 就行。
pub const DEFAULT_BUDGET_MS: u64 = 300_000;
/// 下限：再小连一次 HTTP 握手都跑不完，设成 100ms 只会让所有任务集体超时。
const MIN_BUDGET_MS: u64 = 1_000;
/// 单次运行**上限**的默认值：90 分钟（可配）。
///
/// 为什么做成可配：90 分钟是拍出来的默认值，不是物理上限。「一个账号的任务要跑很久」是真实需求
/// （央视频看视频类任务光节奏等待就 20 分钟量级），硬顶 90 分钟只会逼插件把节奏压紧。
pub const DEFAULT_BUDGET_MAX_MS: u64 = 5_400_000;
const MIN_BUDGET_MAX_MS: u64 = 60_000;
const MAX_BUDGET_MAX_MS: u64 = 43_200_000; // 12 小时

/// 每插件运行队列上限（`run.queue_max`，默认 32）：忙的时候后来的**排队**，超过才拒。
pub const DEFAULT_QUEUE_MAX: usize = 32;
/// 单条最大排队时间（`run.queue_wait_ms`，默认 1 小时）：等这么久还没轮到就丢掉并说清。
/// 单条最长排队时间，默认 **4 小时**。
///
/// 为什么不是 1 小时：排队这条路的旗舰场景就是「N 个账号 × 每个跑很久」——
/// 8 个号 × 20 分钟，第 8 条要等 140 分钟；默认 1 小时会把它当「陈旧」丢掉，
/// 表现是「前 3 个号跑了，后面的没动静，日志里一句 warn」。4 小时能装下 12 个 20 分钟的号。
pub const DEFAULT_QUEUE_WAIT_MS: u64 = 14_400_000;

static RUN_BUDGET_MAX_MS: std::sync::RwLock<Option<u64>> = std::sync::RwLock::new(None);
static RUN_QUEUE_MAX: std::sync::RwLock<Option<usize>> = std::sync::RwLock::new(None);
static RUN_QUEUE_WAIT_MS: std::sync::RwLock<Option<u64>> = std::sync::RwLock::new(None);

/// 上限的钳法（纯函数，好测）：再小连握手都不够，再大就是笔误。
pub fn clamp_budget_max_ms(ms: u64) -> u64 {
    ms.clamp(MIN_BUDGET_MAX_MS, MAX_BUDGET_MAX_MS)
}

/// 单次运行的上限（毫秒）。`run.budget_max_ms` 覆盖默认 90 分钟，夹在 [1 分钟, 12 小时]。
pub fn budget_max_ms() -> u64 {
    clamp_budget_max_ms(
        RUN_BUDGET_MAX_MS
            .read()
            .ok()
            .and_then(|g| *g)
            .unwrap_or(DEFAULT_BUDGET_MAX_MS),
    )
}

/// 队列上限的钳法（纯函数）：0 会让「排队」直接失效，所以下限是 1。
pub fn clamp_queue_max(n: usize) -> usize {
    n.clamp(1, 1000)
}

/// 每插件队列上限。没配 = 默认 32。
pub fn queue_max() -> usize {
    clamp_queue_max(
        RUN_QUEUE_MAX
            .read()
            .ok()
            .and_then(|g| *g)
            .unwrap_or(DEFAULT_QUEUE_MAX),
    )
}

/// 排队上限的钳法（纯函数）：10 秒以下等于「不让排队」。
pub fn clamp_queue_wait_ms(ms: u64) -> u64 {
    ms.clamp(10_000, 86_400_000)
}

/// 单条最大排队时间（毫秒）。没配 = 1 小时。
pub fn queue_wait_ms() -> u64 {
    clamp_queue_wait_ms(
        RUN_QUEUE_WAIT_MS
            .read()
            .ok()
            .and_then(|g| *g)
            .unwrap_or(DEFAULT_QUEUE_WAIT_MS),
    )
}

fn set_run_budget_max(ms: Option<u64>) {
    if let Ok(mut g) = RUN_BUDGET_MAX_MS.write() {
        *g = ms;
    }
}

fn set_run_queue(max: Option<usize>, wait_ms: Option<u64>) {
    if let Ok(mut g) = RUN_QUEUE_MAX.write() {
        *g = max;
    }
    if let Ok(mut g) = RUN_QUEUE_WAIT_MS.write() {
        *g = wait_ms;
    }
}
/// 覆盖默认预算的环境变量（毫秒）。
pub const ENV_BUDGET_MS: &str = "PANEL_BUDGET_MS";

/// 本次启动生效的默认预算。环境变量非法就退回默认值（同 `max_bytes`：不因为一个笔误拒绝启动），
/// 但会在 `[1s, 90min]` 之间夹一次 —— 0 或天文数字都是笔误，夹回可用范围比报错更有用。
pub fn budget_ms() -> u64 {
    // 优先级：环境变量 > config.json 的 `run.budget_ms` > 默认（加了中间这层，
    // 界面上的「单次运行预算」就是它；环境变量留着给老部署/临时覆盖）。
    if let Ok(s) = std::env::var(ENV_BUDGET_MS) {
        return match s.trim().parse::<u64>() {
            Ok(n) => n.clamp(MIN_BUDGET_MS, budget_max_ms()),
            Err(_) => DEFAULT_BUDGET_MS,
        };
    }
    match RUN_BUDGET_MS.read().ok().and_then(|g| *g) {
        Some(n) => n.clamp(MIN_BUDGET_MS, budget_max_ms()),
        None => DEFAULT_BUDGET_MS,
    }
}

/// config.json 里的默认预算（毫秒）。设成 `None` = 回到内置默认。
static RUN_BUDGET_MS: std::sync::RwLock<Option<u64>> = std::sync::RwLock::new(None);

pub fn set_run_budget(ms: Option<u64>) {
    if let Ok(mut g) = RUN_BUDGET_MS.write() {
        *g = ms;
    }
}

/// 读配置。缺文件 = 空对象（不是错误：新装的面板没有 config.json 很正常）。
pub fn load(dir: &Path) -> Result<Value, String> {
    let path = dir.join(FILE);
    let meta = match std::fs::metadata(&path) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Value::Object(Default::default()))
        }
        Err(e) => return Err(format!("读 {} 失败: {e}", path.display())),
    };
    // 先看大小再读：超限的文件连读都不读，别先分配几十 MB 再拒绝。
    let cap = max_bytes();
    if meta.len() > cap {
        return Err(format!(
            "{} 有 {} 字节，超过上限 {} 字节（{} 可调）。\n\
             这份配置每个插件各拿一份、每次 config() 都要用；要存数据请用插件自己的 SQLite。",
            path.display(),
            meta.len(),
            cap,
            ENV_MAX
        ));
    }
    let s = std::fs::read_to_string(&path).map_err(|e| format!("读 {} 失败: {e}", path.display()))?;
    serde_json::from_str(&s).map_err(|e| format!("{} 不是合法 JSON: {e}", path.display()))
}


/// 插件自己的配置放在哪：`<PANEL_DATA_DIR>/plugin-data/<name>/config.json`。
///
/// 为什么**不放**在 `config.json` 里：那是面板级配置（每个插件各拿一份快照），
/// 混进去之后「插件自己的设置」和「面板的设置」会互相覆盖，而且插件一多就变成一坨。
/// 也不放在 `plugins/` 旁边：那个目录会被静态服务暴露，配置里通常有账号。
pub fn plugin_config_path(data_dir: &Path, name: &str) -> std::path::PathBuf {
    data_dir.join("plugin-data").join(name).join(FILE)
}

/// 读某个插件自己的配置。缺文件 = 空对象（还没配过很正常）。
pub fn load_plugin(data_dir: &Path, name: &str) -> Result<Value, String> {
    let path = plugin_config_path(data_dir, name);
    let meta = match std::fs::metadata(&path) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Value::Object(Default::default()))
        }
        Err(e) => return Err(format!("读 {} 失败: {e}", path.display())),
    };
    let cap = max_bytes();
    if meta.len() > cap {
        return Err(format!(
            "{} 有 {} 字节，超过上限 {} 字节（{} 可调）",
            path.display(),
            meta.len(),
            cap,
            ENV_MAX
        ));
    }
    let s = std::fs::read_to_string(&path).map_err(|e| format!("读 {} 失败: {e}", path.display()))?;
    serde_json::from_str(&s).map_err(|e| format!("{} 不是合法 JSON: {e}", path.display()))
}

#[cfg(test)]
#[path = "config_tests.rs"]
mod tests;

// ---------------------------------------------------------------- 服务段
//
// 以前监听地址只能靠环境变量 `PANEL_BIND`、反代只能靠 `PANEL_ALLOW_ANY_HOST` ——
// 需求：更多可配置项（绑定端口、地址、x-forwarded-for）。
// 现在这些进 config.json 的 `server` 段，界面上是表单：
//
//   "server": { "bind": "127.0.0.1:3030", "trust_xff": false, "allow_any_host": false }
//
// 优先级：环境变量 > config.json > 内置默认（环境变量是老部署的兼容路径，不能丢）。
// `bind` 只能启动时读一次（改端口必须重启）；另两个是**热**的，中间件每请求读缓存。

/// `server` 段里那两个热开关。
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub struct Server {
    /// 信任 `X-Forwarded-For`（放在反代后面才该开）。开了以后 HTTP 错误日志会记真实来源 IP。
    pub trust_xff: bool,
    /// 没有令牌时也接受任意 Host（给同机反代留口子；等于 `PANEL_ALLOW_ANY_HOST=1`）。
    pub allow_any_host: bool,
}

static SERVER: std::sync::RwLock<Option<Server>> = std::sync::RwLock::new(None);

pub fn set_server(s: Server) {
    if let Ok(mut g) = SERVER.write() {
        *g = Some(s);
    }
}

pub fn server() -> Server {
    SERVER.read().ok().and_then(|g| *g).unwrap_or_default()
}

/// 从整份配置里取 `server` 段（不合法就退回默认，不让一个拼错的键把面板弄挂）。
pub fn server_from_value(v: &Value) -> (Option<String>, Server) {
    let seg = v.get("server");
    let bind = seg
        .and_then(|s| s.get("bind"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    let flag = |k: &str| {
        seg.and_then(|s| s.get(k))
            .and_then(Value::as_bool)
            .unwrap_or(false)
    };
    (
        bind,
        Server {
            trust_xff: flag("trust_xff"),
            allow_any_host: flag("allow_any_host"),
        },
    )
}

/// 启动流程里调：把 config.json 的 `server` 段读进缓存（读不到就用默认）。
pub fn init_server(dir: &Path) -> Option<String> {
    let value = load(dir).unwrap_or_else(|_| Value::Object(Default::default()));
    let (bind, s) = server_from_value(&value);
    set_server(s);
    set_run_budget(
        value
            .get("run")
            .and_then(|r| r.get("budget_ms"))
            .and_then(Value::as_u64),
    );
    set_limits(limits_from_value(&value));
    bind
}

/// 配置保存/重读之后调：让热开关立刻跟上（不用重启）。
/// `server.trust_xff` / `server.allow_any_host` / `run.budget_ms` 都在这里刷新。
/// 出站代理（`net.proxy`）：空/缺省 = 直连。
///
/// 放在 config.json 而不是环境变量：服务是 launchd/systemd 起的，那里没有 shell 环境；
/// 而且用户改这个是在界面上（设置 → 配置 → 网络），改完立刻生效，不用重启。
pub fn proxy_from_value(v: &Value) -> String {
    v.get("net")
        .and_then(|n| n.get("proxy"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_string()
}

/// 前缀代理（`net.prefix`）与它适用的站点（`net.prefix_hosts`，空 = 默认 GitHub 名单）。
///
/// 和 HTTP 正向代理的区别：这个是把 URL **拼在**前缀后面（gh-proxy 那一类镜像），
/// 所以必须限定站点 —— 镜像只管 GitHub，别的请求拼上去就是全盘坏掉。
pub fn prefix_from_value(v: &Value) -> (String, Vec<String>) {
    let net = v.get("net");
    let url = net
        .and_then(|n| n.get("prefix"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_string();
    let hosts = match net.and_then(|n| n.get("prefix_hosts")) {
        Some(Value::Array(a)) => a
            .iter()
            .filter_map(Value::as_str)
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect(),
        Some(Value::String(s)) => s
            .split(|c: char| c == ',' || c.is_whitespace())
            .map(|x| x.trim().to_string())
            .filter(|x| !x.is_empty())
            .collect(),
        _ => Vec::new(),
    };
    (url, hosts)
}

/// GitHub 通道：`net.github_mirrors`（有序前缀，`""` = 直连）+ `net.github_token`。
///
/// 旧键 `net.prefix` 仍然生效：没写 `github_mirrors` 时它就是 mirrors 的第一项（老配置不变）。
pub fn github_from_value(v: &Value) -> (Vec<String>, String) {
    let net = v.get("net");
    let list = |key: &str| -> Vec<String> {
        match net.and_then(|n| n.get(key)) {
            Some(Value::Array(a)) => a
                .iter()
                .filter_map(Value::as_str)
                .map(|s| s.trim().to_string())
                .collect(),
            Some(Value::String(s)) => s
                .split(|c: char| c == ',' || c.is_whitespace())
                .map(|x| x.trim().to_string())
                .collect(),
            _ => Vec::new(),
        }
    };
    let mut mirrors = list("github_mirrors");
    mirrors.retain(|m| !m.is_empty() || true); // 空串有意保留（= 直连）
    if mirrors.iter().all(|m| m.trim().is_empty()) {
        // 没配新键 → 退回老的单前缀（空 = 直连）
        let (pfx, _) = prefix_from_value(v);
        let pfx = pfx.trim().to_string();
        mirrors = if pfx.is_empty() {
            crate::gh::DEFAULT_MIRRORS.iter().map(|s| s.to_string()).collect()
        } else {
            vec![pfx, String::new()]
        };
    }
    let token = net
        .and_then(|n| n.get("github_token"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_string();
    (mirrors, token)
}

pub fn refresh_server(v: &Value) {
    // 出站代理也是热的：保存即换客户端，下一发请求就走新代理
    crate::outbound::set_proxy(&proxy_from_value(v));
    // 前缀代理：同样热生效，下一发 GitHub 请求就换个走法
    let (pfx, hosts) = prefix_from_value(v);
    crate::outbound::set_prefix(&pfx, &hosts);
    // GitHub 通道：mirrors 有序 + token，面板自己和插件共用这一份
    let (mirrors, token) = github_from_value(v);
    crate::gh::set(mirrors, token, hosts);
    set_server(server_from_value(v).1);
    set_run_budget(
        v.get("run")
            .and_then(|r| r.get("budget_ms"))
            .and_then(Value::as_u64),
    );
    // 单次上限、队列上限、单条排队上限 —— 都是热的
    set_run_budget_max(
        v.get("run")
            .and_then(|r| r.get("budget_max_ms"))
            .and_then(Value::as_u64),
    );
    set_run_queue(
        v.get("run")
            .and_then(|r| r.get("queue_max"))
            .and_then(Value::as_u64)
            .map(|n| n as usize),
        v.get("run")
            .and_then(|r| r.get("queue_wait_ms"))
            .and_then(Value::as_u64),
    );
    set_limits(limits_from_value(v));
}

// ---------------------------------------------------------------- 保留与上限
//
// 「设置 → 配置」里的第二组：保留多少条记录、HTTP 错误日志每分钟最多记几条、
// 单个插件源码多大。都是**热**的：清理任务每 5 分钟读一次，中间件每条错误读一次，
// 保存后下一轮就按新值走，不用重启。

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    /// 日志保留条数（`log.keep`）。
    pub logs_keep: i64,
    /// 运行记录保留条数（`runs.keep`）。
    pub runs_keep: i64,
    /// HTTP 错误日志每分钟最多几条（`log.http_per_min`）。0 = 不限。
    ///
    /// 为什么要有：扫描器一秒几十个 404 时，这些行是**证据**但也是噪音 ——
    /// 用户报过「日志被刷满」。限流后前 N 条照记，超出的丢弃（不是不记，是先到先得）。
    pub http_per_min: u32,
    /// 单个插件源码上限（`plugins.max_bytes`）。
    pub plugin_max_bytes: u64,
}

impl Default for Limits {
    fn default() -> Self {
        Limits {
            logs_keep: 20_000,
            runs_keep: 20_000,
            http_per_min: 0,
            plugin_max_bytes: 8 * 1024 * 1024,
        }
    }
}

/// 下限/上限：这些值写错了会把面板弄成「什么都留不住」或「一个 1 GiB 的插件卡死界面」。
const MIN_KEEP: i64 = 100;
const MAX_KEEP: i64 = 2_000_000;
const MIN_PLUGIN_BYTES: u64 = 64 * 1024;
const MAX_PLUGIN_BYTES: u64 = 64 * 1024 * 1024;
const MAX_HTTP_PER_MIN: u32 = 100_000;

static LIMITS: std::sync::RwLock<Option<Limits>> = std::sync::RwLock::new(None);

pub fn set_limits(l: Limits) {
    if let Ok(mut g) = LIMITS.write() {
        *g = Some(l);
    }
}

pub fn limits() -> Limits {
    LIMITS.read().ok().and_then(|g| *g).unwrap_or_default()
}

/// 从整份配置里取这几项：缺失或类型不对时采用 `Limits::default()`（保留 20,000 条、
/// HTTP 错误日志不限流、插件 8 MiB）；合法整数再夹到允许范围，不拒绝启动。
pub fn limits_from_value(v: &Value) -> Limits {
    let d = Limits::default();
    let num = |path: &[&str]| -> Option<u64> {
        let mut cur = v;
        for k in path {
            cur = cur.get(k)?;
        }
        cur.as_u64()
    };
    let keep = |path: &[&str], fallback: i64| -> i64 {
        match num(path) {
            Some(n) => (n as i64).clamp(MIN_KEEP, MAX_KEEP),
            None => fallback,
        }
    };
    Limits {
        logs_keep: keep(&["log", "keep"], d.logs_keep),
        runs_keep: keep(&["runs", "keep"], d.runs_keep),
        http_per_min: num(&["log", "http_per_min"])
            .map(|n| (n as u32).min(MAX_HTTP_PER_MIN))
            .unwrap_or(d.http_per_min),
        plugin_max_bytes: num(&["plugins", "max_bytes"])
            .map(|n| n.clamp(MIN_PLUGIN_BYTES, MAX_PLUGIN_BYTES))
            .unwrap_or(d.plugin_max_bytes),
    }
}

