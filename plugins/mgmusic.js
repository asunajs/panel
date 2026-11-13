// 由 cmusic-others/mgmusic-plugin/llrt/mgmusic.js 复制生成（panel-llrt 0.4.x 版）—— **不要手改这个文件**。
//
//   改代码：编辑插件项目的 llrt/，然后 `npm run build:panelllrt`
//   mgmusic-plugin 0.3.4 · 插件 `mgmusic`
// plugins/mgmusic.js —— 咪咕音乐金币中心（新面板 0.4.x / LLRT 运行时）
//
// 这份是**老面板 0.3.x（QuickJS）插件 v0.3.4 的移植版**。
// 业务逻辑（协议、签名 mgSign、ce、证书、评论、窗口任务、幂等）**一行都没动**，
// 只做运行环境适配 —— 字节级协议是上一轮实测出来的，改它没有意义。
//   · 老文件：cmusic-others/mgmusic-plugin/mgmusic.js（v0.3.4，2131 行）
//   · 现在只依赖 ECMAScript + `panel:host`（既没有 `host-api`，也没有 `node:fs`）
//
// 从 v0.3.4 迁过来改了哪些**契约**（只列环境层）：
//   1. `import {...} from 'host-api'` → `import host from 'panel:host'`。新的注册类 op
//      全是 async（要 await）；同步步骤机那几处用 `*_sync` 同步版，代码结构没变。
//      日志级别：success → info、fail → error（0.4.x 只有 log/info/warn/error）。
//   2. **存储**：老版把 config.json / login-pending.json 写在 cwd 相对路径上；新面板
//      **不给插件设 cwd**（相对路径会落到面板进程的工作目录），所以改用宿主 sqlite 的
//      `state` 表（和 mcloud 同一张表，键自带命名空间）：
//        config.json        → `mgmusic.config`
//        login-pending.json → `mgmusic.login_pending`
//      表在模块加载期自己建（新面板不预建，CREATE TABLE IF NOT EXISTS 幂等）。
//   3. **播种**（只在 `mgmusic.config` 不存在时跑一次）：`await host.self_config()` 非空 →
//      校验后写进 state；空 → DEFAULTS。这是 `plugin-data/<插件>/config.json` 的正式入口：
//      迁移老账号 = 把老面板的 config.json 复制到
//      `<PANEL_DATA_DIR>/plugin-data/mgmusic/config.json`。**之后一切读写只走 state**，
//      那份文件只当导入源，不回写。
//   4. **10 条老路由 → 一个 `host.register_web` 分发器**（按 `evt.rel` + `evt.method`）。
//      URL（`/plugins/mgmusic/api/*`）与 JSON 载荷形状**保持老面板原样**，前端不用改。
//      注意 WebUI 请求和 cron 共用同一个 busy 锁、插件侧预算是 30s：界面上的「立即跑一次」
//      该打面板任务接口 `POST /api/tasks/mgmusic/daily/run`，别打本插件的 `/run`（只作兼容保留）。
//   5. **任务只有一条**：`register_cron('daily', '0 5 0,12,22 * * *', 120000)`（6 段带秒）
//      + 同名的 `register_handler('daily', …)`（新面板的 cron 只登记调度、不绑函数）。
//      老版的 sign / comment / report 三个 handler **不注册成任务**了 —— 它们在任务页会变成
//      三行多余的手动任务；现在只走 HTTP 路由（界面按钮打 /plugins/mgmusic/api/{sign,comment,report}）。
//      **不写任务摘要**（不调 `host.set_job_summary`）：任务行只该有名字/计划/预算/状态，
//      每次跑出来的一串数字属于「运行记录」和日志 —— 写进任务行看着就像漏出来的日志
//      （用户 2026-11-13 指出）。老版塞在 `register_cron` opts 里的 summary 也就此丢掉。
//   6. **nav**：`register_nav` → `await host.sidebar({ title: '咪咕音乐', order: 80 })`；
//      `worker_hint('exclusive')` 删掉（新面板一插件一 runtime）。
//   7. **定时器**：新面板 `setInterval` 直接抛错、`setTimeout` 上限 5000ms。老代码只用
//      setTimeout 做任务间隔（默认 1–3s），保留；配置里写 >5s 的间隔按 5s 等（唯一一处
//      时间上的收敛，见 taskPause）。要长等请用 `await host.sleep(ms)`。
//
// ── 存储（接手的人看这两句就够）
//   · 配置与临时数据全在**宿主 sqlite 的 `state` 表**里，三个键：
//       `mgmusic.config`         ← 老 config.json（账号 devices / topup_min / cert / tls / comment …）
//       `mgmusic.login_pending`  ← 老 login-pending.json（短信登录中间态；只在成功后删一次）
//     表由本文件在**加载期**自己建（`CREATE TABLE IF NOT EXISTS state`，幂等；mcloud 也是插件建的）。
//   · **播种**用 `await host.self_config()`：state 里没有 `mgmusic.config` 时读一次 ——
//     非空就 validateConfig 后写进 state，空就用 DEFAULTS。迁移老账号的正式通路是
//     「把老面板的 `.../plugin-data/mgmusic/config.json` 复制到
//     `<PANEL_DATA_DIR>/plugin-data/mgmusic/config.json` 再重载插件」。
//     播种之后**只读 state**，那份文件只当导入源，不回写。
//
// 任务：
//   daily  —— 00:05 / 12:05 / 22:05：login-access + qsq + zyd-sign + 窗口打卡
//             + 听歌补报到 topup_min + 领取已达档位
//   report —— 手动/面板建任务：补报一次 song-duration 并领奖
//
// 说明：
//   · 协议结论基于 2026-10-15 静态 + 2026-11-05 H5 抓包实测
//   · songTime 按 ce 记账；上报/领奖必须同一 ce
//   · 所有接口幂等；重复 report 无副作用
//   · 咪咕接口对 Accept-Encoding 敏感，统一 identity，避免 gzip 无法解
import host from 'panel:host';

const VERSION = '0.4.0';
const PLUGIN = 'mgmusic';
const API = '/plugins/mgmusic/api'; // 老面板用它拼 register_route；0.4.x 由 evt.rel 分发，这里只作说明
const RUN_BUDGET_MS = 100000; // 面板 cron 预算 120s，内部留 20s 收尾

// ──────────────────────────────────────────────────────────── 日志
//
// 老 host-api 是同步的具名导入（info/success/fail/warn/error）；新面板只有
// log/info/warn/error 四级、而且返回 Promise。这一层垫片让下面几百处调用原样不动。
// success / fail 在 0.4.x 没有对应级别，按契约映射成 info / error。
function emitLog(level, msg) {
    try {
        // 日志失败不该让业务失败，挂掉的 promise 也不该变成 unhandled rejection。
        void Promise.resolve(host[level](String(msg))).catch(function () {});
    } catch (e) {
        /* 忽略 */
    }
}
function info(msg) { emitLog('info', msg); }
function warn(msg) { emitLog('warn', msg); }
function error(msg) { emitLog('error', msg); }
function success(msg) { emitLog('info', msg); }
function fail(msg) { emitLog('error', msg); }

// ──────────────────────────────────────────────────────────── 状态存储（state 表）
//
// 参数是 JS 数组，返回 {rows, truncated?}。同步版占住本插件的引擎线程 —— 正是老代码要的
// 「同步步骤机」语义，所以 loadConfig / saveConfig 保持同步、返回形状不变。
const ST_CONFIG = 'mgmusic.config';
const ST_LOGIN_PENDING = 'mgmusic.login_pending';
// 「播种做过没有」的标记：值 'file'（从 config.json 导入过）或 'state'（本来就有账号）。
// 有它就不再从导入源覆盖 state —— 否则用户在界面上删光账号会被文件偷偷复活。
const ST_SEED_MARK = 'mgmusic.seed_marker';

// 新面板不预建 state（mcloud 也是插件自己建的）。加载期建一次，幂等。
// updated_at 是 TEXT：写 `String(Date.now())`，和 mcloud 一致。
host.sqlite_exec_sync(
    'CREATE TABLE IF NOT EXISTS state (key TEXT PRIMARY KEY, value TEXT NOT NULL, updated_at TEXT NOT NULL)',
    [],
);

function stateGet(key) {
    const r = host.sqlite_query_sync('SELECT value FROM state WHERE key = ?1', [key]);
    const rows = (r && Array.isArray(r.rows)) ? r.rows : [];
    if (!rows.length) return null;
    const v = rows[0].value;
    return (v === undefined || v === null) ? null : String(v);
}

function stateSet(key, text) {
    host.sqlite_exec_sync(
        'INSERT INTO state (key, value, updated_at) VALUES (?1, ?2, ?3) '
        + 'ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at',
        [key, String(text), String(Date.now())],
    );
}

function stateDel(key) {
    host.sqlite_exec_sync('DELETE FROM state WHERE key = ?1', [key]);
}

const BASE = 'https://app.c.nf.migu.cn';
const COMMENT_BASE = 'https://app.pd.nf.migu.cn';
const ACTIVITY = 'MAC_KF_CBH_JBZX';
const REFERER = 'https://h5.nf.migu.cn/app/v4/n/gold-coin-center/index.html';
const UA = 'Mozilla/5.0 (iPad; CPU OS 18_7 like Mac OS X) AppleWebKit/605.1.15 '
    + '(KHTML, like Gecko) Mobile/15E148 migumobilemusic/8.9.4';

const PASSPORT = 'https://passport.migu.cn';
const LOGIN_PAGE = 'https://passport.migu.cn/login?sourceid=100001&apptype=0&authType=MiguPassport&display=web&logintype=1';
const LOGIN_UA = 'Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 '
    + '(KHTML, like Gecko) Chrome/124.0.0.0 Safari/537.36';

const SD_PATH = '/act-message/report/song-duration/v1';
const SD_UA = 'Mozilla/5.0 (Linux; U; Android 16; zh-cn; M2102K1C Build/SKQ1.211006.001) AppleWebKit/537.36 '
    + '(KHTML, like Gecko) Version/4.0 Chrome/148.0.7778.215 Mobile Safari/537.36';

// ce / song-duration body 共用的加法 Vigenère key（16 字节 ASCII）
const MG_KEY = 'ccTWaprX2aWmTIgA';

// 签名只追加证书 hex 字符串的前 32 个 ASCII 字符。
// 这里只嵌这 32 个字符；要换证书时在配置里填完整 cert 即可覆盖。
const DEFAULT_CERT_PREFIX = '308202333082019ca00302010202044d';

// 任务窗口（小时，左闭右开）；null = 全天可报
const REPORT_TASKS = [
    ['qsq', '每日浏览社区频道', 8, null],
    ['zyd-sign', '前往中国移动APP', 5, null],
    ['noon-check', '每日饭点打卡', 3, [12, 13]],
    ['night-check', '每日晚安打卡', 3, [22, 23]],
];

// 「每日评论任意歌曲」任务 id；评论是 App 侧评论事件记账，不走 task-report。
const COMMENT_TASK_ID = 'c840d039adfd4c04920d92c04120baa8';

const DEFAULTS = {
    devices: [],
    topup_min: 30,
    report_ms: 60000,
    timeout_ms: 15000,
    task_interval: {
        min_ms: 1000,
        max_ms: 3000,
    },
    cert: '',
    tls: '', // '' = 默认 rustls；'native' = 平台 TLS（老网关用）
    comment: {
        enabled: true,
        song_id: '600929000006498997',
        content: '好听',
        resource_type: '2',
        comment_info: '',
        target_comment_id: '',
        log_id: '',
        fk_token: '',
        source: 'app',
        mode: 'android', // android = Android V005 头（实测可评论）；ios = iOS 头；h5 = H5 头
        path: '/user/api/comment/add/v2.0',
        body: null,
        extra: {},
    },
};

