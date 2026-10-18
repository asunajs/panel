// host-api —— 日志（规范 v2）。
//
// 这是面板 `src/log/` 的 **JS 侧等价实现**：同一份插件源码在面板内外跑，
// 日志必须长得一模一样 —— 行格式、JSONL 字段、脱敏规则，一个字都不能差。
// 三份实现（Rust / JS / Python）的**唯一执行保障**是仓库根上的共享向量
// `testdata/redact-cases.json` 与 `testdata/log-format-cases.json`：
// 三边的测试都读同一个文件，谁改歪了谁的测试就红。
//
// ## 和 v1 的区别（这是一次有意的破坏性变更）
//
// v1 只有 `log/warn/error` 三档，格式是 `[host-api] [level] msg`，只写 stderr。
// v2 对齐宿主：八档语义 type、`YYYY-MM-DD HH:MM:SS.mmm [type] [scope] msg`，
// 并且**同时写 JSONL 文件**（按天切、留 7 天）—— 面板外跑的日志因此能直接
// 丢给面板视图看，也能 `jq` 筛。
//
// ## 输出到 stderr
//
// 面板内的 console transport 写 stdout；适配器写 **stderr**，这是刻意的：
// `node plugin.js > data.txt` 时要让 `data.txt` 只装插件**自己 print 的业务数据**，
// 日志混进去会毁掉重定向。行格式本身完全一致，差的只是流向。
//
// ## 配置（环境变量）
//
// | 变量 | 默认 | 说明 |
// | --- | --- | --- |
// | `HOST_LOG_LEVEL` | `debug` | 最低过滤级别 |
// | `HOST_LOG_DIR` | `./logs` | JSONL 目录；`off` / `0` / `-` = 不落盘 |
// | `HOST_LOG_KEEP_DAYS` | `7` | 保留天数 |
// | `HOST_LOG_COLOR` | 自动 | `0`/`1` 强制关/开 ANSI 颜色 |
// | `HOST_LOG_SCOPE` | `plugin` | 没设置插件名时的 scope |

import { closeSync, mkdirSync, openSync, readdirSync, unlinkSync, writeSync } from 'node:fs';
import { join } from 'node:path';

/** 八档语义 type。顺序 = 面板 `Type::ALL`，别乱动（测试按它比集合）。 */
export const TYPES = ['debug', 'info', 'start', 'success', 'fail', 'warn', 'error', 'fatal'];

/** type → 过滤级别。`start/success/fail` 是**语义着色**，过滤级别都是 info。 */
export const LEVEL_OF = {
  debug: 'debug',
  info: 'info',
  start: 'info',
  success: 'info',
  fail: 'info',
  warn: 'warn',
  error: 'error',
  fatal: 'fatal',
};

const RANK = { debug: 0, info: 1, warn: 2, error: 3, fatal: 4 };

/** 终端配色，与 `src/log/console.rs` 的 `color_of` 一一对应。 */
const COLORS = {
  info: '\x1b[36m',
  warn: '\x1b[33m',
  error: '\x1b[31m',
  fatal: '\x1b[31;1m',
  success: '\x1b[32m',
  debug: '\x1b[90m',
  start: '\x1b[35m',
  fail: '\x1b[31m',
};
const RESET = '\x1b[0m';

/* ============================== 值的格式化 ============================== */

/** 与宿主 `__fmt` 一致：任意值 → 可打印字符串。 */
export function fmtValue(v) {
  if (typeof v === 'string') return v;
  if (v === null) return 'null';
  if (v === undefined) return 'undefined';
  const t = typeof v;
  if (t === 'number' || t === 'boolean' || t === 'bigint') return String(v);
  if (t === 'function') return `[Function ${v.name || 'anonymous'}]`;
  if (t === 'symbol') return String(v);
  if (v instanceof Error) return v.stack || `${v.name}: ${v.message}`;
  try {
    return JSON.stringify(v);
  } catch {
    return String(v);
  }
}

/** 多参按空格拼接（与 `console.log` 一致）。 */
export function fmtArgs(args) {
  return args.map(fmtValue).join(' ');
}

/* ================================= 脱敏 ================================= */