const HEX = '0123456789abcdef';

// ──────────────────────────────────────────────────────────── 字节 / 编码

/** 字符串 → UTF-8 字节。QuickJS 没有 TextEncoder，这里按码点手写。 */
function utf8(s) {
    const out = [];
    for (let i = 0; i < s.length; i += 1) {
        let cp = s.charCodeAt(i);
        if (cp >= 0xd800 && cp <= 0xdbff && i + 1 < s.length) {
            const low = s.charCodeAt(i + 1);
            if (low >= 0xdc00 && low <= 0xdfff) {
                cp = 0x10000 + ((cp - 0xd800) << 10) + (low - 0xdc00);
                i += 1;
            } else {
                cp = 0xfffd;
            }
        } else if (cp >= 0xd800 && cp <= 0xdfff) {
            cp = 0xfffd;
        }
        if (cp < 0x80) {
            out.push(cp);
        } else if (cp < 0x800) {
            out.push(0xc0 | (cp >> 6), 0x80 | (cp & 0x3f));
        } else if (cp < 0x10000) {
            out.push(0xe0 | (cp >> 12), 0x80 | ((cp >> 6) & 0x3f), 0x80 | (cp & 0x3f));
        } else {
            out.push(0xf0 | (cp >> 18), 0x80 | ((cp >> 12) & 0x3f),
                0x80 | ((cp >> 6) & 0x3f), 0x80 | (cp & 0x3f));
        }
    }
    return Uint8Array.from(out);
}

function toHexUpper(bytes) {
    let s = '';
    for (let i = 0; i < bytes.length; i += 1) {
        const b = bytes[i];
        s += HEX[(b >> 4) & 15] + HEX[b & 15];
    }
    return s.toUpperCase();
}

/** 加法 Vigenère（字节 + key[i%16]），输出大写 hex；ce 与 song-duration body 共用。 */
function vigenereHex(text, key) {
    const b = utf8(text);
    const k = utf8(key);
    let s = '';
    for (let i = 0; i < b.length; i += 1) {
        const v = (b[i] + k[i % k.length]) & 0xff;
        s += HEX[(v >> 4) & 15] + HEX[v & 15];
    }
    return s.toUpperCase();
}

function mgEncode(text) {
    return vigenereHex(text, MG_KEY);
}

function makeCe(uid, msisdn, deviceId) {
    // 2026-11-05 实测 H5 ce 明文：deviceId → uid → msisdn，没有 ip。
    const pt = 'deviceId=' + deviceId + '&uid=' + uid + '&msisdn=' + msisdn;
    return vigenereHex(pt, MG_KEY);
}

// ──────────────────────────────────────────────────────────── MD5 变体

const MD5_S = [
    7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22,
    5, 9, 14, 20, 5, 9, 14, 20, 5, 9, 14, 20, 5, 9, 14, 20,
    4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23,
    6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21,
];

const MD5_K = (() => {
    const k = new Uint32Array(64);
    for (let i = 0; i < 64; i += 1) {
        k[i] = Math.floor(Math.abs(Math.sin(i + 1)) * 4294967296) >>> 0;
    }
    return k;
})();

// 注意：这是本 so 的特征 —— 标准 MD5 IV 的 B/D 互换了
const MD5_IV = [0x67452301, 0x10325476, 0x98badcfe, 0xefcdab89];

function rotl(x, n) {
    return ((x << n) | (x >>> (32 - n))) >>> 0;
}

function md5Block(state, block) {
    let a = state[0];
    let b = state[1];
    let c = state[2];
    let d = state[3];
    const aa = a;
    const bb = b;
    const cc = c;
    const dd = d;
    const m = new Uint32Array(16);
    for (let j = 0; j < 16; j += 1) {
        const o = j * 4;
        m[j] = ((block[o] | (block[o + 1] << 8) | (block[o + 2] << 16) | (block[o + 3] << 24)) >>> 0);
    }
    for (let i = 0; i < 64; i += 1) {
        let f;
        let g;
        if (i < 16) {
            f = (b & c) | (~b & d);
            g = i;
        } else if (i < 32) {
            f = (d & b) | (~d & c);
            g = (5 * i + 1) & 15;
        } else if (i < 48) {
            f = b ^ c ^ d;
            g = (3 * i + 5) & 15;
        } else {
            f = c ^ (b | ~d);
            g = (7 * i) & 15;
        }
        f = (f + a + MD5_K[i] + m[g]) >>> 0;
        a = d;
        d = c;
        c = b;
        b = (b + rotl(f, MD5_S[i])) >>> 0;
    }
    return [
        (aa + a) >>> 0,
        (bb + b) >>> 0,
        (cc + c) >>> 0,
        (dd + d) >>> 0,
    ];
}

function whiten(bytes, s) {
    const out = new Uint8Array(bytes.length);
    for (let i = 0; i < bytes.length; i += 1) {
        s = (s + (bytes[i] << 1) + 1) & 0xff;
        out[i] = s;
    }
    return { bytes: out, s: s };
}

/**
 * 签名流程：start(cert) → append(each) → end()。
 *
 * 数据 = 所有 append + cert 前 32 个 ASCII 字符；
 * 每个 64B 块先做 s=(s+2b+1)&0xff 的链式白化，再走 MD5 压缩；
 * 尾巴补 0x80 + 0 到 64B，**不写 64 位长度字段**；尾巴为空则连 0x80 都不补。
 */
function mgSign(appends, cert) {
    const certPrefix = (cert && cert.length >= 32 ? cert : DEFAULT_CERT_PREFIX).slice(0, 32);
    const chunks = appends.concat([utf8(certPrefix)]);
    let total = 0;
    for (let i = 0; i < chunks.length; i += 1) total += chunks[i].length;
    const data = new Uint8Array(total);
    let off = 0;
    for (let i = 0; i < chunks.length; i += 1) {
        data.set(chunks[i], off);
        off += chunks[i].length;
    }

    let state = MD5_IV.slice();
    let s = 0x7a;
    const full = Math.floor(data.length / 64);
    for (let bi = 0; bi < full; bi += 1) {
        const w = whiten(data.subarray(bi * 64, (bi + 1) * 64), s);
        s = w.s;
        state = md5Block(state, w.bytes);
    }
    const rem = data.subarray(full * 64);
    if (rem.length > 0) {
        const w = whiten(rem, s);
        s = w.s;
        const last = new Uint8Array(64);
        last.set(w.bytes);
        last[rem.length] = 0x80;
        state = md5Block(state, last);
    }

    // 16 字节 state 按小端逐 word 转 hex
    let out = '';
    for (let i = 0; i < 4; i += 1) {
        const v = state[i];
        for (let j = 0; j < 4; j += 1) {
            const byte = (v >>> (8 * j)) & 0xff;
            out += HEX[(byte >> 4) & 15] + HEX[byte & 15];
        }
    }
    return out.toUpperCase();
}

// ──────────────────────────────────────────────────────────── 工具

function clampInt(v, min, max, def) {
    const n = Number(v);
    if (!Number.isFinite(n)) return def;
    return Math.min(max, Math.max(min, Math.floor(n)));
}

function deviceLabel(dev, i) {
    return (dev && dev.label) ? String(dev.label) : ('device-' + (i + 1));
}

function mask(v, head, tail) {
    const s = String(v === undefined || v === null ? '' : v);
    if (s.length <= head + tail) return s.slice(0, 2) + '***';
    return s.slice(0, head) + '***' + s.slice(-tail);
}

function parseJsonSafe(text) {
    try {
        return JSON.parse(text);
    } catch (e) {
        return null;
    }
}

function apiCode(data) {
    if (!data || data.code === undefined || data.code === null) return '';
    return String(data.code);
}

function apiInfo(data) {
    if (!data) return '';
    const v = data.info !== undefined ? data.info
        : (data.message !== undefined ? data.message : data.msg);
    return v === undefined || v === null ? '' : String(v);
}

function uuidV4() {
    let s = '';
    for (let i = 0; i < 36; i += 1) {
        if (i === 8 || i === 13 || i === 18 || i === 23) {
            s += '-';
        } else if (i === 14) {
            s += '4';
        } else if (i === 19) {
            s += HEX[(Math.random() * 4 | 0) + 8];
        } else {
            s += HEX[Math.random() * 16 | 0];
        }
    }
    return s;
}


// ──────────────────────────────────────────────────────────── 短信登录（passport 网页版）

const lastLoginSendAt = {};
let pendingLogin = null;

const B64 = 'ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/';

function base64Bytes(bytes) {
    let out = '';
    for (let i = 0; i < bytes.length; i += 3) {
        const b0 = bytes[i];
        const b1 = i + 1 < bytes.length ? bytes[i + 1] : 0;
        const b2 = i + 2 < bytes.length ? bytes[i + 2] : 0;
        out += B64.charAt(b0 >> 2);
        out += B64.charAt(((b0 & 3) << 4) | (b1 >> 4));
        out += i + 1 < bytes.length ? B64.charAt(((b1 & 15) << 2) | (b2 >> 6)) : '=';
        out += i + 2 < bytes.length ? B64.charAt(b2 & 63) : '=';
    }
    return out;
}

function modPow(base, exp, mod) {
    let r = 1n;
    let b = base % mod;
    let e = exp;
    while (e > 0n) {
        if ((e & 1n) === 1n) r = (r * b) % mod;
        b = (b * b) % mod;
        e >>= 1n;
    }
    return r;
}

/** RSA PKCS#1 v1.5 公钥加密；passport 网页 JS 的 RSAKey.encrypt 返回 hex，不是 Base64。 */
function rsaEncrypt(text, modulusHex, exponentHex) {
    let hex = String(modulusHex || '').replace(/^0x/i, '').replace(/\s+/g, '');
    if (hex.length % 2 === 1) hex = '0' + hex;
    // modulus 是 1024-bit，JSON 里常带一个符号位 00；去掉它才是真正的 k=128
    if (hex.length > 256 && hex.slice(0, 2) === '00') hex = hex.slice(2);
    const k = hex.length / 2;
    if (k < 64 || k > 512) throw new Error('passport 公钥长度异常: ' + hex.length);
    const n = BigInt('0x' + hex);
    const e = BigInt('0x' + String(exponentHex || '010001'));
    const msg = utf8(String(text));
    if (msg.length > k - 11) throw new Error('RSA 明文过长');
    const em = new Uint8Array(k);
    em[0] = 0;
    em[1] = 2;
    let pos = 2;
    while (pos < k - msg.length - 1) {
        let b = 0;
        while (b === 0) b = Math.floor(Math.random() * 256);
        em[pos] = b;
        pos += 1;
    }
    em[pos] = 0;
    pos += 1;
    for (let i = 0; i < msg.length; i += 1) em[pos + i] = msg[i];
    let m = 0n;
    for (let i = 0; i < em.length; i += 1) m = (m << 8n) + BigInt(em[i]);
    let c = modPow(m, e, n);
    const out = new Uint8Array(k);
    for (let i = k - 1; i >= 0; i -= 1) {
        out[i] = Number(c & 255n);
        c >>= 8n;
    }
    let hexOut = '';
    for (let i = 0; i < out.length; i += 1) {
        hexOut += HEX[(out[i] >> 4) & 15] + HEX[out[i] & 15];
    }
    return hexOut;
}

function loginHeaders(extra) {
    return Object.assign({
        'user-agent': LOGIN_UA,
        accept: 'application/json, text/javascript, */*; q=0.01',
        'accept-language': 'zh-CN,zh;q=0.9',
        'accept-encoding': 'identity',
        referer: LOGIN_PAGE,
        origin: 'https://passport.migu.cn',
        'x-requested-with': 'XMLHttpRequest',
    }, extra || {});
}

function passportFetch(method, path, body, extra) {
    return fetchRaw(method, PASSPORT + path, loginHeaders(extra), body,
        { timeout_ms: 15000, tls: '' });
}

function setCookieHeader(res) {
    const h = (res && res.headers) || {};
    let raw = h['set-cookie'] || h['Set-Cookie'] || '';
    if (Array.isArray(raw)) raw = raw.join('\n');
    if (typeof raw !== 'string' || !raw) return '';
    const out = [];
    const parts = raw.split(/\n|,(?=\s*[A-Za-z0-9_-]+=)/);
    for (let i = 0; i < parts.length; i += 1) {
        const m = parts[i].match(/^\s*([^=;,\s]+)=([^;\n,]*)/);
        if (m && !/^(path|expires|max-age|domain|secure|httponly|samesite)$/i.test(m[1])) {
            out.push(m[1] + '=' + m[2]);
        }
    }
    return out.join('; ');
}

function mergeCookies() {
    const map = {};
    for (let i = 0; i < arguments.length; i += 1) {
        const raw = String(arguments[i] || '');
        const parts = raw.split(';');
        for (let j = 0; j < parts.length; j += 1) {
            const m = parts[j].match(/^\s*([^=;\s]+)=([\s\S]*)$/);
            if (m && m[1] && !/^(path|expires|max-age|domain|secure|httponly|samesite)$/i.test(m[1])) {
                map[m[1]] = m[2];
            }
        }
    }
    return Object.keys(map).map(function (k) { return k + '=' + map[k]; }).join('; ');
}

function findUid(obj, depth) {
    if (obj === null || obj === undefined || depth > 6) return '';
    if (Array.isArray(obj)) {
        for (let i = 0; i < obj.length; i += 1) {
            const u = findUid(obj[i], depth + 1);
            if (u) return u;
        }
        return '';
    }
    if (typeof obj !== 'object') return '';
    const keys = Object.keys(obj);
    for (let i = 0; i < keys.length; i += 1) {
        const k = keys[i];
        if (/^(uid|userId|user_id|passportUid|accountId)$/i.test(k)) {
            const v = obj[k];
            if (typeof v === 'string' || typeof v === 'number') {
                const s = String(v).trim();
                if (s.length >= 6 && s.length <= 128) return s;
            }
        }
    }
    for (let i = 0; i < keys.length; i += 1) {
        const u = findUid(obj[keys[i]], depth + 1);
        if (u) return u;
    }
    return '';
}

function uidFromText(text) {
    const s = String(text || '');
    const pats = [
        /"uid"\s*:\s*"([^"]+)"/i,
        /"userId"\s*:\s*"([^"]+)"/i,
        /[?&]uid=([^&"'\s]+)/i,
        /\buid\s*[:=]\s*['"]([^'"]+)['"]/i,
    ];
    for (let i = 0; i < pats.length; i += 1) {
        const m = s.match(pats[i]);
        if (m && m[1]) {
            let v = m[1];
            try { v = decodeURIComponent(v); } catch (e) { /* ignore */ }
            if (v.length >= 6 && v.length <= 128) return v;
        }
    }
    return '';
}

function uidFromCookies(cookie) {
    const parts = String(cookie || '').split(';');
    for (let i = 0; i < parts.length; i += 1) {
        const m = parts[i].match(/^\s*(uid|userId|user_id)\s*=\s*([^;]+)$/i);
        if (m && m[2]) {
            let v = m[2].trim();
            try { v = decodeURIComponent(v); } catch (e) { /* ignore */ }
            if (v.length >= 6 && v.length <= 128) return v;
        }
    }
    return '';
}

function loginPublicKey() {
    const r = passportFetch('POST', '/password/publickey', '',
        { 'content-type': 'application/x-www-form-urlencoded; charset=UTF-8' });
    const d = parseJsonSafe(r.body) || {};
    if (Number(d.status) !== 2000 || !d.result || !d.result.modulus) {
        throw new Error('获取 passport 公钥失败: HTTP ' + r.status + ' status=' + d.status
            + ' ' + (d.message || ''));
    }
    return {
        modulus: String(d.result.modulus),
        exponent: String(d.result.publicExponent || '010001'),
        cookie: setCookieHeader(r),
    };
}

function sendLoginCode(msisdn) {
    const phone = String(msisdn || '').trim();
    if (!/^1[3-9]\d{9}$/.test(phone)) throw new Error('手机号格式不对');
    const now = Date.now();
    // 按手机号分别限流：账号 A 发过码不应阻止账号 B 发码。
    if (lastLoginSendAt[phone] && now - lastLoginSendAt[phone] < 60000) {
        throw new Error('该手机号发码太频繁，请 60 秒后再试');
    }
    lastLoginSendAt[phone] = now;
    const key = loginPublicKey();
    const enc = rsaEncrypt(phone, key.modulus, key.exponent);
    const qs = [
        'msisdn=' + encodeURIComponent(enc),
        'captcha=',
        'sourceID=100001',
        'imgcodeType=',
        'fingerPrint=',
        'fingerPrintDetail=',
        'isAsync=true',
        '_=' + now,
    ].join('&');
    const sendExtra = {};
    if (key.cookie) sendExtra.cookie = key.cookie;
    const r = passportFetch('GET', '/login/dynamicpassword?' + qs, undefined, sendExtra);
    const d = parseJsonSafe(r.body) || {};
    if (Number(d.status) !== 2000) {
        throw new Error('发码失败: ' + (d.message || ('status=' + d.status)));
    }
    return {
        ok: true,
        message: d.message || '验证码已发送，请查收短信',
        cookie: mergeCookies(key.cookie, setCookieHeader(r)),
    };
}

function uidFromToken(token) {
    const parts = String(token || '').split('.');
    if (parts.length < 2) return '';
    try {
        let p = parts[1].replace(/-/g, '+').replace(/_/g, '/');
        while (p.length % 4) p += '=';
        const txt = atob(p);
        const d = parseJsonSafe(txt);
        return (d ? findUid(d, 0) : '') || uidFromText(txt);
    } catch (e) {
        return '';
    }
}

function probeLoginUser(token, cookie) {
    const paths = [
        '/portal/userinfo',
        '/portal/user/info',
        '/portal/account/info',
        '/portal/user/getUserInfo',
        '/portal/user/current',
        '/user/nav?sourceid=100001',
    ];
    const cookies = [
        cookie,
        mergeCookies(cookie, 'mgpt_session_id=' + token),
        mergeCookies(cookie, 'mgnd_session_id=' + token),
        mergeCookies(cookie, 'token=' + token),
    ];
    for (let i = 0; i < paths.length; i += 1) {
        for (let j = 0; j < cookies.length; j += 1) {
            try {
                const sep = paths[i].indexOf('?') >= 0 ? '&' : '?';
                const url = paths[i] + sep + 'token=' + encodeURIComponent(token);
                const r = passportFetch('GET', url, undefined, {
                    cookie: cookies[j] || '',
                    authorization: 'Bearer ' + token,
                    'x-token': token,
                    accept: 'application/json, text/html, */*; q=0.01',
                });
                const d = parseJsonSafe(r.body);
                let uid = d ? findUid(d, 0) : '';
                if (!uid) uid = uidFromText(r.body);
                if (uid) return { uid: uid, cookie: mergeCookies(cookies[j], setCookieHeader(r)) };
            } catch (e) {
                // 继续探测；不在日志里打印 token
            }
        }
    }
    return null;
}

function fetchH5Cookies(cookie, token) {
    try {
        const url = REFERER + (token ? ('?token=' + encodeURIComponent(token)) : '');
        const r = fetchRaw('GET', url, {
            'user-agent': UA,
            accept: 'text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8',
            'accept-language': 'zh-CN,zh-Hans;q=0.9',
            'accept-encoding': 'identity',
            cookie: cookie || '',
        }, undefined, { timeout_ms: 15000, tls: '' });
        return {
            cookie: setCookieHeader(r),
            uid: uidFromText(r.body),
        };
    } catch (e) {
        return { cookie: '', uid: '' };
    }
}

function saveLoginDevice(msisdn, uid, deviceid, ce, cookie) {
    if (!uid || String(uid) === '0') {
        throw new Error('拒绝保存 uid=0：服务端会把它当游客/其他账号，不能用于领豆');
    }
    const cfg = loadConfig();
    let idx = -1;
    for (let i = 0; i < cfg.devices.length; i += 1) {
        if (String(cfg.devices[i].msisdn || '') === String(msisdn)) {
            idx = i;
            break;
        }
    }
    const label = 'migu-' + String(msisdn).slice(-4);
    if (idx < 0) {
        cfg.devices.push({
            label: label,
            msisdn: String(msisdn),
            uid: String(uid),
            deviceid: String(deviceid),
            ce: String(ce),
            cookie: String(cookie || ''),
            topup_min: cfg.topup_min,
        });
        idx = cfg.devices.length - 1;
    } else {
        cfg.devices[idx].label = cfg.devices[idx].label || label;
        cfg.devices[idx].msisdn = String(msisdn);
        cfg.devices[idx].uid = String(uid);
        cfg.devices[idx].deviceid = String(deviceid);
        cfg.devices[idx].ce = String(ce);
        cfg.devices[idx].cookie = String(cookie || '');
    }
    return saveConfig(cfg);
}