/** query / JSON 字段名里命中这些**子串**就脱值（与 src/log/mod.rs 一致）。 */
const SECRET_KEYS = [
  'token',
  'password',
  'passwd',
  'pwd',
  'secret',
  'jwt',
  'ssotoken',
  'auth',
  'cookie',
  'session',
  'sid',
  'ticket',
  'signature',
  'credential',
  // 云厂商 / 网关的 API key 家族（2026-10 审计中4）。刻意写成完整词：
  // 裸 `key` 会把 `keyword` / `key=value` 这类排查用的正常参数一并误伤。
  'api_key',
  'apikey',
  'access_key',
  'private_key',
];

/** 整行脱到行尾的请求头（值里可能带 `;` 和空格，只能整行砍）。 */
const SECRET_HEADERS = [
  'authorization',
  'proxy-authorization',
  'cookie',
  'set-cookie',
  'x-auth-token',
  'x-csrf-token',
  'x-xsrf-token',
  // 中4：`X-Api-Key` 是各家网关的凭据头；`x-panel-token` 是**本项目自己**的
  // 面板令牌 —— 插件拿到它就能冒充操作者，它出现在日志里必须整行脱。
  'x-api-key',
  'x-panel-token',
];

/** 值的终止符：ASCII 的 `& # ; " ' , ) ] }` + 全角标点 + 空白。 */
function isTerminator(c) {
  return (
    c === '&' ||
    c === '#' ||
    c === ';' ||
    c === '"' ||
    c === "'" ||
    c === ',' ||
    c === ')' ||
    c === ']' ||
    c === '}' ||
    c === '）' ||
    c === '】' ||
    c === '》' ||
    c === '，' ||
    c === '。' ||
    c === '、' ||
    /\s/.test(c)
  );
}

function isSecretKey(key) {
  const k = String(key).toLowerCase();
  return SECRET_KEYS.some((s) => k.includes(s));
}

/** 单字符值不可能是凭据 —— 排除掉，否则 `PANEL_COOKIE_SECURE=1` 这种提示会被弄坏。 */
function isFlagValue(v) {
  return v.length === 1 && (/[0-9A-Za-z]/.test(v) || v === '-');
}

function isKeyChar(c) {
  return /[0-9A-Za-z]/.test(c) || c === '_' || c === '-' || c === '.';
}

/// 词边界：**ASCII** 语义，**刻意不是** Unicode 的「字母数字」。
///
/// 汉字算不算字母，直接决定 `用token=abc123` 脱不脱：Rust 侧原本用
/// `char::is_alphanumeric()`（汉字算字母 → 不算边界 → 整条漏脱），JS/Python 是
/// ASCII（算边界 → 脱掉）。三边里唯一一次实质性分叉就是它，2026-10-15 统一到 ASCII：
/// 漏脱一个令牌比多脱一点危险得多。
function isWordBoundary(s, i) {
  if (i === 0) return true;
  return !/[0-9A-Za-z_]/.test(s[i - 1]);
}

/**
 * 0) URL userinfo：`scheme://user:pass@host/x` → `scheme://***:***@host/x`。
 *
 * 审计中4：`https://alice:hunter2@host/x` 会把**账号和口令**原样留进 JSONL 一周。
 * 判定收紧到 RFC 3986 的 authority 段（与 `src/log/mod.rs::redact_url_userinfo` 逐字符一致）：
 * `://` 前必须真有 scheme，`@` 必须在 authority 内 —— 邮箱 `alice@example.com`
 * 与 path 里的 `@` 因此不会被误伤。输出形状跟输入一致（有 `:` 才是 `***:***`），
 * 所以对自己幂等。
 */
function redactUrlUserinfo(s) {
  let out = '';
  let i = 0;
  while (i < s.length) {
    if (s[i] === ':' && i + 2 < s.length && s[i + 1] === '/' && s[i + 2] === '/') {
      // 往回找 scheme 起点（ALPHA *( ALPHA / DIGIT / "+" / "-" / "." )）
      let start = i;
      while (start > 0 && /[0-9A-Za-z+\-.]/.test(s[start - 1])) start -= 1;
      const schemeOk = start < i && /[A-Za-z]/.test(s[start]);
      if (schemeOk) {
        const authStart = i + 3;
        let at = -1;
        let k = authStart;
        while (k < s.length) {
          const c = s[k];
          if (c === '@') {
            at = k;
            break;
          }
          // authority 到此为止：往后是 path / query / fragment / 正文
          if (c === '/' || c === '?' || c === '#' || c === '"' || c === "'" || c === '\\' || /\s/.test(c)) break;
          k += 1;
        }
        if (at >= 0) {
          out += s.slice(i, i + 3); // `://` 原样留着
          out += s.slice(authStart, at).includes(':') ? '***:***' : '***';
          out += '@';
          i = at + 1;
          continue;
        }
      }
    }
    out += s[i];
    i += 1;
  }
  return out;
}

/** 1) `Cookie:` / `Authorization:` … 整行值 → `***`。 */
function redactHeaders(s) {
  let out = '';
  let i = 0;
  while (i < s.length) {
    const prev = i > 0 ? s[i - 1] : '';
    // 全角左括号也要算起点：这是个中文项目，`（Cookie: …）` 比半角常见得多，
    // 只认半角就是「整条漏脱」。配套的右括号在 isTerminator 里 —— 两边必须成对。
    const boundary =
      i === 0 ||
      prev === '\n' ||
      /\s/.test(prev) ||
      ['(', '[', '{', ',', ';', '（', '【', '《'].includes(prev);
    if (boundary) {
      const lower = s.slice(i, i + 24).toLowerCase();
      let hit = null;
      for (const h of SECRET_HEADERS) {
        if (!lower.startsWith(h)) continue;
        const after = i + h.length;
        let k = after;
        while (k < s.length && (s[k] === ' ' || s[k] === '\t')) k += 1;
        if (s[k] === ':') {
          hit = [after, k];
          break;
        }
      }
      if (hit) {
        const colon = hit[1];
        // 头在括号之后（插件把 header 表打在一行里）→ 脱到配对的右括号，
        // 不然会把 `)` 一起吃掉、把行弄断；否则脱到行尾。
        const inline = i > 0 && ['(', '[', '{', '（', '【', '《'].includes(s[i - 1]);
        out += s.slice(i, colon + 1);
        let k = colon + 1;
        while (k < s.length && (s[k] === ' ' || s[k] === '\t')) {
          out += s[k];
          k += 1;
        }
        out += '***';
        while (k < s.length) {
          if (s[k] === '\n') break;
          if (inline && [')', ']', '}', '）', '】', '》'].includes(s[k])) break;
          k += 1;
        }
        i = k;
        continue;
      }
    }
    out += s[i];
    i += 1;
  }
  return out;
}

/** 2) JSON 字段 `"token": "…"` → `"***"`。 */
function redactJsonFields(s) {
  let out = '';
  let i = 0;
  while (i < s.length) {
    if (s[i] === '"') {
      const ks = i + 1;
      let j = ks;
      while (j < s.length && s[j] !== '"' && s[j] !== '\n') j += 1;
      if (s[j] === '"' && j > ks) {
        const key = s.slice(ks, j);
        let k = j + 1;
        while (k < s.length && /\s/.test(s[k])) k += 1;
        if (s[k] === ':') {
          let m = k + 1;
          while (m < s.length && /\s/.test(s[m])) m += 1;
          if (s[m] === '"' && isSecretKey(key)) {
            out += s.slice(i, k + 1);
            out += '"***"';
            let n = m + 1;
            while (n < s.length && s[n] !== '"' && s[n] !== '\n') n += 1;
            i = n < s.length ? n + 1 : n;
            continue;
          }
        }
      }
    }
    out += s[i];
    i += 1;
  }
  return out;
}