function followLoginRedirect(redirectURL, token, cookie) {
    let raw = String(redirectURL || '').trim();
    if (!raw) return { cookie: cookie, uid: '', body: '' };
    if (raw.indexOf('//') === 0) raw = 'https:' + raw;
    else if (raw.charAt(0) === '/') raw = PASSPORT + raw;
    else if (!/^https?:\/\//i.test(raw)) raw = PASSPORT + '/' + raw;
    if (!/^https?:\/\//i.test(raw)) return { cookie: cookie, uid: '', body: '' };
    const sep = raw.indexOf('?') >= 0 ? '&' : '?';
    const url = raw + sep + 'callbackURL=&relayState=&token=' + encodeURIComponent(token || '')
        + '&logintype=SMS_CODE';
    try {
        const ck = mergeCookies(cookie, 'mgpt_session_id=' + token);
        const r = fetchRaw('GET', url, loginHeaders({
            cookie: ck,
            accept: 'text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8',
        }), undefined, { timeout_ms: 15000, tls: '' });
        return {
            cookie: mergeCookies(ck, setCookieHeader(r)),
            uid: uidFromText(r.body) || uidFromText(url),
            body: r.body || '',
        };
    } catch (e) {
        return { cookie: cookie, uid: '', body: '' };
    }
}

function loginWithCode(msisdn, code) {
    const phone = String(msisdn || '').trim();
    const sms = String(code || '').trim();
    if (!/^1[3-9]\d{9}$/.test(phone)) throw new Error('手机号格式不对');
    if (!/^\d{6}$/.test(sms)) throw new Error('验证码必须是 6 位数字');

    const key = loginPublicKey();
    const encPhone = rsaEncrypt(phone, key.modulus, key.exponent);
    const encCode = rsaEncrypt(sms, key.modulus, key.exponent);
    const body = [
        'sourceID=100001',
        'appType=0',
        'relayState=',
        'msisdn=' + encodeURIComponent(encPhone),
        'dynamicPassword=' + encodeURIComponent(encCode),
        'captcha=',
        'fingerPrint=',
        'fingerPrintDetail=',
        'isAsync=true',
    ].join('&');
    const authExtra = { 'content-type': 'application/x-www-form-urlencoded; charset=UTF-8' };
    if (key.cookie) authExtra.cookie = key.cookie;
    const r = passportFetch('POST', '/authn/dynamicpassword', body, authExtra);
    const d = parseJsonSafe(r.body) || {};
    if (Number(d.status) !== 2000) {
        throw new Error('登录失败: ' + (d.message || ('status=' + d.status)));
    }

    // 网页 passport 只回 LTToken/USessionID，不回 uid：先看响应里有没有，
    // 没有就用手机号走 H5 身份页解析（见下）。**拿不到真实 uid 就不保存设备** ——
    // 写错账号比这次失败更糟（老版就是这么定的，保留）。
    let uid = '';
    const cfgNow = loadConfig();
    // 同一手机号之前登录过就沿用它的 deviceid（ce 是 uid+msisdn+deviceid 的哈希，
    // 换 deviceid 等于换设备，服务端会当成新登录）。
    let deviceid = '';
    for (let i = 0; i < cfgNow.devices.length; i += 1) {
        const dv = cfgNow.devices[i];
        if (String(dv.msisdn || '') === phone && dv.deviceid) {
            deviceid = String(dv.deviceid);
            break;
        }
    }
    if (!deviceid) deviceid = uuidV4();
    if (!uid) uid = findUid(d);
    if (!uid || String(uid) === '0') {
        // 网页 passport 只回 LTToken/USessionID，不回 uid；用手机号走 H5 身份页解析。
        try {
            uid = resolveUidByPhone(phone, deviceid, cfgNow);
        } catch (e) {
            throw new Error('登录成功，但按手机号解析真实 uid 失败：'
                + (e && e.message ? e.message : e));
        }
    }
    if (!uid || String(uid) === '0') {
        throw new Error('登录成功，但没解析到真实 uid；为避免写错账号，本次不保存设备。');
    }
    const ce = makeCe(uid, phone, deviceid);
    const cfg = saveLoginDevice(phone, uid, deviceid, ce, '');
    let dev = null;
    for (let i = 0; i < cfg.devices.length; i += 1) {
        if (String(cfg.devices[i].msisdn || '') === phone) {
            dev = cfg.devices[i];
            break;
        }
    }
    // 老版删 login-pending.json；0.4.x 是 state 键，删不到就忽略（幂等）。
    try { stateDel(ST_LOGIN_PENDING); } catch (e) { /* 没有就忽略 */ }
    const lines = ['[migu-' + phone.slice(-4) + '] 短信登录成功，H5 凭证已生成'
        + '（uid=' + mask(String(uid), 6, 4) + ', deviceid=' + mask(deviceid, 8, 4) + '）'];
    try {
        const st = queryState(dev, cfg);
        lines.push('[migu-' + phone.slice(-4) + '] H5 凭证测试：音符=' + st.bean
            + ' 任务=' + Object.keys(st.tasks).length + ' 个');
    } catch (e) {
        lines.push('[migu-' + phone.slice(-4) + '] H5 凭证测试失败：'
            + (e && e.message ? e.message : e));
    }
    return { ok: true, lines: lines, config: cfg };
}

function inWindow(win) {
    if (!win) return true;
    const h = new Date().getHours();
    return h >= win[0] && h < win[1];
}

/** 任务之间的随机间隔；墙钟超过 45s 后不再加，避免撞面板 60s 预算。 */
function sleepMs(ms) {
    return new Promise(function (resolve) { setTimeout(resolve, ms); });
}

async function taskPause(cfg, started) {
    const t = (cfg && cfg.task_interval) || {};
    const min = Number(t.min_ms || 0);
    const max = Number(t.max_ms || 0);
    if (max <= 0) return;
    if (started && Date.now() - started > 45000) return;
    let ms = min + Math.floor(Math.random() * (Math.max(min, max) - min + 1));
    // 新面板 setTimeout 上限 5000ms（超了直接抛错）。老配置允许 max_ms 到 15000，
    // 这里收敛到 5s —— 唯一一处时间上的行为差异，为了让「配了 10s 间隔」不至于整轮炸掉。
    // 真需要长等应该用 await host.sleep(ms)。
    if (ms > 5000) ms = 5000;
    if (ms > 0) await sleepMs(ms);
}

function progress(label, text) {
    info('[' + label + '] ' + text);
}

function overBudget(started) {
    return started && Date.now() - started > RUN_BUDGET_MS;
}

function loadConfig() {
    let raw;
    try {
        // 老版是 config()（读 plugin-data/<插件>/config.json）；0.4.x 读 state 键。
        const text = stateGet(ST_CONFIG);
        raw = text ? (JSON.parse(text) || {}) : {};
        if (!raw || typeof raw !== 'object' || Array.isArray(raw)) raw = {};
    } catch (e) {
        throw new Error('读取配置失败: ' + (e && e.message ? e.message : e));
    }
    const cfg = Object.assign({}, DEFAULTS, raw);
    if (!Array.isArray(cfg.devices)) cfg.devices = [];
    delete cfg.app_import; // 老版本的「macOS App 账号导入」已整段删除（2026-11-13），旧字段直接忽略
    cfg.comment = Object.assign({}, DEFAULTS.comment,
        (raw && typeof raw.comment === 'object' && !Array.isArray(raw.comment)) ? raw.comment : {});
    cfg.topup_min = clampInt(cfg.topup_min, 0, 1440, DEFAULTS.topup_min);
    delete cfg.readonly; // 旧配置里的 readonly 字段直接忽略
    cfg.report_ms = clampInt(cfg.report_ms, 1000, 6 * 3600 * 1000, DEFAULTS.report_ms);
    cfg.timeout_ms = clampInt(cfg.timeout_ms, 1000, 60000, DEFAULTS.timeout_ms);
    cfg.task_interval = Object.assign({}, DEFAULTS.task_interval,
        (raw && typeof raw.task_interval === 'object' && !Array.isArray(raw.task_interval)) ? raw.task_interval : {});
    cfg.task_interval.min_ms = clampInt(cfg.task_interval.min_ms, 0, 15000, DEFAULTS.task_interval.min_ms);
    cfg.task_interval.max_ms = clampInt(cfg.task_interval.max_ms, 0, 15000, DEFAULTS.task_interval.max_ms);
    if (cfg.task_interval.max_ms < cfg.task_interval.min_ms) cfg.task_interval.max_ms = cfg.task_interval.min_ms;
    cfg.cert = typeof cfg.cert === 'string' ? cfg.cert.trim() : '';
    cfg.tls = cfg.tls === 'native' ? 'native' : '';
    return cfg;
}

function devCe(dev) {
    if (dev && dev.ce) return String(dev.ce);
    if (!dev || !dev.uid || !dev.msisdn) {
        throw new Error('缺 ce 且缺 uid/msisdn，无法自铸 ce');
    }
    return makeCe(String(dev.uid), String(dev.msisdn), String(dev.deviceid || ''));
}

function certOf(cfg) {
    return cfg.cert && cfg.cert.length >= 32 ? cfg.cert : DEFAULT_CERT_PREFIX;
}

// ──────────────────────────────────────────────────────────── HTTP

function fetchRaw(method, url, headers, body, cfg) {
    const opts = {
        method: method,
        headers: headers,
        timeout_ms: cfg.timeout_ms,
    };
    if (body !== undefined) opts.body = body;
    if (cfg.tls === 'native') opts.tls = 'native';
    let res;
    try {
        res = host.http_fetch(url, opts);   // 同步 op（同一份实现，SSRF 门也在）
    } catch (e) {
        throw new Error('请求失败: ' + (e && e.message ? e.message : e));
    }
    if (res && res.error) throw new Error('请求失败: ' + res.error);
    if (!res || typeof res.body !== 'string') {
        throw new Error('响应形状异常: ' + JSON.stringify(res).slice(0, 200));
    }
    const enc = res.headers && (res.headers['content-encoding'] || res.headers['Content-Encoding']);
    if (enc && String(enc).toLowerCase().indexOf('gzip') >= 0) {
        throw new Error('响应是 gzip（插件未内置解压）；请把 accept-encoding 设为 identity');
    }
    return res;
}

function h5Headers(dev, ce) {
    const h = {
        appid: 'music',
        test: '00',
        ce: ce,
        deviceid: dev.deviceid ? String(dev.deviceid) : '',
        imei: 'h5page',
        imsi: 'h5page',
        channel: '0140070',
        subchannel: '0140070',
        activityid: ACTIVITY,
        platform: 'iOS',
        uid: dev.uid ? String(dev.uid) : '',
        logid: 'cfrom=&appId=music',
        'user-agent': UA,
        version: '8.9.4',
        ua: 'Ios_migu',
        origin: 'https://h5.nf.migu.cn',
        referer: REFERER,
        accept: 'application/json, text/plain, */*',
        'accept-language': 'zh-CN,zh-Hans;q=0.9',
        'accept-encoding': 'identity',
    };
    if (dev.cookie) h.cookie = String(dev.cookie);
    return h;
}

function reportHeaders(dev, ce, ts, sign, bodyHex) {
    return {
        randomsessionkey: '000000',
        'mgm-user-agent': 'M2102K1C',
        'user-agent': SD_UA,
        channel: '0147151',
        language: 'Chinese',
        ua: 'Android_migu',
        mode: 'android',
        appid: 'music',
        brand: 'OnePlus',
        recommendstatus: '1',
        version: '8.9.3',
        'mgm-network-type': '03',
        'mgm-network-operators': '03',
        'mgm-network-standard': '05',
        'accept-language': 'zh-CN,zh;q=0.8',
        subchannel: '',
        hwid: '',
        oaid: '',
        platform: 'M2102K1C',
        userlevel: '0',
        osversion: 'Android 16',
        pkgname: 'cmccwm.mobilemusic',
        verify: 'verify',
        logid: ts,
        ce: ce,
        os: 'Android',
        buildid: '20260929205658',
        token: '',
        'content-type': 'application/json;charset=utf-8',
        'content-length': String(bodyHex.length),
        signversion: 'V005',
        sign: sign,
        timestamp: ts,
        'accept-encoding': 'identity',
    };
}

function nativeHeaders(dev, ce, ts, sign, body) {
    const h = {
        appid: 'music',
        channel: '0140070',
        subchannel: '0140070',
        ce: ce,
        deviceid: dev.deviceid ? String(dev.deviceid) : '',
        uid: dev.uid ? String(dev.uid) : '',
        imei: 'h5page',
        imsi: 'h5page',
        'user-agent': 'MGMobileMusic/8.9.4 (iPad; iOS 26.6; Scale/2.00)',
        ua: 'Ios_migu',
        version: '8.9.4',
        platform: 'iOS',
        os: 'iOS',
        osversion: 'iOS 26.6',
        brand: 'Apple',
        pkgname: 'com.migu.migumobilemusic',
        language: 'Chinese',
        'accept-language': 'zh-CN,zh;q=0.9',
        'accept-encoding': 'identity',
        'content-type': 'application/json;charset=utf-8',
        'content-length': String(utf8(body).length),
        signversion: 'V005',
        sign: sign,
        timestamp: ts,
    };
    if (dev.cookie) h.cookie = String(dev.cookie);
    return h;
}

/** Android App 的评论头；插件内置 mgSign 是 Android V005，实测可发评论。 */
function androidCommentHeaders(dev, ce, ts, sign, body, resourceId) {
    const h = {
        appid: 'music',
        language: 'zh-Hans-CN',
        'user-agent': 'MGMobileMusic/8.9.4 (Linux; Android 16)',
        signversion: 'V005',
        randomsessionkey: '1',
        brand: 'OnePlus',
        channel: '0147151',
        ua: 'Android_migu',
        version: '8.9.3',
        os: 'Android',
        platform: 'Android',
        subchannel: '0147151',
        pkgname: 'cmccwm.mobilemusic',
        'accept-language': 'zh-Hans-CN;q=1',
        timestamp: ts,
        sign: sign,
        accept: '*/*',
        'content-type': 'application/json',
        'accept-encoding': 'identity',
        recommendstatus: '1',
        mode: 'android',
        ce: ce,
        uid: dev.uid ? String(dev.uid) : '',
        logid: 'gd@' + String(resourceId || '') + '@90000002',
        'content-length': String(utf8(body).length),
    };
    if (dev.cookie) h.cookie = String(dev.cookie);
    return h;
}

function commentBody(dev, cfg) {
    const c = cfg.comment || {};
    if (c.body && typeof c.body === 'object') return JSON.stringify(c.body);
    // 2026-11-05 抓包：App 实际只发这五个字段，source 固定 app。
    // Android 头实测 fkToken 可空；iOS 头仍需要 App 生成的 fkToken。
    const obj = {
        commentInfo: String(c.comment_info || c.content || '好听'),
        resourceType: String(c.resource_type || '2021'),
        resourceId: String(c.song_id || ''),
        userId: String(dev.uid || ''),
        fkToken: String(c.fk_token || ''),
        source: 'app',
    };
    if (c.extra && typeof c.extra === 'object') Object.assign(obj, c.extra);
    return JSON.stringify(obj);
}

/** 评论任务：只在 comment.enabled=true 时执行；默认关闭，未实测。 */
function commentTask(dev, cfg, label) {
    const c = cfg.comment || {};
    const st = queryState(dev, cfg);
    const t = st.tasks[COMMENT_TASK_ID] || {};
    const done = Number(t.completeTime || 0) >= Number(t.needCompleteTime || 1);
    if (!c.enabled) {
        return ['[' + label + '] 评论: 未开启（任务 '
            + Number(t.completeTime || 0) + '/' + Number(t.needCompleteTime || 1) + '），跳过'];
    }
    if (!c.song_id && !(c.body && typeof c.body === 'object')) {
        return ['[' + label + '] 评论: 缺 comment.song_id，跳过'];
    }
    if (done) {
        return ['[' + label + '] 评论: 今日已完成,跳过'];
    }

    const ce = devCe(dev);
    const ts = String(Date.now());
    const body = commentBody(dev, cfg);
    const path = c.path || DEFAULTS.comment.path;
    let res;
    if (c.mode === 'h5') {
        const headers = h5Headers(dev, ce);
        headers['content-type'] = 'application/json';
        headers['content-length'] = String(utf8(body).length);
        res = fetchRaw('POST', COMMENT_BASE + path, headers, body, cfg);
    } else if (c.mode === 'ios') {
        const sign = mgSign([utf8(path + ce + ts + body)], certOf(cfg));
        const headers = nativeHeaders(dev, ce, ts, sign, body);
        headers['content-type'] = 'application/json';
        headers.pkgname = 'iOSMigu';
        headers.logid = 'gd@' + String(c.song_id || '') + '@90000002';
        headers.accept = '*/*';
        res = fetchRaw('POST', COMMENT_BASE + path, headers, body, cfg);
    } else {
        // 2026-11-06 实测：Android 头 + mgSign(path+ce+ts+body) 可发评论；
        // fkToken 可以留空，服务端仍回 000000，任务随即 1/1。
        const sign = mgSign([utf8(path + ce + ts + body)], certOf(cfg));
        const headers = androidCommentHeaders(dev, ce, ts, sign, body, c.song_id);
        res = fetchRaw('POST', COMMENT_BASE + path, headers, body, cfg);
    }
    const data = parseJsonSafe(res.body) || {};
    const code = apiCode(data);
    const lines = ['[' + label + '] 评论: HTTP ' + res.status
        + ' code=' + code + ' ' + (apiInfo(data) || res.body.slice(0, 120))];
    if (code === '000000') {
        const st2 = queryState(dev, cfg);
        const t2 = st2.tasks[COMMENT_TASK_ID] || {};
        if (Number(t2.completeTime || 0) >= Number(t2.needCompleteTime || 1)) {
            lines.push('[' + label + '] 评论: OK 任务已完成');
        } else {
            lines.push('[' + label + '] 评论: WARN 接口成功但任务未变 1/1（可能审核/延迟，勿重复）');
        }
    } else {
        lines.push('[' + label + '] 评论: FAIL ' + (apiInfo(data) || '接口未成功'));
    }
    return lines;
}

function callApi(dev, cfg, method, path, bodyObj) {
    const ce = devCe(dev);
    const headers = h5Headers(dev, ce);
    let body;
    if (bodyObj !== undefined) {
        body = JSON.stringify(bodyObj);
        headers['content-type'] = 'application/json;charset=utf-8';
    }
    const res = fetchRaw(method, BASE + path, headers, body, cfg);
    let data;
    try {
        data = JSON.parse(res.body);
    } catch (e) {
        throw new Error('接口返回非 JSON (HTTP ' + res.status + '): ' + res.body.slice(0, 120));
    }
    return { status: res.status, data: data, raw: res.body, headers: res.headers };
}

function ensureOk(res, what) {
    const code = apiCode(res.data);
    if (code !== '000000') {
        throw new Error(what + ': ' + (code || '?') + ' ' + apiInfo(res.data));
    }
    return res.data;
}

// ──────────────────────────────────────────────────────────── 咪咕业务

function queryState(dev, cfg) {
    const qt = ensureOk(callApi(dev, cfg, 'GET', '/act-task/query-tasks'), 'query-tasks');
    const tasks = {};
    const list = Array.isArray(qt.data) ? qt.data : [];
    for (let i = 0; i < list.length; i += 1) {
        const t = list[i];
        if (t && t.id !== undefined) tasks[t.id] = t;
    }
    const beanRes = callApi(dev, cfg, 'GET', '/act-jbzx/ai-bean/query-available');
    const beanData = ensureOk(beanRes, 'ai-bean');
    const bean = (beanData.data && beanData.data.availableNum !== undefined)
        ? Number(beanData.data.availableNum) : -1;
    return { tasks: tasks, bean: bean };
}

/** 金币中心签到状态；签到不是 task-report，独立接口 /act-jbzx/sign。 */
function querySignState(dev, cfg) {
    const r = ensureOk(callApi(dev, cfg, 'GET', '/act-jbzx/sign/status'), 'sign-status');
    return r.data || {};
}

/** 新人 7 天签到状态；只有返回 days/isSign 的账号才走这条链。 */
function queryNewUserSignState(dev, cfg) {
    const r = ensureOk(callApi(dev, cfg, 'GET', '/act-jbzx/new-user-receive/status'), 'new-user-sign-status');
    return r.data || {};
}

/**
 * 新人 7 天签到。2026-11-06 抓包：新账号的 App 点签到实际走
 * POST /act-jbzx/new-user-receive（body {}），返回 scoreNum=6；
 * 普通 /act-jbzx/sign 对这类账号回 200013“暂无法参与”。
 */
function newUserSignTask(dev, cfg, label) {
    let st;
    try {
        st = queryNewUserSignState(dev, cfg);
    } catch (e) {
        return null;
    }
    if (!Array.isArray(st.days) || st.isSign === undefined) return null;
    const num = Number(st.currentDay || 0);
    if (st.isSign) {
        return {
            ok: true,
            skipped: true,
            lines: ['[' + label + '] 签到: 新人签到 今日已领（第' + (num || 1) + '天），跳过'],
        };
    }
    const r = callApi(dev, cfg, 'POST', '/act-jbzx/new-user-receive', {});
    const code = apiCode(r.data);
    if (code === '000000') {
        const score = (r.data && r.data.data && r.data.data.scoreNum !== undefined)
            ? ' +' + r.data.data.scoreNum : '';
        const lines = ['[' + label + '] 签到: 新人签到 OK' + score];
        try {
            const after = queryNewUserSignState(dev, cfg);
            if (after.isSign) {
                lines.push('[' + label + '] 签到: 已确认（新人第' + Number(after.currentDay || num + 1) + '天）');
            }
        } catch (e) {
            lines.push('[' + label + '] 签到: 回读失败 ' + (e && e.message ? e.message : e));
        }
        return { ok: true, action: true, lines: lines };
    }
    if (code === '200001') {
        return { ok: true, skipped: true, lines: ['[' + label + '] 签到: 新人签到 今日已领，跳过'] };
    }
    return null;
}

/**
 * 每日签到。已签直接跳过；未签才 POST /act-jbzx/sign。
 * 2026-11-06 实测：普通账号返回 isSign=true、signNum=2；再 POST 回 200001“今天已签到”。
 * 新账号走 new-user-receive，普通 sign 会回 200013。
 */
function signTask(dev, cfg, label) {
    const lines = [];
    const nu = newUserSignTask(dev, cfg, label);
    if (nu) return nu;
    let before;
    try {
        before = querySignState(dev, cfg);
    } catch (e) {
        return { ok: false, lines: ['[' + label + '] 签到: 查询失败 ' + (e && e.message ? e.message : e)] };
    }
    const num = Number(before.signNum || 0);
    if (before.isSign) {
        return {
            ok: true,
            skipped: true,
            lines: ['[' + label + '] 签到: 今日已签（第' + (num || 1) + '天），跳过'],
        };
    }
    const r = callApi(dev, cfg, 'POST', '/act-jbzx/sign');
    const code = apiCode(r.data);
    if (code === '000000') {
        lines.push('[' + label + '] 签到: OK' + (r.data && r.data.data && r.data.data.scoreNum !== undefined
            ? ' +' + r.data.data.scoreNum : ''));
    } else if (code === '200001') {
        lines.push('[' + label + '] 签到: 今日已签，跳过');
    } else if (code === '200013') {
        // 新号可能只走 new-user-receive；再试一次新人签到。
        const retry = newUserSignTask(dev, cfg, label);
        if (retry) return retry;
        // 仍无新人链：账号不符合活动资格；不是插件错误，不要污染 daily 失败数。
        lines.push('[' + label + '] 签到: SKIP 200013 暂无法参与（账号不符合活动资格）');
        return { ok: true, skipped: true, lines: lines };
    } else {
        return { ok: false, lines: ['[' + label + '] 签到: FAIL ' + code + ' ' + apiInfo(r.data)] };
    }
    try {
        const after = querySignState(dev, cfg);
        if (after.isSign) {
            lines.push('[' + label + '] 签到: 已确认（第' + Number(after.signNum || num + 1) + '天）');
        }
    } catch (e) {
        // 写成功但回读失败时不要误报失败
        lines.push('[' + label + '] 签到: 回读失败 ' + (e && e.message ? e.message : e));
    }
    return { ok: true, action: code === '000000', skipped: code === '200001', lines: lines };
}

/** 读 H5 当前登录身份；这是防“uid/ce 对不上却照写”的硬校验。 */
function queryIdentity(dev, cfg) {
    const r = ensureOk(callApi(dev, cfg, 'GET', '/v2.0/user/getMyPage.do'), 'getMyPage');
    const u = (r.data && r.data.userInfo) || {};
    return {
        uid: String(u.userId || ''),
        nick: String(u.nickName || ''),
        msisdn: String(u.msisdn || ''),
    };
}

function assertIdentity(dev, cfg) {
    const id = queryIdentity(dev, cfg);
    if (!id.uid) throw new Error('身份校验失败：getMyPage 没返回 userId');
    if (dev.uid && String(dev.uid) !== id.uid) {
        throw new Error('身份校验失败：配置 uid=' + mask(String(dev.uid), 6, 4)
            + '，服务端返回 uid=' + mask(id.uid, 6, 4) + '（拒绝写错账号）');
    }
    return id;
}

/**
 * 用手机号解析真实 uid。
 *
 * 2026-11-05 实测：`/v2.0/user/getMyPage.do` 是按 ce 里的 msisdn 找账号，
 * 不要求 uid 头与账号一致；因此短信登录成功后可以先用临时 uid 探测，
 * 拿到真实 userId 后再用真实 uid 重建 ce。这样不依赖 macOS App。
 */
function resolveUidByPhone(phone, deviceid, cfg) {
    const probeUid = 'probe';
    const probe = {
        uid: probeUid,
        msisdn: String(phone),
        deviceid: String(deviceid),
        ce: makeCe(probeUid, String(phone), String(deviceid)),
    };
    const id = queryIdentity(probe, cfg);
    if (!id.uid || id.uid === '0') {
        throw new Error('按手机号解析 uid 失败：getMyPage 没返回 userId');
    }
    return id.uid;
}

function getSongTask(dev, cfg) {
    const r = ensureOk(callApi(dev, cfg, 'GET', '/act-jbzx/song-task/v2.0'), 'song-task');
    return r.data || {};
}

/** 上报 song-duration。返回 { status, body, headers, ts }。 */
function reportDuration(dev, durMs, cfg) {
    const ce = devCe(dev);
    const ts = String(Date.now());
    const pt = JSON.stringify({ duration: Math.floor(durMs), transactionId: uuidV4() });
    const bodyHex = mgEncode(pt);
    const sign = mgSign([utf8(SD_PATH + ce + ts + bodyHex)], certOf(cfg));
    const headers = reportHeaders(dev, ce, ts, sign, bodyHex);
    const res = fetchRaw('POST', BASE + SD_PATH, headers, bodyHex, cfg);
    return { status: res.status, body: res.body, headers: res.headers, ts: ts };
}

function reportCode(body) {
    const data = parseJsonSafe(body);
    return data ? apiCode(data) : '';
}

function receiveSongRewards(dev, cfg, label) {
    const lines = [];
    const st = getSongTask(dev, cfg);
    const songTime = Number(st.songTime || 0);
    const tasks = Array.isArray(st.tasks) ? st.tasks : [];
    let gained = 0;
    let failed = 0;
    for (let i = 0; i < tasks.length; i += 1) {
        const t = tasks[i];
        if (!t || t.id === undefined) continue;
        if (Number(t.status) === 2) continue; // 2 = 已领取
        const r = callApi(dev, cfg, 'POST', '/act-jbzx/receive-task-rewards/v2.0', { taskId: t.id });
        const code = apiCode(r.data);
        if (code === '000000') {
            const n = Number((r.data.data || {}).aiBeanNum || 0);
            gained += n;
            lines.push('[' + label + '] ' + t.id + ': 领取 +' + n + ' 音符');
        } else if (code === '200002') {
            // 已领，静默
        } else {
            failed += 1;
            lines.push('[' + label + '] ' + t.id + ': FAIL ' + code + ' ' + apiInfo(r.data));
        }
    }
    lines.push('[' + label + '] 听歌: songTime=' + (songTime / 60000).toFixed(1) + 'min 本轮领奖+' + gained);
    return { lines: lines, gained: gained, failed: failed, songTime: songTime };
}

function topupSongtime(dev, cfg, label) {
    const targetMin = Number(dev.topup_min !== undefined ? dev.topup_min : cfg.topup_min);
    if (!(targetMin > 0)) {
        return { lines: ['[' + label + '] 听歌补报已关闭(topup_min=0)'], ok: true };
    }
    const st = getSongTask(dev, cfg);
    const songTime = Number(st.songTime || 0);
    const targetMs = Math.floor(targetMin * 60000);
    const need = targetMs - songTime;
    if (need <= 0) {
        return {
            lines: ['[' + label + '] 听歌 ' + (songTime / 60000).toFixed(1) + 'min ≥ ' + targetMin + 'min, 无需补报'],
            ok: true,
        };
    }
    let r;
    try {
        r = reportDuration(dev, need, cfg);
    } catch (e) {
        return {
            lines: ['[' + label + '] 补报 ' + (need / 60000).toFixed(1) + 'min 失败: '
                + (e && e.message ? e.message : e), '[' + label + '] 补报失败, 中止'],
            ok: false,
        };
    }
    const code = reportCode(r.body);
    const ok = code === '000000' || r.body.indexOf('000000') >= 0;
    const lines = ['[' + label + '] 补报 ' + (need / 60000).toFixed(1) + 'min: HTTP '
        + r.status + ' ' + r.body.slice(0, 120)];
    if (!ok) lines.push('[' + label + '] 补报失败, 中止');
    return { lines: lines, ok: ok };
}

async function runDevice(dev, cfg, label, started) {
    const lines = [];
    progress(label, '开始');
    const meta = {
        failed: false,
        errors: 0,
        before: -1,
        after: -1,
        done: 0,
        total: 0,
        claimed: 0,
        signed: 0,
        actions: 0,
        skips: 0,
    };
    function markFail() {
        meta.failed = true;
        meta.errors += 1;
    }

    let id;
    try {
        id = assertIdentity(dev, cfg);
        lines.push('[' + label + '] 身份: uid=' + mask(id.uid, 6, 4)
            + (id.nick ? (' nick=' + id.nick) : ''));
        progress(label, '身份校验通过');
    } catch (e) {
        markFail();
        lines.push('[' + label + '] 身份校验失败: ' + (e && e.message ? e.message : e));
        lines.push('[' + label + '] 本轮不执行任何写操作');
        lines.meta = meta;
        return lines;
    }

    const st = queryState(dev, cfg);
    meta.before = st.bean;
    meta.total = Object.keys(st.tasks).length;
    lines.push('[' + label + '] 期初: 音符=' + st.bean + ' 任务数=' + meta.total);
    progress(label, '期初 音符=' + st.bean + ' 任务数=' + meta.total);

    const login = callApi(dev, cfg, 'GET', '/act-task/login-access');
    const loginCode = apiCode(login.data);
    if (loginCode !== '000000') {
        markFail();
        lines.push('[' + label + '] login-access: FAIL ' + loginCode + ' ' + apiInfo(login.data));
    } else {
        lines.push('[' + label + '] login-access: OK');
        progress(label, 'login-access OK');
    }

    // 所有需要 mgSign 的写操作都先跑完，再进入 await/随机间隔；
    // 这样长任务不会在签名中途撞上执行边界。
    progress(label, '听歌补报/领奖: 检查中');
    const top = topupSongtime(dev, cfg, label);
    if (!top.ok) markFail();
    for (let i = 0; i < top.lines.length; i += 1) lines.push(top.lines[i]);

    progress(label, '签到: 检查中');
    const sg = signTask(dev, cfg, label);
    for (let j = 0; j < sg.lines.length; j += 1) { lines.push(sg.lines[j]); info(sg.lines[j]); }
    if (!sg.ok) markFail();
    else if (sg.action) { meta.actions += 1; meta.signed += 1; }
    else if (sg.skipped) meta.skips += 1;

    progress(label, '评论: 检查任务');
    const cmt = commentTask(dev, cfg, label);
    for (let i = 0; i < cmt.length; i += 1) {
        lines.push(cmt[i]);
        if (cmt[i].indexOf('FAIL') >= 0) markFail();
    }

    await taskPause(cfg, started);
    for (let i = 0; i < REPORT_TASKS.length; i += 1) {
        const tid = REPORT_TASKS[i][0];
        const name = REPORT_TASKS[i][1];
        const win = REPORT_TASKS[i][3];
        const t = st.tasks[tid] || {};
        if (Number(t.completeTime || 0) >= Number(t.needCompleteTime || 1)) {
            meta.skips += 1;
            lines.push('[' + label + '] ' + name + ': 今日已完成,跳过');
            continue;
        }
        if (!inWindow(win)) {
            meta.skips += 1;
            lines.push('[' + label + '] ' + name + ': 非窗口时段(' + win.join('-') + '),跳过');
            continue;
        }
        if (overBudget(started)) {
            lines.push('[' + label + '] ' + name + ': 超出本轮预算,跳过');
            meta.skips += 1;
            break;
        }
        await taskPause(cfg, started);
        meta.actions += 1;
        progress(label, name + ': 上报中');
        const r = callApi(dev, cfg, 'POST', '/act-task/task-report', { taskId: tid });
        const code = apiCode(r.data);
        const extra = (r.data.data && r.data.data.drawNum !== undefined)
            ? ' drawNum=' + r.data.data.drawNum : '';
        if (code === '000000') {
            lines.push('[' + label + '] ' + name + ': OK' + extra);
            progress(label, name + ': OK' + extra);
        } else {
            markFail();
            lines.push('[' + label + '] ' + name + ': FAIL ' + code + ' ' + apiInfo(r.data) + extra);
            progress(label, name + ': FAIL ' + code);
        }
    }

    const rec = receiveSongRewards(dev, cfg, label);
    meta.claimed += rec.gained;
    if (rec.failed > 0) {
        meta.failed = true;
        meta.errors += rec.failed;
    }
    for (let i = 0; i < rec.lines.length; i += 1) lines.push(rec.lines[i]);

    let st2;
    try {
        st2 = queryState(dev, cfg);
    } catch (e) {
        markFail();
        lines.push('[' + label + '] 期末查询失败: ' + (e && e.message ? e.message : e));
        lines.meta = meta;
        return lines;
    }
    meta.after = st2.bean;
    const ids = Object.keys(st2.tasks);
    meta.total = ids.length;
    for (let i = 0; i < ids.length; i += 1) {
        const t = st2.tasks[ids[i]];
        if (Number(t.completeTime || 0) >= Number(t.needCompleteTime || 1)) meta.done += 1;
    }
    const delta = (meta.before >= 0 && meta.after >= 0) ? (meta.after - meta.before) : 0;
    lines.push('[' + label + '] 结果: 音符=' + meta.before + '→' + meta.after
        + ' (' + (delta >= 0 ? '+' : '') + delta + ')'
        + ' 完成=' + meta.done + '/' + meta.total
        + ' 领取=+' + meta.claimed
        + ' 动作=' + meta.actions
        + ' 跳过=' + meta.skips
        + ' 失败=' + meta.errors);
    info(lines[lines.length - 1]);
    lines.meta = meta;
    return lines;
}

function logLines(lines) {
    for (let i = 0; i < lines.length; i += 1) {
        const line = lines[i];
        // 注意：「daily 完成：…失败 0」不是失败；只有 FAIL/异常/身份校验失败，
        // 或「失败 1/2…」这种非零计数才走 fail。否则成功汇总会被日志页染红。
        const nonZeroFail = /失败\s*(?:账号|数)?\s*[=:：]?\s*[1-9]\d*/.test(line);
        if (/FAIL|异常|身份校验失败|补报失败/.test(line) || nonZeroFail) fail(line);
        else if (/OK|领取|无需补报|已完成/.test(line)) success(line);
        else info(line);
    }
}

async function runDaily() {
    const started = Date.now();
    const cfg = loadConfig();
    if (!cfg.devices.length) {
        const line = '[mgmusic] 未配置 devices，跳过';
        warn(line);
        return { ok: true, skipped: true, lines: [line] };
    }

    const lines = ['[mgmusic] daily 开始：' + cfg.devices.length + ' 个账号'];
    let failedDevices = 0;
    let claimed = 0;
    let signed = 0;
    let before = -1;
    let after = -1;
    let done = 0;
    let total = 0;
    for (let i = 0; i < cfg.devices.length; i += 1) {
        const dev = cfg.devices[i];
        const label = deviceLabel(dev, i);
        if (overBudget(started)) {
            lines.push('[mgmusic] 超出本轮预算，剩余账号跳过');
            break;
        }
        try {
            const part = await runDevice(dev, cfg, label, started);
            const meta = part.meta || {};
            for (let j = 0; j < part.length; j += 1) lines.push(part[j]);
            if (meta.failed) failedDevices += 1;
            claimed += Number(meta.claimed || 0);
            signed += Number(meta.signed || 0);
            if (meta.before >= 0) before = before < 0 ? meta.before : before + meta.before;
            if (meta.after >= 0) after = after < 0 ? meta.after : after + meta.after;
            done += Number(meta.done || 0);
            total += Number(meta.total || 0);
            await taskPause(cfg, started);
        } catch (e) {
            failedDevices += 1;
            lines.push('[' + label + '] FAIL ' + (e && e.message ? e.message : e));
        }
    }

    const duration = Date.now() - started;
    const summary = '[mgmusic] daily 完成：' + cfg.devices.length
        + ' 个账号，失败 ' + failedDevices
        + '，签到 +' + signed
        + '，领取 +' + claimed
        + '，耗时 ' + duration + 'ms';
    lines.push(summary);
    logLines([summary]);

    const beforeText = before < 0 ? '?' : String(before);
    const afterText = after < 0 ? '?' : String(after);
    const short = cfg.devices.length + ' 个账号 · 音符 ' + beforeText + '→' + afterText
        + ' · 完成 ' + done + '/' + total
        + ' · 领取 +' + claimed
        + (failedDevices > 0 ? (' · 失败 ' + failedDevices) : '');
    // ⚠ 这一行**不**回写任务摘要（见文件头第 5 条的说明）：任务表只放名字/计划/预算/状态，
    // 每次跑的数字在「运行记录」和日志里，混进任务行会被当成日志。

    if (failedDevices > 0) {
        const err = new Error(failedDevices + '/' + cfg.devices.length + ' 个账号失败');
        err.lines = lines;
        throw err;
    }
    return { ok: true, lines: lines, summary: short };
}

function statusAll(uid) {
    const cfg = loadConfig();
    const targets = pickDevices(cfg, uid);
    if (uid && !targets.length) throw new Error('没有匹配 uid 的账号');
    const lines = [];
    let failCount = 0;
    for (let i = 0; i < targets.length; i += 1) {
        const dev = targets[i];
        const label = deviceLabel(dev, i);
        try {
            const id = assertIdentity(dev, cfg);
            lines.push('[' + label + '] 身份: uid=' + mask(id.uid, 6, 4)
                + (id.nick ? (' nick=' + id.nick) : ''));
            const st = queryState(dev, cfg);
            lines.push('[' + label + '] 音符=' + st.bean + ' ce=' + mask(devCe(dev), 8, 4));
            try {
                let nu = null;
                try { nu = queryNewUserSignState(dev, cfg); } catch (e) { nu = null; }
                if (nu && Array.isArray(nu.days) && nu.isSign !== undefined) {
                    lines.push('  签到: 新人签到' + (nu.isSign ? '今日已领' : '今日未领')
                        + '（第' + Number(nu.currentDay || 0) + '天）');
                } else {
                    const sg = querySignState(dev, cfg);
                    lines.push('  签到: ' + (sg.isSign ? '今日已签' : '今日未签')
                        + '（第' + Number(sg.signNum || 0) + '天）');
                }
            } catch (e) {
                lines.push('  签到: 查询失败 ' + (e && e.message ? e.message : e));
            }
            const ids = Object.keys(st.tasks);
            for (let j = 0; j < ids.length; j += 1) {
                const t = st.tasks[ids[j]];
                lines.push('  ' + (t.taskName || ids[j]) + ': '
                    + Number(t.completeTime || 0) + '/' + Number(t.needCompleteTime || 1));
            }
        } catch (e) {
            failCount += 1;
            lines.push('[' + label + '] FAIL ' + (e && e.message ? e.message : e));
        }
    }
    if (!targets.length) lines.push('（未配置 devices）');
    return { ok: failCount === 0, lines: lines };
}

function pickDevices(cfg, uid) {
    if (!uid) return cfg.devices;
    return cfg.devices.filter(function (d) {
        return String(d.uid || '') === String(uid) || String(d.label || '') === String(uid);
    });
}

function signAll(uid) {
    const cfg = loadConfig();
    if (!cfg.devices.length) throw new Error('未配置 devices');
    const devices = pickDevices(cfg, uid);
    if (!devices.length) throw new Error('没有匹配 uid 的账号');
    const lines = [];
    let failCount = 0;
    let signed = 0;
    for (let i = 0; i < devices.length; i += 1) {
        const dev = devices[i];
        const label = deviceLabel(dev, i);
        try {
            const r = signTask(dev, cfg, label);
            for (let j = 0; j < r.lines.length; j += 1) lines.push(r.lines[j]);
            if (!r.ok) failCount += 1;
            if (r.action) signed += 1;
        } catch (e) {
            failCount += 1;
            lines.push('[' + label + '] 签到 FAIL ' + (e && e.message ? e.message : e));
        }
    }
    lines.push('[mgmusic] 签到完成：' + devices.length + ' 个账号，签到 +' + signed + '，失败 ' + failCount);
    logLines(lines);
    return { ok: failCount === 0, lines: lines };
}

function commentAll(uid) {
    const cfg = loadConfig();
    if (!cfg.devices.length) throw new Error('未配置 devices');
    if (!cfg.comment.enabled) throw new Error('comment.enabled=false；在管理设置里开启评论');
    const devices = pickDevices(cfg, uid);
    if (!devices.length) throw new Error('没有匹配 uid 的账号');
    const lines = [];
    let failCount = 0;
    for (let i = 0; i < devices.length; i += 1) {
        const dev = devices[i];
        const label = deviceLabel(dev, i);
        try {
            const part = commentTask(dev, cfg, label);
            for (let j = 0; j < part.length; j += 1) lines.push(part[j]);
            for (let j = 0; j < part.length; j += 1) {
                if (part[j].indexOf('FAIL') >= 0 || part[j].indexOf('WARN') >= 0) failCount += 1;
            }
        } catch (e) {
            failCount += 1;
            lines.push('[' + label + '] 评论 FAIL ' + (e && e.message ? e.message : e));
        }
    }
    lines.push('[mgmusic] 评论完成：' + devices.length + ' 个账号，失败 ' + failCount);
    logLines(lines);
    return { ok: failCount === 0, lines: lines };
}

function reportAll(ms) {
    const started = Date.now();
    const cfg = loadConfig();
    if (!cfg.devices.length) throw new Error('未配置 devices');
    const lines = [];
    let failCount = 0;
    for (let i = 0; i < cfg.devices.length; i += 1) {
        const dev = cfg.devices[i];
        const label = deviceLabel(dev, i);
        try {
            const r = reportDuration(dev, ms, cfg);
            const code = reportCode(r.body);
            lines.push('[' + label + '] song-duration +' + ms + 'ms: HTTP ' + r.status
                + ' ' + r.body.slice(0, 120));
            if (code !== '000000' && r.body.indexOf('000000') < 0) failCount += 1;
            const rec = receiveSongRewards(dev, cfg, label);
            if (rec.failed > 0) failCount += rec.failed;
            for (let j = 0; j < rec.lines.length; j += 1) lines.push(rec.lines[j]);
            lines.push('[' + label + '] 补报结果: 领取 +' + rec.gained
                + '，失败 ' + rec.failed + '，songTime='
                + (Number(rec.songTime || 0) / 60000).toFixed(1) + 'min');
        } catch (e) {
            failCount += 1;
            lines.push('[' + label + '] report FAIL ' + (e && e.message ? e.message : e));
        }
    }
    lines.push('[mgmusic] 补报完成：' + cfg.devices.length + ' 个账号，失败 '
        + failCount + '，耗时 ' + (Date.now() - started) + 'ms');
    logLines(lines);
    return { ok: failCount === 0, lines: lines };
}

function selftest() {
    const got = mgSign([utf8('hello')], DEFAULT_CERT_PREFIX);
    const want = '0B646EAD2072F5A932D2F0A742BE72ED';
    return { ok: got === want, got: got, want: want, version: VERSION };
}

// ──────────────────────────────────────────────────────────── 配置读写 / 路由

function validateConfig(input) {
    if (!input || typeof input !== 'object' || Array.isArray(input)) {
        throw new Error('配置必须是 JSON 对象');
    }
    const cfg = Object.assign({}, DEFAULTS, input);
    if (!Array.isArray(cfg.devices)) throw new Error('devices 必须是数组');
    if (input.comment !== undefined && (typeof input.comment !== 'object' || Array.isArray(input.comment))) {
        throw new Error('comment 必须是对象');
    }
    delete cfg.app_import; // 已删除的功能：老配置/老界面带上来的字段一律丢掉，别留在 state 里
    cfg.comment = Object.assign({}, DEFAULTS.comment, input.comment || {});
    cfg.comment.enabled = cfg.comment.enabled === undefined ? true : !!cfg.comment.enabled;
    cfg.comment.song_id = String(cfg.comment.song_id || '').trim();
    cfg.comment.content = String(cfg.comment.content || '好听').trim() || '好听';
    cfg.comment.resource_type = String(cfg.comment.resource_type || '2021').trim() || '2021';
    cfg.comment.comment_info = String(cfg.comment.comment_info || '');
    cfg.comment.target_comment_id = String(cfg.comment.target_comment_id || '');
    cfg.comment.log_id = String(cfg.comment.log_id || '');
    cfg.comment.fk_token = String(cfg.comment.fk_token || '');
    cfg.comment.source = String(cfg.comment.source || '1');
    cfg.comment.mode = cfg.comment.mode === 'h5' ? 'h5' : (cfg.comment.mode === 'ios' ? 'ios' : 'android');
    cfg.comment.path = String(cfg.comment.path || DEFAULTS.comment.path);
    if (cfg.comment.path.charAt(0) !== '/') throw new Error('comment.path 必须以 / 开头');
    if (cfg.comment.body !== null && cfg.comment.body !== undefined
        && (typeof cfg.comment.body !== 'object' || Array.isArray(cfg.comment.body))) {
        throw new Error('comment.body 必须是对象或 null');
    }
    if (cfg.comment.extra !== null && cfg.comment.extra !== undefined
        && (typeof cfg.comment.extra !== 'object' || Array.isArray(cfg.comment.extra))) {
        throw new Error('comment.extra 必须是对象');
    }
    cfg.topup_min = clampInt(cfg.topup_min, 0, 1440, DEFAULTS.topup_min);
    delete cfg.readonly; // 旧配置里的 readonly 字段直接忽略
    cfg.report_ms = clampInt(cfg.report_ms, 1000, 6 * 3600 * 1000, DEFAULTS.report_ms);
    cfg.timeout_ms = clampInt(cfg.timeout_ms, 1000, 60000, DEFAULTS.timeout_ms);
    if (input.task_interval !== undefined && (typeof input.task_interval !== 'object' || Array.isArray(input.task_interval))) {
        throw new Error('task_interval 必须是对象');
    }
    cfg.task_interval = Object.assign({}, DEFAULTS.task_interval, input.task_interval || {});
    cfg.task_interval.min_ms = clampInt(cfg.task_interval.min_ms, 0, 15000, DEFAULTS.task_interval.min_ms);
    cfg.task_interval.max_ms = clampInt(cfg.task_interval.max_ms, 0, 15000, DEFAULTS.task_interval.max_ms);
    if (cfg.task_interval.max_ms < cfg.task_interval.min_ms) cfg.task_interval.max_ms = cfg.task_interval.min_ms;
    cfg.cert = typeof cfg.cert === 'string' ? cfg.cert.trim() : '';
    cfg.tls = cfg.tls === 'native' ? 'native' : '';
    if (cfg.cert && cfg.cert.length < 32) throw new Error('cert 至少 32 个 hex 字符（或留空用内置前缀）');
    for (let i = 0; i < cfg.devices.length; i += 1) {
        const d = cfg.devices[i];
        if (!d || typeof d !== 'object' || Array.isArray(d)) {
            throw new Error('devices[' + i + '] 必须是对象');
        }
        if (!d.ce && (!d.uid || !d.msisdn)) {
            throw new Error('devices[' + i + ']: ce 为空时必须给 uid + msisdn');
        }
        if (!d.deviceid) throw new Error('devices[' + i + ']: 缺 deviceid');
        if (d.topup_min !== undefined) d.topup_min = clampInt(d.topup_min, 0, 1440, DEFAULTS.topup_min);
    }
    return cfg;
}

function saveConfig(input) {
    const cfg = validateConfig(input);
    // 老版 fs.writeFileSync('config.json', …)；0.4.x 写 state 键（同步 op，结构不变）。
    stateSet(ST_CONFIG, JSON.stringify(cfg, null, 2));
    return cfg;
}

/**
 * 播种：`mgmusic.config` 不存在时跑一次（模块加载期 await 一次）。
 *
 * 1) `await host.self_config()` 非空 → validateConfig 后写进 state。
 *    这是 `plugin-data/<插件>/config.json` 的**正式入口**：迁移老面板的账号，
 *    就是把 `~/Library/Application Support/plugin-panel/data/plugin-data/mgmusic/config.json`
 *    复制到 `<PANEL_DATA_DIR>/plugin-data/mgmusic/config.json`，再重载插件。
 * 2) 没给（或是空对象）→ 用 DEFAULTS。
 *
 * 之后一切读写只走 state：那份文件当导入源，不在这里回写。
 */
async function seedConfig() {
    let imported = null;
    try {
        imported = await host.self_config();
    } catch (e) {
        warn('[' + PLUGIN + '] self_config 读取失败：' + (e && e.message ? e.message : e));
    }
    const fileCfg = (imported && typeof imported === 'object' && !Array.isArray(imported)
        && Object.keys(imported).length > 0) ? imported : null;

    const curText = stateGet(ST_CONFIG);

    // ① 第一次跑（state 里还没有配置）：按老规矩播种。
    if (curText === null) {
        if (fileCfg) {
            try {
                const cfg = validateConfig(fileCfg);
                stateSet(ST_CONFIG, JSON.stringify(cfg, null, 2));
                stateSet(ST_SEED_MARK, 'file');
                info('[' + PLUGIN + '] 已从 plugin-data/' + PLUGIN + '/config.json 播种配置：'
                    + cfg.devices.length + ' 个设备');
                return true;
            } catch (e) {
                // 迁移过来的配置不合规时**不静默吞掉**：喊出来，再用默认配置起步。
                warn('[' + PLUGIN + '] plugin-data/' + PLUGIN + '/config.json 校验失败，回落到默认配置：'
                    + (e && e.message ? e.message : e));
            }
        }
        stateSet(ST_CONFIG, JSON.stringify(validateConfig(DEFAULTS), null, 2));
        // ⚠ 这里**不**打播种标记：万一有人先启动、后放 config.json，下一次重启还能补上。
        return false;
    }

    // ② state 已有配置。只有在「还没从文件播过种 + 现在一个账号都没有」时才补播一次 ——
    //    这是「先启动面板、后拷 config.json」那种次序错误的唯一补救机会。
    //    已经播过种（标记在）就绝不再覆盖：用户在界面上删光了账号也不该被文件偷偷复活。
    if (stateGet(ST_SEED_MARK) !== null) return false;
    let cur = null;
    try { cur = JSON.parse(curText); } catch (e) { cur = null; }
    const curN = (cur && Array.isArray(cur.devices)) ? cur.devices.length : 0;
    if (curN === 0 && fileCfg) {
        try {
            const cfg = validateConfig(fileCfg);
            if (cfg.devices.length > 0) {
                stateSet(ST_CONFIG, JSON.stringify(cfg, null, 2));
                stateSet(ST_SEED_MARK, 'file');
                warn('[' + PLUGIN + '] 迁移补播种：state 里一个账号都没有，已从 plugin-data/'
                    + PLUGIN + '/config.json 导入 ' + cfg.devices.length
                    + ' 个设备。正确次序是「先放 config.json，再启动面板」。');
                return true;
            }
        } catch (e) {
            warn('[' + PLUGIN + '] 迁移补播种失败（config.json 校验不通过）：'
                + (e && e.message ? e.message : e));
        }
    }
    if (curN > 0) stateSet(ST_SEED_MARK, 'state'); // 已经有账号了：以后不再从文件覆盖
    return false;
}

function json(status, obj) {
    return {
        status: status,
        headers: { 'content-type': 'application/json; charset=utf-8' },
        body: JSON.stringify(obj),
    };
}

function route(fn) {
    return async function (req) {
        try {
            return await fn(req);
        } catch (e) {
            const out = { ok: false, error: (e && e.message ? e.message : String(e)) };
            if (e && e.lines) out.lines = e.lines;
            return json(200, out);
        }
    };
}

function parseBody(req) {
    if (!req || req.body === undefined || req.body === null || req.body === '') return {};
    if (typeof req.body === 'object') return req.body;
    try {
        return JSON.parse(req.body);
    } catch (e) {
        throw new Error('请求体不是 JSON');
    }
}

// ──────────────────────────────────────────────────────────── 注册（0.4.x 契约）

// 加载期第一步：播种配置（只在 state 里没有 mgmusic.config 时读一次 self_config）。
// 老版这里是「config() 直接读 plugin-data/mgmusic/config.json」；新面板把这一步显式化。
try {
    await seedConfig();
} catch (e) {
    warn('[' + PLUGIN + '] 配置播种失败：' + (e && e.message ? e.message : e));
}

// 侧栏一格（老 register_nav({label, icon})；0.4.x 没有 icon 字段）。
await host.sidebar({ title: '咪咕音乐', order: 80 });

// ──────────────────────────────────────────────────────────── 路由（0.4.x：一个分发器）
//
// 老面板是 register_route(path, method, fn, opts) 一条一条注册；新面板只有一个
// register_web(evt => …)，宿主把 /plugins/mgmusic/** 里没命中静态文件的请求都交给它。
// 分发键 = method + ' ' + evt.rel（rel 是 /plugins/mgmusic/ 之后那段，例如 'api/config'）。
// **URL 与 JSON 载荷形状与老面板一致**，前端不用改。
const WEB_ROUTES = {
    'GET api/config': function () {
        const cfg = loadConfig();
        return json(200, { ok: true, config: cfg });
    },
    'POST api/config': function (req) {
        const body = parseBody(req);
        const input = body && body.config ? body.config : body;
        // ⚠ 整份替换语义：漏 `devices` 会被当成「把账号全删了」（老面板也是这个语义，
        // 但这是**静默丢账号**，代价太大）。宁可报错让人带上 devices。
        if (!input || typeof input !== 'object' || !Object.prototype.hasOwnProperty.call(input, 'devices')) {
            throw new Error('config.devices 必须给：这个接口是整份替换，只改某一项也要带上现有的 devices，'
                + '否则等于把账号全删了');
        }
        const cfg = saveConfig(input);
        return json(200, { ok: true, config: cfg });
    },
    'POST api/login/send': function (req) {
        const body = parseBody(req);
        return json(200, sendLoginCode(body.msisdn));
    },
    'POST api/login/verify': function (req) {
        const body = parseBody(req);
        return json(200, loginWithCode(body.msisdn, body.code));
    },
    'GET api/status': function () {
        return json(200, statusAll());
    },
    'POST api/status': function (req) {
        const body = parseBody(req);
        return json(200, statusAll(body.uid || body.label || ''));
    },
    // 兼容保留。⚠ 新面板 WebUI 请求的预算是 30s，120s 的 daily 打这里会被砍在半路
    // （任务本身幂等，被砍不会写坏，但看不到结果）。界面上的「立即跑一次」请打
    // 面板任务接口 POST /api/tasks/mgmusic/daily/run —— 那条走调度器，预算 120s。
    'POST api/run': async function () {
        return json(200, await runDaily());
    },
    'POST api/sign': function (req) {
        const body = parseBody(req);
        return json(200, signAll(body.uid));
    },
    'POST api/comment': function (req) {
        const body = parseBody(req);
        return json(200, commentAll(body.uid));
    },
    'POST api/report': function (req) {
        const body = parseBody(req);
        const cfg = loadConfig();
        const ms = clampInt(body.ms, 1000, 6 * 3600 * 1000, cfg.report_ms);
        return json(200, reportAll(ms));
    },
    'GET api/selftest': function () {
        return json(200, selftest());
    },
};

await host.register_web(async function (evt) {
    const method = String((evt && evt.method) || 'GET').toUpperCase();
    const rel = String((evt && evt.rel) || '').replace(/^\/+/, '').replace(/\/+$/, '');
    const key = method + ' ' + rel;
    const fn = WEB_ROUTES[key];
    if (!fn) return json(404, { ok: false, error: '没有这条路由: ' + key });
    return route(fn)(evt);
});

// 定时任务。老写法把 summary 塞在 opts 里（{budget_ms, summary}）；新面板 register_cron 的
// opts 只认 params / lead_ms（多传会被拒）。cron 是 6 段（带秒）写法，和 v0.3.4 完全一致。
await host.register_cron('daily', '0 5 0,12,22 * * *', 120000);
//
// ⚠ 0.4.x 的 register_cron **只登记调度，不绑函数**（老面板第三个参数就是函数）。
// 宿主派发时按**任务名**去 __handlers 里找 handler（engine.rs 的 __dispatch），
// 找不到就抛「没有这个 handler: daily」—— 实测每一拍都失败（duration 0.0s）。
// 所以定时任务必须再补一个**同名** handler；名字不同就等于没绑。
await host.register_handler('daily', function () {
    return runDaily();
});
//
// 摘要一律不写（连描述型也不写）：任务表保持和 mcloud 那几条一样干净。
// 顺带记一条坑：`set_job_summary` **不能在加载期 await** —— `Cmd::Load` 同步 await 插件加载，
// register_cron 的消息要等加载返回才被调度器消费，此刻调用会抛「没有这个任务: mgmusic/daily」
// 并把整个插件加载搞挂（实测踩到过）。真要用它也请等第一次运行。

// ⚠ **只登记一条任务**（用户 2026-11-13 指出：「咪咕音乐应该只有一个每日任务才对」）。
//
// 老版（v0.3.4）除了 daily 还 register_handler('sign'/'comment'/'report')，那在新面板里会
// 各变成任务页上的一行「手动任务」—— 于是插件有 4 行，而其中三行本来只是按钮。
// 现在这三个动作只走插件自己的 HTTP 路由（界面的「签到 / 评论 / 补报」就是打
// POST /plugins/mgmusic/api/{sign,comment,report}），任务表里保持**只有 daily 一条**。
// 代价：不能在面板「任务」页手动派发这三件事 —— 要手动就去插件页点按钮（那本来就是入口）。

// ── 加载日志（老版的启动自述；config() 换成 state，其余不变）。
try {
    const cfg = loadConfig();
    info(PLUGIN + ' v' + VERSION + ' 已加载：' + cfg.devices.length
        + ' 个设备，topup_min=' + cfg.topup_min);
} catch (e) {
    warn(PLUGIN + ' v' + VERSION + ' 已加载，但配置读取失败：'
        + (e && e.message ? e.message : e));
}