/** 3) 裸的 `key=value`（`?`/`&` 拼在 URL 上的那种也算）→ `***`。 */
function redactKv(s) {
  let out = '';
  let i = 0;
  while (i < s.length) {
    if (isWordBoundary(s, i) && /[A-Za-z_]/.test(s[i])) {
      const ks = i;
      let j = i;
      while (j < s.length && isKeyChar(s[j])) j += 1;
      if (s[j] === '=' && j > ks) {
        const key = s.slice(ks, j);
        let ve = j + 1;
        while (ve < s.length && !isTerminator(s[ve])) ve += 1;
        const value = s.slice(j + 1, ve);
        if (isSecretKey(key) && !isFlagValue(value)) {
          out += s.slice(ks, j + 1);
          out += '***';
          i = ve;
          continue;
        }
        // 不是敏感 key：把 key 整体吐出去，从 '=' 继续
        out += s.slice(i, j);
        i = j;
        continue;
      }
    }
    out += s[i];
    i += 1;
  }
  return out;
}

/** 4) `Bearer xxx` → `Bearer ***`。 */
function redactBearer(s) {
  let out = '';
  let i = 0;
  while (i < s.length) {
    if (isWordBoundary(s, i) && s.slice(i, i + 6).toLowerCase() === 'bearer') {
      const after = i + 6;
      if (after < s.length && /\s/.test(s[after])) {
        out += s.slice(i, after + 1);
        out += '***';
        let k = after + 1;
        while (k < s.length && !/\s/.test(s[k]) && s[k] !== '"' && s[k] !== "'") k += 1;
        i = k;
        continue;
      }
    }
    out += s[i];
    i += 1;
  }
  return out;
}

/** 5) 11 位手机号 → `136****6001`（保留首 3 末 4，仍能区分账号）。 */
function redactPhones(s) {
  let out = '';
  let i = 0;
  while (i < s.length) {
    if (s[i] === '1' && (i === 0 || !/[0-9]/.test(s[i - 1]))) {
      let j = i;
      while (j < s.length && /[0-9]/.test(s[j])) j += 1;
      const len = j - i;
      const nextIsDigit = j < s.length && /[0-9]/.test(s[j]);
      if (len === 11 && s[i + 1] >= '3' && s[i + 1] <= '9' && !nextIsDigit) {
        out += s.slice(i, i + 3) + '****' + s.slice(i + 7, j);
        i = j;
        continue;
      }
    }
    out += s[i];
    i += 1;
  }
  return out;
}

/**
 * 把消息里能识别的凭据 / 手机号换成 `***` / `****`。
 *
 * 规则顺序与 `src/log/mod.rs::redact` 完全一致（顺序有意义：JSON 那条必须排在
 * 裸 kv 前面，否则 `{"token":"abc"}` 里那个 `"` 会被当成 key 的分隔符）。
 */
export function redact(msg) {
  let s = String(msg);
  // URL userinfo 排最前面：它是**结构**脱敏（userinfo 里可能含 `=` `&`），
  // 先换成 `***`，后面的 kv / Bearer 才不会在密码串里做二次解释；也保证幂等。
  s = redactUrlUserinfo(s);
  s = redactHeaders(s);
  s = redactJsonFields(s);
  s = redactKv(s);
  s = redactBearer(s);
  s = redactPhones(s);
  return s;
}

/* ================================= 时间 ================================= */

function pad(n, w = 2) {
  return String(n).padStart(w, '0');
}

/** `YYYY-MM-DD HH:MM:SS.mmm`（**本地时间**，与宿主一致）。 */
export function stamp(d = new Date()) {
  return (
    `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())} ` +
    `${pad(d.getHours())}:${pad(d.getMinutes())}:${pad(d.getSeconds())}.${pad(d.getMilliseconds(), 3)}`
  );
}

/** JSONL 里的 ts：带毫秒与本地偏移的 RFC3339。 */
export function rfc3339(d = new Date()) {
  const offMin = -d.getTimezoneOffset();
  const sign = offMin >= 0 ? '+' : '-';
  const abs = Math.abs(offMin);
  return (
    `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}T` +
    `${pad(d.getHours())}:${pad(d.getMinutes())}:${pad(d.getSeconds())}.${pad(d.getMilliseconds(), 3)}` +
    `${sign}${pad(Math.floor(abs / 60))}:${pad(abs % 60)}`
  );
}

/* =============================== 文件出口 =============================== */

const FILE_PREFIX = 'plugin';

class FileSink {
  constructor(dir, keepDays) {
    this.dir = dir;
    this.keepDays = keepDays;
    this.date = null;
    this.fd = null;
    this.written = 0;
    this.dropped = 0;
    try {
      mkdirSync(dir, { recursive: true });
    } catch {
      this.disabled = true;
    }
    if (!this.disabled) this.cleanup();
  }

  path(dateStr) {
    return join(this.dir, `${FILE_PREFIX}-${dateStr}.jsonl`);
  }

  write(line) {
    if (this.disabled) {
      this.dropped += 1;
      return;
    }
    const today = stamp().slice(0, 10);
    if (this.date !== today) {
      // 换天（或首次写入）：靠写入时判日期，不在半夜凭空造空文件
      if (this.fd !== null) {
        try {
          closeSync(this.fd);
        } catch {
          /* 关不掉就算了 */
        }
        this.fd = null;
      }
      try {
        this.fd = openSync(this.path(today), 'a');
        this.date = today;
        this.cleanup();
      } catch {
        this.disabled = true;
        this.dropped += 1;
        return;
      }
    }
    try {
      // 逐行同步写：**必须有**。面板侧那条「BufWriter 把数据攥在手里、文件一直是
      // 0 字节」的坑在这里同样成立 —— 而文件存在的全部意义就是给外部的
      // `tail -f` / `jq` 看，晚一小时等于没有。
      writeSync(this.fd, line + '\n');
      this.written += 1;
    } catch {
      this.dropped += 1;
    }
  }

  cleanup() {
    if (this.disabled || !this.keepDays) return;
    let names = [];
    try {
      names = readdirSync(this.dir);
    } catch {
      return;
    }
    const today = new Date();
    today.setHours(0, 0, 0, 0);
    const oldest = new Date(today.getTime() - (this.keepDays - 1) * 86400000);
    const head = `${FILE_PREFIX}-`;
    for (const name of names) {
      if (!name.startsWith(head) || !name.endsWith('.jsonl')) continue;
      const day = name.slice(head.length, -'.jsonl'.length);
      const t = Date.parse(`${day}T00:00:00`);
      if (Number.isNaN(t)) continue;
      if (t < oldest.getTime()) {
        try {
          unlinkSync(join(this.dir, name));
        } catch {
          /* 删不掉不算错 */
        }
      }
    }
  }

  close() {
    if (this.fd !== null) {
      try {
        closeSync(this.fd);
      } catch {
        /* ignore */
      }
      this.fd = null;
    }
  }
}

/* ================================ Logger ================================ */

/** 全局配置（首次用到时读环境变量）。 */
const cfg = {
  loaded: false,
  min: 'debug',
  dir: './logs',
  keepDays: 7,
  color: null,
  sink: null,
};

function loadConfig() {
  if (cfg.loaded) return;
  cfg.loaded = true;
  const env = process.env;

  // `warning` 是 `warn` 的别名：Rust 侧 `Level::parse` 认它，这里不认的话
  // 同一个词在两种宿主里有两种命运（一个当选 warn、一个静默退到 debug）。
  let lv = (env.HOST_LOG_LEVEL || '').toLowerCase();
  if (lv === 'warning') lv = 'warn';
  cfg.min = RANK[lv] !== undefined ? lv : 'debug';

  const dir = env.HOST_LOG_DIR;
  if (dir === undefined) cfg.dir = './logs';
  else if (['off', '0', 'false', '-', ''].includes(dir.toLowerCase())) cfg.dir = '';
  else cfg.dir = dir;

  const keep = parseInt(env.HOST_LOG_KEEP_DAYS || '', 10);
  // clamp 到 >= 0：`keepDays - 1` 在负数时会让保留窗口落到**未来**，
  // 于是「比窗口旧」对当天的文件也成立 —— 一启动就把刚写的日志删掉。
  // Rust 侧用的是 u64 + saturating_sub，天然不会这样。
  cfg.keepDays = Number.isFinite(keep) ? Math.max(0, keep) : 7;

  if (env.HOST_LOG_COLOR === '0') cfg.color = false;
  else if (env.HOST_LOG_COLOR) cfg.color = true;

  if (cfg.dir) cfg.sink = new FileSink(cfg.dir, cfg.keepDays);
}

function colorsEnabled() {
  loadConfig();
  if (cfg.color !== null) return cfg.color;
  if (process.env.NO_COLOR !== undefined) return false;
  if (process.env.FORCE_COLOR !== undefined) return process.env.FORCE_COLOR !== '0';
  // 适配器的日志走 stderr（见文件头）—— 是不是 tty 要看 stderr
  return !!process.stderr.isTTY;
}

function levelEnabled(type) {
  loadConfig();
  return RANK[LEVEL_OF[type]] >= RANK[cfg.min];
}

/** 一条日志 → 显示行（与宿主 `LogEntry::line` 逐字符一致）。 */
export function lineOf(entry) {
  return `${entry.ts} [${entry.type}] [${entry.scope}] ${entry.msg}`;
}

/** 一条日志 → JSONL 的对象（字段与宿主 `LogEntry::to_json` 一致）。 */
export function jsonOf(entry) {
  const o = {
    ts: entry.rfc,
    type: entry.type,
    level: LEVEL_OF[entry.type],
    scope: entry.scope,
  };
  if (entry.plugin) o.plugin = entry.plugin;
  if (entry.run) o.run = entry.run;
  o.msg = entry.msg;
  return o;
}

/** 测试用：把配置重置回去（读环境变量的动作只做一次）。 */
export function resetConfigForTest() {
  cfg.loaded = false;
  cfg.sink?.close();
  cfg.sink = null;
  cfg.color = null;
}

/**
 * 造一个 logger。
 *
 * 返回的是**可调用对象**（`lg('x')` 等价 `lg.info('x')`），因为宿主侧
 * `log(...)` 就是一个函数；这样 `const L = createLogger(); L('a'); L.warn('b')`
 * 两种写法都通。
 */
export function createLogger({ scope, plugin, run } = {}) {
  const sc = scope || process.env.HOST_LOG_SCOPE || process.env.PANEL_PLUGIN || 'plugin';

  const emit = (type, args) => {
    if (!levelEnabled(type)) return;
    const now = new Date();
    const entry = {
      ts: stamp(now),
      rfc: rfc3339(now),
      type,
      scope: sc,
      plugin: plugin || null,
      run: run || null,
      // ⚠ 脱敏在**构造条目时**做（唯一入口），不是展示时做 —— 缓冲、文件、
      // 终端拿到的全都是脱敏后的，不存在「哪条路径忘了脱」。
      msg: redact(fmtArgs(args)),
    };

    const line = lineOf(entry);
    // 彩色行**不是**在纯文本行上套颜色：宿主把 `[type]` 提到了最前面
    // （`{色}[{type}]{复位} {时间} [{scope}] {msg}`），这样扫一眼颜色就能定位级别，
    // 不用先跳过时间戳。见 `src/log/console.rs`，三份实现必须一致。
    const text = colorsEnabled()
      ? `${COLORS[type]}[${type}]${RESET} ${entry.ts} [${entry.scope}] ${entry.msg}`
      : line;
    try {
      process.stderr.write(text + '\n');
    } catch {
      /* 终端没了也不该让插件挂掉 */
    }

    loadConfig();
    cfg.sink?.write(JSON.stringify(jsonOf(entry)));
  };

  const lg = (...args) => emit('info', args);
  for (const t of TYPES) lg[t] = (...args) => emit(t, args);
  lg.child = (sub) => createLogger({ scope: sc ? `${sc}:${sub}` : String(sub), plugin, run });
  lg.withRun = (r) => createLogger({ scope: sc, plugin, run: r });
  lg.scope = sc;
  /** 直接发一条原始条目（运行器内部用：它自己的 scope 与插件不同）。 */
  lg.emit = emit;
  return lg;
}

/** 测试与运行器用：拿当前配置（只读）。 */
export function logState() {
  loadConfig();
  return { min: cfg.min, dir: cfg.dir, keepDays: cfg.keepDays, written: cfg.sink?.written ?? 0, dropped: cfg.sink?.dropped ?? 0 };
}
