// 由 src/entries/exchange.ts 打包生成（panel-llrt 版）—— **不要手改这个文件**。
//
//   改代码：编辑 src/，然后 `npm run build:panelllrt`
//   mcloud-plugin 0.2.25 · 插件 `mcloud-exchange`

// src/panelllrt/hostApi.ts
import panelHost from "panel:host";
var host = panelHost;
var globals = globalThis;
function text(msg) {
  if (typeof msg === "string") return msg;
  try {
    return JSON.stringify(msg);
  } catch {
    return String(msg);
  }
}
function emit(op, msg) {
  try {
    void Promise.resolve(host[op](text(msg))).catch(() => {
    });
  } catch {
  }
}
function log(msg) {
  emit("info", msg);
}
function debug(msg) {
  emit("info", "[debug] " + text(msg));
}
function warn(msg) {
  emit("warn", msg);
}
function error(msg) {
  emit("error", msg);
}
function fatal(msg) {
  emit("error", "[fatal] " + text(msg));
}
function start(msg) {
  emit("info", "[start] " + text(msg));
}
function success(msg) {
  emit("info", "[ok] " + text(msg));
}
function fail(msg) {
  emit("warn", "[fail] " + text(msg));
}
function wrapJob(name, handler) {
  return (evt) => {
    const payload = {
      name,
      plugin: evt?.plugin ?? "",
      expr: "",
      runs: 0,
      scheduledAt: "",
      firedAt: (/* @__PURE__ */ new Date()).toISOString(),
      lateMs: 0,
      jitterMs: 0,
      realigned: false,
      manual: evt?.manual === true,
      params: null
    };
    return handler(payload);
  };
}
function registry() {
  if (globals.__handlers == null) globals.__handlers = /* @__PURE__ */ Object.create(null);
  return globals.__handlers;
}
function register_cron(name, expr, handler, opts) {
  registry()[name] = wrapJob(name, handler);
  const budget = typeof opts?.budget_ms === "number" ? opts.budget_ms : 3e4;
  const extra = {};
  if (opts != null && "params" in opts) extra.params = opts.params ?? null;
  if (typeof opts?.lead_ms === "number") extra.lead_ms = opts.lead_ms;
  const extraArg = Object.keys(extra).length ? extra : void 0;
  try {
    void Promise.resolve(host.register_cron(name, expr, budget, extraArg)).catch((e) => {
      emit("error", "注册任务 " + name + "（" + expr + "）失败: " + text(e));
    });
  } catch (e) {
    emit("error", "注册任务 " + name + " 失败: " + text(e));
  }
}
function sqlite_query(sql, params) {
  const r = host.sqlite_query_sync(sql, params ?? []);
  return Array.isArray(r?.rows) ? r.rows : [];
}
function sqlite_exec(sql, params) {
  const r = host.sqlite_exec_sync(sql, params ?? []);
  return {
    changes: typeof r?.changes === "number" ? r.changes : 0,
    lastInsertRowid: typeof r?.last_insert_rowid === "number" ? r.last_insert_rowid : 0
  };
}
function http_fetch(url, options) {
  try {
    const r = host.http_fetch(url, options ?? {});
    return { status: r?.status ?? 0, headers: r?.headers ?? {}, body: r?.body ?? "" };
  } catch (e) {
    return { error: text(e) };
  }
}
function worker_hint(_hint) {
}
var panel = {
  version: () => String(host.panel_version()),
  atLeast: (v) => host.panel_at_least(String(v)) === true
};
globals.panel = panel;

// src/crypto/bytes.ts
function utf8(s) {
  const out = [];
  for (let i = 0; i < s.length; i += 1) {
    let cp = s.charCodeAt(i);
    if (cp >= 55296 && cp <= 56319 && i + 1 < s.length) {
      const low = s.charCodeAt(i + 1);
      if (low >= 56320 && low <= 57343) {
        cp = 65536 + (cp - 55296 << 10) + (low - 56320);
        i += 1;
      } else {
        cp = 65533;
      }
    } else if (cp >= 55296 && cp <= 57343) {
      cp = 65533;
    }
    if (cp < 128) {
      out.push(cp);
    } else if (cp < 2048) {
      out.push(192 | cp >> 6, 128 | cp & 63);
    } else if (cp < 65536) {
      out.push(224 | cp >> 12, 128 | cp >> 6 & 63, 128 | cp & 63);
    } else {
      out.push(
        240 | cp >> 18,
        128 | cp >> 12 & 63,
        128 | cp >> 6 & 63,
        128 | cp & 63
      );
    }
  }
  return new Uint8Array(out);
}
function fromUtf8(b) {
  let s = "";
  let codePoint = 0;
  let bytesSeen = 0;
  let bytesNeeded = 0;
  let lower = 128;
  let upper = 191;
  const emit2 = (cp) => {
    if (cp < 65536) {
      s += String.fromCharCode(cp);
    } else {
      const v = cp - 65536;
      s += String.fromCharCode(55296 + (v >> 10), 56320 + (v & 1023));
    }
  };
  const reset = () => {
    codePoint = 0;
    bytesSeen = 0;
    bytesNeeded = 0;
    lower = 128;
    upper = 191;
  };
  let i = 0;
  while (i < b.length) {
    const byte = b[i];
    let reprocess = false;
    if (bytesNeeded === 0) {
      if (byte <= 127) {
        s += String.fromCharCode(byte);
      } else if (byte >= 194 && byte <= 223) {
        bytesNeeded = 1;
        codePoint = byte & 31;
      } else if (byte >= 224 && byte <= 239) {
        if (byte === 224) lower = 160;
        if (byte === 237) upper = 159;
        bytesNeeded = 2;
        codePoint = byte & 15;
      } else if (byte >= 240 && byte <= 244) {
        if (byte === 240) lower = 144;
        if (byte === 244) upper = 143;
        bytesNeeded = 3;
        codePoint = byte & 7;
      } else {
        s += "�";
      }
    } else if (byte < lower || byte > upper) {
      reset();
      s += "�";
      reprocess = true;
    } else {
      lower = 128;
      upper = 191;
      codePoint = codePoint << 6 | byte & 63;
      bytesSeen += 1;
      if (bytesSeen === bytesNeeded) {
        emit2(codePoint);
        reset();
      }
    }
    if (!reprocess) i += 1;
  }
  if (bytesNeeded !== 0) s += "�";
  return s;
}
var HEX = "0123456789abcdef";
function toHex(b) {
  let s = "";
  for (let i = 0; i < b.length; i += 1) {
    const v = b[i];
    s += HEX[v >> 4] + HEX[v & 15];
  }
  return s;
}
var B64 = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
function toBase64(b) {
  let s = "";
  for (let i = 0; i < b.length; i += 3) {
    const n = b[i] << 16 | (b[i + 1] ?? 0) << 8 | (b[i + 2] ?? 0);
    s += B64[n >> 18 & 63] + B64[n >> 12 & 63];
    s += i + 1 < b.length ? B64[n >> 6 & 63] : "=";
    s += i + 2 < b.length ? B64[n & 63] : "=";
  }
  return s;
}
function fromBase64(s) {
  const clean = s.replace(/[^A-Za-z0-9+/]/g, "");
  const out = new Uint8Array(Math.floor(clean.length * 6 / 8));
  let bits2 = 0;
  let acc = 0;
  let o = 0;
  for (let i = 0; i < clean.length; i += 1) {
    const v = B64.indexOf(clean[i]);
    if (v < 0) continue;
    acc = acc << 6 | v;
    bits2 += 6;
    if (bits2 >= 8) {
      bits2 -= 8;
      out[o] = acc >> bits2 & 255;
      o += 1;
    }
  }
  return out.subarray(0, o);
}
function writeU32BE(out, off, v) {
  out[off] = v >>> 24 & 255;
  out[off + 1] = v >>> 16 & 255;
  out[off + 2] = v >>> 8 & 255;
  out[off + 3] = v & 255;
}
function writeU32LE(out, off, v) {
  out[off] = v & 255;
  out[off + 1] = v >>> 8 & 255;
  out[off + 2] = v >>> 16 & 255;
  out[off + 3] = v >>> 24 & 255;
}
function readU32BE(b, off) {
  return (b[off] << 24 | b[off + 1] << 16 | b[off + 2] << 8 | b[off + 3]) >>> 0;
}
function readU32LE(b, off) {
  return (b[off + 3] << 24 | b[off + 2] << 16 | b[off + 1] << 8 | b[off]) >>> 0;
}
function lengthBits(bytes) {
  const bits2 = bytes * 8;
  return { hi: Math.floor(bits2 / 4294967296) >>> 0, lo: bits2 >>> 0 };
}

// src/mcloud/envelope.ts
var isRecord = (v) => typeof v === "object" && v !== null && !Array.isArray(v);
function parseSignInEnvelope(body) {
  let v;
  try {
    v = JSON.parse(body);
  } catch (e) {
    throw new Error(`签到响应不是 JSON：${e instanceof Error ? e.message : String(e)}`);
  }
  if (!isRecord(v)) {
    throw new Error(`签到响应不是对象：${body.slice(0, 120)}`);
  }
  const raw = v.code;
  let code = -1;
  if (typeof raw === "number") code = Number.isFinite(raw) ? raw : -1;
  else if (typeof raw === "string" && raw.trim() !== "" && !Number.isNaN(Number(raw))) {
    code = Number(raw);
  }
  const msg = v.msg ?? v.message;
  const message = typeof msg === "string" ? msg : "";
  return {
    code,
    message,
    result: v.result,
    ok: code === 0
  };
}
function resultObject(env) {
  return isRecord(env.result) ? env.result : {};
}

// src/mcloud/client.ts
var isFetchError = (v) => typeof v === "object" && v !== null && typeof v.error === "string";
function pickError(msg) {
  return msg.replace(/^\s*请求失败[:：]\s*/, "").trim() || msg;
}
function hostFetchOptions(req, timeoutMs) {
  const opts = {
    method: req.method,
    headers: req.headers,
    body: req.body,
    timeout_ms: timeoutMs
  };
  if (req.tls !== void 0 && req.url.startsWith("https:")) opts.tls = req.tls;
  return opts;
}
function callOnce(transport2, req) {
  const t0 = Date.now();
  const fail2 = (error3) => ({
    status: 0,
    tookMs: Date.now() - t0,
    env: void 0,
    ok: false,
    error: error3,
    raw: "",
    body: "",
    setCookies: []
  });
  let res;
  try {
    res = transport2(req);
  } catch (e) {
    return fail2(`请求失败：${e instanceof Error ? e.message : String(e)}`);
  }
  if (isFetchError(res)) return fail2(pickError(res.error));
  if (typeof res !== "object" || res === null || typeof res.body !== "string") {
    return fail2(`请求返回的形状不认识：${JSON.stringify(res)?.slice(0, 200) ?? String(res)}`);
  }
  const tookMs = Date.now() - t0;
  const raw = res.body.length > 500 ? `${res.body.slice(0, 500)}…` : res.body;
  let env;
  let error2;
  if (res.status !== 200) {
    error2 = `HTTP ${res.status}`;
  } else {
    try {
      env = parseSignInEnvelope(res.body);
      if (!env.ok) error2 = `code=${env.code}${env.message ? ` msg=${env.message}` : ""}`;
    } catch (e) {
      error2 = e instanceof Error ? e.message : String(e);
    }
  }
  return {
    status: res.status,
    tookMs,
    env,
    ok: res.status === 200 && !!env?.ok,
    error: error2,
    raw,
    body: res.body,
    setCookies: extractSetCookies(res.headers)
  };
}
var CONTENT_TYPE_JSON = "application/json;charset=UTF-8";
function applyDefaultContentType(req) {
  if (req.body === void 0) return req;
  const declared = Object.keys(req.headers).some((k) => k.toLowerCase() === "content-type");
  if (declared) return req;
  return { ...req, headers: { ...req.headers, "content-type": CONTENT_TYPE_JSON } };
}
function callRawText(transport2, req) {
  const t0 = Date.now();
  const fail2 = (error2) => ({
    ok: false,
    status: 0,
    text: "",
    chars: 0,
    tookMs: Date.now() - t0,
    error: error2
  });
  let res;
  try {
    res = transport2(req);
  } catch (e) {
    return fail2(`请求失败：${e instanceof Error ? e.message : String(e)}`);
  }
  const tookMs = Date.now() - t0;
  if (isFetchError(res)) return { ...fail2(pickError(res.error)), tookMs };
  if (typeof res !== "object" || res === null || typeof res.body !== "string") {
    return {
      ...fail2(`请求返回的形状不认识：${JSON.stringify(res)?.slice(0, 200) ?? String(res)}`),
      tookMs
    };
  }
  const chars = res.body.length;
  if (res.status !== 200) {
    return { ok: false, status: res.status, text: res.body, chars, tookMs, error: `HTTP ${res.status}` };
  }
  return { ok: true, status: 200, text: res.body, chars, tookMs, error: void 0 };
}
function extractSetCookies(headers) {
  const raw = [];
  for (const [k, v] of Object.entries(headers)) {
    if (k.toLowerCase() === "set-cookie") raw.push(v);
  }
  const out = [];
  for (const line of raw) {
    let cur = line;
    const parts = [];
    let idx = 0;
    for (; ; ) {
      const m = /,\s*([A-Za-z0-9_.-]+)=/.exec(cur.slice(idx));
      if (!m || m.index === void 0) break;
      parts.push(cur.slice(idx, idx + m.index));
      idx += m.index + 1;
    }
    parts.push(cur.slice(idx));
    for (const p of parts) {
      cur = p.trim();
      if (!cur) continue;
      const pair = cur.split(";")[0]?.trim() ?? "";
      if (pair.includes("=")) out.push(pair);
    }
  }
  return out;
}
function mergeCookieJar(existing, incoming) {
  const jar = /* @__PURE__ */ new Map();
  const eat = (s) => {
    for (const part of s.split(";")) {
      const p = part.trim();
      if (!p) continue;
      const eq = p.indexOf("=");
      if (eq <= 0) continue;
      const k = p.slice(0, eq).trim();
      const v = p.slice(eq + 1).trim();
      if (v === "" || /^(deleted|expired)$/i.test(v)) jar.delete(k);
      else jar.set(k, v);
    }
  };
  eat(existing);
  for (const c of incoming) eat(c);
  return [...jar.entries()].map(([k, v]) => `${k}=${v}`).join("; ");
}

// src/store/keys.ts
var DEFAULT_ACCT = "default";
var PLUGIN_NS = "mcloud.";
var ACCT_PREFIX = "mcloud.acct.";
var STATE_CURRENT_ACCOUNT = "mcloud.account.current";
var STATE_ACCOUNTS = "mcloud.accounts";
var KEY_SCOPE = {
  // ── 账号运行期状态 ──────────────────────────────────────────
  "mcloud.session": "account",
  "mcloud.session.last_check": "account",
  "mcloud.h5.auth": "account",
  // 宿主"能不能用 tls:native"的探测结论（= 这个面板构建支不支持直播）。
  // **与账号无关**，但按账号存最省事 —— 结论一样，多存几份不花钱。
  "mcloud.host.native_tls": "account",
  "mcloud.h5.exchange": "account",
  "mcloud.h5.jwt": "account",
  // 单次预算（秒）：**全局** —— 它是"这一趟跑多久"的策略，不是某个号的读数。
  "mcloud.daily.budget_s": "global",
  "mcloud.daily.done_date": "account",
  "mcloud.daily.last_run": "account",
  // ⚠ 登录这一族是**本机**的，不是某个账号的。
  //
  //   · `deviceId` —— 本机设备指纹。参考实现（`accounts-page.tsx`）的原话：
  //     「扫码时它作为 `dID` 上报，服务端据此定位会话。它**跨重启稳定**；
  //      换掉会让旧的扫码会话立即失效，但已登录账号不受影响。」
  //     ⇒ 它是"这台机器长什么样"，写在**账号**名下是错的。
  //   · `qr` / `sms` —— "我正在进行的那次登录"（还没变成账号）。
  //   · `base` / `userDomainId` —— 登录服下发的主机，同样是本机的。
  //
  //   ⚠ 这几格**原来就是扁平的全局键**。把它们改成按账号是**我的改动**，不是原行为
  //   —— 而"语义说不清楚时保持原行为"才是对的。见下面那段纪律。
  "mcloud.login.deviceId": "global",
  "mcloud.login.qr": "global",
  "mcloud.login.sms": "global",
  // 手机端（App 链路）那次登录。与 `sms` **分开存**：两端的滑块语义不同
  //（PC 取图自己解、App 真机是开 WebView），共用一格会让"进行中的是哪一端"
  // 变成要靠猜。
  "mcloud.login.app": "global",
  // 每条任务跑哪些账号。**global** —— 它说的是这条任务用哪些号，
  // 不是某个号自己的设置（放进账号袋会让 A 的名单被 B 读到）。
  // ⚠ 动态键（键里带任务名）⇒ 靠  登记，见下面那张表。
  "mcloud.task.accounts.": "global",
  // 运行日志的**环形副本**（最近 N 行）。global —— 它记的是"这台机器上发生过什么"，
  // 不属于某个号；而且它要能在**面板重启之后**还读得到（面板的内存日志环是重启就没的，
  // 见 findings F-085）。写的时候按"全局"落，界面上再按插件名过滤。
  "mcloud.log.ring": "global",
  "mcloud.login.base": "global",
  "mcloud.login.userDomainId": "global",
  "mcloud.sign.stage": "account",
  "mcloud.sign.last": "account",
  "mcloud.sign.done": "account",
  // 上传那台状态机**按任务分槽**：`mcloud.upload.state` 是 App 任务那一格，
  // `mcloud.upload.state.expansion` 是任务拓展那一格（见 tasks/upload.ts 文件头）。
  // 槽位名带任务 ⇒ 整串是拼出来的，静态扫描扫不到，所以按前缀也登记一条。
  "mcloud.upload.state": "account",
  // 我们自己传上去、还没收尾的云盘文件（按账号；链尾兜底清理用）。
  "mcloud.cloud.owned": "account",
  "mcloud.taskexpansion.state": "account",
  "mcloud.month.backup": "account",
  "mcloud.apptask.state": "account",
  "mcloud.apptaskmatrix.last": "account",
  "mcloud.apptaskmatrix.group_index": "account",
  "mcloud.prizes.last": "account",
  // 「领奖」页：待领取列表的缓存与进行中的那一单。
  // 按账号 —— 奖品记录、单号、手机号都是某一个号的；串了号就是拿 A 的单号去领 B 的东西。
  "mcloud.prizes.cache": "account",
  "mcloud.prizes.claim": "account",
  "mcloud.multiple.last": "account",
  "mcloud.multiple.done": "account",
  "mcloud.revival.done": "account",
  "mcloud.revival.last": "account",
  "mcloud.receive.last": "account",
  "mcloud.receive.queue": "account",
  "mcloud.cloudsnapshot.last": "account",
  "mcloud.exchangelist.last": "account",
  "mcloud.exchangelist.raw": "account",
  // 界面上「选一件」的下拉缓存（全量清单里挑出来的四格）。
  // **按账号** —— 清单与"能不能换"都是账号级的。日界作废，见 exchangeList.ts。
  "mcloud.exchangelist.pick": "account",
  // 「我关注的奖品」：按账号存（不同账号的清单不同，关注的 id 也可能不同）。
  "mcloud.exchange.subs": "account",
  "mcloud.exchange.plan": "account",
  // 定时抢兑每个账号已经真正发起过几次（按账号）。
  "mcloud.exchange.schedule.runs": "account",
  "mcloud.exchange.schedule.runs_gen": "account",
  "mcloud.exchange.order.step": "account",
  "mcloud.exchange.order.baseline": "account",
  "mcloud.exchange.order.plan": "account",
  "mcloud.exchange.order.round": "account",
  "mcloud.exchange.order.ordered_hour": "account",
  "mcloud.exchange.order.last_wait": "account",
  "mcloud.live.gate": "account",
  "mcloud.live.gate.reading": "account",
  "mcloud.live.gate_base": "account",
  "mcloud.live.reading": "account",
  "mcloud.live.draw": "account",
  "mcloud.live.codes": "account",
  "mcloud.live.listen": "account",
  "mcloud.live.listen_room": "account",
  "mcloud.live.listen_seconds": "account",
  // 直播页上那个「听弹幕时长」是**任务级**的（所有账号用同一段），
  // 与账号级诊断口子分开；账号级仍可作为没设任务级时的回落。
  "mcloud.live.listen_seconds.task": "global",
  "mcloud.live.url": "account",
  "mcloud.live.wake_plan": "account",
  "mcloud.live.ws_base": "account",
  "mcloud.live.ws_heartbeat_reply": "account",
  "mcloud.live.im_base": "account",
  "mcloud.smid.deviceId": "account",
  "mcloud.smid.fingerprint": "account",
  "mcloud.auth.refresh.last": "account",
  // ── 红包派对（移动云手机的活动站，另一套凭据：它自己的 `token` 头）──
  // 整趟的进度台账：一次 drain 里跨步活着的只有它（活动站票与华为侧的票**跑完就抹**，
  // 见 `tasks/redpackState.ts` 文件头）。按账号 —— 队列、按天的写账（`written`）
  // 与给用户看的那几行都属于某一个号；串了号就是拿 A 的队列去发 B 的写请求。
  "mcloud.redpack.state": "account",
  // 开没开。**按账号**：参照实现那份默认配置就是每个号一格
  // （`红包派对.开启`，默认 true），所以"这一个号开没开"确实是账号的属性。
  "mcloud.redpack.enabled": "account",
  // 指到别处用的口子（CI / 只读探针）。与 `mcloud.live.gate_base` 同一类，按账号。
  "mcloud.redpack.base": "account",
  // ── 算力大作战（`National_TokenPK`，另一个活动站；只带云盘 jwttoken）──
  // 整趟进度台账（计划队列、已领/待领、逐行账）。按账号：队列与"哪条领过了"都是某个号的。
  "mcloud.tokenpk.state": "account",
  // 开没开。参照实现 `DefaultEnabled: true` ⇒ 没写过就是开。按账号。
  "mcloud.tokenpk.enabled": "account",
  // 上传那台状态机给这一族单独开的槽位（与 App 任务 / 任务拓展分开）。
  "mcloud.tokenpk.upload": "account",
  // 代抽的开关与每日上限（缺省不抽：花的是用户的次数）。
  "mcloud.tokenpk.draw": "account",
  // 代抽的按天账（今天抽了几次、中了什么）—— 上限按天算，第二趟不重置。
  "mcloud.tokenpk.draw.state": "account",
  // ── 实例级配置 / 跨账号共享 ────────────────────────────────
  /** 账号表本身。 */
  "mcloud.accounts": "global",
  /** "当前选中哪个账号"。⚠ 它是**选择**，不是某个账号的状态。 */
  "mcloud.account.current": "global",
  /** 上游域名。 */
  "mcloud.base_url": "global",
  /** 登录方式偏好（扫码 / 短信 / 账密）。 */
  "mcloud.login.pref": "global",
  /** 几点抢兑 —— 是**配置**，跟账号无关。 */
  "mcloud.exchange.target_hour": "global",
  // 「定时抢兑」的目标：与手动目标分开，避免改一个把另一个带跑。
  // 全局 —— 定时任务按「任务账号」逐个跑，但只认这一件目标。
  "mcloud.exchange.schedule.plan": "global",
  "mcloud.exchange.schedule.mode": "global",
  "mcloud.exchange.schedule.cron": "global",
  "mcloud.exchange.schedule.until": "global",
  "mcloud.exchange.schedule.once_date": "global",
  "mcloud.exchange.schedule.max_runs": "global",
  "mcloud.exchange.schedule.gen": "global",
  "mcloud.exchange.schedule.plan_name": "global",
  // 多计划：计划表 + 每条计划已跑次数。
  "mcloud.exchange.plans": "global",
  "mcloud.exchange.plan.runs": "global",
  "mcloud.exchange.plans.migrated": "global",
  "mcloud.exchange.schedule.enabled": "global",
  "mcloud.exchange.lead_sec": "global",
  /** 提前量 —— "网络慢的人该调大"，是**这台机器**的调参，与账号无关。 */
  "mcloud.exchange.order.lead_ms": "global",
  /** 领小红花的开关。 */
  "mcloud.live.claim_enabled": "global",
  /** 推送配置（渠道 / 规则 / 事件绑到哪个渠道）。 */
  "mcloud.push.config": "global",
  "mcloud.push.rules": "global",
  "mcloud.push.bindings": "global",
  /** ⚠ 共享口令池 —— 参考实现明写"实例级：跨 mcloud 用户、跨重启"。 */
  "mcloud.kouling.pool": "global",
  /** 运行日志（每条记录里自己带账号）。 */
  "mcloud.run.log": "global",
  /** 任务清单缓存。 */
  "mcloud.tasklist.last": "global",
  /** 数据页 AI豆读数缓存（余额 + 当日汇总，按账号；用户口径 2 小时）。 */
  "mcloud.beans.cache": "account"
};
var KEY_PREFIX_SCOPE = [
  // 每条任务"跑哪些账号"（`mcloud.task.accounts.日常`）—— 键里带任务名，
  // 静态扫描扫不到整串，所以按前缀登记。⚠ **global**：它说的是"这条任务用哪些号"，
  // 不是"某个号自己的设置"。
  ["mcloud.task.accounts.", "global"],
  ["mcloud.last_error.", "account"],
  // 票与换票进度：**按配方**分（`mcloud.h5.jwt.sign-in` / `.msg-push-on`）。
  // ⚠ 账号那一维由 `scoped()` 加（袋名），所以这里只剩配方这一段。
  //   两个维度都必要：不分账号会串身份，不分配方会用错页面的票。
  ["mcloud.h5.jwt.", "account"],
  ["mcloud.h5.exchange.", "account"],
  // 矩阵按**市场**分（`mcloud.apptaskmatrix.g.sign_in_3`）—— 动态拼出来的，
  // 静态扫描扫不到整串，所以按前缀登记。
  ["mcloud.apptaskmatrix.g.", "account"],
  // 上传状态机**按任务分槽**（`mcloud.upload.state.expansion`）。槽位名是拼出来的，
  // 精确表里只有 App 任务那一格 ⇒ 这里补前缀。⚠ 不能落成 global：
  // 它记的是"这个号传了什么、传到哪"，串号就会拿 A 的 fileId 去收尾。
  ["mcloud.upload.state.", "account"]
];
function scopeOf(key) {
  const exact = KEY_SCOPE[key];
  if (exact !== void 0) return exact;
  let best;
  for (const [prefix, scope] of KEY_PREFIX_SCOPE) {
    if (key.startsWith(prefix) && (best === void 0 || prefix.length > best.len)) {
      best = { len: prefix.length, scope };
    }
  }
  return best?.scope;
}
function acctIdOf(store2, override) {
  if (override !== void 0 && override !== "") return override;
  const id = (store2.getState(STATE_CURRENT_ACCOUNT) ?? "").trim();
  return id === "" ? DEFAULT_ACCT : id;
}
function acctBagKey(acctId) {
  return `${ACCT_PREFIX}${acctId}`;
}
function readBag(store2, acctId) {
  const raw = store2.getState(acctBagKey(acctId));
  if (raw === void 0 || raw === null || raw === "") return {};
  try {
    const v = JSON.parse(raw);
    if (typeof v !== "object" || v === null || Array.isArray(v)) return {};
    const out = {};
    for (const [k, val] of Object.entries(v)) {
      if (typeof val === "string") out[k] = val;
    }
    return out;
  } catch {
    return {};
  }
}
function writeBag(store2, acctId, bag, nowIso) {
  store2.setState(acctBagKey(acctId), JSON.stringify(bag), nowIso);
}
var UnregisteredKeyError = class extends Error {
  constructor(key) {
    super(
      `状态键「${key}」没有在 store/keys.ts 的 KEY_SCOPE 里登记。
⚠ 不许默认当成全局键 —— 那正是多账号串号那一串 bug 的成因（见该文件头）。
判据：账号 A 写进去之后，账号 B 读到它会不会做出**错的**决定？会 ⇒ 'account'。`
    );
    this.name = "UnregisteredKeyError";
  }
};
function routeGet(store2, key, acctId) {
  if (key.startsWith(ACCT_PREFIX)) return store2.getState(key) ?? null;
  if (!key.startsWith(PLUGIN_NS)) return store2.getState(key) ?? null;
  const scope = scopeOf(key);
  if (scope === void 0) throw new UnregisteredKeyError(key);
  if (scope === "global") return store2.getState(key) ?? null;
  const bag = readBag(store2, acctIdOf(store2, acctId));
  return bag[key] ?? null;
}
function routeSet(store2, key, value, nowIso, acctId) {
  if (key.startsWith(ACCT_PREFIX)) {
    store2.setState(key, value, nowIso);
    return;
  }
  if (!key.startsWith(PLUGIN_NS)) {
    store2.setState(key, value, nowIso);
    return;
  }
  const scope = scopeOf(key);
  if (scope === void 0) throw new UnregisteredKeyError(key);
  if (scope === "global") {
    store2.setState(key, value, nowIso);
    return;
  }
  const acct = acctIdOf(store2, acctId);
  const bag = readBag(store2, acct);
  bag[key] = value;
  writeBag(store2, acct, bag, nowIso);
}
function migrateFlatKeys(store2, nowIso) {
  const acctId = acctIdOf(store2);
  const bag = readBag(store2, acctId);
  const moved = [];
  for (const [key, scope] of Object.entries(KEY_SCOPE)) {
    if (scope !== "account") continue;
    const flat = store2.getState(key);
    if (flat === void 0 || flat === null || flat === "") continue;
    if (bag[key] === void 0) {
      bag[key] = flat;
      moved.push(key);
    }
    store2.setState(key, "", nowIso);
  }
  if (moved.length > 0) writeBag(store2, acctId, bag, nowIso);
  return { moved };
}

// src/mcloud/auth.ts
function lenientB64(s) {
  let payload = "";
  for (const c of s) {
    if (c === "=") break;
    if (/[A-Za-z0-9+/]/.test(c)) payload += c;
  }
  if (payload.length === 0 || payload.length % 4 === 1) return void 0;
  const pad = payload.length % 4 === 0 ? "" : "=".repeat(4 - payload.length % 4);
  let text2;
  try {
    text2 = fromUtf8(fromBase64(payload + pad));
  } catch {
    return void 0;
  }
  return text2.length === 0 ? void 0 : text2;
}
function isPlainDecimal(s) {
  if (s.length === 0) return false;
  let i = 0;
  if (s[0] === "+" || s[0] === "-") i = 1;
  if (i >= s.length) return false;
  for (; i < s.length; i++) {
    const c = s.charCodeAt(i);
    if (c < 48 || c > 57) return false;
  }
  return true;
}
function parseAuth(auth) {
  if (auth.trim() === "") return void 0;
  const i = auth.indexOf("Basic ");
  const clean = i < 0 ? auth : auth.slice(0, i) + auth.slice(i + "Basic ".length);
  const decoded = lenientB64(clean);
  if (decoded === void 0) return void 0;
  const parts = decoded.split(":", 4);
  const platform = parts[0] ?? "";
  const phone = parts[1] ?? "";
  const token = parts[2] ?? "";
  const seg = token.split("|")[3]?.trim();
  const expire = seg !== void 0 && isPlainDecimal(seg) ? Math.trunc(Number(seg)) || 0 : 0;
  return { raw: `Basic ${clean}`, platform, phone, token, expire };
}
function stripInvisible(raw) {
  return raw.replace(/[\uFEFF\u200B-\u200D\u2060]/g, "");
}
function stripOuterQuotes(raw) {
  const head = raw.charAt(0);
  if (raw.length >= 2 && (head === '"' || head === "'" || head === "`") && raw.charAt(raw.length - 1) === head) {
    return raw.slice(1, -1).trim();
  }
  return raw;
}
function hardenAuthorization(raw) {
  const t = stripInvisible(raw).trim();
  if (t === "") return { ok: false, error: "凭据是空的" };
  const unquoted = stripOuterQuotes(t);
  const line = /^authorization\s*[:=]\s*([\s\S]*)$/i.exec(unquoted);
  let rest = (line !== null ? line[1] ?? "" : unquoted).trim();
  let bearer = false;
  for (; ; ) {
    const m = /^(basic|bearer)[\s:=]*/i.exec(rest);
    if (m === null) break;
    if (/^bearer$/i.test(m[1] ?? "")) bearer = true;
    rest = rest.slice(m[0].length).trim();
  }
  const compact = rest.replace(/\s+/g, "");
  if (compact === "") return { ok: false, error: "只有 `Basic`，没有凭据本体" };
  if (/^(pc|mobile|app)[\s]?:/i.test(compact)) {
    return { ok: true, value: `${bearer ? "Bearer" : "Basic"} ${toBase64(utf8(compact))}` };
  }
  if (!/^[A-Za-z0-9+/=_-]+$/.test(compact)) {
    return {
      ok: false,
      error: '这串里还有 base64 之外的字符 —— 正确形状是 `Basic base64("pc:手机号:token|…")`；把 `pc:手机号:…` 那串**明文**直接贴进来也行（会自动编）'
    };
  }
  const info = parseAuth(compact);
  if (info === void 0 || info.raw === "") {
    return {
      ok: false,
      error: '认不出这串凭据（base64 解不开）—— 正确形状是 `Basic base64("pc:手机号:token|…")`；把 `pc:手机号:…` 那串**明文**直接贴进来也行（会自动编）'
    };
  }
  return { ok: true, value: bearer ? `Bearer ${compact}` : `Basic ${compact}` };
}

// src/mcloud/accounts.ts
function maskAuth(raw) {
  const phone = phoneOf(raw);
  const len = raw.length;
  if (phone !== void 0) return `${phone.slice(0, 1)}****${phone.slice(-4)} · ${len} 字符`;
  return `（认不出手机号）· ${len} 字符`;
}
function phoneOf(raw) {
  const plain = /1\d{10}/.exec(raw);
  if (plain !== null) return plain[0];
  const m = /^\s*basic\s+([A-Za-z0-9+/=]+)\s*$/i.exec(raw);
  if (m === null) return void 0;
  try {
    const decoded = fromUtf8(fromBase64(m[1]));
    const inB64 = /1\d{10}/.exec(decoded);
    return inB64 === null ? void 0 : inB64[0];
  } catch {
    return void 0;
  }
}
function normalizeAuth(raw) {
  const r = hardenAuthorization(raw);
  if (r.ok && r.value !== void 0) return r.value;
  return raw.replace(/\uFEFF/g, "").trim();
}
function loadAccounts(store2) {
  const raw = store2.getState(STATE_ACCOUNTS);
  if (raw === null || raw === void 0 || raw.trim() === "") return [];
  let parsed;
  try {
    parsed = JSON.parse(raw);
  } catch {
    return [];
  }
  if (!Array.isArray(parsed)) return [];
  const out = [];
  for (const it of parsed) {
    if (it === null || typeof it !== "object") continue;
    const o = it;
    if (typeof o.id !== "string" || typeof o.auth !== "string" || o.auth === "") continue;
    const auth = normalizeAuth(o.auth);
    const info = parseAuth(auth);
    const pc = o.pcPlatform;
    out.push({
      id: o.id,
      label: typeof o.label === "string" && o.label !== "" ? o.label : o.id,
      auth,
      // 老账号没存过这两格 ⇒ 从凭据里现解一次（不用迁移）
      platform: typeof o.platform === "string" ? o.platform : info?.platform ?? "",
      pcPlatform: pc === "win" || pc === "mac" ? pc : void 0,
      userDomainId: typeof o.userDomainId === "string" && o.userDomainId !== "" ? o.userDomainId : void 0,
      disabled: o.disabled === true,
      addedAt: typeof o.addedAt === "string" ? o.addedAt : ""
    });
  }
  return out;
}
function saveAccounts(store2, list, nowIso) {
  store2.setState(STATE_ACCOUNTS, JSON.stringify(list), nowIso);
}
function selectAccount(store2, id, nowIso) {
  if (id !== "" && !loadAccounts(store2).some((a) => a.id === id)) return false;
  store2.setState(STATE_CURRENT_ACCOUNT, id, nowIso);
  return true;
}
function setAccountAuth(store2, id, auth, nowIso) {
  if (auth === "") return void 0;
  const list = loadAccounts(store2);
  const idx = list.findIndex((a) => a.id === id);
  if (idx < 0) return void 0;
  if (list[idx].auth === auth) return list[idx];
  list[idx] = { ...list[idx], auth };
  saveAccounts(store2, list, nowIso);
  return list[idx];
}
function setAccountDomain(store2, id, domain, nowIso) {
  if (domain === "") return void 0;
  const list = loadAccounts(store2);
  const idx = list.findIndex((a) => a.id === id);
  if (idx < 0) return void 0;
  if (list[idx].userDomainId === domain) return list[idx];
  list[idx] = { ...list[idx], userDomainId: domain };
  saveAccounts(store2, list, nowIso);
  return list[idx];
}
function currentAccountDomain(store2) {
  const a = currentAccount(store2);
  return a?.userDomainId !== void 0 && a.userDomainId !== "" ? a.userDomainId : null;
}
function currentAccount(store2) {
  const id = (store2.getState(STATE_CURRENT_ACCOUNT) ?? "").trim();
  if (id === "") return void 0;
  return loadAccounts(store2).find((a) => a.id === id);
}

// src/store/types.ts
var SESSION_KEY = "mcloud.session";
function loadSession(store2) {
  const raw = store2.getState(SESSION_KEY);
  if (!raw) return null;
  try {
    const v = JSON.parse(raw);
    const cookie = typeof v.cookie === "string" ? v.cookie : "";
    const jwt = typeof v.jwt === "string" ? v.jwt : "";
    if (cookie === "" && jwt === "") return null;
    return {
      cookie,
      jwt: jwt === "" ? void 0 : jwt,
      deviceId: typeof v.deviceId === "string" ? v.deviceId : void 0,
      userAgent: typeof v.userAgent === "string" ? v.userAgent : void 0,
      savedAt: typeof v.savedAt === "string" ? v.savedAt : ""
    };
  } catch {
    return null;
  }
}
function saveSession(store2, s, nowIso) {
  store2.setState(SESSION_KEY, JSON.stringify({ ...s, savedAt: nowIso }), nowIso);
}

// src/store/scoped.ts
var WRAPPED = Symbol.for("__mcloud_scoped_store__");
var RAW = Symbol.for("__mcloud_raw_store__");
var ACCT = Symbol.for("__mcloud_scoped_account__");
function scopedAccountOf(store2) {
  const v = store2[ACCT];
  return typeof v === "string" && v !== "" ? v : void 0;
}
function rawStoreOf(s) {
  const raw = s[RAW];
  return raw ?? s;
}
function scoped(base, acctId) {
  if (base[WRAPPED] === true) {
    throw new Error(
      "store 已经被 scoped() 包过一次了 —— 双重包装会让 `mcloud.acct.*` 也去查登记表然后抛错。"
    );
  }
  const wrapped = {
    ...base,
    getState: (key) => routeGet(base, key, acctId),
    setState: (key, value, nowIso) => routeSet(base, key, value, nowIso, acctId)
  };
  Object.defineProperty(wrapped, WRAPPED, { value: true, enumerable: false });
  if (acctId !== void 0 && acctId !== "") {
    Object.defineProperty(wrapped, ACCT, { value: acctId, enumerable: false });
  }
  Object.defineProperty(wrapped, RAW, { value: base, enumerable: false });
  return wrapped;
}

// src/crypto/md5.ts
var S = [
  7,
  12,
  17,
  22,
  7,
  12,
  17,
  22,
  7,
  12,
  17,
  22,
  7,
  12,
  17,
  22,
  5,
  9,
  14,
  20,
  5,
  9,
  14,
  20,
  5,
  9,
  14,
  20,
  5,
  9,
  14,
  20,
  4,
  11,
  16,
  23,
  4,
  11,
  16,
  23,
  4,
  11,
  16,
  23,
  4,
  11,
  16,
  23,
  6,
  10,
  15,
  21,
  6,
  10,
  15,
  21,
  6,
  10,
  15,
  21,
  6,
  10,
  15,
  21
];
var K = new Uint32Array(64);
for (let i = 0; i < 64; i += 1) {
  K[i] = Math.floor(Math.abs(Math.sin(i + 1)) * 4294967296) >>> 0;
}
function rotl(x, n) {
  return (x << n | x >>> 32 - n) >>> 0;
}
function padLE(bytes) {
  const { hi, lo } = lengthBits(bytes.length);
  const total = (bytes.length + 8 >> 6) + 1 << 6;
  const out = new Uint8Array(total);
  out.set(bytes);
  out[bytes.length] = 128;
  writeU32LE(out, total - 8, lo);
  writeU32LE(out, total - 4, hi);
  return out;
}
function md5(bytes) {
  let a0 = 1732584193;
  let b0 = 4023233417;
  let c0 = 2562383102;
  let d0 = 271733878;
  const padded = padLE(bytes);
  const m = new Uint32Array(16);
  for (let off = 0; off < padded.length; off += 64) {
    for (let j = 0; j < 16; j += 1) m[j] = readU32LE(padded, off + j * 4);
    let a = a0;
    let b = b0;
    let c = c0;
    let d = d0;
    for (let i = 0; i < 64; i += 1) {
      let f;
      let g;
      if (i < 16) {
        f = b & c | ~b & d;
        g = i;
      } else if (i < 32) {
        f = d & b | ~d & c;
        g = (5 * i + 1) % 16;
      } else if (i < 48) {
        f = b ^ c ^ d;
        g = (3 * i + 5) % 16;
      } else {
        f = c ^ (b | ~d);
        g = 7 * i % 16;
      }
      f = f + a + K[i] + m[g] >>> 0;
      a = d;
      d = c;
      c = b;
      b = b + rotl(f, S[i]) >>> 0;
    }
    a0 = a0 + a >>> 0;
    b0 = b0 + b >>> 0;
    c0 = c0 + c >>> 0;
    d0 = d0 + d >>> 0;
  }
  const out = new Uint8Array(16);
  writeU32LE(out, 0, a0);
  writeU32LE(out, 4, b0);
  writeU32LE(out, 8, c0);
  writeU32LE(out, 12, d0);
  return out;
}

// src/crypto/tables.ts
var SHA256_K = [
  1116352408,
  1899447441,
  3049323471,
  3921009573,
  961987163,
  1508970993,
  2453635748,
  2870763221,
  3624381080,
  310598401,
  607225278,
  1426881987,
  1925078388,
  2162078206,
  2614888103,
  3248222580,
  3835390401,
  4022224774,
  264347078,
  604807628,
  770255983,
  1249150122,
  1555081692,
  1996064986,
  2554220882,
  2821834349,
  2952996808,
  3210313671,
  3336571891,
  3584528711,
  113926993,
  338241895,
  666307205,
  773529912,
  1294757372,
  1396182291,
  1695183700,
  1986661051,
  2177026350,
  2456956037,
  2730485921,
  2820302411,
  3259730800,
  3345764771,
  3516065817,
  3600352804,
  4094571909,
  275423344,
  430227734,
  506948616,
  659060556,
  883997877,
  958139571,
  1322822218,
  1537002063,
  1747873779,
  1955562222,
  2024104815,
  2227730452,
  2361852424,
  2428436474,
  2756734187,
  3204031479,
  3329325298
];
var SHA256_H = [
  1779033703,
  3144134277,
  1013904242,
  2773480762,
  1359893119,
  2600822924,
  528734635,
  1541459225
];

// src/crypto/sha256.ts
function rotr(x, n) {
  return (x >>> n | x << 32 - n) >>> 0;
}
function sha256(bytes) {
  const h = Uint32Array.from(SHA256_H);
  const { hi, lo } = lengthBits(bytes.length);
  const total = (bytes.length + 8 >> 6) + 1 << 6;
  const padded = new Uint8Array(total);
  padded.set(bytes);
  padded[bytes.length] = 128;
  writeU32BE(padded, total - 8, hi);
  writeU32BE(padded, total - 4, lo);
  const w = new Uint32Array(64);
  for (let off = 0; off < padded.length; off += 64) {
    for (let i = 0; i < 16; i += 1) w[i] = readU32BE(padded, off + i * 4);
    for (let i = 16; i < 64; i += 1) {
      const x = w[i - 15];
      const y = w[i - 2];
      const s0 = (rotr(x, 7) ^ rotr(x, 18) ^ x >>> 3) >>> 0;
      const s1 = (rotr(y, 17) ^ rotr(y, 19) ^ y >>> 10) >>> 0;
      w[i] = w[i - 16] + s0 + w[i - 7] + s1 >>> 0;
    }
    let a = h[0];
    let b = h[1];
    let c = h[2];
    let d = h[3];
    let e = h[4];
    let f = h[5];
    let g = h[6];
    let hh = h[7];
    for (let i = 0; i < 64; i += 1) {
      const S1 = (rotr(e, 6) ^ rotr(e, 11) ^ rotr(e, 25)) >>> 0;
      const ch = (e & f ^ ~e & g) >>> 0;
      const t1 = hh + S1 + ch + SHA256_K[i] + w[i] >>> 0;
      const S0 = (rotr(a, 2) ^ rotr(a, 13) ^ rotr(a, 22)) >>> 0;
      const maj = (a & b ^ a & c ^ b & c) >>> 0;
      const t2 = S0 + maj >>> 0;
      hh = g;
      g = f;
      f = e;
      e = d + t1 >>> 0;
      d = c;
      c = b;
      b = a;
      a = t1 + t2 >>> 0;
    }
    h[0] = h[0] + a >>> 0;
    h[1] = h[1] + b >>> 0;
    h[2] = h[2] + c >>> 0;
    h[3] = h[3] + d >>> 0;
    h[4] = h[4] + e >>> 0;
    h[5] = h[5] + f >>> 0;
    h[6] = h[6] + g >>> 0;
    h[7] = h[7] + hh >>> 0;
  }
  const out = new Uint8Array(32);
  for (let i = 0; i < 8; i += 1) writeU32BE(out, i * 4, h[i]);
  return out;
}

// src/crypto/hmac.ts
var md5Hex = (d) => toHex(md5(anyBytes(d)));
var sha256Hex = (d) => toHex(sha256(anyBytes(d)));
function anyBytes(d) {
  return typeof d === "string" ? utf8(d) : d;
}

// src/crypto/aes.ts
function gfMul(a, b) {
  let r = 0;
  let x = a & 255;
  let y = b & 255;
  while (y > 0) {
    if (y & 1) r ^= x;
    const hi = x & 128;
    x = x << 1 & 255;
    if (hi) x ^= 27;
    y >>= 1;
  }
  return r & 255;
}
function rotl8(x, n) {
  return (x << n | x >> 8 - n) & 255;
}
var SBOX = (() => {
  const s = new Uint8Array(256);
  for (let a = 0; a < 256; a += 1) {
    let inv = 0;
    if (a !== 0) {
      let r = 1;
      let b = a;
      let e = 254;
      while (e > 0) {
        if (e & 1) r = gfMul(r, b);
        b = gfMul(b, b);
        e >>= 1;
      }
      inv = r;
    }
    s[a] = (inv ^ rotl8(inv, 1) ^ rotl8(inv, 2) ^ rotl8(inv, 3) ^ rotl8(inv, 4) ^ 99) & 255;
  }
  return s;
})();
var INV_SBOX = (() => {
  const inv = new Uint8Array(256);
  for (let a = 0; a < 256; a += 1) inv[SBOX[a]] = a;
  return inv;
})();
function fitKey16(key) {
  const out = new Uint8Array(16);
  const src = utf8(key);
  out.set(src.subarray(0, 16));
  return out;
}
function aes128KeyExpansion(key) {
  if (key.length !== 16) throw new Error(`AES: 密钥必须是 16 字节（得到 ${key.length}）`);
  const RCON = [1, 2, 4, 8, 16, 32, 64, 128, 27, 54];
  const w = new Uint8Array(176);
  w.set(key);
  for (let i = 4; i < 44; i += 1) {
    let t0 = w[(i - 1) * 4];
    let t1 = w[(i - 1) * 4 + 1];
    let t2 = w[(i - 1) * 4 + 2];
    let t3 = w[(i - 1) * 4 + 3];
    if (i % 4 === 0) {
      const tmp = t0;
      t0 = SBOX[t1] ^ RCON[i / 4 - 1];
      t1 = SBOX[t2];
      t2 = SBOX[t3];
      t3 = SBOX[tmp];
    }
    const p = (i - 4) * 4;
    w[i * 4] = w[p] ^ t0;
    w[i * 4 + 1] = w[p + 1] ^ t1;
    w[i * 4 + 2] = w[p + 2] ^ t2;
    w[i * 4 + 3] = w[p + 3] ^ t3;
  }
  return w;
}
function aes128EncryptBlock(rk, block) {
  if (block.length !== 16) throw new Error(`AES: 块必须是 16 字节（得到 ${block.length}）`);
  const s = new Uint8Array(block);
  const addRoundKey = (round) => {
    for (let i = 0; i < 16; i += 1) s[i] = s[i] ^ rk[round * 16 + i];
  };
  const subBytes = () => {
    for (let i = 0; i < 16; i += 1) s[i] = SBOX[s[i]];
  };
  const shiftRows = () => {
    for (let r = 1; r < 4; r += 1) {
      const row = [s[r], s[r + 4], s[r + 8], s[r + 12]];
      for (let c = 0; c < 4; c += 1) s[r + 4 * c] = row[(c + r) % 4];
    }
  };
  const mixColumns = () => {
    for (let c = 0; c < 4; c += 1) {
      const i = 4 * c;
      const a0 = s[i];
      const a1 = s[i + 1];
      const a2 = s[i + 2];
      const a3 = s[i + 3];
      s[i] = gfMul(a0, 2) ^ gfMul(a1, 3) ^ a2 ^ a3;
      s[i + 1] = a0 ^ gfMul(a1, 2) ^ gfMul(a2, 3) ^ a3;
      s[i + 2] = a0 ^ a1 ^ gfMul(a2, 2) ^ gfMul(a3, 3);
      s[i + 3] = gfMul(a0, 3) ^ a1 ^ a2 ^ gfMul(a3, 2);
    }
  };
  addRoundKey(0);
  for (let round = 1; round <= 9; round += 1) {
    subBytes();
    shiftRows();
    mixColumns();
    addRoundKey(round);
  }
  subBytes();
  shiftRows();
  addRoundKey(10);
  return s;
}
function pkcs7Pad(msg, blockSize) {
  const pad = blockSize - msg.length % blockSize;
  const out = new Uint8Array(msg.length + pad);
  out.set(msg);
  out.fill(pad, msg.length);
  return out;
}
function aes128CbcEncrypt(key, iv, msg) {
  if (iv.length !== 16) throw new Error("AES: IV 必须是 16 字节");
  if (msg.length % 16 !== 0) throw new Error(`AES: 明文必须是 16 的整数倍（得到 ${msg.length}）`);
  const rk = aes128KeyExpansion(key);
  const out = new Uint8Array(msg.length);
  let prev = new Uint8Array(iv);
  for (let off = 0; off < msg.length; off += 16) {
    const blk = new Uint8Array(16);
    for (let i = 0; i < 16; i += 1) blk[i] = msg[off + i] ^ prev[i];
    prev = aes128EncryptBlock(rk, blk);
    out.set(prev, off);
  }
  return out;
}
var AES_FIXED_IV = (() => {
  const out = new Uint8Array(16);
  for (let i = 0; i < 16; i += 1) out[i] = "0102030405060708".charCodeAt(i);
  return out;
})();
function aesEncryptCompatHex(key, message) {
  const padded = pkcs7Pad(utf8(message), 16);
  return toHex(aes128CbcEncrypt(fitKey16(key), AES_FIXED_IV, padded));
}

// src/crypto/desTables.ts
var PC1 = [
  57,
  49,
  41,
  33,
  25,
  17,
  9,
  1,
  58,
  50,
  42,
  34,
  26,
  18,
  10,
  2,
  59,
  51,
  43,
  35,
  27,
  19,
  11,
  3,
  60,
  52,
  44,
  36,
  63,
  55,
  47,
  39,
  31,
  23,
  15,
  7,
  62,
  54,
  46,
  38,
  30,
  22,
  14,
  6,
  61,
  53,
  45,
  37,
  29,
  21,
  13,
  5,
  28,
  20,
  12,
  4
];
var PC2 = [
  14,
  17,
  11,
  24,
  1,
  5,
  3,
  28,
  15,
  6,
  21,
  10,
  23,
  19,
  12,
  4,
  26,
  8,
  16,
  7,
  27,
  20,
  13,
  2,
  41,
  52,
  31,
  37,
  47,
  55,
  30,
  40,
  51,
  45,
  33,
  48,
  44,
  49,
  39,
  56,
  34,
  53,
  46,
  42,
  50,
  36,
  29,
  32
];
var BIT_SHIFTS = [1, 2, 4, 6, 8, 10, 12, 14, 15, 17, 19, 21, 23, 25, 27, 28];
var SBOX_P = [
  {
    0: 8421888,
    268435456: 32768,
    536870912: 8421378,
    805306368: 2,
    1073741824: 512,
    1342177280: 8421890,
    1610612736: 8389122,
    1879048192: 8388608,
    2147483648: 514,
    2415919104: 8389120,
    2684354560: 33280,
    2952790016: 8421376,
    3221225472: 32770,
    3489660928: 8388610,
    3758096384: 0,
    4026531840: 33282,
    134217728: 0,
    402653184: 8421890,
    671088640: 33282,
    939524096: 32768,
    1207959552: 8421888,
    1476395008: 512,
    1744830464: 8421378,
    2013265920: 2,
    2281701376: 8389120,
    2550136832: 33280,
    2818572288: 8421376,
    3087007744: 8389122,
    3355443200: 8388610,
    3623878656: 32770,
    3892314112: 514,
    4160749568: 8388608,
    1: 32768,
    268435457: 2,
    536870913: 8421888,
    805306369: 8388608,
    1073741825: 8421378,
    1342177281: 33280,
    1610612737: 512,
    1879048193: 8389122,
    2147483649: 8421890,
    2415919105: 8421376,
    2684354561: 8388610,
    2952790017: 33282,
    3221225473: 514,
    3489660929: 8389120,
    3758096385: 32770,
    4026531841: 0,
    134217729: 8421890,
    402653185: 8421376,
    671088641: 8388608,
    939524097: 512,
    1207959553: 32768,
    1476395009: 8388610,
    1744830465: 2,
    2013265921: 33282,
    2281701377: 32770,
    2550136833: 8389122,
    2818572289: 514,
    3087007745: 8421888,
    3355443201: 8389120,
    3623878657: 0,
    3892314113: 33280,
    4160749569: 8421378
  },
  {
    0: 1074282512,
    16777216: 16384,
    33554432: 524288,
    50331648: 1074266128,
    67108864: 1073741840,
    83886080: 1074282496,
    100663296: 1073758208,
    117440512: 16,
    134217728: 540672,
    150994944: 1073758224,
    167772160: 1073741824,
    184549376: 540688,
    201326592: 524304,
    218103808: 0,
    234881024: 16400,
    251658240: 1074266112,
    8388608: 1073758208,
    25165824: 540688,
    41943040: 16,
    58720256: 1073758224,
    75497472: 1074282512,
    92274688: 1073741824,
    109051904: 524288,
    125829120: 1074266128,
    142606336: 524304,
    159383552: 0,
    176160768: 16384,
    192937984: 1074266112,
    209715200: 1073741840,
    226492416: 540672,
    243269632: 1074282496,
    260046848: 16400,
    268435456: 0,
    285212672: 1074266128,
    301989888: 1073758224,
    318767104: 1074282496,
    335544320: 1074266112,
    352321536: 16,
    369098752: 540688,
    385875968: 16384,
    402653184: 16400,
    419430400: 524288,
    436207616: 524304,
    452984832: 1073741840,
    469762048: 540672,
    486539264: 1073758208,
    503316480: 1073741824,
    520093696: 1074282512,
    276824064: 540688,
    293601280: 524288,
    310378496: 1074266112,
    327155712: 16384,
    343932928: 1073758208,
    360710144: 1074282512,
    377487360: 16,
    394264576: 1073741824,
    411041792: 1074282496,
    427819008: 1073741840,
    444596224: 1073758224,
    461373440: 524304,
    478150656: 0,
    494927872: 16400,
    511705088: 1074266128,
    528482304: 540672
  },
  {
    0: 260,
    1048576: 0,
    2097152: 67109120,
    3145728: 65796,
    4194304: 65540,
    5242880: 67108868,
    6291456: 67174660,
    7340032: 67174400,
    8388608: 67108864,
    9437184: 67174656,
    10485760: 65792,
    11534336: 67174404,
    12582912: 67109124,
    13631488: 65536,
    14680064: 4,
    15728640: 256,
    524288: 67174656,
    1572864: 67174404,
    2621440: 0,
    3670016: 67109120,
    4718592: 67108868,
    5767168: 65536,
    6815744: 65540,
    7864320: 260,
    8912896: 4,
    9961472: 256,
    11010048: 67174400,
    12058624: 65796,
    13107200: 65792,
    14155776: 67109124,
    15204352: 67174660,
    16252928: 67108864,
    16777216: 67174656,
    17825792: 65540,
    18874368: 65536,
    19922944: 67109120,
    20971520: 256,
    22020096: 67174660,
    23068672: 67108868,
    24117248: 0,
    25165824: 67109124,
    26214400: 67108864,
    27262976: 4,
    28311552: 65792,
    29360128: 67174400,
    30408704: 260,
    31457280: 65796,
    32505856: 67174404,
    17301504: 67108864,
    18350080: 260,
    19398656: 67174656,
    20447232: 0,
    21495808: 65540,
    22544384: 67109120,
    23592960: 256,
    24641536: 67174404,
    25690112: 65536,
    26738688: 67174660,
    27787264: 65796,
    28835840: 67108868,
    29884416: 67109124,
    30932992: 67174400,
    31981568: 4,
    33030144: 65792
  },
  {
    0: 2151682048,
    65536: 2147487808,
    131072: 4198464,
    196608: 2151677952,
    262144: 0,
    327680: 4198400,
    393216: 2147483712,
    458752: 4194368,
    524288: 2147483648,
    589824: 4194304,
    655360: 64,
    720896: 2147487744,
    786432: 2151678016,
    851968: 4160,
    917504: 4096,
    983040: 2151682112,
    32768: 2147487808,
    98304: 64,
    163840: 2151678016,
    229376: 2147487744,
    294912: 4198400,
    360448: 2151682112,
    425984: 0,
    491520: 2151677952,
    557056: 4096,
    622592: 2151682048,
    688128: 4194304,
    753664: 4160,
    819200: 2147483648,
    884736: 4194368,
    950272: 4198464,
    1015808: 2147483712,
    1048576: 4194368,
    1114112: 4198400,
    1179648: 2147483712,
    1245184: 0,
    1310720: 4160,
    1376256: 2151678016,
    1441792: 2151682048,
    1507328: 2147487808,
    1572864: 2151682112,
    1638400: 2147483648,
    1703936: 2151677952,
    1769472: 4198464,
    1835008: 2147487744,
    1900544: 4194304,
    1966080: 64,
    2031616: 4096,
    1081344: 2151677952,
    1146880: 2151682112,
    1212416: 0,
    1277952: 4198400,
    1343488: 4194368,
    1409024: 2147483648,
    1474560: 2147487808,
    1540096: 64,
    1605632: 2147483712,
    1671168: 4096,
    1736704: 2147487744,
    1802240: 2151678016,
    1867776: 4160,
    1933312: 2151682048,
    1998848: 4194304,
    2064384: 4198464
  },
  {
    0: 128,
    4096: 17039360,
    8192: 262144,
    12288: 536870912,
    16384: 537133184,
    20480: 16777344,
    24576: 553648256,
    28672: 262272,
    32768: 16777216,
    36864: 537133056,
    40960: 536871040,
    45056: 553910400,
    49152: 553910272,
    53248: 0,
    57344: 17039488,
    61440: 553648128,
    2048: 17039488,
    6144: 553648256,
    10240: 128,
    14336: 17039360,
    18432: 262144,
    22528: 537133184,
    26624: 553910272,
    30720: 536870912,
    34816: 537133056,
    38912: 0,
    43008: 553910400,
    47104: 16777344,
    51200: 536871040,
    55296: 553648128,
    59392: 16777216,
    63488: 262272,
    65536: 262144,
    69632: 128,
    73728: 536870912,
    77824: 553648256,
    81920: 16777344,
    86016: 553910272,
    90112: 537133184,
    94208: 16777216,
    98304: 553910400,
    102400: 553648128,
    106496: 17039360,
    110592: 537133056,
    114688: 262272,
    118784: 536871040,
    122880: 0,
    126976: 17039488,
    67584: 553648256,
    71680: 16777216,
    75776: 17039360,
    79872: 537133184,
    83968: 536870912,
    88064: 17039488,
    92160: 128,
    96256: 553910272,
    100352: 262272,
    104448: 553910400,
    108544: 0,
    112640: 553648128,
    116736: 16777344,
    120832: 262144,
    124928: 537133056,
    129024: 536871040
  },
  {
    0: 268435464,
    256: 8192,
    512: 270532608,
    768: 270540808,
    1024: 268443648,
    1280: 2097152,
    1536: 2097160,
    1792: 268435456,
    2048: 0,
    2304: 268443656,
    2560: 2105344,
    2816: 8,
    3072: 270532616,
    3328: 2105352,
    3584: 8200,
    3840: 270540800,
    128: 270532608,
    384: 270540808,
    640: 8,
    896: 2097152,
    1152: 2105352,
    1408: 268435464,
    1664: 268443648,
    1920: 8200,
    2176: 2097160,
    2432: 8192,
    2688: 268443656,
    2944: 270532616,
    3200: 0,
    3456: 270540800,
    3712: 2105344,
    3968: 268435456,
    4096: 268443648,
    4352: 270532616,
    4608: 270540808,
    4864: 8200,
    5120: 2097152,
    5376: 268435456,
    5632: 268435464,
    5888: 2105344,
    6144: 2105352,
    6400: 0,
    6656: 8,
    6912: 270532608,
    7168: 8192,
    7424: 268443656,
    7680: 270540800,
    7936: 2097160,
    4224: 8,
    4480: 2105344,
    4736: 2097152,
    4992: 268435464,
    5248: 268443648,
    5504: 8200,
    5760: 270540808,
    6016: 270532608,
    6272: 270540800,
    6528: 270532616,
    6784: 8192,
    7040: 2105352,
    7296: 2097160,
    7552: 0,
    7808: 268435456,
    8064: 268443656
  },
  {
    0: 1048576,
    16: 33555457,
    32: 1024,
    48: 1049601,
    64: 34604033,
    80: 0,
    96: 1,
    112: 34603009,
    128: 33555456,
    144: 1048577,
    160: 33554433,
    176: 34604032,
    192: 34603008,
    208: 1025,
    224: 1049600,
    240: 33554432,
    8: 34603009,
    24: 0,
    40: 33555457,
    56: 34604032,
    72: 1048576,
    88: 33554433,
    104: 33554432,
    120: 1025,
    136: 1049601,
    152: 33555456,
    168: 34603008,
    184: 1048577,
    200: 1024,
    216: 34604033,
    232: 1,
    248: 1049600,
    256: 33554432,
    272: 1048576,
    288: 33555457,
    304: 34603009,
    320: 1048577,
    336: 33555456,
    352: 34604032,
    368: 1049601,
    384: 1025,
    400: 34604033,
    416: 1049600,
    432: 1,
    448: 0,
    464: 34603008,
    480: 33554433,
    496: 1024,
    264: 1049600,
    280: 33555457,
    296: 34603009,
    312: 1,
    328: 33554432,
    344: 1048576,
    360: 1025,
    376: 34604032,
    392: 33554433,
    408: 34603008,
    424: 0,
    440: 34604033,
    456: 1049601,
    472: 1024,
    488: 33555456,
    504: 1048577
  },
  {
    0: 134219808,
    1: 131072,
    2: 134217728,
    3: 32,
    4: 131104,
    5: 134350880,
    6: 134350848,
    7: 2048,
    8: 134348800,
    9: 134219776,
    10: 133120,
    11: 134348832,
    12: 2080,
    13: 0,
    14: 134217760,
    15: 133152,
    2147483648: 2048,
    2147483649: 134350880,
    2147483650: 134219808,
    2147483651: 134217728,
    2147483652: 134348800,
    2147483653: 133120,
    2147483654: 133152,
    2147483655: 32,
    2147483656: 134217760,
    2147483657: 2080,
    2147483658: 131104,
    2147483659: 134350848,
    2147483660: 0,
    2147483661: 134348832,
    2147483662: 134219776,
    2147483663: 131072,
    16: 133152,
    17: 134350848,
    18: 32,
    19: 2048,
    20: 134219776,
    21: 134217760,
    22: 134348832,
    23: 131072,
    24: 0,
    25: 131104,
    26: 134348800,
    27: 134219808,
    28: 134350880,
    29: 133120,
    30: 2080,
    31: 134217728,
    2147483664: 131072,
    2147483665: 2048,
    2147483666: 134348832,
    2147483667: 133152,
    2147483668: 32,
    2147483669: 134348800,
    2147483670: 134217728,
    2147483671: 134219808,
    2147483672: 134350880,
    2147483673: 134217760,
    2147483674: 134219776,
    2147483675: 0,
    2147483676: 133120,
    2147483677: 2080,
    2147483678: 131104,
    2147483679: 134350848
  }
];
var SBOX_MASK = [
  4160749569,
  528482304,
  33030144,
  2064384,
  129024,
  8064,
  504,
  2147483679
];

// src/crypto/des.ts
function fitKey8(key) {
  const out = new Uint8Array(8);
  const src = utf8(key);
  out.set(src.subarray(0, 8));
  return out;
}
function zeroPadToBlock(msg, blockSize) {
  const rem = msg.length % blockSize;
  if (rem === 0) return msg;
  const out = new Uint8Array(msg.length + (blockSize - rem));
  out.set(msg);
  return out;
}
function keyWords(key) {
  const be = (o) => (key[o] << 24 | key[o + 1] << 16 | key[o + 2] << 8 | key[o + 3]) >>> 0;
  return [be(0), be(4)];
}
function desSubKeys(key) {
  const [w0, w1] = keyWords(key);
  const words = [w0, w1];
  const keyBits = [];
  for (let i = 0; i < 56; i += 1) {
    const p = PC1[i] - 1;
    keyBits[i] = words[p >>> 5] >>> 31 - p % 32 & 1;
  }
  const subKeys = [];
  for (let n = 0; n < 16; n += 1) {
    const subKey = [];
    const bitShift = BIT_SHIFTS[n];
    for (let i = 0; i < 24; i += 1) {
      const left = keyBits[(PC2[i] - 1 + bitShift) % 28] << 31 - i % 6;
      const right = keyBits[28 + (PC2[i + 24] - 1 + bitShift) % 28] << 31 - i % 6;
      const a = i / 6 | 0;
      subKey[a] = (subKey[a] ?? 0) | left;
      subKey[4 + a] = (subKey[4 + a] ?? 0) | right;
    }
    subKey[0] = (subKey[0] << 1 | subKey[0] >>> 31) >>> 0;
    for (let i = 1; i < 7; i += 1) subKey[i] = subKey[i] >>> (i - 1) * 4 + 3;
    subKey[7] = (subKey[7] << 5 | subKey[7] >>> 27) >>> 0;
    subKeys.push(subKey);
  }
  return subKeys;
}
function desCryptBlock(l, r, subKeys) {
  let L = l >>> 0;
  let R = r >>> 0;
  const exLR = (off, mask) => {
    const t2 = (L >>> off ^ R) & mask;
    R = (R ^ t2) >>> 0;
    L = (L ^ t2 << off) >>> 0;
  };
  const exRL = (off, mask) => {
    const t2 = (R >>> off ^ L) & mask;
    L = (L ^ t2) >>> 0;
    R = (R ^ t2 << off) >>> 0;
  };
  exLR(4, 252645135);
  exLR(16, 65535);
  exRL(2, 858993459);
  exRL(8, 16711935);
  exLR(1, 1431655765);
  for (let round = 0; round < 16; round += 1) {
    const sk = subKeys[round];
    let f = 0;
    for (let i = 0; i < 8; i += 1) {
      const idx = ((R ^ sk[i]) & SBOX_MASK[i]) >>> 0;
      f |= SBOX_P[i][idx] ?? 0;
    }
    const nl = R;
    const nr = (L ^ f) >>> 0;
    L = nl;
    R = nr;
  }
  const t = L;
  L = R;
  R = t;
  exLR(1, 1431655765);
  exRL(8, 16711935);
  exRL(2, 858993459);
  exLR(16, 65535);
  exLR(4, 252645135);
  return [L >>> 0, R >>> 0];
}
function desEcbEncrypt(key, msg) {
  if (key.length !== 8) throw new Error(`DES: 密钥必须是 8 字节（得到 ${key.length}）`);
  const padded = zeroPadToBlock(msg, 8);
  if (padded.length === 0) return new Uint8Array(0);
  const subKeys = desSubKeys(key);
  const out = new Uint8Array(padded.length);
  for (let off = 0; off < padded.length; off += 8) {
    const l = (padded[off] << 24 | padded[off + 1] << 16 | padded[off + 2] << 8 | padded[off + 3]) >>> 0;
    const r = (padded[off + 4] << 24 | padded[off + 5] << 16 | padded[off + 6] << 8 | padded[off + 7]) >>> 0;
    const [cl, cr] = desCryptBlock(l, r, subKeys);
    out[off] = cl >>> 24 & 255;
    out[off + 1] = cl >>> 16 & 255;
    out[off + 2] = cl >>> 8 & 255;
    out[off + 3] = cl & 255;
    out[off + 4] = cr >>> 24 & 255;
    out[off + 5] = cr >>> 16 & 255;
    out[off + 6] = cr >>> 8 & 255;
    out[off + 7] = cr & 255;
  }
  return out;
}
function desEncryptBase64(message, key) {
  const padded = zeroPadToBlock(utf8(message), 8);
  if (padded.length === 0) return "";
  return toBase64(desEcbEncrypt(fitKey8(key), padded));
}

// src/crypto/gzip.ts
var GZIP_OS = 19;
var CRC_TABLE = (() => {
  const t = new Uint32Array(256);
  for (let n = 0; n < 256; n += 1) {
    let c = n;
    for (let k = 0; k < 8; k += 1) c = c & 1 ? 3988292384 ^ c >>> 1 : c >>> 1;
    t[n] = c >>> 0;
  }
  return t;
})();
function crc32(data) {
  let c = 4294967295;
  for (let i = 0; i < data.length; i += 1) {
    c = CRC_TABLE[(c ^ data[i]) & 255] ^ c >>> 8;
  }
  return (c ^ 4294967295) >>> 0;
}
function deflateStored(data) {
  const MAX = 65535;
  const chunks = data.length === 0 ? [data.subarray(0, 0)] : [];
  for (let off = 0; off < data.length; off += MAX) {
    chunks.push(data.subarray(off, Math.min(off + MAX, data.length)));
  }
  let total = 0;
  for (const c of chunks) total += 5 + c.length;
  const out = new Uint8Array(total);
  let p = 0;
  chunks.forEach((c, i) => {
    const last = i === chunks.length - 1;
    out[p++] = last ? 1 : 0;
    const len = c.length;
    out[p++] = len & 255;
    out[p++] = len >>> 8 & 255;
    out[p++] = ~len & 255;
    out[p++] = ~len >>> 8 & 255;
    out.set(c, p);
    p += len;
  });
  return out;
}
function gzipWithOs19(data) {
  const body = deflateStored(data);
  const out = new Uint8Array(10 + body.length + 8);
  out[0] = 31;
  out[1] = 139;
  out[2] = 8;
  out[3] = 0;
  out[4] = 0;
  out[5] = 0;
  out[6] = 0;
  out[7] = 0;
  out[8] = 0;
  out[9] = GZIP_OS;
  out.set(body, 10);
  const crc = crc32(data);
  const n = data.length >>> 0;
  const tail = 10 + body.length;
  out[tail] = crc & 255;
  out[tail + 1] = crc >>> 8 & 255;
  out[tail + 2] = crc >>> 16 & 255;
  out[tail + 3] = crc >>> 24 & 255;
  out[tail + 4] = n & 255;
  out[tail + 5] = n >>> 8 & 255;
  out[tail + 6] = n >>> 16 & 255;
  out[tail + 7] = n >>> 24 & 255;
  return out;
}
function gzipWithOs19Base64(data) {
  return toBase64(gzipWithOs19(data));
}

// src/crypto/rsa.ts
function derRead(buf, at) {
  const tag = buf[at];
  let p = at + 1;
  let len = buf[p];
  p += 1;
  if ((len & 128) !== 0) {
    const nLenBytes = len & 127;
    if (nLenBytes === 0 || nLenBytes > 4) {
      throw new Error(`RSA: 不支持的长度形式（0x80|${nLenBytes}）`);
    }
    len = 0;
    for (let i = 0; i < nLenBytes; i += 1) len = len * 256 + buf[p + i];
    p += nLenBytes;
  }
  return { tag, start: p, end: p + len };
}
function parseSpkiRsaPublicKey(b64) {
  const der = fromBase64(b64);
  const outer = derRead(der, 0);
  if (outer.tag !== 48) throw new Error("RSA: SPKI 不是 SEQUENCE");
  const algo = derRead(der, outer.start);
  if (algo.tag !== 48) throw new Error("RSA: AlgorithmIdentifier 缺失");
  const bits2 = derRead(der, algo.end);
  if (bits2.tag !== 3) throw new Error("RSA: BIT STRING 缺失");
  if (der[bits2.start] !== 0) throw new Error("RSA: BIT STRING 有未用位");
  const inner = derRead(der, bits2.start + 1);
  if (inner.tag !== 48) throw new Error("RSA: 公钥不是 SEQUENCE");
  const intAt = (p) => {
    const it = derRead(der, p);
    if (it.tag !== 2) throw new Error("RSA: 期望 INTEGER");
    let v = 0n;
    for (let i = it.start; i < it.end; i += 1) v = v << 8n | BigInt(der[i]);
    return { v, next: it.end };
  };
  const nn = intAt(inner.start);
  const ee = intAt(nn.next);
  if (nn.v <= 0n || ee.v <= 0n) throw new Error("RSA: 公钥分量非正数");
  return { n: nn.v, e: ee.v };
}
function modPow(base, exp, m) {
  let result = 1n;
  let b = base % m;
  let e = exp;
  while (e > 0n) {
    if (e & 1n) result = result * b % m;
    b = b * b % m;
    e >>= 1n;
  }
  return result;
}
function bytesToBig(bytes) {
  let v = 0n;
  for (let i = 0; i < bytes.length; i += 1) v = v << 8n | BigInt(bytes[i]);
  return v;
}
function bigToBytes(v, len) {
  const out = new Uint8Array(len);
  let x = v;
  for (let i = len - 1; i >= 0; i -= 1) {
    out[i] = Number(x & 0xffn);
    x >>= 8n;
  }
  if (x !== 0n) throw new Error(`RSA: 模幂结果装不进 ${len} 字节`);
  return out;
}
function rsaEncryptPkcs1v15(key, message, rng) {
  const k = Math.ceil(key.n.toString(16).length / 2);
  if (message.length > k - 11) {
    throw new Error(`RSA: 明文太长（${message.length} > ${k - 11}）`);
  }
  const psLen = k - message.length - 3;
  const ps = new Uint8Array(psLen);
  let filled = 0;
  let rounds = 0;
  while (filled < psLen) {
    if ((rounds += 1) > 64) {
      throw new Error("RSA: 随机源像是坏了（连取 64 轮都填不满 PS，取到的几乎全是 0）");
    }
    const chunk = rng(Math.max(8, psLen - filled));
    for (let i = 0; i < chunk.length && filled < psLen; i += 1) {
      if (chunk[i] !== 0) {
        ps[filled] = chunk[i];
        filled += 1;
      }
    }
  }
  const em = new Uint8Array(k);
  em[0] = 0;
  em[1] = 2;
  em.set(ps, 2);
  em[2 + psLen] = 0;
  em.set(message, 3 + psLen);
  const c = modPow(bytesToBig(em), key.e, key.n);
  return bigToBytes(c, k);
}
function rsaEncryptBase64(publicKeyB64, message, rng) {
  return toBase64(rsaEncryptPkcs1v15(parseSpkiRsaPublicKey(publicKeyB64), message, rng));
}

// src/mcloud/sign.ts
var SALT_MAP = [
  [1, "seedMdYYLIZfbCxg"],
  [2, "sekaMdYYLIZfbCfm"]
];
function saltFor(saltKey) {
  return saltKey === 2 ? SALT_MAP[1][1] : SALT_MAP[0][1];
}
function generateSignature(id, timestamp, nonce2, saltKey, other) {
  const salt = saltFor(saltKey);
  return md5Hex(salt + id + timestamp + nonce2 + other + salt);
}
function signHeadersWith(id, nonce2, timestampMs, saltKey, other) {
  return {
    "x-request-id": id,
    "x-timestamp": timestampMs,
    "x-nonce": nonce2,
    "x-signature": generateSignature(id, timestampMs, nonce2, saltKey, other)
  };
}

// src/mcloud/rand.ts
var counter = 0;
function rnd32() {
  return Math.floor(Math.random() * 4294967296) >>> 0;
}
var HEX2 = "0123456789abcdef";
function uuidV4() {
  const b = new Uint8Array(16);
  for (let i = 0; i < 16; i += 4) {
    const v = rnd32();
    b[i] = v >>> 24 & 255;
    b[i + 1] = v >>> 16 & 255;
    b[i + 2] = v >>> 8 & 255;
    b[i + 3] = v & 255;
  }
  b[6] = b[6] & 15 | 64;
  b[8] = b[8] & 63 | 128;
  let s = "";
  for (let i = 0; i < 16; i += 1) {
    const v = b[i];
    s += HEX2[v >> 4] + HEX2[v & 15];
    if (i === 3 || i === 5 || i === 7 || i === 9) s += "-";
  }
  return s;
}
function nonce() {
  counter = (counter + 1) % 1e6;
  const r = rnd32().toString(16).padStart(8, "0");
  return `${Date.now().toString(16)}-${counter.toString(16)}-${r}`;
}

// src/mcloud/h5.ts
var MARKET_NAME = "sign_in_3";
var H5_APP_VERSION = "12.6.0.0";
function buildH5Request(spec) {
  const ts = spec.signInputs?.timestampMs ?? String(Date.now());
  const sign = signHeadersWith(
    spec.signInputs?.id ?? uuidV4(),
    spec.signInputs?.nonce ?? nonce(),
    ts,
    spec.saltKey,
    spec.other ?? ""
  );
  const headers = {
    Accept: "*/*",
    // ⚠ 跟本次的 marketname 走，不是恒定的 sign_in_3
    activityid: spec.marketName ?? MARKET_NAME,
    appversion: H5_APP_VERSION,
    Referer: spec.referer,
    showloading: spec.showLoading
  };
  if (spec.isDeviceId) headers.isdeviceid = "true";
  if (spec.session.jwt) headers.jwttoken = spec.session.jwt;
  if (spec.session.deviceId) headers.deviceid = spec.session.deviceId;
  if (spec.session.userAgent) headers["user-agent"] = spec.session.userAgent;
  if (spec.session.cookie) headers.cookie = spec.session.cookie;
  if (spec.method === "POST") {
    headers["content-type"] = spec.contentType ?? "application/json";
  }
  return {
    url: spec.url,
    method: spec.method,
    headers: { ...headers, ...sign },
    body: spec.method === "POST" ? spec.body ?? "null" : void 0
  };
}
var MCLOUD_HOST = "https://m.mcloud.139.com";
var EP = {
  infoV3: { origin: MCLOUD_HOST, path: "/ycloud/signin/page/infoV3" },
  startSignIn: { origin: MCLOUD_HOST, path: "/ycloud/signin/page/startSignIn" },
  receiveV3: { origin: MCLOUD_HOST, path: "/ycloud/signin/page/receiveV3" },
  taskExpansion: { origin: MCLOUD_HOST, path: "/ycloud/signin/page/taskExpansion" },
  receiveTaskExpansion: { origin: MCLOUD_HOST, path: "/ycloud/signin/page/receiveTaskExpansion" },
  receiveRevivalReward: { origin: MCLOUD_HOST, path: "/ycloud/signin/page/receiveRevivalReward" },
  multiple: { origin: MCLOUD_HOST, path: "/ycloud/signin/page/multiple" },
  prizeRecord: { origin: MCLOUD_HOST, path: "/ycloud/signin/public/record" },
  cloudRecordV2: { origin: MCLOUD_HOST, path: "/ycloud/signin/public/cloudRecordV2" },
  taskListV2: { origin: MCLOUD_HOST, path: "/ycloud/signin/task/taskListV2" },
  taskListV3: { origin: MCLOUD_HOST, path: "/ycloud/signin/task/taskListV3" },
  // 兑换清单：**只读**，查询串只有 client 与 clientVersion，**不带 deviceId**
  exchangeList: { origin: MCLOUD_HOST, path: "/ycloud/signin/page/exchangeList" }
};
function resolveUrl(name, base) {
  const ep = EP[name];
  return base ? base + ep.path : ep.origin + ep.path;
}
var REFERER_SIGN_IN = `${MCLOUD_HOST}/mcloud/sign_in/index.html`;
var SHOW_LOADING = "true";
var EP_METHOD = {
  infoV3: "GET",
  //  ?client=app（有 deviceId 时再 &deviceId=…）
  startSignIn: "GET",
  //  ?client=app（有 deviceId 时再 &deviceId=…）
  receiveV3: "POST",
  // json body
  taskExpansion: "GET",
  // 更正记录：这一条我起初写成 POST，查 `receive_task_expansion_request` 后改成 GET
  // （`h5(false, HttpRequest::get(url), "true")`）。表是从源码抄的，抄错就会得到 405——
  // 而 405 只会说"方法不对"，不会说"该用哪个"。
  receiveTaskExpansion: "GET",
  receiveRevivalReward: "POST",
  // json body `{}`
  multiple: "GET",
  prizeRecord: "GET",
  // 带 query 参数
  cloudRecordV2: "GET",
  // 带 query 参数
  taskListV2: "POST",
  // body 带 group=<组名>
  taskListV3: "POST",
  // body 带 group=""（空串）
  exchangeList: "GET"
};
function rootOrDefault(base, fallback) {
  const t = (base ?? "").trim();
  return t === "" ? fallback : t.replace(/\/+$/, "");
}

// src/mcloud/smidRules.ts
var SKLAND_SM_CONFIG = {
  organization: "FXlyfmWg2AzwbrxDKSv5",
  appId: "default",
  publicKey: "MIGfMA0GCSqGSIb3DQEBAQUAA4GNADCBiQKBgQC8KHAcHbkCn5rxGgGJE+07tY+pt86D/oZ7sA51FaEBv2jgno2TI9zHJVYKJynmiKpixgwUcv93EfWIrU/p/UCs5Vu+odS3I4UBp3R7IZ1A0W01FkumAHYW2PQpMm8ueQKPLUq/idkpG/9b2JDv/qU+Ks36nbUPwlW4CjdfrV+V9QIDAQAB",
  protocol: "https",
  apiHost: "slw.h5cmpassport.com:9090",
  apiPath: "/deviceprofile/v4"
};
var DES_RULE = {
  appId: {
    cipher: "DES",
    is_encrypt: 1,
    key: "a9kbown8",
    obfuscated_name: "um"
  },
  box: {
    is_encrypt: 0,
    obfuscated_name: "aw"
  },
  canvas: {
    cipher: "DES",
    is_encrypt: 1,
    key: "ultuhs59",
    obfuscated_name: "iu"
  },
  clientSize: {
    cipher: "DES",
    is_encrypt: 1,
    key: "7s9eixjh",
    obfuscated_name: "na"
  },
  organization: {
    cipher: "DES",
    is_encrypt: 1,
    key: "14e30pp1",
    obfuscated_name: "nl"
  },
  os: {
    cipher: "DES",
    is_encrypt: 1,
    key: "j862vr1f",
    obfuscated_name: "lz"
  },
  platform: {
    cipher: "DES",
    is_encrypt: 1,
    key: "pgutfbo7",
    obfuscated_name: "lm"
  },
  plugins: {
    cipher: "DES",
    is_encrypt: 1,
    key: "x98c92bm",
    obfuscated_name: "hy"
  },
  pmf: {
    cipher: "DES",
    is_encrypt: 1,
    key: "gwvvsmr4",
    obfuscated_name: "yu"
  },
  protocol: {
    is_encrypt: 0,
    obfuscated_name: "protocol"
  },
  referer: {
    cipher: "DES",
    is_encrypt: 1,
    key: "gka1ljjt",
    obfuscated_name: "mf"
  },
  res: {
    cipher: "DES",
    is_encrypt: 1,
    key: "ht2n6dwf",
    obfuscated_name: "ut"
  },
  rtype: {
    cipher: "DES",
    is_encrypt: 1,
    key: "k2p9ifij",
    obfuscated_name: "fm"
  },
  sdkver: {
    cipher: "DES",
    is_encrypt: 1,
    key: "q4ij0mv5",
    obfuscated_name: "ir"
  },
  status: {
    cipher: "DES",
    is_encrypt: 1,
    key: "moyspi7i",
    obfuscated_name: "kp"
  },
  subVersion: {
    cipher: "DES",
    is_encrypt: 1,
    key: "egsyl21v",
    obfuscated_name: "dk"
  },
  svm: {
    cipher: "DES",
    is_encrypt: 1,
    key: "yf780e9w",
    obfuscated_name: "ee"
  },
  time: {
    cipher: "DES",
    is_encrypt: 1,
    key: "btb17oag",
    obfuscated_name: "jt"
  },
  timezone: {
    cipher: "DES",
    is_encrypt: 1,
    key: "l94h8clc",
    obfuscated_name: "bl"
  },
  tn: {
    cipher: "DES",
    is_encrypt: 1,
    key: "crnw0x7e",
    obfuscated_name: "as"
  },
  trees: {
    cipher: "DES",
    is_encrypt: 1,
    key: "x6the9nq",
    obfuscated_name: "ij"
  },
  ua: {
    cipher: "DES",
    is_encrypt: 1,
    key: "bewicpxu",
    obfuscated_name: "pu"
  },
  url: {
    cipher: "DES",
    is_encrypt: 1,
    key: "llj8yd1x",
    obfuscated_name: "sb"
  },
  version: {
    is_encrypt: 0,
    obfuscated_name: "version"
  },
  vpw: {
    cipher: "DES",
    is_encrypt: 1,
    key: "f1zlae4b",
    obfuscated_name: "qb"
  }
};

// src/mcloud/smid.ts
var SMID_HOST = `${SKLAND_SM_CONFIG.protocol}://${SKLAND_SM_CONFIG.apiHost}`;
var SMID_PATH = SKLAND_SM_CONFIG.apiPath;
var SMID_OK_CODE = 1100;
var DEVICE_ID_PREFIX = "B";
var SMID_UA = "Mozilla/5.0 (Linux; Android 14; 22041216C Build/TP1A.220624.014; wv) AppleWebKit/537.36 (KHTML, like Gecko) Version/4.0 Chrome/128.0.6613.88 Mobile Safari/537.36";
var SMID_ORIGIN = "https://m.mcloud.139.com";
var SMID_REFERER = "https://m.mcloud.139.com/portal/mobilecloud/index.html";
var SMID_PAGE_URL = "https://m.mcloud.139.com/portal/mobilecloud/index.html?path=newsignin&sourceid=1002&token=";
var SMID_PLUGINS = "ChromePDFViewerPortableDocumentFormatinternal-pdf-viewer2,ChromiumPDFViewerPortableDocumentFormatinternal-pdf-viewer2,MicrosoftEdgePDFViewerPortableDocumentFormatinternal-pdf-viewer2,PDFViewerPortableDocumentFormatinternal-pdf-viewer2,WebKitbuilt-inPDFPortableDocumentFormatinternal-pdf-viewer2";
var SCREENS = [
  [375, 667],
  [390, 844],
  [414, 896],
  [360, 640],
  [360, 740],
  [412, 915],
  [1920, 1080],
  [1366, 768]
];
var PLATFORMS = ["Linux armv8l", "Linux aarch64"];
var DPRS = [2, 3];
var TOUCH_POINTS = [1, 5];
var CONNECTION_RTTS = [50, 100, 150];
var CPU_COUNTS = [4, 6, 8, 10, 12];
function pick(arr, rnd) {
  return arr[Math.floor(rnd() * arr.length)];
}
function newFingerprint(rnd = Math.random) {
  const s = pick(SCREENS, rnd);
  return {
    screenWidth: s[0],
    screenHeight: s[1],
    dpr: pick(DPRS, rnd),
    platform: pick(PLATFORMS, rnd),
    maxTouchPoints: pick(TOUCH_POINTS, rnd),
    connectionRtt: pick(CONNECTION_RTTS, rnd),
    cpucount: pick(CPU_COUNTS, rnd),
    // canvas 是一个 8 位 hex 摘要 —— 它也是"这台设备"的特征，所以同样只掷一次
    canvas: md5Hex(uuidV4()).slice(0, 8)
  };
}
var STATE_FINGERPRINT = "mcloud.smid.fingerprint";
function loadFingerprint(store2) {
  const raw = store2.getState(STATE_FINGERPRINT);
  if (!raw) return null;
  try {
    const v = JSON.parse(raw);
    if (typeof v.screenWidth !== "number" || typeof v.canvas !== "string") return null;
    return {
      screenWidth: v.screenWidth,
      screenHeight: v.screenHeight ?? v.screenWidth,
      dpr: v.dpr ?? 2,
      platform: v.platform ?? "Linux armv8l",
      maxTouchPoints: v.maxTouchPoints ?? 1,
      connectionRtt: v.connectionRtt ?? 50,
      cpucount: v.cpucount ?? 4,
      canvas: v.canvas
    };
  } catch {
    return null;
  }
}
function ensureFingerprint(store2, nowIso, rnd = Math.random) {
  const had = loadFingerprint(store2);
  if (had) return had;
  const fp = newFingerprint(rnd);
  store2.setState(STATE_FINGERPRINT, JSON.stringify(fp), nowIso);
  return fp;
}
function getTn(obj) {
  const out = [];
  for (const key of Object.keys(obj).sort()) {
    const v = obj[key];
    if (typeof v === "number") {
      out.push(String(Number.isInteger(v) ? v * 1e4 : Math.floor(v * 1e4)));
    } else if (typeof v === "object" && v !== null) {
      out.push(getTn(v));
    } else {
      out.push(String(v));
    }
  }
  return out.join("");
}
function buildSmid(d, uid) {
  const p = (n, w) => String(n).padStart(w, "0");
  const timeStr = p(d.getFullYear(), 4) + p(d.getMonth() + 1, 2) + p(d.getDate(), 2) + p(d.getHours(), 2) + p(d.getMinutes(), 2) + p(d.getSeconds(), 2);
  const v = `${timeStr}${md5Hex(uid)}00`;
  return `${v}${md5Hex(`smsk_web_${v}`).slice(0, 14)}0`;
}
function buildSmBody(fp, box, now, rnd = Math.random) {
  const level = Math.round(rnd() * 100) / 100;
  const body = {
    protocol: 4,
    organization: SKLAND_SM_CONFIG.organization,
    appId: SKLAND_SM_CONFIG.appId,
    os: "web",
    version: "3.0.0",
    sdkver: "3.0.0",
    box,
    rtype: "all",
    smid: buildSmid(now, uuidV4()),
    subVersion: "1.0.0",
    time: 7,
    platform: fp.platform,
    clientSize: `0_0_0_0_${fp.screenWidth}_${fp.screenHeight}_${fp.screenWidth}_${fp.screenHeight}`,
    res: `${fp.screenWidth}_${fp.screenHeight}_${fp.dpr}_2`,
    status: "0010",
    timezone: -480,
    vpw: uuidV4(),
    svm: now.getTime(),
    trees: uuidV4(),
    pmf: now.getTime(),
    cdp: 0,
    maxTouchPoints: fp.maxTouchPoints,
    connectionRtt: fp.connectionRtt,
    cpucount: fp.cpucount,
    battery: { charging: pick([0, 1], rnd), level },
    canvas: fp.canvas,
    plugins: SMID_PLUGINS,
    referer: "",
    ua: SMID_UA,
    url: SMID_PAGE_URL,
    tn: ""
  };
  body.tn = md5Hex(getTn(body));
  return body;
}
function encryptByDesRules(obj, rules = DES_RULE) {
  const out = {};
  for (const [key, value] of Object.entries(obj)) {
    const rule = rules[key];
    if (!rule) {
      out[key] = value;
      continue;
    }
    if (rule.is_encrypt === 1) {
      out[rule.obfuscated_name] = desEncryptBase64(String(value), rule.key ?? "");
    } else {
      out[rule.obfuscated_name] = value;
    }
  }
  return out;
}
function encryptSmBody(body, priId, rules = DES_RULE) {
  const gz = gzipWithOs19Base64(utf8(JSON.stringify(encryptByDesRules(body, rules))));
  return aesEncryptCompatHex(priId, gz);
}
function smidUploadRequest(fp, box, now, rnd = Math.random, base) {
  const uid = uuidV4();
  const priId = md5Hex(uid).slice(0, 16);
  const ep = rsaEncryptBase64(
    SKLAND_SM_CONFIG.publicKey,
    utf8(uid),
    (n) => {
      const b = new Uint8Array(n);
      for (let i = 0; i < n; i += 1) b[i] = Math.floor(rnd() * 256) & 255;
      return b;
    }
  );
  const data = encryptSmBody(buildSmBody(fp, box, now, rnd), priId);
  return {
    // ⚠ `base` 覆盖**必须**有：这台主机是第三方（cmpassport 风控），
    // 而"CI 全程只打 mock"是本仓的硬规则。少了这个口子，E2E 会真去打那个域 ——
    // 那不是"测试不严"，是**把 CI 变成对外发流量**。
    url: `${rootOrDefault(base, SMID_HOST)}${SMID_PATH}`,
    method: "POST",
    headers: {
      "Content-Type": "application/json",
      origin: SMID_ORIGIN,
      referer: SMID_REFERER,
      "user-agent": SMID_UA
    },
    body: JSON.stringify({
      appId: SKLAND_SM_CONFIG.appId,
      organization: SKLAND_SM_CONFIG.organization,
      ep,
      data,
      os: "web",
      encode: 5,
      compress: 2
    })
  };
}
function parseDeviceId(body) {
  let v;
  try {
    v = JSON.parse(body);
  } catch (e) {
    return { ok: false, error: `smid 响应不是 JSON：${e instanceof Error ? e.message : String(e)}` };
  }
  if (v.code !== SMID_OK_CODE) {
    return { ok: false, error: `smid 上报失败：code=${String(v.code ?? "?")}（期望 ${SMID_OK_CODE}）` };
  }
  const detail = v.detail;
  const raw = detail && typeof detail === "object" ? detail.deviceId : void 0;
  if (typeof raw !== "string" || raw === "") {
    return { ok: false, error: "smid 响应没有 detail.deviceId" };
  }
  return { ok: true, deviceId: DEVICE_ID_PREFIX + raw };
}

// src/mcloud/h5session.ts
var BASE_USER_YUN = "https://user-njs.yun.139.com";
var BASE_MCLOUD = "https://m.mcloud.139.com";
var H5_SALT_KEY = 2;
var H5_CONTENT_TYPE = "application/json;charset=UTF-8";
var H5_APP_VERSION2 = "12.6.0.0";
var SPEC_TOKEN_PATH = "/user/querySpecTokenV2";
var TIMESTAMP_PATH = "/ycloud/api/timestamp";
var TYRZ_LOGIN_PATH = "/ycloud/auth-service/auth/tyrzLogin";
var SIGN_IN_RECIPE = {
  toSourceId: "001005",
  marketName: "sign_in_3",
  sourceId: "1002",
  h5AppId: "cy62d5a5e8373a272aeca467accad61722c51cfd48",
  pagePath: "newsignin",
  portalPage: "mobilecloud",
  targetSourceId: "001005"
};
var MSG_PUSH_ON_RECIPE = {
  toSourceId: "001005",
  marketName: "msgpushon",
  sourceId: "1062",
  h5AppId: void 0,
  pagePath: "openNoticeGift",
  portalPage: "hcyview",
  targetSourceId: void 0
};
var FALLBACK_RECIPE = "sign-in";
function recipeParams(recipe) {
  return recipe === "msg-push-on" ? MSG_PUSH_ON_RECIPE : SIGN_IN_RECIPE;
}
function pageUrl(p, ssoToken) {
  let url = `${BASE_MCLOUD}/portal/${p.portalPage}/index.html?path=${p.pagePath}&sourceid=${p.sourceId}`;
  if (p.h5AppId !== void 0) url += `&enableShare=1&appID=${p.h5AppId}`;
  url += `&token=${ssoToken}`;
  if (p.targetSourceId !== void 0) url += `&targetSourceId=${p.targetSourceId}`;
  if (p.h5AppId === void 0) url += "&enableShare=1";
  return url;
}
function h5Headers(params, referer, jwt, showLoading) {
  const h = {
    Accept: "*/*",
    activityid: params.marketName,
    appversion: H5_APP_VERSION2,
    Referer: referer,
    showloading: showLoading
  };
  if (jwt !== void 0) h.jwttoken = jwt;
  return h;
}
function specTokenRequest(params, authorization, base) {
  return {
    url: `${rootOrDefault(base, BASE_USER_YUN)}${SPEC_TOKEN_PATH}`,
    method: "POST",
    headers: { "Content-Type": H5_CONTENT_TYPE, Authorization: authorization },
    body: JSON.stringify({ toSourceId: params.toSourceId })
  };
}
function timestampRequest(params, referer, base) {
  return {
    url: `${rootOrDefault(base, BASE_MCLOUD)}${TIMESTAMP_PATH}`,
    method: "GET",
    headers: h5Headers(params, referer, void 0, "true"),
    body: void 0
  };
}
function tyrzLoginRequest(params, ssoToken, sign, referer, base) {
  const body = JSON.stringify({
    marketName: params.marketName,
    openAccount: 0,
    sourceId: params.sourceId,
    token: ssoToken
  });
  return {
    url: `${rootOrDefault(base, BASE_MCLOUD)}${TYRZ_LOGIN_PATH}`,
    method: "POST",
    headers: {
      ...h5Headers(params, referer, void 0, "true"),
      Origin: BASE_MCLOUD,
      "x-request-id": sign["x-request-id"],
      "x-timestamp": sign["x-timestamp"],
      "x-nonce": sign["x-nonce"],
      "x-signature": sign["x-signature"]
    },
    body
  };
}
function makeSign(serverTimestamp) {
  return signHeadersWith(uuidV4(), nonce(), serverTimestamp, H5_SALT_KEY, "");
}
function parseSsoToken(body) {
  let v;
  try {
    v = JSON.parse(body);
  } catch (e) {
    return { ok: false, error: `querySpecTokenV2 响应不是 JSON：${e instanceof Error ? e.message : String(e)}` };
  }
  if (v.success !== true) {
    return {
      ok: false,
      error: `querySpecTokenV2 失败：code=${String(v.code ?? "?")} message=${String(v.message ?? "")}`
    };
  }
  const data = v.data;
  const token = data && typeof data === "object" ? data.token : void 0;
  if (typeof token !== "string" || token === "") {
    return { ok: false, error: "querySpecTokenV2 响应没有 data.token" };
  }
  return { ok: true, value: token };
}
function parseServerTimestamp(body) {
  let v;
  try {
    v = JSON.parse(body);
  } catch (e) {
    return { ok: false, error: `api/timestamp 响应不是 JSON：${e instanceof Error ? e.message : String(e)}` };
  }
  const r = v.result;
  if (typeof r === "number" && Number.isFinite(r)) return { ok: true, value: String(r) };
  if (typeof r === "string" && r !== "") return { ok: true, value: r };
  if (r === void 0) return { ok: false, error: "api/timestamp 响应没有 result" };
  return { ok: false, error: `api/timestamp 的 result 不是整数：${JSON.stringify(r)}` };
}
function parseJwtToken(body) {
  let v;
  try {
    v = JSON.parse(body);
  } catch (e) {
    return { ok: false, error: `tyrzLogin 响应不是 JSON：${e instanceof Error ? e.message : String(e)}` };
  }
  const code = v.code;
  let codeOk;
  if (typeof code === "number") codeOk = code === 0;
  else if (typeof code === "string") codeOk = code === "0" || code === "0000";
  else if (code === null || code === void 0) codeOk = v.success === true;
  else return { ok: false, error: `tyrzLogin 的 code 类型意外：${JSON.stringify(code)}` };
  if (!codeOk) {
    const msg = typeof v.msg === "string" ? v.msg : typeof v.message === "string" ? v.message : "(无消息)";
    return { ok: false, error: `tyrzLogin 失败：code=${JSON.stringify(code ?? null)} msg=${msg}` };
  }
  const result = v.result;
  if (!result || typeof result !== "object") return { ok: false, error: "tyrzLogin 响应没有 result" };
  const r = result;
  if (typeof r.token !== "string" || r.token === "") {
    return { ok: false, error: "tyrzLogin 响应没有 result.token" };
  }
  return {
    ok: true,
    // `mobile` 可能缺失 —— 它是"确认签的是哪个账号"的回显，不是凭据的一部分，
    // 缺了不该让整次登录失败
    value: { jwt: r.token, mobile: typeof r.mobile === "string" ? r.mobile : void 0 }
  };
}
function normalizeAuthorization(raw) {
  const r = hardenAuthorization(raw);
  if (!r.ok || r.value === void 0) return { ok: false, error: r.error ?? "认不出这串凭据" };
  return { ok: true, value: r.value };
}
function authFingerprint(sha256hex) {
  return sha256hex.slice(0, 16);
}

// src/mcloud/sessionExchange.ts
var STATE_AUTH = "mcloud.h5.auth";
function loadAuthRaw(store2) {
  const scopedId = scopedAccountOf(store2);
  if (scopedId !== void 0) {
    const one = loadAccounts(store2).find((a) => a.id === scopedId);
    if (one !== void 0) return one.auth;
  }
  const cur = currentAccount(store2);
  if (cur !== void 0) return cur.auth;
  return store2.getState(STATE_AUTH) ?? "";
}
var STATE_EXCHANGE = "mcloud.h5.exchange";
var STATE_JWT = "mcloud.h5.jwt";
var STATE_DEVICE_ID = "mcloud.smid.deviceId";
var JWT_TTL_MS = 100 * 60 * 1e3;
function parse(raw) {
  if (!raw) return null;
  try {
    return JSON.parse(raw);
  } catch {
    return null;
  }
}
function scopedKey(base, store2, recipe) {
  const acct = scopedAccountOf(store2) ?? currentAccount(store2)?.id;
  if (acct === void 0 && recipe === FALLBACK_RECIPE) return base;
  return `${base}.${acct ?? "default"}.${recipe}`;
}
function jwtUserDomainId(jwt) {
  const parts = jwt.split(".");
  if (parts.length < 2) return null;
  let payload;
  try {
    const b64 = parts[1].replace(/-/g, "+").replace(/_/g, "/");
    const padded = b64 + "=".repeat((4 - b64.length % 4) % 4);
    payload = JSON.parse(fromUtf8(fromBase64(padded)));
  } catch {
    return null;
  }
  const top = payload["userDomainId"];
  if (typeof top === "string" && top !== "") return top;
  const sub = payload["sub"];
  if (typeof sub === "string" && sub.trim().startsWith("{")) {
    try {
      const inner = JSON.parse(sub);
      const v = inner["userDomainId"];
      if (typeof v === "string" && v !== "") return v;
    } catch {
    }
  }
  return null;
}
function backfillDomainFromJwt(store2, jwt, nowIso) {
  const cur = currentAccount(store2);
  if (cur === void 0 || (cur.userDomainId ?? "") !== "") return null;
  const dom = jwtUserDomainId(jwt);
  if (dom === null) return null;
  setAccountDomain(store2, cur.id, dom, nowIso);
  return dom;
}
function jwtKey(store2, recipe = FALLBACK_RECIPE) {
  return scopedKey(STATE_JWT, store2, recipe);
}
function exchangeKey(store2, recipe = FALLBACK_RECIPE) {
  return scopedKey(STATE_EXCHANGE, store2, recipe);
}
function loadExchange(store2, recipe = FALLBACK_RECIPE) {
  return parse(store2.getState(exchangeKey(store2, recipe)));
}
function loadJwt(store2, recipe = FALLBACK_RECIPE) {
  return parse(store2.getState(jwtKey(store2, recipe)));
}
function jwtFresh(store2, nowMs, recipe = FALLBACK_RECIPE) {
  const r = loadJwt(store2, recipe);
  if (!r) return false;
  const at = Date.parse(r.at);
  return Number.isFinite(at) && nowMs - at < JWT_TTL_MS;
}
function startExchange(store2, raw, nowIso, recipe = FALLBACK_RECIPE) {
  const norm = normalizeAuthorization(raw);
  if (!norm.ok || norm.value === void 0) return { ok: false, error: norm.error };
  const fingerprint = authFingerprint(sha256Hex(norm.value));
  store2.setState(STATE_AUTH, norm.value, nowIso);
  store2.setState(exchangeKey(store2, recipe), JSON.stringify({
    step: "spec-token",
    recipe,
    fingerprint,
    startedAt: nowIso
  }), nowIso);
  return { ok: true, fingerprint };
}
function runExchangeStep(deps, recipe = FALLBACK_RECIPE) {
  const st = loadExchange(deps.store, recipe);
  const auth = loadAuthRaw(deps.store) || null;
  if (!st || st.step === "done" || !auth) {
    return { ok: true, note: "今天不用换票", done: true };
  }
  const rp = recipeParams(st.recipe ?? recipe);
  const finish = (s, ok, note) => {
    s.step = "done";
    s.finishedAt = deps.nowIso;
    s.ok = ok;
    s.note = note;
    delete s.ssoToken;
    delete s.ts;
    deps.store.setState(exchangeKey(deps.store, recipe), JSON.stringify(s), deps.nowIso);
    return { ok, note, done: true };
  };
  if (st.step === "smid") {
    const fp = ensureFingerprint(deps.store, deps.nowIso);
    const req = smidUploadRequest(
      fp,
      deps.store.getState(STATE_DEVICE_ID) ?? "",
      new Date(deps.nowIso),
      Math.random,
      deps.baseUrl
    );
    const res2 = callRawText(deps.transport, req);
    if (!res2.ok) return failStep(deps, st, `④ 换 deviceId 失败：${res2.error ?? "未知失败"}`);
    const got = parseDeviceId(res2.text);
    if (!got.ok || got.deviceId === void 0) return failStep(deps, st, `④ ${got.error}`);
    deps.store.setState(STATE_DEVICE_ID, got.deviceId, deps.nowIso);
    const cur = loadSession(deps.store);
    saveSession(
      deps.store,
      {
        cookie: cur?.cookie ?? "",
        jwt: cur?.jwt ?? "",
        deviceId: got.deviceId,
        userAgent: cur?.userAgent,
        savedAt: ""
      },
      deps.nowIso
    );
    return finish(st, true, `④ 换到 deviceId（${got.deviceId.length} 字符，B 开头）—— 领取类接口现在能用了`);
  }
  if (st.step === "spec-token") {
    const res2 = callRawText(deps.transport, specTokenRequest(rp, auth, deps.baseUrl));
    if (!res2.ok) return failStep(deps, st, `① 换 SSO token 失败：${res2.error ?? "未知失败"}`);
    const p2 = parseSsoToken(res2.text);
    if (!p2.ok || p2.value === void 0) return failStep(deps, st, `① ${p2.error}`);
    st.ssoToken = p2.value;
    st.referer = pageUrl(rp, p2.value);
    st.step = "timestamp";
    deps.store.setState(exchangeKey(deps.store, recipe), JSON.stringify(st), deps.nowIso);
    return { ok: true, note: "① 换到 SSO token（**不缓存**，下一步立刻用）", done: false };
  }
  if (st.step === "timestamp") {
    const res2 = callRawText(deps.transport, timestampRequest(rp, st.referer ?? "", deps.baseUrl));
    if (!res2.ok) return failStep(deps, st, `② 取时间戳失败：${res2.error ?? "未知失败"}`);
    const p2 = parseServerTimestamp(res2.text);
    if (!p2.ok || p2.value === void 0) return failStep(deps, st, `② ${p2.error}`);
    st.ssoToken = st.ssoToken ?? "";
    st.ts = p2.value;
    st.step = "tyrz-login";
    deps.store.setState(exchangeKey(deps.store, recipe), JSON.stringify(st), deps.nowIso);
    return { ok: true, note: `② 服务端时间戳 ${p2.value}（**必须用它**，本地时钟偏一点就是 401）`, done: false };
  }
  const ts = st.ts;
  if (ts === void 0 || !st.ssoToken) {
    return finish(st, false, "③ 前置状态丢了（SSO token 或时间戳）—— SSO token 不缓存，得从头再来");
  }
  const res = callRawText(
    deps.transport,
    tyrzLoginRequest(rp, st.ssoToken, makeSign(ts), st.referer ?? "", deps.baseUrl)
  );
  if (!res.ok) return failStep(deps, st, `③ 换 jwttoken 失败：${res.error ?? "未知失败"}`);
  const p = parseJwtToken(res.text);
  if (!p.ok || p.value === void 0) return failStep(deps, st, `③ ${p.error}`);
  const rec = {
    at: deps.nowIso,
    jwt: p.value.jwt,
    referer: st.referer ?? "",
    mobile: p.value.mobile,
    fingerprint: st.fingerprint
  };
  deps.store.setState(jwtKey(deps.store, recipe), JSON.stringify(rec), deps.nowIso);
  backfillDomainFromJwt(deps.store, p.value.jwt, deps.nowIso);
  const sess = loadSession(deps.store);
  const needDevice = (sess?.deviceId ?? "") === "";
  if (needDevice) {
    saveSession(
      deps.store,
      {
        cookie: sess?.cookie ?? "",
        jwt: p.value.jwt,
        deviceId: sess?.deviceId,
        userAgent: sess?.userAgent,
        savedAt: ""
      },
      deps.nowIso
    );
    st.step = "smid";
    deps.store.setState(exchangeKey(deps.store, recipe), JSON.stringify(st), deps.nowIso);
    return {
      ok: true,
      note: `③ 换到 jwttoken（${p.value.jwt.length} 字符）—— 再走一步换 \`deviceId\`（领取类接口要它）`,
      done: false
    };
  }
  saveSession(
    deps.store,
    {
      cookie: sess?.cookie ?? "",
      jwt: p.value.jwt,
      deviceId: sess?.deviceId,
      userAgent: sess?.userAgent,
      savedAt: ""
    },
    deps.nowIso
  );
  return finish(
    st,
    true,
    `③ 换到 jwttoken（${p.value.jwt.length} 字符${p.value.mobile ? `，回显 ${maskMobile(p.value.mobile)}` : ""}）—— 约 ${Math.round(JWT_TTL_MS / 6e4)} 分钟后该续` + // ⚠ 主动说出"缺 deviceId"：不说的话，用户第一次用 `receive` 会看到
    // 「活动太火爆啦」，然后以为过会儿就好 —— 而它其实永远不会好（是参数问题）。
    ((sess?.deviceId ?? "") === "" && (loadSession(deps.store)?.deviceId ?? "") === "" ? "。⚠ 这份会话**没有 `deviceId`**：领取类接口（receive）会回 615，而它的文案是「活动太火爆啦」—— 那不是在限流，是缺参数。要修：在会话 JSON 里带上 `deviceId`（H5 那一份是 `B` + 88 字符 base64）" : "")
  );
}
function failStep(deps, st, note) {
  st.step = "done";
  st.finishedAt = deps.nowIso;
  st.ok = false;
  st.note = note;
  delete st.ssoToken;
  delete st.ts;
  deps.store.setState(exchangeKey(deps.store, st.recipe ?? FALLBACK_RECIPE), JSON.stringify(st), deps.nowIso);
  return { ok: false, note, done: true };
}
var RETRY_AFTER_MS = 30 * 60 * 1e3;
function maskMobile(m) {
  return m.length >= 7 ? `${m.slice(0, 3)}****${m.slice(-4)}` : "***";
}

// src/mcloud/authRefresh.ts
var REFRESH_BASE = "https://user-njs.yun.139.com";
var REFRESH_PATH = "/user/auth/refreshToken";
var APP_CLIENT_TYPE = "412";
var PC_CLIENT_TYPE = "414";
function parseAuthorization(raw) {
  const b64 = raw.replace(/^Basic\s+/i, "").trim();
  if (!b64) return null;
  let plain;
  try {
    plain = fromUtf8(fromBase64(b64));
  } catch {
    return null;
  }
  const i = plain.indexOf(":");
  if (i < 0) return null;
  const j = plain.indexOf(":", i + 1);
  if (j < 0) {
    return { platform: plain.slice(0, i), phone: "", token: plain.slice(i + 1) };
  }
  return { platform: plain.slice(0, i), phone: plain.slice(i + 1, j), token: plain.slice(j + 1) };
}
function rebuildAuthorization(platform, phone, token) {
  return `Basic ${toBase64(utf8(`${platform}:${phone}:${token}`))}`;
}
function clientTypeFor(platform) {
  return platform === "pc" ? PC_CLIENT_TYPE : APP_CLIENT_TYPE;
}
function escapeJsonString(s) {
  let out = "";
  for (const ch of s) {
    const c = ch.codePointAt(0);
    if (ch === '"') out += '\\"';
    else if (ch === "\\") out += "\\\\";
    else if (ch === "\n") out += "\\n";
    else if (ch === "\r") out += "\\r";
    else if (ch === "	") out += "\\t";
    else if (c < 32) out += `\\u${c.toString(16).padStart(4, "0")}`;
    else out += ch;
  }
  return out;
}
function refreshRequest(raw, userDomainId, platform) {
  const domain = userDomainId === "" ? "" : `"userDomainId":"${escapeJsonString(userDomainId)}",`;
  const body = `{${domain}"clientType":"${escapeJsonString(clientTypeFor(platform))}"}`;
  return {
    url: `${REFRESH_BASE}${REFRESH_PATH}`,
    method: "POST",
    headers: {
      Authorization: raw
      // content-type 由传输层补（`application/json;charset=UTF-8`）—— 见 client.ts
    },
    body
  };
}
function parseRefresh(body) {
  let v;
  try {
    v = JSON.parse(body);
  } catch (e) {
    return { ok: false, error: `回来的不是 JSON：${e instanceof Error ? e.message : String(e)}` };
  }
  const code = v.code;
  const codeOk = typeof code === "string" && (code === "0000" || code === "0") || typeof code === "number" && code === 0;
  const success2 = typeof v.success === "boolean" ? v.success : codeOk;
  if (!success2 || !codeOk) {
    const msg = String(v.message ?? v.msg ?? "");
    const c = String(code ?? "");
    if (c === "05050009") return { ok: false, error: "这个号已经失效了 —— 重新登录一次" };
    return { ok: false, error: msg === "" ? "服务端拒绝了这一枪" : msg };
  }
  const data = v.data;
  if (!data || typeof data !== "object") return { ok: false, error: "没拿到新的登录信息" };
  const token = data.token;
  if (typeof token !== "string" || token === "") {
    return { ok: false, error: "refreshToken 响应没有 data.token" };
  }
  const exp = data.expireTime;
  const expireSeconds = typeof exp === "number" ? Math.trunc(exp) : typeof exp === "string" && /^\d+$/.test(exp) ? Number(exp) : void 0;
  return { ok: true, result: { token, expireSeconds } };
}
function applyRefreshResult(raw, res) {
  const seg = parseAuthorization(raw);
  if (!seg) return { ok: false, error: "存的那串登录信息读不出来 —— 重新登录一次" };
  if (seg.phone === "") return { ok: false, error: "那串登录信息里没有手机号 —— 重新登录一次" };
  return { ok: true, raw: rebuildAuthorization(seg.platform, seg.phone, res.token) };
}
var DEFAULT_LEAD_DAYS = 10;
function tokenExpireMs(token) {
  const f = token.split("|")[3]?.trim() ?? "";
  if (!/^[+-]?\d*\.?\d+(e[+-]?\d+)?$/i.test(f)) return 0;
  const v = Number(f);
  return Number.isFinite(v) && v !== 0 ? Math.trunc(v) : 0;
}
function credentialExpireMs(raw) {
  const seg = parseAuthorization(raw);
  if (!seg) return -1;
  return tokenExpireMs(seg.token);
}
function decide(expireMs, nowMs, leadDays = DEFAULT_LEAD_DAYS) {
  if (expireMs < 0) return "manual-only";
  if (expireMs === 0) return "needed";
  const leadMs = Math.trunc(leadDays * 864e5);
  return expireMs - nowMs <= leadMs ? "needed" : "not-yet";
}
function daysLeft(expireMs, nowMs) {
  if (expireMs <= 0) return void 0;
  return Math.floor((expireMs - nowMs) / 864e5);
}
function assertUsable(raw) {
  const seg = parseAuthorization(raw);
  if (!seg) return { ok: false, error: "服务端回来的东西读不出来" };
  if (seg.platform === "" || seg.phone === "" || seg.token === "") {
    return { ok: false, error: "服务端回来的东西是空的，没存" };
  }
  return { ok: true };
}
var STATE_LAST = "mcloud.auth.refresh.last";
function runAuthRefresh(deps, force = false, leadDays = DEFAULT_LEAD_DAYS, target = {}) {
  const raw = target.auth ?? loadAuthRaw(deps.store);
  if (raw === "") {
    return { ok: true, skipped: true, note: "还没有账号 —— 先加一个" };
  }
  const nowMs = Date.parse(deps.nowIso);
  const expireMs = credentialExpireMs(raw);
  const decision = force ? "needed" : decide(expireMs, nowMs, leadDays);
  if (decision === "manual-only") {
    return {
      ok: true,
      skipped: true,
      note: "这条存的东西读不出来 —— 要重新登录一次"
    };
  }
  if (decision === "not-yet") {
    const d2 = daysLeft(expireMs, nowMs);
    return {
      ok: true,
      skipped: true,
      note: `还有 ${d2 ?? "?"} 天，不用刷`
    };
  }
  const jwt = loadJwt(deps.store);
  const targetDomain = target.accountId === void 0 ? null : loadAccounts(deps.store).find((a) => a.id === target.accountId)?.userDomainId ?? null;
  const domain = targetDomain ?? currentAccountDomain(deps.store) ?? (jwt ? jwtUserDomainId(jwt.jwt) : null) ?? "";
  const seg = parseAuthorization(raw);
  const req = refreshRequest(raw, domain, seg.platform);
  const res = callRawText(deps.transport, req);
  if (!res.ok) return { ok: false, note: `没刷成：${res.error ?? "连不上"}` };
  const parsed = parseRefresh(res.text);
  if (!parsed.ok || !parsed.result) return { ok: false, note: `没刷成：${parsed.error ?? "看不懂回来的东西"}` };
  const applied = applyRefreshResult(raw, parsed.result);
  if (!applied.ok) return { ok: false, note: `没刷成：${applied.error}` };
  const usable2 = assertUsable(applied.raw);
  if (!usable2.ok) return { ok: false, note: `没存下来：${usable2.error}` };
  const who = target.accountId !== void 0 ? loadAccounts(deps.store).find((a) => a.id === target.accountId) : currentAccount(deps.store);
  if (who !== void 0) setAccountAuth(deps.store, who.id, applied.raw, deps.nowIso);
  else deps.store.setState(STATE_AUTH, applied.raw, deps.nowIso);
  deps.store.setState(STATE_LAST, JSON.stringify({
    at: deps.nowIso,
    expireSeconds: parsed.result.expireSeconds ?? null,
    oldFingerprint: seg.token.slice(-6),
    newFingerprint: parsed.result.token.slice(-6)
  }), deps.nowIso);
  startExchange(deps.store, applied.raw, deps.nowIso);
  const newExpire = tokenExpireMs(parsed.result.token);
  const d = daysLeft(newExpire, nowMs);
  return {
    ok: true,
    raw: applied.raw,
    note: d === void 0 ? "已续期" : `已续期，还能用 ${d} 天`
  };
}

// src/mcloud/exchangeTiming.ts
var WINDOW_SECOND = 40;
var WINDOW_TAIL_MS = 15e3;
var FIRE_LEAD_MS = 10;
var WINDOW_LEAD_MS = 6e4 - WINDOW_SECOND * 1e3;
var HOUR_MS = 36e5;
function mod(a, n) {
  return (a % n + n) % n;
}
function hourStartMs(nowMs) {
  return nowMs - mod(nowMs, HOUR_MS);
}
function hourKey(nowMs) {
  return String(hourStartMs(nowMs));
}
var NO_WAIT = { needed: false, untilMs: 0, ms: 0 };
function planWindowWait(nowMs) {
  const into = mod(nowMs, HOUR_MS);
  const startMs = (59 * 60 + WINDOW_SECOND) * 1e3;
  if (into >= startMs || into < WINDOW_TAIL_MS) return NO_WAIT;
  const untilMs2 = hourStartMs(nowMs) + startMs;
  return { needed: true, untilMs: untilMs2, ms: untilMs2 - nowMs };
}
function planFireWait(nowMs, leadMs = FIRE_LEAD_MS) {
  const into = mod(nowMs, HOUR_MS);
  if (into < WINDOW_LEAD_MS) return NO_WAIT;
  const untilMs2 = hourStartMs(nowMs) + HOUR_MS - leadMs;
  if (untilMs2 <= nowMs) return NO_WAIT;
  return { needed: true, untilMs: untilMs2, ms: untilMs2 - nowMs };
}
var DEFAULT_TARGET_HOUR = 10;
var STATE_TARGET_HOUR = "mcloud.exchange.target_hour";
function targetHourOf(store2) {
  const raw = store2.getState(STATE_TARGET_HOUR);
  if (raw === void 0 || raw === null) return DEFAULT_TARGET_HOUR;
  const t = raw.trim();
  if (t === "") return null;
  const n = Number(t);
  return Number.isInteger(n) && n >= 0 && n <= 23 ? n : DEFAULT_TARGET_HOUR;
}
function planTargetWait(nowMs, targetHour, tailMs = WINDOW_TAIL_MS) {
  if (targetHour === null) return planWindowWait(nowMs);
  const d = new Date(nowMs);
  const localHour = d.getHours();
  const minutesIntoHour = d.getMinutes() * 6e4 + d.getSeconds() * 1e3 + d.getMilliseconds();
  if (localHour === targetHour && minutesIntoHour < tailMs) return NO_WAIT;
  if (localHour === mod(targetHour - 1, 24) && minutesIntoHour >= (59 * 60 + WINDOW_SECOND) * 1e3) {
    return NO_WAIT;
  }
  const openAt = (base) => {
    const t = new Date(base.getTime());
    t.setHours(mod(targetHour - 1, 24), 59, WINDOW_SECOND, 0);
    return t.getTime();
  };
  let target = openAt(d);
  if (target <= nowMs) {
    const t = new Date(nowMs + 24 * 36e5);
    target = openAt(t);
  }
  return { needed: true, untilMs: target, ms: target - nowMs };
}

// src/mcloud/runLog.ts
var RUN_LOG_KEY = "mcloud.run.log";
var RUNS_PER_TASK = 20;
var MAX_STEPS = 40;
var MAX_NOTE = 300;
function clip(s, max2 = MAX_NOTE) {
  const t = typeof s === "string" ? s : String(s ?? "");
  return t.length <= max2 ? t : `…${t.slice(t.length - max2 + 1)}`;
}
function cleanStep(s) {
  return {
    name: clip(s.name, 60),
    ok: s.ok === true,
    note: clip(s.note),
    idle: s.idle === true,
    ...s.pending === true ? { pending: true } : {},
    // ⚠ 这个标记要跟着走：界面靠它把"内部管道"（换票之类）成功时藏起来。
    //   漏掉的话，界面上会冒出「换票（签到族）：没有待换票的任务」这种话。
    ...s.plumb === true ? { plumb: true } : {}
  };
}
function readRuns(store2) {
  const raw = store2.getState(RUN_LOG_KEY);
  if (raw === void 0 || raw === null || raw === "") return [];
  try {
    const v = JSON.parse(raw);
    if (!Array.isArray(v)) return [];
    return v.filter(
      (r) => typeof r === "object" && r !== null && typeof r.id === "string"
    );
  } catch {
    return [];
  }
}
function appendRun(store2, rec) {
  const steps = rec.steps.slice(0, MAX_STEPS).map(cleanStep);
  const tail = rec.acct === void 0 || rec.acct === "" ? "" : `-${rec.acct}`;
  const id = `${rec.task}-${Date.parse(rec.at) || 0}${tail}`;
  const all = readRuns(store2);
  const next = {
    id,
    task: rec.task,
    ...rec.acct === void 0 ? {} : { acct: rec.acct },
    ...rec.who === void 0 ? {} : { who: rec.who },
    at: rec.at,
    ms: Math.max(0, Math.round(rec.ms)),
    ok: rec.ok === true,
    note: clip(rec.note),
    steps,
    truncated: rec.steps.length > MAX_STEPS
  };
  const kept = all.filter((r) => r.id !== id);
  kept.push(next);
  const byTask = /* @__PURE__ */ new Map();
  for (const r of kept) {
    const list = byTask.get(r.task) ?? [];
    list.push(r);
    byTask.set(r.task, list);
  }
  const trimmed = [];
  for (const list of byTask.values()) {
    list.sort((a, b) => a.at < b.at ? 1 : a.at > b.at ? -1 : 0);
    trimmed.push(...list.slice(0, RUNS_PER_TASK));
  }
  trimmed.sort((a, b) => a.at < b.at ? 1 : a.at > b.at ? -1 : 0);
  store2.setState(RUN_LOG_KEY, JSON.stringify(trimmed), rec.at);
  return id;
}

// src/mcloud/session.ts
function jwtExpiryOf(raw, nowIso) {
  const parts = raw.split(".");
  if (parts.length < 2) return null;
  let payload;
  try {
    payload = fromUtf8(fromBase64(parts[1].replace(/-/g, "+").replace(/_/g, "/")));
  } catch {
    return null;
  }
  let exp;
  try {
    exp = JSON.parse(payload).exp;
  } catch {
    return null;
  }
  if (typeof exp !== "number" || !Number.isFinite(exp)) return null;
  const atMs = exp * 1e3;
  const now = Date.parse(nowIso);
  return {
    atIso: new Date(atMs).toISOString(),
    // `now` 坏了就当没过期（宁可不吓人）
    expired: Number.isNaN(now) ? false : now >= atMs
  };
}
function jwtExpiry(store2, nowIso) {
  return jwtExpiryOf(loadSession(store2)?.jwt ?? "", nowIso);
}
var STATE_BASE_URL = "mcloud.base_url";
function loadBaseUrl(store2) {
  const v = store2.getState(STATE_BASE_URL);
  if (!v) return "";
  const t = v.trim();
  if (!/^https?:\/\//.test(t)) return "";
  return t.replace(/\/+$/, "");
}

// src/mcloud/tasks/base.ts
function hostBase(deps, fallback) {
  const b = (deps.baseUrl ?? "").trim();
  return b === "" ? fallback : b.replace(/\/+$/, "");
}
var DEFAULT_GAP_MS = [500, 2e3];
function gapDelayMs(deps) {
  const [lo, hi] = deps.gapMs ?? DEFAULT_GAP_MS;
  if (!(hi > 0) || hi < lo) return 0;
  return lo + Math.floor(Math.random() * (hi - lo + 1));
}
function waitGap(deps) {
  const ms = gapDelayMs(deps);
  if (ms > 0) waitMs(deps, ms);
  return ms;
}
function depsNowMs(deps) {
  return (deps.nowMs ?? Date.now)();
}
function waitMs(deps, ms) {
  if (!(ms > 0)) return;
  const start2 = depsNowMs(deps);
  const end = start2 + Math.min(ms, 1e4);
  let frozen = 0;
  while (depsNowMs(deps) < end) {
    if (deps.spin) deps.spin();
    else {
      const t0 = Date.now();
      while (Date.now() - t0 < 2) {
      }
    }
    if (depsNowMs(deps) === start2) {
      frozen += 1;
      if (frozen > 5) return;
    } else {
      frozen = 0;
    }
  }
}
var ERR_PREFIX = "mcloud.last_error.";
function recordError(store2, task, note, nowIso) {
  store2.setState(`${ERR_PREFIX}${task}`, `${note} @ ${nowIso}`, nowIso);
}
function clearError(store2, task, nowIso) {
  store2.setState(`${ERR_PREFIX}${task}`, "", nowIso);
}
function sessionHeaders(s) {
  return { jwt: s.jwt, deviceId: s.deviceId, userAgent: s.userAgent, cookie: s.cookie };
}
function runTask(deps, spec, judge) {
  const late = deps.lateMs ?? 0;
  if (spec.skipIfLaterThanMs !== void 0 && late > spec.skipIfLaterThanMs) {
    const mins = Math.round(late / 6e4);
    const note2 = `错过太久（晚了 ${mins} 分钟，阈值 ${Math.round(spec.skipIfLaterThanMs / 6e4)} 分钟）→ 主动跳过`;
    recordError(deps.store, spec.task, note2, deps.nowIso);
    return { ok: true, skipped: true, note: note2 };
  }
  const session = loadSession(deps.store);
  if (!session) {
    const note2 = "还没有网页会话 —— 上面「换票」那一步说了原因";
    recordError(deps.store, spec.task, note2, deps.nowIso);
    return { ok: false, note: note2 };
  }
  const method = spec.method ?? EP_METHOD[spec.endpoint] ?? "GET";
  const url = resolveUrl(spec.endpoint, deps.baseUrl) + (spec.query ? `?${spec.query}` : "");
  const req = buildH5Request({
    url,
    method,
    body: spec.body,
    contentType: spec.contentType,
    referer: REFERER_SIGN_IN,
    showLoading: SHOW_LOADING,
    isDeviceId: spec.isDeviceId ?? method === "POST",
    saltKey: 1,
    marketName: spec.marketName,
    session: sessionHeaders(session)
  });
  const res = callOnce(deps.transport, req);
  if (res.setCookies.length > 0) {
    saveSession(
      deps.store,
      { ...session, cookie: mergeCookieJar(session.cookie, res.setCookies) },
      deps.nowIso
    );
  }
  if (!res.ok) {
    const explained = spec.explainFail?.(res.env, res.error ?? "");
    if (explained !== void 0) {
      recordError(deps.store, spec.task, explained, deps.nowIso);
      return { ok: false, note: explained };
    }
    var note = res.error ?? "未知失败";
    if (res.env?.code === 90001) {
      const exp = jwtExpiry(deps.store, deps.nowIso);
      const clock = (iso) => {
        const d = new Date(iso);
        if (Number.isNaN(d.getTime())) return "刚才";
        return `${String(d.getHours()).padStart(2, "0")}:${String(d.getMinutes()).padStart(2, "0")}`;
      };
      note = exp !== null && exp.expired ? `凭据过期了（今天 ${clock(exp.atIso)} 就到期）—— 去「账号」页重新导入会话` : "这一环要的票没被认（不是账号被踢）—— 重新导入会话最快";
    }
    if (spec.okWhenError?.(note)) {
      clearError(deps.store, spec.task, deps.nowIso);
      const plain = note.replace(
        /^code=(-?\d+)(?: msg=(.*))?$/,
        (_m, _c, msg) => msg === void 0 || msg.trim() === "" ? "" : msg.trim()
      );
      return { ok: true, note: plain === "" ? "已经做过了" : `已经做过了（服务端说：${plain}）` };
    }
    recordError(deps.store, spec.task, note, deps.nowIso);
    return { ok: false, note };
  }
  const verdict = judge(res.env, res);
  if (verdict.snapshot !== void 0 && spec.stateKey) {
    deps.store.setState(spec.stateKey, JSON.stringify({ at: deps.nowIso, lateMs: late, value: verdict.snapshot }), deps.nowIso);
  }
  if (verdict.ok) clearError(deps.store, spec.task, deps.nowIso);
  else recordError(deps.store, spec.task, verdict.note, deps.nowIso);
  return { ok: verdict.ok, note: verdict.note };
}

// node_modules/fflate/esm/browser.js
var u8 = Uint8Array;
var u16 = Uint16Array;
var i32 = Int32Array;
var fleb = new u8([
  0,
  0,
  0,
  0,
  0,
  0,
  0,
  0,
  1,
  1,
  1,
  1,
  2,
  2,
  2,
  2,
  3,
  3,
  3,
  3,
  4,
  4,
  4,
  4,
  5,
  5,
  5,
  5,
  0,
  /* unused */
  0,
  0,
  /* impossible */
  0
]);
var fdeb = new u8([
  0,
  0,
  0,
  0,
  1,
  1,
  2,
  2,
  3,
  3,
  4,
  4,
  5,
  5,
  6,
  6,
  7,
  7,
  8,
  8,
  9,
  9,
  10,
  10,
  11,
  11,
  12,
  12,
  13,
  13,
  /* unused */
  0,
  0
]);
var clim = new u8([16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15]);
var freb = function(eb, start2) {
  var b = new u16(31);
  for (var i = 0; i < 31; ++i) {
    b[i] = start2 += 1 << eb[i - 1];
  }
  var r = new i32(b[30]);
  for (var i = 1; i < 30; ++i) {
    for (var j = b[i]; j < b[i + 1]; ++j) {
      r[j] = j - b[i] << 5 | i;
    }
  }
  return { b, r };
};
var _a = freb(fleb, 2);
var fl = _a.b;
var revfl = _a.r;
fl[28] = 258, revfl[258] = 28;
var _b = freb(fdeb, 0);
var fd = _b.b;
var revfd = _b.r;
var rev = new u16(32768);
for (i = 0; i < 32768; ++i) {
  x = (i & 43690) >> 1 | (i & 21845) << 1;
  x = (x & 52428) >> 2 | (x & 13107) << 2;
  x = (x & 61680) >> 4 | (x & 3855) << 4;
  rev[i] = ((x & 65280) >> 8 | (x & 255) << 8) >> 1;
}
var x;
var i;
var hMap = (function(cd, mb, r) {
  var s = cd.length;
  var i = 0;
  var l = new u16(mb);
  for (; i < s; ++i) {
    if (cd[i])
      ++l[cd[i] - 1];
  }
  var le = new u16(mb);
  for (i = 1; i < mb; ++i) {
    le[i] = le[i - 1] + l[i - 1] << 1;
  }
  var co;
  if (r) {
    co = new u16(1 << mb);
    var rvb = 15 - mb;
    for (i = 0; i < s; ++i) {
      if (cd[i]) {
        var sv = i << 4 | cd[i];
        var r_1 = mb - cd[i];
        var v = le[cd[i] - 1]++ << r_1;
        for (var m = v | (1 << r_1) - 1; v <= m; ++v) {
          co[rev[v] >> rvb] = sv;
        }
      }
    }
  } else {
    co = new u16(s);
    for (i = 0; i < s; ++i) {
      if (cd[i]) {
        co[i] = rev[le[cd[i] - 1]++] >> 15 - cd[i];
      }
    }
  }
  return co;
});
var flt = new u8(288);
for (i = 0; i < 144; ++i)
  flt[i] = 8;
var i;
for (i = 144; i < 256; ++i)
  flt[i] = 9;
var i;
for (i = 256; i < 280; ++i)
  flt[i] = 7;
var i;
for (i = 280; i < 288; ++i)
  flt[i] = 8;
var i;
var fdt = new u8(32);
for (i = 0; i < 32; ++i)
  fdt[i] = 5;
var i;
var flrm = /* @__PURE__ */ hMap(flt, 9, 1);
var fdrm = /* @__PURE__ */ hMap(fdt, 5, 1);
var max = function(a) {
  var m = a[0];
  for (var i = 1; i < a.length; ++i) {
    if (a[i] > m)
      m = a[i];
  }
  return m;
};
var bits = function(d, p, m) {
  var o = p / 8 | 0;
  return (d[o] | d[o + 1] << 8) >> (p & 7) & m;
};
var bits16 = function(d, p) {
  var o = p / 8 | 0;
  return (d[o] | d[o + 1] << 8 | d[o + 2] << 16) >> (p & 7);
};
var shft = function(p) {
  return (p + 7) / 8 | 0;
};
var slc = function(v, s, e) {
  if (s == null || s < 0)
    s = 0;
  if (e == null || e > v.length)
    e = v.length;
  return new u8(v.subarray(s, e));
};
var ec = [
  "unexpected EOF",
  "invalid block type",
  "invalid length/literal",
  "invalid distance",
  "stream finished",
  "no stream handler",
  ,
  // determined by compression function
  "no callback",
  "invalid UTF-8 data",
  "extra field too long",
  "date not in range 1980-2099",
  "filename too long",
  "stream finishing",
  "invalid zip data"
  // determined by unknown compression method
];
var err = function(ind, msg, nt) {
  var e = new Error(msg || ec[ind]);
  e.code = ind;
  if (Error.captureStackTrace)
    Error.captureStackTrace(e, err);
  if (!nt)
    throw e;
  return e;
};
var inflt = function(dat, st, buf, dict) {
  var sl = dat.length, dl = dict ? dict.length : 0;
  if (!sl || st.f && !st.l)
    return buf || new u8(0);
  var noBuf = !buf;
  var resize = noBuf || st.i != 2;
  var noSt = st.i;
  if (noBuf)
    buf = new u8(sl * 3);
  var cbuf = function(l2) {
    var bl = buf.length;
    if (l2 > bl) {
      var nbuf = new u8(Math.max(bl * 2, l2));
      nbuf.set(buf);
      buf = nbuf;
    }
  };
  var final = st.f || 0, pos = st.p || 0, bt = st.b || 0, lm = st.l, dm = st.d, lbt = st.m, dbt = st.n;
  var tbts = sl * 8;
  do {
    if (!lm) {
      final = bits(dat, pos, 1);
      var type = bits(dat, pos + 1, 3);
      pos += 3;
      if (!type) {
        var s = shft(pos) + 4, l = dat[s - 4] | dat[s - 3] << 8, t = s + l;
        if (t > sl) {
          if (noSt)
            err(0);
          break;
        }
        if (resize)
          cbuf(bt + l);
        buf.set(dat.subarray(s, t), bt);
        st.b = bt += l, st.p = pos = t * 8, st.f = final;
        continue;
      } else if (type == 1)
        lm = flrm, dm = fdrm, lbt = 9, dbt = 5;
      else if (type == 2) {
        var hLit = bits(dat, pos, 31) + 257, hcLen = bits(dat, pos + 10, 15) + 4;
        var tl = hLit + bits(dat, pos + 5, 31) + 1;
        pos += 14;
        var ldt = new u8(tl);
        var clt = new u8(19);
        for (var i = 0; i < hcLen; ++i) {
          clt[clim[i]] = bits(dat, pos + i * 3, 7);
        }
        pos += hcLen * 3;
        var clb = max(clt), clbmsk = (1 << clb) - 1;
        var clm = hMap(clt, clb, 1);
        for (var i = 0; i < tl; ) {
          var r = clm[bits(dat, pos, clbmsk)];
          pos += r & 15;
          var s = r >> 4;
          if (s < 16) {
            ldt[i++] = s;
          } else {
            var c = 0, n = 0;
            if (s == 16)
              n = 3 + bits(dat, pos, 3), pos += 2, c = ldt[i - 1];
            else if (s == 17)
              n = 3 + bits(dat, pos, 7), pos += 3;
            else if (s == 18)
              n = 11 + bits(dat, pos, 127), pos += 7;
            while (n--)
              ldt[i++] = c;
          }
        }
        var lt = ldt.subarray(0, hLit), dt = ldt.subarray(hLit);
        lbt = max(lt);
        dbt = max(dt);
        lm = hMap(lt, lbt, 1);
        dm = hMap(dt, dbt, 1);
      } else
        err(1);
      if (pos > tbts) {
        if (noSt)
          err(0);
        break;
      }
    }
    if (resize)
      cbuf(bt + 131072);
    var lms = (1 << lbt) - 1, dms = (1 << dbt) - 1;
    var lpos = pos;
    for (; ; lpos = pos) {
      var c = lm[bits16(dat, pos) & lms], sym = c >> 4;
      pos += c & 15;
      if (pos > tbts) {
        if (noSt)
          err(0);
        break;
      }
      if (!c)
        err(2);
      if (sym < 256)
        buf[bt++] = sym;
      else if (sym == 256) {
        lpos = pos, lm = null;
        break;
      } else {
        var add = sym - 254;
        if (sym > 264) {
          var i = sym - 257, b = fleb[i];
          add = bits(dat, pos, (1 << b) - 1) + fl[i];
          pos += b;
        }
        var d = dm[bits16(dat, pos) & dms], dsym = d >> 4;
        if (!d)
          err(3);
        pos += d & 15;
        var dt = fd[dsym];
        if (dsym > 3) {
          var b = fdeb[dsym];
          dt += bits16(dat, pos) & (1 << b) - 1, pos += b;
        }
        if (pos > tbts) {
          if (noSt)
            err(0);
          break;
        }
        if (resize)
          cbuf(bt + 131072);
        var end = bt + add;
        if (bt < dt) {
          var shift = dl - dt, dend = Math.min(dt, end);
          if (shift + bt < 0)
            err(3);
          for (; bt < dend; ++bt)
            buf[bt] = dict[shift + bt];
        }
        for (; bt < end; ++bt)
          buf[bt] = buf[bt - dt];
      }
    }
    st.l = lm, st.p = lpos, st.b = bt, st.f = final;
    if (lm)
      final = 1, st.m = lbt, st.d = dm, st.n = dbt;
  } while (!final);
  return bt != buf.length && noBuf ? slc(buf, 0, bt) : buf.subarray(0, bt);
};
var et = /* @__PURE__ */ new u8(0);
var zls = function(d, dict) {
  if ((d[0] & 15) != 8 || d[0] >> 4 > 7 || (d[0] << 8 | d[1]) % 31)
    err(6, "invalid zlib data");
  if ((d[1] >> 5 & 1) == +!dict)
    err(6, "invalid zlib data: " + (d[1] & 32 ? "need" : "unexpected") + " dictionary");
  return (d[1] >> 3 & 4) + 2;
};
function unzlibSync(data, opts) {
  return inflt(data.subarray(zls(data, opts && opts.dictionary), -4), { i: 2 }, opts && opts.out, opts && opts.dictionary);
}
var td = typeof TextDecoder != "undefined" && /* @__PURE__ */ new TextDecoder();
var tds = 0;
try {
  td.decode(et, { stream: true });
  tds = 1;
} catch (e) {
}

// src/crypto/png.ts
var SIGNATURE = [137, 80, 78, 71, 13, 10, 26, 10];
function u32(b, o) {
  return (b[o] << 24 | b[o + 1] << 16 | b[o + 2] << 8 | b[o + 3]) >>> 0;
}
function channelsOf(colorType) {
  switch (colorType) {
    case 0:
      return 1;
    // 灰度
    case 2:
      return 3;
    // RGB
    case 3:
      return 1;
    // 调色板索引
    case 4:
      return 2;
    // 灰度 + alpha
    case 6:
      return 4;
    // RGBA
    default:
      throw new Error(`PNG 色彩类型不认识：${colorType}`);
  }
}
function unfilter(raw, w, h, bpp) {
  const stride = w * bpp;
  const out = new Uint8Array(stride * h);
  let pos = 0;
  for (let y = 0; y < h; y += 1) {
    const ft = raw[pos];
    pos += 1;
    const rowStart = y * stride;
    const prevStart = (y - 1) * stride;
    for (let x = 0; x < stride; x += 1) {
      const cur = raw[pos + x];
      const a = x >= bpp ? out[rowStart + x - bpp] : 0;
      const b = y > 0 ? out[prevStart + x] : 0;
      const c = y > 0 && x >= bpp ? out[prevStart + x - bpp] : 0;
      let v;
      switch (ft) {
        case 0:
          v = cur;
          break;
        case 1:
          v = cur + a;
          break;
        case 2:
          v = cur + b;
          break;
        case 3:
          v = cur + (a + b >> 1);
          break;
        case 4: {
          const p = a + b - c;
          const pa = Math.abs(p - a);
          const pb = Math.abs(p - b);
          const pc = Math.abs(p - c);
          v = cur + (pa <= pb && pa <= pc ? a : pb <= pc ? b : c);
          break;
        }
        default:
          throw new Error(`PNG 滤波类型不认识：${ft}（第 ${y} 行）`);
      }
      out[rowStart + x] = v & 255;
    }
    pos += stride;
  }
  return out;
}
function decodeImage(bytes) {
  for (let i = 0; i < 8; i += 1) {
    if (bytes[i] !== SIGNATURE[i]) throw new Error("不是 PNG（签名不对）");
  }
  let w = 0;
  let h = 0;
  let bitDepth = 0;
  let colorType = 0;
  let interlace = 0;
  let palette;
  let transparency;
  const idat = [];
  let p = 8;
  while (p + 8 <= bytes.length) {
    const len = u32(bytes, p);
    const type = String.fromCharCode(bytes[p + 4], bytes[p + 5], bytes[p + 6], bytes[p + 7]);
    const body = bytes.subarray(p + 8, p + 8 + len);
    if (type === "IHDR") {
      w = u32(body, 0);
      h = u32(body, 4);
      bitDepth = body[8];
      colorType = body[9];
      interlace = body[12];
    } else if (type === "PLTE") {
      palette = body.slice();
    } else if (type === "tRNS") {
      transparency = body.slice();
    } else if (type === "IDAT") {
      idat.push(body);
    } else if (type === "IEND") {
      break;
    }
    p += 12 + len;
  }
  if (w <= 0 || h <= 0) throw new Error(`PNG 尺寸不合法：${w}x${h}`);
  if (bitDepth !== 8) throw new Error(`PNG 位深只支持 8（这张是 ${bitDepth}）`);
  if (interlace !== 0) throw new Error("PNG 是隔行的，不支持（硬解会得到错位的图）");
  if (idat.length === 0) throw new Error("PNG 没有 IDAT");
  const ch = channelsOf(colorType);
  let total = 0;
  for (const d of idat) total += d.length;
  const merged = new Uint8Array(total);
  let off = 0;
  for (const d of idat) {
    merged.set(d, off);
    off += d.length;
  }
  const raw = unfilter(unzlibSync(merged), w, h, ch);
  const n = w * h;
  const rgba = new Uint8ClampedArray(n * 4);
  if (colorType === 0) {
    for (let i = 0; i < n; i += 1) {
      const v = raw[i];
      rgba[i * 4] = v;
      rgba[i * 4 + 1] = v;
      rgba[i * 4 + 2] = v;
      rgba[i * 4 + 3] = 255;
    }
  } else if (colorType === 2) {
    for (let i = 0; i < n; i += 1) {
      rgba[i * 4] = raw[i * 3];
      rgba[i * 4 + 1] = raw[i * 3 + 1];
      rgba[i * 4 + 2] = raw[i * 3 + 2];
      rgba[i * 4 + 3] = 255;
    }
  } else if (colorType === 3) {
    if (!palette) throw new Error("PNG 是调色板图但没有 PLTE");
    for (let i = 0; i < n; i += 1) {
      const idx = raw[i];
      rgba[i * 4] = palette[idx * 3] ?? 0;
      rgba[i * 4 + 1] = palette[idx * 3 + 1] ?? 0;
      rgba[i * 4 + 2] = palette[idx * 3 + 2] ?? 0;
      rgba[i * 4 + 3] = transparency?.[idx] ?? 255;
    }
  } else if (colorType === 4) {
    for (let i = 0; i < n; i += 1) {
      const v = raw[i * 2];
      rgba[i * 4] = v;
      rgba[i * 4 + 1] = v;
      rgba[i * 4 + 2] = v;
      rgba[i * 4 + 3] = raw[i * 2 + 1];
    }
  } else {
    rgba.set(raw.subarray(0, n * 4));
  }
  return { data: rgba, width: w, height: h };
}

// src/crypto/jpeg.ts
var ZIGZAG = new Uint8Array([
  0,
  1,
  8,
  16,
  9,
  2,
  3,
  10,
  17,
  24,
  32,
  25,
  18,
  11,
  4,
  5,
  12,
  19,
  26,
  33,
  40,
  48,
  41,
  34,
  27,
  20,
  13,
  6,
  7,
  14,
  21,
  28,
  35,
  42,
  49,
  56,
  57,
  50,
  43,
  36,
  29,
  22,
  15,
  23,
  30,
  37,
  44,
  51,
  58,
  59,
  52,
  45,
  38,
  31,
  39,
  46,
  53,
  60,
  61,
  54,
  47,
  55,
  62,
  63
]);
var FIX_0_298631336 = 2446;
var FIX_0_390180644 = 3196;
var FIX_0_541196100 = 4433;
var FIX_0_765366865 = 6270;
var FIX_0_899976223 = 7373;
var FIX_1_175875602 = 9633;
var FIX_1_501321110 = 12299;
var FIX_1_847759065 = 15137;
var FIX_1_961570560 = 16069;
var FIX_2_053119869 = 16819;
var FIX_2_562915447 = 20995;
var FIX_3_072711026 = 25172;
var CONST_BITS = 13;
var PASS1_BITS = 2;
function descale(x, n) {
  return x + (1 << n - 1) >> n;
}
function clampCoeff(v) {
  if (v > 32767) return 32767;
  if (v < -32767) return -32767;
  return v;
}
function idctBlock(block, ws, plane, stride, off) {
  for (let c = 0; c < 8; c += 1) {
    if (block[c + 8] === 0 && block[c + 16] === 0 && block[c + 24] === 0 && block[c + 32] === 0 && block[c + 40] === 0 && block[c + 48] === 0 && block[c + 56] === 0) {
      const dc = block[c] << PASS1_BITS;
      ws[c] = dc;
      ws[c + 8] = dc;
      ws[c + 16] = dc;
      ws[c + 24] = dc;
      ws[c + 32] = dc;
      ws[c + 40] = dc;
      ws[c + 48] = dc;
      ws[c + 56] = dc;
      continue;
    }
    const z2 = block[c + 16];
    const z3 = block[c + 48];
    const z1 = (z2 + z3) * FIX_0_541196100;
    const t2 = z1 - z3 * FIX_1_847759065;
    const t3 = z1 + z2 * FIX_0_765366865;
    const d0 = block[c];
    const d4 = block[c + 32];
    const t0 = d0 + d4 << CONST_BITS;
    const t1 = d0 - d4 << CONST_BITS;
    const t10 = t0 + t3;
    const t13 = t0 - t3;
    const t11 = t1 + t2;
    const t12 = t1 - t2;
    const p0 = block[c + 56];
    const p1 = block[c + 40];
    const p2 = block[c + 24];
    const p3 = block[c + 8];
    const w1 = p0 + p3;
    const w2 = p1 + p2;
    const w3 = p0 + p2;
    const w4 = p1 + p3;
    const w5 = (w3 + w4) * FIX_1_175875602;
    let q0 = p0 * FIX_0_298631336;
    let q1 = p1 * FIX_2_053119869;
    let q2 = p2 * FIX_3_072711026;
    let q3 = p3 * FIX_1_501321110;
    const s1 = w1 * -FIX_0_899976223;
    const s2 = w2 * -FIX_2_562915447;
    const s3 = w3 * -FIX_1_961570560 + w5;
    const s4 = w4 * -FIX_0_390180644 + w5;
    q0 += s1 + s3;
    q1 += s2 + s4;
    q2 += s2 + s3;
    q3 += s1 + s4;
    ws[c] = descale(t10 + q3, CONST_BITS - PASS1_BITS);
    ws[c + 56] = descale(t10 - q3, CONST_BITS - PASS1_BITS);
    ws[c + 8] = descale(t11 + q2, CONST_BITS - PASS1_BITS);
    ws[c + 48] = descale(t11 - q2, CONST_BITS - PASS1_BITS);
    ws[c + 16] = descale(t12 + q1, CONST_BITS - PASS1_BITS);
    ws[c + 40] = descale(t12 - q1, CONST_BITS - PASS1_BITS);
    ws[c + 24] = descale(t13 + q0, CONST_BITS - PASS1_BITS);
    ws[c + 32] = descale(t13 - q0, CONST_BITS - PASS1_BITS);
  }
  for (let r = 0; r < 8; r += 1) {
    const o = r * 8;
    const base = off + r * stride;
    if (ws[o + 1] === 0 && ws[o + 2] === 0 && ws[o + 3] === 0 && ws[o + 4] === 0 && ws[o + 5] === 0 && ws[o + 6] === 0 && ws[o + 7] === 0) {
      let v = descale(ws[o], PASS1_BITS + 3) + 128;
      if (v < 0) v = 0;
      else if (v > 255) v = 255;
      plane[base] = v;
      plane[base + 1] = v;
      plane[base + 2] = v;
      plane[base + 3] = v;
      plane[base + 4] = v;
      plane[base + 5] = v;
      plane[base + 6] = v;
      plane[base + 7] = v;
      continue;
    }
    const z2 = ws[o + 2];
    const z3 = ws[o + 6];
    const z1 = (z2 + z3) * FIX_0_541196100;
    const t2 = z1 - z3 * FIX_1_847759065;
    const t3 = z1 + z2 * FIX_0_765366865;
    const d0 = ws[o];
    const d4 = ws[o + 4];
    const t0 = d0 + d4 << CONST_BITS;
    const t1 = d0 - d4 << CONST_BITS;
    const t10 = t0 + t3;
    const t13 = t0 - t3;
    const t11 = t1 + t2;
    const t12 = t1 - t2;
    const p0 = ws[o + 7];
    const p1 = ws[o + 5];
    const p2 = ws[o + 3];
    const p3 = ws[o + 1];
    const w1 = p0 + p3;
    const w2 = p1 + p2;
    const w3 = p0 + p2;
    const w4 = p1 + p3;
    const w5 = (w3 + w4) * FIX_1_175875602;
    let q0 = p0 * FIX_0_298631336;
    let q1 = p1 * FIX_2_053119869;
    let q2 = p2 * FIX_3_072711026;
    let q3 = p3 * FIX_1_501321110;
    const s1 = w1 * -FIX_0_899976223;
    const s2 = w2 * -FIX_2_562915447;
    const s3 = w3 * -FIX_1_961570560 + w5;
    const s4 = w4 * -FIX_0_390180644 + w5;
    q0 += s1 + s3;
    q1 += s2 + s4;
    q2 += s2 + s3;
    q3 += s1 + s4;
    plane[base] = clampByte(descale(t10 + q3, CONST_BITS + PASS1_BITS + 3) + 128);
    plane[base + 7] = clampByte(descale(t10 - q3, CONST_BITS + PASS1_BITS + 3) + 128);
    plane[base + 1] = clampByte(descale(t11 + q2, CONST_BITS + PASS1_BITS + 3) + 128);
    plane[base + 6] = clampByte(descale(t11 - q2, CONST_BITS + PASS1_BITS + 3) + 128);
    plane[base + 2] = clampByte(descale(t12 + q1, CONST_BITS + PASS1_BITS + 3) + 128);
    plane[base + 5] = clampByte(descale(t12 - q1, CONST_BITS + PASS1_BITS + 3) + 128);
    plane[base + 3] = clampByte(descale(t13 + q0, CONST_BITS + PASS1_BITS + 3) + 128);
    plane[base + 4] = clampByte(descale(t13 - q0, CONST_BITS + PASS1_BITS + 3) + 128);
  }
}
function clampByte(v) {
  return v < 0 ? 0 : v > 255 ? 255 : v;
}
function buildHuffman(counts, values) {
  const lookup = new Uint16Array(512);
  const mincode = new Int32Array(17);
  const maxcode = new Int32Array(17).fill(-1);
  const valptr = new Int32Array(17);
  let code = 0;
  let k = 0;
  for (let len = 1; len <= 16; len += 1) {
    const n = counts[len];
    valptr[len] = k;
    mincode[len] = code;
    if (n > 0) {
      maxcode[len] = code + n - 1;
      if (len <= 9) {
        const shift = 9 - len;
        for (let i = 0; i < n; i += 1) {
          const start2 = code + i << shift;
          const packed = len << 8 | values[k + i];
          for (let j = 0; j < 1 << shift; j += 1) lookup[start2 + j] = packed;
        }
      }
    }
    k += n;
    code = code + n << 1;
  }
  if (k !== values.length) throw new Error("JPEG 的 Huffman 表长度不一致（DHT 损坏）");
  return { lookup, maxcode, mincode, valptr, values };
}
function buildAxis(comp, max2, srcLen, dstLen) {
  const i0 = new Int32Array(dstLen);
  const i1 = new Int32Array(dstLen);
  const frac = new Int32Array(dstLen);
  const last = srcLen - 1;
  if (comp !== max2 && max2 === comp * 2) {
    for (let d = 0; d < dstLen; d += 1) {
      let u = Math.floor((d * 2 + 1) * comp * 128 / max2) - 128;
      if (u < 0) u = 0;
      const maxU = last * 256;
      if (u > maxU) u = maxU;
      const a = u >> 8;
      i0[d] = a;
      i1[d] = a < last ? a + 1 : a;
      frac[d] = u & 255;
    }
    return { i0, i1, frac };
  }
  for (let d = 0; d < dstLen; d += 1) {
    const a = Math.min(Math.floor(d * comp / max2), last);
    i0[d] = a;
    i1[d] = a;
    frac[d] = 0;
  }
  return { i0, i1, frac };
}
function isJpeg(bytes) {
  return bytes.length >= 3 && bytes[0] === 255 && bytes[1] === 216 && bytes[2] === 255;
}
function decodeJpeg(bytes) {
  if (!isJpeg(bytes)) throw new Error("不是 JPEG（缺少 SOI 标记 FF D8）");
  const dcTables = [];
  const acTables = [];
  const qtables = [];
  let pos = 2;
  let width = 0;
  let height = 0;
  let hMax = 1;
  let vMax = 1;
  let restartInterval = 0;
  let components = [];
  const scannedComps = [];
  let sawSof = false;
  let sawScan = false;
  let dcPred = new Int32Array(0);
  let block = new Int32Array(64);
  let ws = new Int32Array(64);
  let entropyPos = 0;
  let bitBuf = 0;
  let bitCnt = 0;
  let marker = 0;
  let markerPos = -1;
  function fillOne() {
    if (entropyPos >= bytes.length) {
      marker = -1;
      markerPos = entropyPos;
      return;
    }
    const b = bytes[entropyPos];
    if (b !== 255) {
      bitBuf = (bitBuf << 8 | b) >>> 0;
      bitCnt += 8;
      entropyPos += 1;
      return;
    }
    const n = entropyPos + 1 < bytes.length ? bytes[entropyPos + 1] : -1;
    if (n === 0) {
      bitBuf = (bitBuf << 8 | 255) >>> 0;
      bitCnt += 8;
      entropyPos += 2;
      return;
    }
    marker = n;
    markerPos = entropyPos;
  }
  function ensure(n) {
    while (bitCnt < n && marker === 0) fillOne();
  }
  function readBits(n) {
    if (n === 0) return 0;
    ensure(n);
    if (bitCnt < n) throw new Error("JPEG 扫描数据提前结束（文件被截断？）");
    const v = bitBuf >>> bitCnt - n & (1 << n) - 1;
    bitCnt -= n;
    return v;
  }
  function decodeHuff(t) {
    ensure(9);
    if (bitCnt >= 9) {
      const entry = t.lookup[bitBuf >>> bitCnt - 9 & 511];
      if (entry !== 0) {
        bitCnt -= entry >>> 8;
        return entry & 255;
      }
    }
    let code = 0;
    for (let len = 1; len <= 16; len += 1) {
      code = code << 1 | readBits(1);
      const mx = t.maxcode[len];
      if (mx >= 0 && code <= mx) {
        return t.values[t.valptr[len] + code - t.mincode[len]];
      }
    }
    throw new Error("JPEG 的 Huffman 码不合法（扫描数据损坏）");
  }
  function receiveExtend(s) {
    const v = readBits(s);
    return v < 1 << s - 1 ? v - ((1 << s) - 1) : v;
  }
  function restartScan(mcuIndex) {
    bitBuf = 0;
    bitCnt = 0;
    let q = marker !== 0 && markerPos >= 0 ? markerPos : entropyPos;
    while (q + 1 < bytes.length && bytes[q] === 255 && bytes[q + 1] === 255) q += 1;
    if (q + 1 >= bytes.length || bytes[q] !== 255) {
      throw new Error(`JPEG 在第 ${mcuIndex} 个 MCU 后缺少重启标记 RSTn（DRI 与实际数据不一致）`);
    }
    const m = bytes[q + 1];
    if (m < 208 || m > 215) {
      throw new Error(`JPEG 在第 ${mcuIndex} 个 MCU 后缺少重启标记 RSTn（遇到 FF ${m.toString(16)}）`);
    }
    entropyPos = q + 2;
    marker = 0;
    markerPos = -1;
    dcPred.fill(0);
  }
  function resyncToMarker() {
    bitBuf = 0;
    bitCnt = 0;
    let q = marker !== 0 && markerPos >= 0 ? markerPos : entropyPos;
    for (; ; ) {
      while (q + 1 < bytes.length) {
        if (bytes[q] !== 255) {
          q += 1;
          continue;
        }
        const m2 = bytes[q + 1];
        if (m2 === 0) {
          q += 2;
          continue;
        }
        if (m2 === 255) {
          q += 1;
          continue;
        }
        break;
      }
      if (q + 1 >= bytes.length) throw new Error("JPEG 扫描段之后找不到下一个标记（文件被截断）");
      const m = bytes[q + 1];
      if (m >= 208 && m <= 215) {
        q += 2;
        continue;
      }
      marker = 0;
      markerPos = -1;
      entropyPos = q;
      return q;
    }
  }
  function decodeBlock(sc) {
    block.fill(0);
    const dcLen = decodeHuff(sc.dc);
    let diff = 0;
    if (dcLen > 0) diff = receiveExtend(dcLen);
    const pred = dcPred[sc.index] + diff | 0;
    dcPred[sc.index] = pred;
    block[0] = clampCoeff(pred * sc.qt[0]);
    let k = 1;
    while (k < 64) {
      const rs = decodeHuff(sc.ac);
      const run = rs >>> 4;
      const size = rs & 15;
      if (size === 0) {
        if (run !== 15) break;
        k += 16;
        continue;
      }
      k += run;
      if (k > 63) throw new Error("JPEG 扫描数据损坏（AC 系数游程越界）");
      const nat = ZIGZAG[k];
      block[nat] = clampCoeff(receiveExtend(size) * sc.qt[nat]);
      k += 1;
    }
  }
  function decodeScan(scanComponents, start2) {
    entropyPos = start2;
    bitBuf = 0;
    bitCnt = 0;
    marker = 0;
    markerPos = -1;
    dcPred.fill(0);
    if (scanComponents.length === 1) {
      const sc = scanComponents[0];
      const comp = sc.comp;
      const bw = Math.ceil(comp.width / 8);
      const bh = Math.ceil(comp.height / 8);
      const total = bw * bh;
      let mcu = 0;
      for (let by = 0; by < bh; by += 1) {
        for (let bx = 0; bx < bw; bx += 1) {
          decodeBlock(sc);
          idctBlock(block, ws, comp.plane, comp.planeW, by * 8 * comp.planeW + bx * 8);
          mcu += 1;
          if (restartInterval > 0 && mcu % restartInterval === 0 && mcu < total) restartScan(mcu);
        }
      }
    } else {
      const mcusX = Math.ceil(width / (8 * hMax));
      const mcusY = Math.ceil(height / (8 * vMax));
      const total = mcusX * mcusY;
      let mcu = 0;
      for (let my = 0; my < mcusY; my += 1) {
        for (let mx = 0; mx < mcusX; mx += 1) {
          for (const sc of scanComponents) {
            const comp = sc.comp;
            for (let vy = 0; vy < comp.v; vy += 1) {
              for (let hx = 0; hx < comp.h; hx += 1) {
                decodeBlock(sc);
                const px = (mx * comp.h + hx) * 8;
                const py = (my * comp.v + vy) * 8;
                idctBlock(block, ws, comp.plane, comp.planeW, py * comp.planeW + px);
              }
            }
          }
          mcu += 1;
          if (restartInterval > 0 && mcu % restartInterval === 0 && mcu < total) restartScan(mcu);
        }
      }
    }
    return resyncToMarker();
  }
  while (pos < bytes.length) {
    if (bytes[pos] !== 255) {
      pos += 1;
      continue;
    }
    const m = pos + 1 < bytes.length ? bytes[pos + 1] : -1;
    if (m === -1) break;
    if (m === 255) {
      pos += 1;
      continue;
    }
    pos += 2;
    if (m === 217) break;
    if (m === 216 || m === 1 || m >= 208 && m <= 215) continue;
    if (pos + 2 > bytes.length) throw new Error("JPEG 段头被截断");
    const segLen = bytes[pos] << 8 | bytes[pos + 1];
    if (segLen < 2 || pos + segLen > bytes.length) {
      throw new Error("JPEG 段长度越界（文件被截断或损坏）");
    }
    const seg = pos + 2;
    const end = pos + segLen;
    switch (m) {
      case 192:
      case 193: {
        if (sawSof) throw new Error("JPEG 出现了多个 SOF（不支持分层/多帧）");
        if (end - seg < 6) throw new Error("JPEG 的 SOF 段太短");
        const precision = bytes[seg];
        if (precision !== 8) {
          if (precision === 12) throw new Error("JPEG 是 12-bit 的，不支持：本解码器只解 8-bit");
          throw new Error(`JPEG 精度是 ${precision}-bit，不支持：本解码器只解 8-bit`);
        }
        height = bytes[seg + 1] << 8 | bytes[seg + 2];
        width = bytes[seg + 3] << 8 | bytes[seg + 4];
        const n = bytes[seg + 5];
        if (width <= 0 || height <= 0) throw new Error(`JPEG 尺寸不合法：${width}x${height}`);
        if (n === 4) {
          throw new Error("JPEG 有 4 个分量（CMYK/YCCK），不支持：只支持灰度或 YCbCr");
        }
        if (n !== 1 && n !== 3) throw new Error(`JPEG 有 ${n} 个分量，不支持：只支持 1（灰度）或 3（YCbCr）`);
        if (end - seg !== 6 + n * 3) throw new Error("JPEG 的 SOF 段长度与分量数不一致");
        const specs = [];
        hMax = 1;
        vMax = 1;
        for (let i = 0; i < n; i += 1) {
          const o = seg + 6 + i * 3;
          const id = bytes[o];
          const hv = bytes[o + 1];
          const h = hv >> 4;
          const v = hv & 15;
          const tq = bytes[o + 2];
          if (h < 1 || h > 4 || v < 1 || v > 4) {
            throw new Error(`JPEG 采样因子超出范围（分量 ${id}：${h}x${v}，只支持 1..4）`);
          }
          if (tq > 3) throw new Error(`JPEG 量化表编号 ${tq} 超出范围（0..3）`);
          specs.push({ id, h, v, tq });
          if (h > hMax) hMax = h;
          if (v > vMax) vMax = v;
        }
        const mcusX = Math.ceil(width / (8 * hMax));
        const mcusY = Math.ceil(height / (8 * vMax));
        components = specs.map((s) => {
          const cw = Math.ceil(width * s.h / hMax);
          const ch = Math.ceil(height * s.v / vMax);
          const planeW = mcusX * s.h * 8;
          const planeH = mcusY * s.v * 8;
          return {
            id: s.id,
            h: s.h,
            v: s.v,
            tq: s.tq,
            width: cw,
            height: ch,
            planeW,
            plane: new Uint8Array(planeW * planeH)
          };
        });
        dcPred = new Int32Array(components.length);
        scannedComps.length = 0;
        for (let i = 0; i < components.length; i += 1) scannedComps.push(false);
        block = new Int32Array(64);
        ws = new Int32Array(64);
        sawSof = true;
        break;
      }
      case 194:
        throw new Error("这是 progressive（渐进式）JPEG，不支持：本解码器只解 baseline 顺序编码");
      case 195:
        throw new Error("JPEG 用了无损（lossless）编码（SOF3），不支持");
      case 197:
      case 198:
      case 199:
        throw new Error("JPEG 是差分/分层编码（SOF5-7），不支持");
      case 201:
      case 202:
      case 203:
        throw new Error("JPEG 用了算术编码（SOF9-11），不支持：只支持 Huffman 编码");
      case 205:
      case 206:
      case 207:
        throw new Error("JPEG 用了算术编码（SOF13-15），不支持：只支持 Huffman 编码");
      case 204:
        throw new Error("JPEG 用了算术编码（DAC 段），不支持：只支持 Huffman 编码");
      case 196: {
        let q = seg;
        while (q < end) {
          const tcth = bytes[q];
          q += 1;
          const tc = tcth >> 4;
          const th = tcth & 15;
          if (tc > 1 || th > 3) throw new Error(`JPEG 的 Huffman 表标识不合法（Tc=${tc}, Th=${th}）`);
          if (q + 16 > end) throw new Error("JPEG 的 DHT 段被截断");
          const counts = new Uint8Array(17);
          let total = 0;
          for (let i = 1; i <= 16; i += 1) {
            const c = bytes[q];
            q += 1;
            counts[i] = c;
            total += c;
          }
          if (q + total > end) throw new Error("JPEG 的 DHT 段被截断");
          const values = bytes.slice(q, q + total);
          q += total;
          const table = buildHuffman(counts, values);
          if (tc === 0) dcTables[th] = table;
          else acTables[th] = table;
        }
        break;
      }
      case 219: {
        let q = seg;
        while (q < end) {
          const pqtq = bytes[q];
          q += 1;
          const pq = pqtq >> 4;
          const tq = pqtq & 15;
          if (pq > 1 || tq > 3) throw new Error(`JPEG 的量化表标识不合法（Pq=${pq}, Tq=${tq}）`);
          if (q + 64 * (pq + 1) > end) throw new Error("JPEG 的 DQT 段被截断");
          const qt = new Uint16Array(64);
          for (let i = 0; i < 64; i += 1) {
            const nat = ZIGZAG[i];
            if (pq === 0) {
              qt[nat] = bytes[q];
              q += 1;
            } else {
              qt[nat] = bytes[q] << 8 | bytes[q + 1];
              q += 2;
            }
          }
          qtables[tq] = qt;
        }
        break;
      }
      case 221: {
        if (end - seg < 2) throw new Error("JPEG 的 DRI 段太短");
        restartInterval = bytes[seg] << 8 | bytes[seg + 1];
        break;
      }
      case 218: {
        if (!sawSof) throw new Error("JPEG 在 SOF 之前出现了 SOS");
        if (end - seg < 4) throw new Error("JPEG 的 SOS 段太短");
        const ns = bytes[seg];
        if (ns !== 1 && ns !== 3) {
          throw new Error(`JPEG 扫描里有 ${ns} 个分量，不支持：只支持 1 或 3`);
        }
        if (end - seg !== 1 + ns * 2 + 3) throw new Error("JPEG 的 SOS 段长度与分量数不一致");
        const scanComponents = [];
        for (let i = 0; i < ns; i += 1) {
          const cs = bytes[seg + 1 + i * 2];
          const tdta = bytes[seg + 2 + i * 2];
          const index = components.findIndex((c) => c.id === cs);
          if (index < 0) throw new Error(`JPEG 的扫描引用了不存在的分量 ${cs}`);
          const comp = components[index];
          const td2 = tdta >> 4;
          const ta = tdta & 15;
          const dc = dcTables[td2];
          const ac = acTables[ta];
          if (!dc || !ac) throw new Error(`JPEG 缺少 Huffman 表（DC ${td2} / AC ${ta}）`);
          const qt = qtables[comp.tq];
          if (!qt) throw new Error(`JPEG 缺少分量 ${cs} 的量化表 ${comp.tq}`);
          scanComponents.push({ comp, index, dc, ac, qt });
        }
        const o = seg + 1 + ns * 2;
        const ss = bytes[o];
        const se = bytes[o + 1];
        const ahal = bytes[o + 2];
        if (ss !== 0 || se !== 63 || ahal !== 0) {
          if (ss !== 0) throw new Error("这不是 baseline 扫描（谱选择 Ss≠0）：progressive/渐进式 JPEG 不支持");
          throw new Error(`JPEG 扫描参数不是 baseline（Ss=${ss}, Se=${se}, Ah/Al=${ahal}），不支持`);
        }
        for (const sc of scanComponents) scannedComps[sc.index] = true;
        sawScan = true;
        pos = decodeScan(scanComponents, end);
        continue;
      }
      case 224:
      case 225:
      case 226:
      case 227:
      case 228:
      case 229:
      case 230:
      case 231:
      case 232:
      case 233:
      case 234:
      case 235:
      case 236:
      case 237:
      case 238:
      case 239:
      case 254:
        break;
      // APPn / COM：跳过
      default:
        break;
    }
    pos += segLen;
  }
  if (!sawSof) throw new Error("JPEG 里没找到 SOF（可能不是完整的基线 JPEG）");
  if (!sawScan) throw new Error("JPEG 里没找到扫描数据（SOS）");
  for (let i = 0; i < components.length; i += 1) {
    if (!scannedComps[i]) throw new Error(`JPEG 缺少分量 ${components[i].id} 的扫描数据（文件不完整）`);
  }
  return toRgba(components, width, height);
}
function toRgba(components, width, height) {
  const out = new Uint8ClampedArray(width * height * 4);
  if (components.length === 1) {
    const c = components[0];
    const plan = c.plane;
    const pw = c.planeW;
    for (let y = 0; y < height; y += 1) {
      const srcRow = y * pw;
      const dstRow = y * width * 4;
      for (let x = 0; x < width; x += 1) {
        const v = plan[srcRow + x];
        const o = dstRow + x * 4;
        out[o] = v;
        out[o + 1] = v;
        out[o + 2] = v;
        out[o + 3] = 255;
      }
    }
    return { data: out, width, height };
  }
  const yC = components[0];
  const cbC = components[1];
  const crC = components[2];
  const hMax = hMaxOf(components);
  const vMax = vMaxOf(components);
  const yx = buildAxis(yC.h, hMax, yC.width, width);
  const yy = buildAxis(yC.v, vMax, yC.height, height);
  const cbx = buildAxis(cbC.h, hMax, cbC.width, width);
  const cby = buildAxis(cbC.v, vMax, cbC.height, height);
  const crx = buildAxis(crC.h, hMax, crC.width, width);
  const cry = buildAxis(crC.v, vMax, crC.height, height);
  const crR = new Int32Array(256);
  const cbG = new Int32Array(256);
  const crG = new Int32Array(256);
  const cbB = new Int32Array(256);
  for (let i = 0; i < 256; i += 1) {
    const v = i - 128;
    crR[i] = 91881 * v + 32768 >> 16;
    cbG[i] = -22554 * v + 32768 >> 16;
    crG[i] = -46802 * v + 32768 >> 16;
    cbB[i] = 116130 * v + 32768 >> 16;
  }
  for (let y = 0; y < height; y += 1) {
    const y0 = yy.i0[y];
    const y1 = yy.i1[y];
    const yf = yy.frac[y];
    const cb0 = cby.i0[y];
    const cb1 = cby.i1[y];
    const cbf = cby.frac[y];
    const cr0 = cry.i0[y];
    const cr1 = cry.i1[y];
    const crf = cry.frac[y];
    const yRow0 = y0 * yC.planeW;
    const yRow1 = y1 * yC.planeW;
    const cbRow0 = cb0 * cbC.planeW;
    const cbRow1 = cb1 * cbC.planeW;
    const crRow0 = cr0 * crC.planeW;
    const crRow1 = cr1 * crC.planeW;
    for (let x = 0; x < width; x += 1) {
      const yx0 = yx.i0[x];
      const yx1 = yx.i1[x];
      const yxf = yx.frac[x];
      const yTop = yC.plane[yRow0 + yx0] * (256 - yxf) + yC.plane[yRow0 + yx1] * yxf;
      const yBot = yC.plane[yRow1 + yx0] * (256 - yxf) + yC.plane[yRow1 + yx1] * yxf;
      const yv = (256 - yf) * yTop + yf * yBot + 32768 >> 16;
      const cbx0 = cbx.i0[x];
      const cbx1 = cbx.i1[x];
      const cbfx = cbx.frac[x];
      const cbTop = cbC.plane[cbRow0 + cbx0] * (256 - cbfx) + cbC.plane[cbRow0 + cbx1] * cbfx;
      const cbBot = cbC.plane[cbRow1 + cbx0] * (256 - cbfx) + cbC.plane[cbRow1 + cbx1] * cbfx;
      const cbv = (256 - cbf) * cbTop + cbf * cbBot + 32768 >> 16;
      const crx0 = crx.i0[x];
      const crx1 = crx.i1[x];
      const crfx = crx.frac[x];
      const crTop = crC.plane[crRow0 + crx0] * (256 - crfx) + crC.plane[crRow0 + crx1] * crfx;
      const crBot = crC.plane[crRow1 + crx0] * (256 - crfx) + crC.plane[crRow1 + crx1] * crfx;
      const crv = (256 - crf) * crTop + crf * crBot + 32768 >> 16;
      const o = (y * width + x) * 4;
      out[o] = clampByte(yv + crR[crv]);
      out[o + 1] = clampByte(yv + cbG[cbv] + crG[crv]);
      out[o + 2] = clampByte(yv + cbB[cbv]);
      out[o + 3] = 255;
    }
  }
  return { data: out, width, height };
}
function hMaxOf(components) {
  let h = 1;
  for (const c of components) if (c.h > h) h = c.h;
  return h;
}
function vMaxOf(components) {
  let v = 1;
  for (const c of components) if (c.v > v) v = c.v;
  return v;
}

// src/mcloud/slide.ts
var ALPHA_OPAQUE_THRESHOLD = 128;
function toGrayFast(data, i) {
  return data[i] * 77 + data[i + 1] * 150 + data[i + 2] * 29 >> 8;
}
function matchSlide(bgData, puzzleData, bgW, bgH, pzW, pzH) {
  const bgPixels = bgData.data;
  const pzPixels = puzzleData.data;
  const contourX = [];
  const contourY = [];
  const bgEdge = new Uint8Array(bgW * bgH);
  for (let y = 1; y < bgH - 1; y += 1) {
    for (let x = 1; x < bgW - 1; x += 1) {
      const tl = toGrayFast(bgPixels, (y - 1) * bgW + (x - 1) << 2);
      const tc = toGrayFast(bgPixels, (y - 1) * bgW + x << 2);
      const tr = toGrayFast(bgPixels, (y - 1) * bgW + (x + 1) << 2);
      const ml = toGrayFast(bgPixels, y * bgW + (x - 1) << 2);
      const mr = toGrayFast(bgPixels, y * bgW + (x + 1) << 2);
      const bl = toGrayFast(bgPixels, (y + 1) * bgW + (x - 1) << 2);
      const bc = toGrayFast(bgPixels, (y + 1) * bgW + x << 2);
      const br = toGrayFast(bgPixels, (y + 1) * bgW + (x + 1) << 2);
      const gx = -tl + tr - 2 * ml + 2 * mr - bl + br;
      const gy = -tl - 2 * tc - tr + bl + 2 * bc + br;
      bgEdge[y * bgW + x] = Math.min(255, Math.abs(gx) + Math.abs(gy) >> 2);
    }
  }
  for (let y = 1; y < pzH - 1; y += 1) {
    for (let x = 1; x < pzW - 1; x += 1) {
      const pi = y * pzW + x << 2;
      if (pzPixels[pi + 3] <= ALPHA_OPAQUE_THRESHOLD) continue;
      const above = pzPixels[((y - 1) * pzW + x) * 4 + 3] <= ALPHA_OPAQUE_THRESHOLD;
      const below = pzPixels[((y + 1) * pzW + x) * 4 + 3] <= ALPHA_OPAQUE_THRESHOLD;
      const left = pzPixels[(y * pzW + (x - 1)) * 4 + 3] <= ALPHA_OPAQUE_THRESHOLD;
      const right = pzPixels[(y * pzW + (x + 1)) * 4 + 3] <= ALPHA_OPAQUE_THRESHOLD;
      if (above || below || left || right) {
        contourX.push(x);
        contourY.push(y);
      }
    }
  }
  const numContours = contourX.length;
  if (numContours === 0) return { offsetX: 0, confidence: 0 };
  const maxOffset = bgW - pzW;
  if (maxOffset < 0) return { offsetX: 0, confidence: 0 };
  let bestOffset = 0;
  let bestScore = 0;
  let totalScore = 0;
  for (let offset = 0; offset <= maxOffset; offset += 1) {
    let sum = 0;
    for (let c = 0; c < numContours; c += 1) {
      const bx = offset + contourX[c];
      if (bx < bgW) sum += bgEdge[contourY[c] * bgW + bx];
    }
    totalScore += sum;
    if (sum > bestScore) {
      bestScore = sum;
      bestOffset = offset;
    }
  }
  const avgScore = totalScore / (maxOffset + 1);
  return { offsetX: bestOffset, confidence: avgScore > 0 ? bestScore / avgScore : 0 };
}
function decodeSlideImage(bytes) {
  if (isJpeg(bytes)) return decodeJpeg(bytes);
  const isPng = bytes.length >= 8 && bytes[0] === 137 && bytes[1] === 80 && bytes[2] === 78 && bytes[3] === 71;
  if (isPng) return decodeImage(bytes);
  throw new Error("滑块题面既不是 PNG 也不是 JPEG（移动云盘换了格式？）");
}
function calcSlideOffset(puzzleBase64, pictureBase64) {
  const puzzle = decodeSlideImage(fromBase64(puzzleBase64));
  const picture = decodeSlideImage(fromBase64(pictureBase64));
  return matchSlide(picture, puzzle, picture.width, picture.height, puzzle.width, puzzle.height);
}
function calcPuzzleOffset(puzzleBase64, pictureBase64) {
  return calcSlideOffset(puzzleBase64, pictureBase64);
}
var MIN_CONFIDENCE = 1.5;
function usable(r) {
  return r.offsetX > 0 && r.confidence >= MIN_CONFIDENCE;
}

// src/mcloud/tasks/exchangeList.ts
var STATE_LIST = "mcloud.exchangelist.last";
var STATE_RAW = "mcloud.exchangelist.raw";
var LIMIT_AVAILABLE = 0;
var LIMIT_TOMORROW = 1;
var LIMIT_EXCHANGED = 2;
var LIMIT_GROUP_EXCHANGED = 4;
function verdictOf(p) {
  if (p.onLine !== void 0 && p.onLine !== 1) return "offline";
  if (p.monthLimit !== void 0 && p.received !== void 0 && p.received >= p.monthLimit) {
    return "month-limit";
  }
  if (p.limit === void 0) return "limit-missing";
  if (p.limit === LIMIT_AVAILABLE) {
    if (p.minRemainderCount === 0) return "min-remainder-zero";
    if (p.count !== void 0 && p.count <= 0) return "sold-out";
    if (p.dailyRemainderCount !== void 0 && p.dailyRemainderCount <= 0) return "daily-empty";
  }
  switch (p.limit) {
    case LIMIT_AVAILABLE:
      return "ok";
    case LIMIT_TOMORROW:
      return "tomorrow";
    case LIMIT_EXCHANGED:
      return "exchanged";
    case LIMIT_GROUP_EXCHANGED:
      return "group-exchanged";
    default:
      return "unknown";
  }
}
function asInt(v) {
  if (typeof v === "number" && Number.isFinite(v)) return Math.trunc(v);
  if (typeof v === "string" && /^-?\d+$/.test(v.trim())) return Number(v.trim());
  return void 0;
}
function asStr(v) {
  return typeof v === "string" && v !== "" ? v : void 0;
}
function parseMonthLimit(v) {
  let o = v;
  if (typeof v === "string") {
    try {
      o = JSON.parse(v);
    } catch {
      return void 0;
    }
  }
  if (o === null || typeof o !== "object" || Array.isArray(o)) return void 0;
  const r = o;
  return asInt(r.limitMonthQuantity ?? r.limit_month_quantity);
}
function parsePrize(raw) {
  if (raw === null || typeof raw !== "object" || Array.isArray(raw)) return void 0;
  const o = raw;
  const p = {
    // ⚠ 实拍是**数字**，不是字符串（原项目特意标了这一点）
    prizeId: asInt(o.prizeId ?? o.prize_id),
    // ⚠ 真机 51/51 是 `prizeName`；`name` 保留是为兼容 mock 与将来可能的另一种形状
    name: asStr(o.prizeName ?? o.name),
    monthLimit: parseMonthLimit(o.probability2),
    received: asInt(o.receiveQuantity ?? o.receive_quantity),
    groupId: asInt(o.groupId ?? o.group_id),
    // ⚠⚠ 价格是 **`pOrder`**（不是 `price`）—— 而且同一份响应里 `POrder` 与 `pOrder`
    // **两种拼法并存**，只读一个键的写法会把一部分商品的价格判成"没给"，
    // 于是"余额够不够"那道判断对那几件**恒假**（看着像"没配置价格"，其实是不检查就下单）。
    // 逐字照 mcloud 的 `parse_price`：小写先试，大写兜底。
    price: asInt(o.pOrder) ?? asInt(o.POrder),
    limit: asInt(o.limit),
    count: asInt(o.count),
    totalCount: asInt(o.totalCount ?? o.total_count),
    dailyRemainderCount: asInt(o.dailyRemainderCount ?? o.daily_remainder_count),
    dailyCount: asInt(o.dailyCount ?? o.daily_count),
    minRemainderCount: asInt(o.minRemainderCount ?? o.min_remainder_count),
    onLine: asInt(o.onLine ?? o.on_line),
    startTime: asStr(o.startTime ?? o.start_time),
    endTime: asStr(o.endTime ?? o.end_time)
  };
  return p.prizeId !== void 0 || p.name !== void 0 ? p : void 0;
}
function parseExchangeList(result) {
  const o = resultObject({ code: 0, message: "", result, ok: true });
  const src = o.groups ?? o;
  const groups = [];
  const all = [];
  if (src && typeof src === "object" && !Array.isArray(src)) {
    for (const key of Object.keys(src)) {
      const v = src[key];
      if (!Array.isArray(v)) continue;
      const prizes = v.map(parsePrize).filter((x) => !!x);
      groups.push({ group: key, prizes });
      all.push(...prizes);
    }
  }
  groups.sort((a, b) => (Number(a.group) || 0) - (Number(b.group) || 0));
  all.sort((a, b) => (a.groupId ?? 0) - (b.groupId ?? 0) || (a.prizeId ?? 0) - (b.prizeId ?? 0));
  return { groups, prizes: all };
}
function summarizeList(prizes) {
  const byVerdict = {};
  const ok = [];
  for (const p of prizes) {
    const v = verdictOf(p);
    byVerdict[v] = (byVerdict[v] ?? 0) + 1;
    if (v === "ok" && ok.length < 20) {
      ok.push({ prizeId: p.prizeId, name: p.name, price: p.price, groupId: p.groupId });
    }
  }
  return { total: prizes.length, okCount: byVerdict.ok ?? 0, byVerdict, ok };
}
function runExchangeList(deps) {
  return runTask(
    deps,
    {
      task: "exchange-list",
      endpoint: "exchangeList",
      method: "GET",
      marketName: "sign_in_3",
      isDeviceId: false,
      stateKey: STATE_RAW
    },
    (env) => {
      const { groups, prizes } = parseExchangeList(env.result);
      const s = summarizeList(prizes);
      const reading = { at: deps.nowIso, groups: groups.length, ...s };
      deps.store.setState(STATE_LIST, JSON.stringify(reading), deps.nowIso);
      savePickCache(deps.store, prizes, deps.nowIso);
      const names = s.ok.slice(0, 3).map(
        (x) => x.price === void 0 ? x.name ?? "#" + x.prizeId : `${x.name ?? "#" + x.prizeId}(${x.price}豆)`
      );
      return {
        ok: true,
        note: `兑换清单：${groups.length} 组 / ${s.total} 件，可兑 ${s.okCount} 件` + (names.length ? `（${names.join("、")}）` : "") + (s.byVerdict.unknown ? `；判不了 ${s.byVerdict.unknown} 件` : ""),
        snapshot: reading
      };
    }
  );
}
var STATE_PICK = "mcloud.exchangelist.pick";
function toPickItems(prizes) {
  const out = [];
  for (const p of prizes) {
    const name = p.name?.trim();
    if (name === void 0 || name === "") continue;
    const item = { name, verdict: verdictOf(p) };
    if (p.prizeId !== void 0) item.id = p.prizeId;
    if (p.groupId !== void 0) item.groupId = p.groupId;
    if (p.price !== void 0) item.price = p.price;
    if (p.dailyRemainderCount !== void 0) item.remain = p.dailyRemainderCount;
    if (p.count !== void 0) item.count = p.count;
    if (p.totalCount !== void 0) item.totalCount = p.totalCount;
    if (p.dailyRemainderCount !== void 0) item.dailyRemainderCount = p.dailyRemainderCount;
    if (p.dailyCount !== void 0) item.dailyCount = p.dailyCount;
    if (p.minRemainderCount !== void 0) item.minRemainderCount = p.minRemainderCount;
    if (p.startTime !== void 0) item.startTime = p.startTime;
    if (p.endTime !== void 0) item.endTime = p.endTime;
    if (p.monthLimit !== void 0) item.monthLimit = p.monthLimit;
    if (p.received !== void 0) item.received = p.received;
    out.push(item);
  }
  return out;
}
function savePickCache(store2, prizes, nowIso) {
  const cache = { date: nowIso.slice(0, 10), items: toPickItems(prizes) };
  store2.setState(STATE_PICK, JSON.stringify(cache), nowIso);
}

// src/mcloud/tasks/exchangeOrder.ts
var STATE_STEP = "mcloud.exchange.order.step";
var STATE_PLAN = "mcloud.exchange.order.plan";
var STATE_BASE = "mcloud.exchange.order.baseline";
var STATE_ROUND = "mcloud.exchange.order.round";
var SLIDER_ROUNDS = 2;
var EXCHANGE_CLIENT_VERSION = "12.6.0";
var SLIDE_FORM_CONTENT_TYPE = "application/x-www-form-urlencoded;charset=UTF-8";
var VISIT_LOG_KEYWORD_EXCHANGE = "newsignin_index_to_exchange";
var VISIT_LOG_KEYWORD_EXCHANGE_TIMES = "newsignin_index_exchange_times";
var VISIT_LOG_MARKET_NAME = "sign_in_3";
var CODE_SLIDER_WRONG = 514;
var PREHEAT_DELAY_MS = 500;
function num(v) {
  if (typeof v === "number" && Number.isFinite(v)) return v;
  if (typeof v === "string" && /^-?\d+$/.test(v.trim())) return Number(v.trim());
  return void 0;
}
function signedReq(deps, spec) {
  const session = loadSession(deps.store);
  return buildH5Request({
    url: spec.url,
    method: spec.method,
    body: spec.body,
    contentType: spec.contentType,
    referer: REFERER_SIGN_IN,
    showLoading: SHOW_LOADING,
    // ⚠ `isDeviceId` 是**逐接口**的：下单那条要，取滑块那条**不要**（照实拍）
    isDeviceId: spec.isDeviceId ?? false,
    saltKey: 1,
    marketName: "sign_in_3",
    session: session ? sessionHeaders(session) : {}
  });
}
function digResult(text2) {
  try {
    const v = JSON.parse(text2);
    const r = v.result;
    return r && typeof r === "object" ? r : v;
  } catch {
    return {};
  }
}
function loadState(deps) {
  const raw = deps.store.getState(STATE_STEP);
  if (!raw) return null;
  try {
    const v = JSON.parse(raw);
    return typeof v.step === "string" ? v : null;
  } catch {
    return null;
  }
}
function saveState(deps, st) {
  deps.store.setState(STATE_STEP, JSON.stringify(st), deps.nowIso);
}
var STATE_LAST_WAIT = "mcloud.exchange.order.last_wait";
var STATE_LEAD_MS = "mcloud.exchange.order.lead_ms";
var STATE_ORDERED_HOUR = "mcloud.exchange.order.ordered_hour";
function exchangeLeadMs(store2) {
  const raw = store2.getState(STATE_LEAD_MS);
  const n = raw === void 0 || raw === null || raw === "" ? Number.NaN : Number(raw);
  return Number.isFinite(n) ? Math.min(2e3, Math.max(0, n)) : FIRE_LEAD_MS;
}
function spinUntil(targetMs, deps) {
  const nowFn = () => depsNowMs(deps);
  const spin = deps.spin ?? (() => {
    const t0 = Date.now();
    while (Date.now() - t0 < 2) {
    }
  });
  const started = nowFn();
  const hardDeadline = started + MAX_WAIT_MS;
  while (true) {
    const now = nowFn();
    if (now >= targetMs) return Math.max(0, now - started);
    if (now - started > MAX_WAIT_MS || now >= hardDeadline) {
      return Math.max(0, now - started);
    }
    spin();
  }
}
var MAX_WAIT_MS = 61 * 6e4;
var MAX_SPIN_MS = 3e4;
function snapshotOrder(store2) {
  return {
    step: store2.getState(STATE_STEP),
    base: store2.getState(STATE_BASE)
  };
}
function restoreOrder(store2, snap, nowIso) {
  if (snap.step === null) store2.setState(STATE_STEP, "", nowIso);
  else store2.setState(STATE_STEP, snap.step, nowIso);
  if (snap.base === null) store2.setState(STATE_BASE, "", nowIso);
  else store2.setState(STATE_BASE, snap.base, nowIso);
}
function currentStep(store2) {
  const raw = store2.getState(STATE_STEP);
  if (raw === void 0 || raw === null || raw === "") return null;
  try {
    const v = JSON.parse(raw);
    return typeof v.step === "string" ? v.step : null;
  } catch {
    return null;
  }
}
function clearOrder(deps) {
  deps.store.setState(STATE_STEP, "", deps.nowIso);
  deps.store.setState(STATE_PLAN, "", deps.nowIso);
  deps.store.setState(STATE_BASE, "", deps.nowIso);
  deps.store.setState(STATE_ROUND, "", deps.nowIso);
}
function startOrder(deps, query) {
  if (query.trim() === "") {
    return { ok: false, note: "兑换要指定奖品（名字或 prizeId）—— 没有它无从下手" };
  }
  saveState(deps, { step: "list", round: 0 });
  deps.store.setState(STATE_PLAN, query.trim(), deps.nowIso);
  return { ok: true, note: `已排队兑换「${query.trim()}」—— 后面每次触发走一步（一次调用一个请求）` };
}
function pickPrize(prizes, query) {
  const q = query.trim();
  if (/^\d+$/.test(q)) return prizes.find((p) => String(p.prizeId) === q);
  return prizes.find((p) => p.name === q);
}
var STEP_TEXT = {
  list: "① 读清单",
  balance: "② 读基准余额（成交复查要拿它算差值）",
  "warm-1": "③ 预热第一枪",
  "warm-2": "③ 预热第二枪",
  slide: "④ 取滑块题面并本地解",
  order: "⑤ 下单",
  verify: "⑥ 复查余额差",
  done: "完成"
};
function runOrderStep(deps) {
  const st = loadState(deps);
  if (!st) return { ok: true, idle: true, note: "没有在排队的兑换（先用 startOrder 起一单）" };
  const query = deps.store.getState(STATE_PLAN) ?? "";
  const step = st.step;
  const label = STEP_TEXT[step];
  const done = (note, ok = true) => {
    saveState(deps, { ...st, step: "done" });
    return { ok, note };
  };
  if (step === "done") return { ok: true, idle: true, note: "这一单已经走完了（要再来一单得重新起）" };
  if (step === "list") {
    const res = callRawText(
      deps.transport,
      signedReq(deps, {
        url: `${hostBase(deps, MCLOUD_HOST)}/ycloud/signin/page/exchangeList?client=app&clientVersion=${encodeURIComponent(EXCHANGE_CLIENT_VERSION)}`,
        method: "GET"
      })
    );
    if (!res.ok) return { ok: false, note: `${label}失败：${res.error ?? "未知失败"}` };
    const { prizes } = parseExchangeList(digResult(res.text));
    const p = pickPrize(prizes, query);
    if (!p) return done(`${label}：清单里没有「${query}」—— 这一单不做（名字要**精确**匹配）`, false);
    const prizeId = num(p.prizeId);
    if (prizeId === void 0) {
      return done(`${label}：清单里那条没有可用的 prizeId ⇒ 这一单不做（不许编一个数字发出去）`, false);
    }
    const plan = { prizeId, prizeName: String(p.name ?? ""), price: num(p.price), query };
    saveState(deps, { ...st, step: "balance", plan });
    return { ok: true, note: `${label}：目标「${p.name}」（prizeId=${p.prizeId}）${p.price === void 0 ? "" : `，${p.price} 豆`} —— 下一步读基准余额` };
  }
  if (step === "balance") {
    const res = callRawText(
      deps.transport,
      signedReq(deps, { url: `${hostBase(deps, MCLOUD_HOST)}/ycloud/signin/page/infoV3?client=app`, method: "GET" })
    );
    if (!res.ok) return { ok: false, note: `${label}失败：${res.error ?? "未知失败"}` };
    let balance;
    try {
      const env = digResult(res.text);
      const v = env.total ?? env.point ?? env.balance;
      if (typeof v === "number") balance = v;
      else if (typeof v === "string" && /^\d+$/.test(v)) balance = Number(v);
    } catch {
    }
    if (balance !== void 0) deps.store.setState(STATE_BASE, String(balance), deps.nowIso);
    saveState(deps, { ...st, step: "warm-1" });
    return { ok: true, note: `${label}：${balance ?? "没读到"}${balance === void 0 ? "（照样往下走 —— 它只是基准，不是判据）" : "，作为成交复查的基准"} —— 下一步预热` };
  }
  if (step === "warm-1" || step === "warm-2") {
    if (step === "warm-1") {
      const w = planWindowWait(depsNowMs(deps));
      if (w.needed) {
        if (w.ms > MAX_SPIN_MS) {
          return {
            ok: true,
            idle: true,
            note: `${label}：还没到窗口（还差 ${Math.round(w.ms / 1e3)}s）—— 交给下一次触发，本次零请求`
          };
        }
        const waited = spinUntil(w.untilMs, deps);
        deps.store.setState(STATE_LAST_WAIT, String(Math.round(waited)), deps.nowIso);
      }
    }
    if (step === "warm-2") spinUntil(depsNowMs(deps) + PREHEAT_DELAY_MS, deps);
    const keyword = step === "warm-1" ? VISIT_LOG_KEYWORD_EXCHANGE : VISIT_LOG_KEYWORD_EXCHANGE_TIMES;
    const res = callRawText(
      deps.transport,
      signedReq(deps, {
        url: `${hostBase(deps, MCLOUD_HOST)}/ycloud/visitlog/journaling?module=uservisit&optkeyword=${encodeURIComponent(keyword)}&sourceid=1002&marketName=${encodeURIComponent(VISIT_LOG_MARKET_NAME)}`,
        method: "POST",
        body: ""
      })
    );
    const warn2 = res.ok ? "" : `（埋点失败但不挡路：${res.error ?? "未知"}）`;
    if (step === "warm-1") {
      saveState(deps, { ...st, step: "warm-2" });
      return { ok: true, note: `${label}：${keyword}${warn2} —— 下一步第二枪（中间那 500ms 由面板的下一次触发承担）` };
    }
    saveState(deps, { ...st, step: "slide" });
    return { ok: true, note: `${label}：${keyword}${warn2} —— 下一步取滑块题面` };
  }
  if (step === "slide") {
    const res = callRawText(
      deps.transport,
      signedReq(deps, {
        url: `${hostBase(deps, MCLOUD_HOST)}/ycloud/auth-service/slide/getSlide`,
        method: "POST",
        // ⚠ 这一条用 **form** content-type（不是 JSON）—— 逐接口的，不是一刀切
        contentType: SLIDE_FORM_CONTENT_TYPE,
        body: "{}",
        isDeviceId: false
      })
    );
    if (!res.ok) return { ok: false, note: `${label}失败：${res.error ?? "未知失败"}` };
    let puzzle;
    let picture;
    try {
      const env = digResult(res.text);
      const pz = env.puzzleImg ?? env.puzzle ?? env.puzzleImage;
      const bg = env.pictureImg ?? env.picture ?? env.backgroundImg;
      if (typeof pz === "string") puzzle = pz;
      if (typeof bg === "string") picture = bg;
    } catch (e) {
      return { ok: false, note: `${label}：题面解析不了（${e instanceof Error ? e.message : String(e)}）` };
    }
    if (!puzzle || !picture) {
      return { ok: false, note: `${label}：题面上没有两张图（回 ${res.text.slice(0, 120)}）` };
    }
    let solution;
    try {
      solution = calcPuzzleOffset(puzzle, picture);
    } catch (e) {
      return done(
        `${label}：图解码失败（${e instanceof Error ? e.message : String(e)}）⇒ 不发下单那一枪（offset 错了那一枪不可能成功，只会白耗一次题面）`,
        false
      );
    }
    if (!usable(solution)) {
      return done(
        `${label}：算不出可用的 offset（解出 ${solution.offsetX}，置信度 ${solution.confidence.toFixed(2)}）⇒ 不发下单那一枪 —— 这一枪发出去是无效的，不是"少一次机会"而是"多一次白打"`,
        false
      );
    }
    saveState(deps, { ...st, step: "order", offset: solution.offsetX });
    return { ok: true, note: `${label}：offset=${solution.offsetX}（置信度 ${solution.confidence.toFixed(2)}）—— 下一步下单` };
  }
  if (step === "order") {
    const f = planFireWait(depsNowMs(deps), exchangeLeadMs(deps.store));
    if (f.needed) {
      if (f.ms > MAX_SPIN_MS) {
        return done(
          `${label}：离整点还有 ${Math.round(f.ms / 1e3)}s —— 这一单的计划过期了（预热与取题该在窗口内做完），不发`,
          false
        );
      }
      const waited = spinUntil(f.untilMs, deps);
      deps.store.setState(STATE_LAST_WAIT, String(Math.round(waited)), deps.nowIso);
    }
    const plan = st.plan;
    if (!plan) return done("内部状态缺计划，这一单作废", false);
    const sess = loadSession(deps.store);
    const deviceId = sess?.deviceId ?? "";
    if (deviceId === "") {
      return done('没有 deviceId（它要现换，见 smid 链）—— 不带它下单会回 615，文案却像"活动太火爆"', false);
    }
    if (st.offset === void 0) return done("内部状态缺 offset，这一单作废（不发一个没算出来的数）", false);
    const body = `{"prizeId":${plan.prizeId},"client":"app","clientVersion":"${EXCHANGE_CLIENT_VERSION}","puzzleOffset":${st.offset},"smsCode":"","deviceId":"${deviceId}"}`;
    const req = signedReq(deps, {
      url: `${hostBase(deps, MCLOUD_HOST)}/ycloud/signin/page/exchangeV3`,
      method: "POST",
      body,
      isDeviceId: true
      // 这条**要** isdeviceid，而取滑块那条不要
    });
    req.headers.origin = MCLOUD_HOST;
    const res = callRawText(deps.transport, req);
    if (!res.ok) return { ok: false, note: `${label}失败：${res.error ?? "未知失败"}` };
    let code;
    try {
      const env = digResult(res.text);
      const c = env.code;
      code = typeof c === "number" ? c : typeof c === "string" && /^-?\d+$/.test(c) ? Number(c) : void 0;
    } catch {
    }
    if (code === CODE_SLIDER_WRONG) {
      const round = (st.round ?? 0) + 1;
      if (round > SLIDER_ROUNDS) {
        return done(`${label}：滑块连续 ${SLIDER_ROUNDS} 轮被判错 ⇒ 停手（上限 SLIDER_ROUNDS 轮）`, false);
      }
      saveState(deps, { ...st, step: "slide", round, offset: void 0 });
      return { ok: true, note: `${label}：514（滑块算错）⇒ 重取一题再来一枪（第 ${round}/${SLIDER_ROUNDS} 轮），**没有重复下单**` };
    }
    deps.store.setState(STATE_ORDERED_HOUR, hourKey(depsNowMs(deps)), deps.nowIso);
    saveState(deps, { ...st, step: "verify" });
    return { ok: true, note: `${label}：服务端回 ${JSON.stringify(env0(res.text)).slice(0, 140)} —— 下一步查余额差（**那句话不算数**）` };
  }
  if (step === "verify") {
    const before = deps.store.getState(STATE_BASE);
    const res = callRawText(
      deps.transport,
      signedReq(deps, { url: `${hostBase(deps, MCLOUD_HOST)}/ycloud/signin/page/infoV3?client=app`, method: "GET" })
    );
    if (!res.ok) return { ok: false, note: `${label}失败：${res.error ?? "未知失败"}` };
    let after;
    try {
      const env = digResult(res.text);
      const v = env.total ?? env.point ?? env.balance;
      if (typeof v === "number") after = v;
    } catch {
    }
    if (!before || after === void 0) {
      return done(`${label}：拿不到余额差（基准「${before || "缺"}」/ 现在「${after ?? "缺"}」）⇒ 成交与否**不知道**，如实报`, false);
    }
    const delta = Number(before) - after;
    if (delta > 0) return done(`${label}：✅ 成交（余额 ${before} → ${after}，扣了 ${delta} 豆）`);
    return done(`${label}：❌ 没成交（余额 ${before} → ${after}，没扣豆）—— 服务端那句话不算数，只有余额差算数`, false);
  }
  return { ok: false, note: `不认识的步骤「${step}」` };
}
function env0(text2) {
  const r = digResult(text2);
  return Object.keys(r).length > 0 ? r : text2.slice(0, 140);
}

// src/drain.ts
function drain(step, deps, opts) {
  const startedAt = opts.startedAt ?? Date.now();
  const notes = [];
  let steps = 0;
  let lastNote = "";
  for (let i = 0; i < opts.maxSteps; i++) {
    if (i > 0 && opts.gap === true) waitGap(deps);
    if (i > 0 && opts.more && !opts.more()) {
      return {
        ok: true,
        idle: true,
        steps,
        note: `${opts.label}：${steps} 步走完${lastNote ? ` —— ${lastNote}` : ""}`,
        notes
      };
    }
    if (Date.now() - startedAt > opts.deadlineMs) {
      return {
        ok: true,
        idle: false,
        steps,
        note: `${opts.label}：走到时间上限（${opts.deadlineMs}ms / ${steps} 步），下一次接着走`,
        notes
      };
    }
    let r;
    try {
      r = step(deps);
    } catch (e) {
      notes.push(`抛异常：${e instanceof Error ? e.message : String(e)}`);
      return { ok: false, idle: false, steps, note: `${opts.label}：第 ${steps + 1} 步抛异常`, notes };
    }
    steps++;
    lastNote = r.note;
    notes.push(r.note);
    opts.onStep?.(r.note, steps);
    if (!r.ok && r.pending !== true) {
      return { ok: false, idle: false, steps, note: `${opts.label}：第 ${steps} 步失败 —— ${r.note}`, notes };
    }
    if (r.idle || r.skipped) {
      return {
        ok: true,
        idle: true,
        pending: r.pending === true,
        steps,
        note: brief(r.note, opts.label),
        notes
      };
    }
  }
  return {
    ok: true,
    idle: false,
    steps,
    // ⚠ 这里**不说**"还有没做完的"。踩过的坑：领取AI豆的队列刚好在第 12 步领空，
    //   而"到上限"和"刚好做完"从这一步看不出区别 —— 于是每一趟都说"还有没做完的"，
    //   用户看到的是一句假话（他的原话：「领取云朵应该没有步数啊」）。
    //   对用户只报**结论**：成了。真想看走到哪一步，运行记录里每一步都在。
    // 同样用最后一步自己的话（跑到上限时，它刚做完的那件事就是结论），
    // 但**截短**：这一格要进 runs 表当一行摘要，不能塞整段原话。
    note: brief(lastNote ?? "", opts.label),
    notes
  };
}
var BRIEF_MAX = 40;
function brief(s, label) {
  const t = s.trim();
  if (t.length === 0) return `${label}：好了`;
  return t.length > BRIEF_MAX ? `${t.slice(0, BRIEF_MAX - 1)}…` : t;
}

// src/store/host.ts
function sqlQ(sql, params) {
  const rows = sqlite_query(sql, params);
  if (!Array.isArray(rows)) {
    throw new Error(`sqlite_query 失败：${rows.error ?? "未知错误"}`);
  }
  return rows;
}
function sqlX(sql, params) {
  const r = sqlite_exec(sql, params);
  if (typeof r?.error === "string" && r.error !== "") {
    throw new Error(`sqlite_exec 失败：${r.error}`);
  }
  return { changes: Number(r?.changes ?? 0), lastInsertRowid: Number(r?.lastInsertRowid ?? 0) };
}
function hostStore() {
  let schemaReady = false;
  return {
    ensureSchema: () => {
      if (schemaReady) return;
      sqlX(
        `CREATE TABLE IF NOT EXISTS mcloud_runs (
           id           INTEGER PRIMARY KEY AUTOINCREMENT,
           task         TEXT    NOT NULL,
           runs         INTEGER NOT NULL,
           scheduled_at TEXT    NOT NULL,
           fired_at     TEXT    NOT NULL,
           late_ms      INTEGER NOT NULL,
           ok           INTEGER NOT NULL,
           note         TEXT
         )`,
        []
      );
      sqlX(
        `CREATE INDEX IF NOT EXISTS idx_mcloud_runs_task_fired ON mcloud_runs (task, fired_at)`,
        []
      );
      sqlX(
        `CREATE TABLE IF NOT EXISTS state (
           key   TEXT PRIMARY KEY,
           value TEXT NOT NULL,
           updated_at TEXT NOT NULL
         )`,
        []
      );
      schemaReady = true;
    },
    getState: (key) => {
      const rows = sqlQ(`SELECT value FROM state WHERE key = ?1`, [key]);
      const row = rows[0];
      return row ? String(row.value) : null;
    },
    setState: (key, value, nowIso) => {
      sqlX(
        `INSERT INTO state (key, value, updated_at) VALUES (?1, ?2, ?3)
         ON CONFLICT(key) DO UPDATE SET value = ?2, updated_at = ?3`,
        [key, value, nowIso]
      );
    },
    recordRun: (r) => {
      const res = sqlX(
        `INSERT INTO mcloud_runs (task, runs, scheduled_at, fired_at, late_ms, ok, note)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)`,
        [r.task, r.runs, r.scheduledAt, r.firedAt, Math.round(r.lateMs), r.ok ? 1 : 0, r.note ?? null]
      );
      return res.lastInsertRowid;
    },
    // 分位数在 SQL 里算，不把行拉回来算：`sqlite_query` 一次最多返回 1000 行，
    // 跑到几万次触发之后「拉回来再算」会静默丢数据 —— 那种错很难发现（数字看着还合理）。
    // 另外两点和内存实现必须同口径：**排除负延迟**（F-001）、**按值排序取分位**（不是按 id）。
    lateStats: (task, lastN = 500) => {
      const agg = sqlQ(
        `SELECT COUNT(*) AS n, MAX(late_ms) AS mx FROM (
           SELECT late_ms FROM mcloud_runs WHERE task = ?1 AND late_ms >= 0 ORDER BY id DESC LIMIT ?2
         )`,
        [task, lastN]
      );
      const n = Number(agg[0]?.n ?? 0);
      const neg = sqlQ(
        `SELECT COUNT(*) AS c FROM (
           SELECT late_ms FROM mcloud_runs WHERE task = ?1 ORDER BY id DESC LIMIT ?2
         ) WHERE late_ms < 0`,
        [task, lastN]
      );
      const pick2 = (q) => {
        if (n === 0) return 0;
        const offset = Math.min(n - 1, Math.max(0, Math.ceil(n * q) - 1));
        const rows = sqlQ(
          `SELECT late_ms AS v FROM (
             SELECT late_ms FROM mcloud_runs WHERE task = ?1 AND late_ms >= 0 ORDER BY id DESC LIMIT ?2
           ) ORDER BY late_ms ASC LIMIT 1 OFFSET ?3`,
          [task, lastN, offset]
        );
        const v = rows[0]?.v;
        return v === void 0 || v === null ? 0 : Number(v);
      };
      return {
        task,
        n,
        lateP50: pick2(0.5),
        lateP99: pick2(0.99),
        lateMax: Number(agg[0]?.mx ?? 0),
        excluded: Number(neg[0]?.c ?? 0)
      };
    },
    recentRuns: (limit = 20) => sqlQ(
      `SELECT task, runs, fired_at, late_ms, ok, note FROM mcloud_runs ORDER BY id DESC LIMIT ?1`,
      [Math.min(limit, 1e3)]
    )
  };
}

// src/mcloud/taskAccounts.ts
function legacyTaskAccountsKey(task) {
  return `mcloud.task.accounts.${task}`;
}
function taskAccountsKey(task, mode = "auto") {
  return `mcloud.task.accounts.${task}.${mode}`;
}
var DEFAULT_TASK_ACCOUNTS = { all: true, ids: [] };
var DEFAULT_TASK_MANUAL_ACCOUNTS = {
  follow: true,
  all: false,
  ids: []
};
var DEFAULT_TASK_ACCOUNTS_BY_TASK = {
  兑换: { all: false, ids: [] }
};
function defaultTaskAccounts(task) {
  const v = DEFAULT_TASK_ACCOUNTS_BY_TASK[task];
  return v ? { ...v } : { ...DEFAULT_TASK_ACCOUNTS };
}
function parseSelection(raw) {
  try {
    const v = JSON.parse(raw);
    const ids = Array.isArray(v.ids) ? v.ids.filter((x) => typeof x === "string") : [];
    return { all: v.all === true, ids };
  } catch {
    return null;
  }
}
function loadTaskAccounts(store2, task) {
  const raw = (store2.getState(taskAccountsKey(task, "auto")) ?? "").trim();
  if (raw !== "") {
    const v = parseSelection(raw);
    return v ?? defaultTaskAccounts(task);
  }
  const legacy = (store2.getState(legacyTaskAccountsKey(task)) ?? "").trim();
  if (legacy !== "") {
    const v = parseSelection(legacy);
    if (v !== null) return v;
  }
  return defaultTaskAccounts(task);
}
function loadManualTaskAccounts(store2, task) {
  const raw = (store2.getState(taskAccountsKey(task, "manual")) ?? "").trim();
  if (raw === "") return { ...DEFAULT_TASK_MANUAL_ACCOUNTS };
  try {
    const v = JSON.parse(raw);
    if (v.follow === true) return { ...DEFAULT_TASK_MANUAL_ACCOUNTS };
    const ids = Array.isArray(v.ids) ? v.ids.filter((x) => typeof x === "string") : [];
    return { follow: false, all: v.all === true, ids };
  } catch {
    return { ...DEFAULT_TASK_MANUAL_ACCOUNTS };
  }
}
function effectiveTaskAccounts(store2, task, mode = "auto") {
  if (mode !== "manual") return loadTaskAccounts(store2, task);
  const m = loadManualTaskAccounts(store2, task);
  return m.follow ? loadTaskAccounts(store2, task) : { all: m.all, ids: m.ids };
}
function expand(store2, sel) {
  const all = loadAccounts(store2);
  const enabled = all.filter((a) => a.disabled !== true);
  if (sel.all) return enabled.map((a) => a.id);
  const want = new Set(sel.ids);
  return enabled.filter((a) => want.has(a.id)).map((a) => a.id);
}
function resolveTaskAccounts(store2, task, mode = "auto") {
  return expand(store2, effectiveTaskAccounts(store2, task, mode));
}
function labelOf(store2, acctId) {
  const a = loadAccounts(store2).find((x) => x.id === acctId);
  return a?.label || a?.id || acctId;
}
function shortLabel(label) {
  if (/^1\d{10}$/.test(label)) return `${label.slice(0, 3)}****${label.slice(-4)}`;
  return label;
}

// src/mcloud/cloudUpload.ts
var AUTH_DOMAINS = [
  "yun.139.com",
  "mcloud.139.com",
  "feixin.10086.cn",
  "mail.10086.cn"
];
function hostOf(rawUrl) {
  const m = /^[a-zA-Z][a-zA-Z0-9+.-]*:\/\/([^/?#]+)/.exec(rawUrl);
  if (!m) return "";
  let host2 = m[1].toLowerCase();
  const at = host2.lastIndexOf("@");
  if (at >= 0) host2 = host2.slice(at + 1);
  const colon = host2.lastIndexOf(":");
  if (colon >= 0) host2 = host2.slice(0, colon);
  return host2;
}
function sendsAuthorizationTo(rawUrl) {
  const host2 = hostOf(rawUrl);
  if (host2 === "") return false;
  return AUTH_DOMAINS.some((suffix) => host2.endsWith(suffix));
}
function authorizeRequest(req, auth) {
  const hasAuth = Object.keys(req.headers).some((k) => k.toLowerCase() === "authorization");
  if (auth === "" || hasAuth || !sendsAuthorizationTo(req.url)) return req;
  return { ...req, headers: { ...req.headers, Authorization: auth } };
}
var EMPTY_ZIP = utf8("PK\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0");

// src/entries/shared.ts
var DEFAULT_REQUEST_TIMEOUT_MS = 1500;
var LONG_REQUEST_TIMEOUT_MS = 2e4;
function makeTransport(timeoutMs = DEFAULT_REQUEST_TIMEOUT_MS) {
  return (req) => {
    const r = applyDefaultContentType(req);
    return http_fetch(r.url, hostFetchOptions(r, timeoutMs));
  };
}
function makeStore() {
  const store2 = hostStore();
  store2.ensureSchema();
  const { moved } = migrateFlatKeys(store2, (/* @__PURE__ */ new Date()).toISOString());
  const s = scoped(store2);
  if (moved.length > 0) {
    log(`状态迁移：${moved.length} 个键搬进账号袋（${moved.join(", ")}）`);
  }
  return s;
}
function depsForAccount(base, acctId, payload, transport2) {
  return taskDeps(scoped(rawStoreOf(base), acctId), transport2, payload);
}
function taskDeps(store2, transport2, payload) {
  const authorized = (req) => transport2(authorizeRequest(req, loadAuthRaw(store2)));
  return {
    store: store2,
    transport: authorized,
    nowIso: (/* @__PURE__ */ new Date()).toISOString(),
    // 基址**每次触发时从 state 读**，不用启动时的快照 ——
    // 否则「刚把基址指到 mock，任务却还在打真上游」
    baseUrl: loadBaseUrl(store2),
    lateMs: payload.lateMs
  };
}

// src/mcloud/scheduleCron.ts
var MIN = 0;
var MAX_HOUR = 23;
function parseNum(s, min, max2) {
  if (!/^\d+$/.test(s)) throw new Error(`不是数字：${s}`);
  const n = Number(s);
  if (n < min || n > max2) throw new Error(`超出范围：${s}`);
  return n;
}
function parseField(field, min, max2) {
  const f = field.trim();
  if (f === "") throw new Error("cron 字段不能为空");
  if (f === "*") return null;
  const out = /* @__PURE__ */ new Set();
  for (const part of f.split(",")) {
    const p = part.trim();
    if (p === "") throw new Error("cron 字段里有空项");
    const base = p.split("/")[0];
    const stepRaw = p.split("/")[1];
    const step = stepRaw === void 0 ? 1 : parseNum(stepRaw, 1, 999);
    let start2 = min;
    let end = max2;
    if (base !== "*") {
      if (base.includes("-")) {
        const [a, b] = base.split("-");
        start2 = parseNum(a, min, max2);
        end = parseNum(b, min, max2);
        if (start2 > end) throw new Error(`范围反了：${base}`);
      } else {
        start2 = parseNum(base, min, max2);
        end = start2;
      }
    }
    for (let n = start2; n <= end; n += step) out.add(n);
  }
  return [...out].sort((a, b) => a - b);
}
function parseCron(expr) {
  const parts = expr.trim().split(/\s+/);
  if (parts.length !== 5) throw new Error("cron 要 5 段：分 时 日 月 周");
  const minute = parseField(parts[0], 0, 59);
  if (minute === null || minute.length !== 1 || minute[0] !== 0) {
    throw new Error("分钟只能是 0（抢兑点都是整点）");
  }
  const hoursRaw = parseField(parts[1], MIN, MAX_HOUR);
  const hours = hoursRaw ?? Array.from({ length: MAX_HOUR + 1 }, (_, i) => i);
  const dom = parseField(parts[2], 1, 31);
  const month = parseField(parts[3], 1, 12);
  const dow = parseField(parts[4], 0, 7);
  return {
    hours,
    dom,
    month,
    dow: dow === null ? null : [...new Set(dow.map((d) => d === 7 ? 0 : d))].sort((a, b) => a - b)
  };
}
function dayMatches(spec, d) {
  if (spec.month !== null && !spec.month.includes(d.getMonth() + 1)) return false;
  const domOk = spec.dom === null || spec.dom.includes(d.getDate());
  const dowOk = spec.dow === null || spec.dow.includes(d.getDay());
  if (spec.dom !== null && spec.dow !== null) return domOk || dowOk;
  return domOk && dowOk;
}
function cronMatchesAt(expr, ms) {
  const spec = parseCron(expr);
  const d = new Date(ms);
  if (d.getMinutes() !== 0) return false;
  return dayMatches(spec, d) && spec.hours.includes(d.getHours());
}
function nextCronTargetMs(expr, nowMs) {
  const spec = parseCron(expr);
  const base = new Date(nowMs);
  for (let day = 0; day < 370; day += 1) {
    const d = new Date(base.getTime());
    d.setDate(base.getDate() + day);
    if (!dayMatches(spec, d)) continue;
    for (const h of spec.hours) {
      const t = new Date(d.getTime());
      t.setHours(h, 0, 0, 0);
      if (t.getTime() >= nowMs) return t.getTime();
    }
  }
  return null;
}

// src/mcloud/exchangePlans.ts
var STATE_PLANS = "mcloud.exchange.plans";
var STATE_PLAN_RUNS = "mcloud.exchange.plan.runs";
var STATE_PLANS_MIGRATED = "mcloud.exchange.plans.migrated";
function newPlanId() {
  return "p" + Date.now().toString(36) + Math.floor(Math.random() * 1e6).toString(36);
}
function loadPlans(store2) {
  const raw = (store2.getState(STATE_PLANS) ?? "").trim();
  if (raw === "") return [];
  try {
    const v = JSON.parse(raw);
    if (!Array.isArray(v)) return [];
    return v.filter((x) => {
      if (x === null || typeof x !== "object") return false;
      const p = x;
      return typeof p.id === "string" && typeof p.accountId === "string" && typeof p.prizeName === "string" && typeof p.cron === "string" && typeof p.leadSec === "number" && typeof p.until === "string" && typeof p.maxRuns === "number" && typeof p.enabled === "boolean";
    });
  } catch {
    return [];
  }
}
function savePlans(store2, plans, nowIso) {
  store2.setState(STATE_PLANS, JSON.stringify(plans), nowIso);
}
function planTarget(plan) {
  return plan.prizeId !== void 0 ? String(plan.prizeId) : plan.prizeName;
}
function enabledPlans(plans) {
  return plans.filter((p) => p.enabled);
}
function unionCron(plans) {
  const hours = /* @__PURE__ */ new Set();
  for (const p of plans) {
    if (!p.enabled) continue;
    try {
      for (const h of parseCron(p.cron).hours) hours.add(h);
    } catch {
    }
  }
  if (hours.size === 0) return "0 10 * * *";
  return "0 " + [...hours].sort((a, b) => a - b).join(",") + " * * *";
}
function maxLeadMs(plans) {
  let ms = 6e4;
  for (const p of plans) {
    if (!p.enabled) continue;
    ms = Math.max(ms, Math.min(300, Math.max(60, p.leadSec)) * 1e3);
  }
  return ms;
}
function nextPlanTargetMs(plans, afterMs) {
  let best = null;
  for (const p of plans) {
    if (!p.enabled) continue;
    const t = nextCronTargetMs(p.cron, afterMs);
    if (t === null) continue;
    if (best === null || t < best) best = t;
  }
  return best;
}
function duePlans(plans, targetMs) {
  return plans.filter((p) => {
    if (!p.enabled) return false;
    try {
      return cronMatchesAt(p.cron, targetMs);
    } catch {
      return false;
    }
  });
}
function fmtLocal(ms) {
  const d = new Date(ms);
  const p2 = (n) => String(n).padStart(2, "0");
  return `${p2(d.getMonth() + 1)}-${p2(d.getDate())} ${p2(d.getHours())}:00`;
}
function planSummary(store2, nowMs = Date.now()) {
  const plans = loadPlans(store2);
  const on = enabledPlans(plans);
  if (on.length === 0) return plans.length === 0 ? "还没设定时计划" : `${plans.length} 个计划（都未启用）`;
  const next = nextPlanTargetMs(on, nowMs);
  const head = `${on.length} 个计划`;
  return next === null ? head : `${head} · 下次 ${fmtLocal(next)}`;
}
function loadPlanRuns(store2) {
  const raw = (store2.getState(STATE_PLAN_RUNS) ?? "").trim();
  if (raw === "") return {};
  try {
    const v = JSON.parse(raw);
    return v !== null && typeof v === "object" ? v : {};
  } catch {
    return {};
  }
}
function planRuns(store2, planId) {
  return loadPlanRuns(store2)[planId] ?? 0;
}
function bumpPlanRuns(store2, planId, nowIso) {
  const all = loadPlanRuns(store2);
  all[planId] = (all[planId] ?? 0) + 1;
  store2.setState(STATE_PLAN_RUNS, JSON.stringify(all), nowIso);
}
function migrateLegacyPlan(store2, nowIso) {
  if (store2.getState(STATE_PLANS_MIGRATED) === "1") return false;
  if (loadPlans(store2).length > 0) {
    store2.setState(STATE_PLANS_MIGRATED, "1", nowIso);
    return false;
  }
  const target = (store2.getState("mcloud.exchange.schedule.plan") ?? "").trim();
  if (target === "") {
    store2.setState(STATE_PLANS_MIGRATED, "1", nowIso);
    return false;
  }
  const name = (store2.getState("mcloud.exchange.schedule.plan_name") ?? "").trim();
  const cron = (store2.getState("mcloud.exchange.schedule.cron") ?? "").trim() || "0 10 * * *";
  const leadSec = Number(store2.getState("mcloud.exchange.lead_sec") ?? "120") || 120;
  const until = (store2.getState("mcloud.exchange.schedule.until") ?? "").trim();
  const maxRuns = Number(store2.getState("mcloud.exchange.schedule.max_runs") ?? "0") || 0;
  const enabledKey = (store2.getState("mcloud.exchange.schedule.enabled") ?? "").trim();
  const enabled = enabledKey === "1" || enabledKey === "" && target !== "";
  let accounts = resolveTaskAccounts(store2, "兑换");
  if (accounts.length === 0) {
    accounts = loadAccounts(store2).filter((a) => a.disabled !== true).map((a) => a.id);
  }
  const plans = accounts.slice(0, 8).map((accountId) => ({
    id: newPlanId(),
    accountId,
    .../^\d+$/.test(target) ? { prizeId: Number(target) } : {},
    prizeName: name !== "" ? name : target,
    cron,
    leadSec,
    until,
    maxRuns,
    enabled
  }));
  store2.setState(STATE_PLANS_MIGRATED, "1", nowIso);
  if (plans.length === 0) return false;
  savePlans(store2, plans, nowIso);
  return true;
}

// src/mcloud/taskReadiness.ts
var H = globalThis.host ?? globalThis.host_api ?? {};
function enabledAccountCount(store2) {
  return loadAccounts(store2).filter((a) => a.disabled !== true).length;
}
function blockersFor(store2, task) {
  if (task === "兑换") {
    const plans = loadPlans(store2);
    if (plans.length === 0) return ["还没设定时计划"];
    const on = plans.filter((p) => p.enabled);
    if (on.length === 0) return ["所有计划都未启用"];
    const accounts = loadAccounts(store2);
    const bad = on.filter((p) => {
      const a = accounts.find((x) => x.id === p.accountId);
      return !a || a.disabled === true;
    });
    if (bad.length === on.length) return ["计划里的账号都不可用"];
    return [];
  }
  return enabledAccountCount(store2) === 0 ? ["还没有账号"] : [];
}

// src/mcloud/logRing.ts
var RING_KEY = "mcloud.log.ring";
var RING_MAX = 400;
function parse2(raw) {
  if (raw === "") return [];
  try {
    const v = JSON.parse(raw);
    if (!Array.isArray(v)) return [];
    return v.filter(
      (x) => typeof x === "object" && x !== null && typeof x.m === "string"
    );
  } catch {
    return [];
  }
}
function appendRing(store2, nowIso, level, msg) {
  const all = parse2(store2.getState(RING_KEY) ?? "");
  all.push({ t: nowIso, l: level, m: msg });
  const kept = all.length > RING_MAX ? all.slice(all.length - RING_MAX) : all;
  store2.setState(RING_KEY, JSON.stringify(kept), nowIso);
}

// src/mcloud/log.ts
function logBoth(store2, level, msg, nowIso) {
  switch (level) {
    case "debug":
      debug(msg);
      break;
    case "warn":
      warn(msg);
      break;
    case "error":
      error(msg);
      break;
    case "fatal":
      fatal(msg);
      break;
    case "start":
      start(msg);
      break;
    case "success":
      success(msg);
      break;
    case "fail":
      fail(msg);
      break;
    default:
      log(msg);
  }
  appendRing(store2, nowIso, level, msg);
}

// src/mcloud/tasks/stepLog.ts
function lineLevel(line) {
  if (line.includes("没成") && /还没有网页会话|上面「换票」|没有会话|未登录|90001|票没被认/.test(line)) {
    return "debug";
  }
  if (line.includes("没成")) return "fail";
  if (/需手动|手动完成|需前往|需要你|手动打开|这一步我们做不了/.test(line)) return "warn";
  if (/（只看）|快照|问状态|流水|本月排名|已开启|有效期|剩余|当前云朵|今日云朵|今日获得|登录账号|过期时间|已完成 \d+ 条|另有 \d+ 条/.test(
    line
  )) {
    return "info";
  }
  if (/已签到|已经签到/.test(line)) return "info";
  const gotReward = /(领到|领取到|获得|中奖|抽到|成功获得|兑换成功.*【)/.test(line) && !/未获得|没获得|没领到|未领到/.test(line);
  if (gotReward) return "success";
  if (/跳过|已经|已领|没有可|没有能|无需|今天|未开启|没开|不发|不领|没问过|走完|做完了|暂无|无待领取|无可领|无.*可领|都领过了/.test(line)) {
    return "debug";
  }
  return "info";
}
function lineText(line) {
  if (line.includes("还没有网页会话") || line.includes("上面「换票」")) {
    const m = /^(.+?)：没成 —— /.exec(line);
    if (m !== null) return `${m[1]}：跳过（没有网页会话）`;
  }
  return line;
}
function stepTally(steps) {
  const failed = steps.filter((x) => !x.ok && x.pending !== true);
  const hard = failed.filter(
    (x) => !/还没有网页会话|上面「换票」|没有会话|未登录|90001|票没被认/.test(x.note)
  ).length;
  return { bad: failed.length, hard };
}

// src/mcloud/tokenGate.ts
function exchangeOnce(deps, recipe) {
  const auth = loadAuthRaw(deps.store);
  if (auth === "") return { ok: false, note: "没有可换票的原生凭据 —— 先加一个账号" };
  const started = startExchange(deps.store, auth, deps.nowIso, recipe);
  if (!started.ok) return { ok: false, note: started.error ?? "这串登录信息用不了" };
  const taskDeps2 = {
    store: deps.store,
    transport: deps.transport,
    nowIso: deps.nowIso,
    baseUrl: deps.baseUrl
  };
  const r = drain(
    (d) => {
      const x = runExchangeStep(d, recipe);
      return { ok: x.ok, note: x.note, idle: x.done };
    },
    taskDeps2,
    { label: `换票（${recipe}）`, maxSteps: 4, deadlineMs: 3e4, startedAt: Date.now() }
  );
  return { ok: r.ok && jwtFresh(deps.store, Date.now(), recipe), note: r.note };
}

// src/mcloud/tasks/exchangeRun.ts
var BUDGET_MS = 48e4;
var DEADLINE_MS = BUDGET_MS - 1e4;
var PLAN_KEY = "mcloud.exchange.plan";
var STATE_SCHEDULE_PLAN = "mcloud.exchange.schedule.plan";
var DEFAULT_SCHEDULE_CRON = "0 10 * * *";
var STATE_SCHEDULE_UNTIL = "mcloud.exchange.schedule.until";
function untilMs(raw) {
  const t = raw.trim();
  if (t === "") return null;
  const m = /^(\d{4})-(\d{2})-(\d{2})(?:[T ](\d{2}):(\d{2}))?$/.exec(t);
  if (!m) return null;
  const d = new Date(
    Number(m[1]),
    Number(m[2]) - 1,
    Number(m[3]),
    Number(m[4] ?? "23"),
    Number(m[5] ?? "59"),
    59,
    0
  );
  return Number.isNaN(d.getTime()) ? null : d.getTime();
}
function scheduleUntilAllowed(store2, targetMs, untilOverride) {
  const raw = untilOverride !== void 0 ? untilOverride : store2.getState(STATE_SCHEDULE_UNTIL) ?? "";
  const until = untilMs(raw.trim());
  if (until !== null && targetMs > until) return { ok: false, note: "已过截止时间" };
  return { ok: true, note: "" };
}
function runExchange(deps, opts = {}) {
  const started = Date.now();
  const steps = [];
  const add = (name, ok, note, idle = false) => {
    steps.push({ name, ok, note, idle });
  };
  const ensureToken = () => {
    if (jwtFresh(deps.store, Date.now(), FALLBACK_RECIPE)) return true;
    const x = exchangeOnce(
      {
        store: deps.store,
        transport: deps.transport,
        nowIso: deps.nowIso,
        baseUrl: deps.baseUrl
      },
      FALLBACK_RECIPE
    );
    add("换票", x.ok, x.note);
    return x.ok && jwtFresh(deps.store, Date.now(), FALLBACK_RECIPE);
  };
  const finish = (ok, note, idle = false) => {
    if (opts.dryRun) {
      add("干跑", true, "只看了能不能兑，**一枪都没发**（真下单走「立即兑换」或定时那一场）", true);
    }
    return {
      ok,
      ...idle ? { idle: true } : {},
      note,
      detail: steps.map((x) => `${x.name}：${x.ok ? "" : "失败 "}${x.note}`),
      steps
    };
  };
  const targetHour = opts.targetHour !== void 0 ? opts.targetHour : targetHourOf(deps.store);
  const gate = planTargetWait(depsNowMs(deps), targetHour);
  if (!opts.dryRun && gate.needed && gate.ms > (opts.maxWaitMs ?? MAX_SPIN_MS)) {
    add(
      "时间闸",
      true,
      targetHour === null ? `还没到窗口（还差 ${Math.round(gate.ms / 1e3)}s）—— 本次零请求` : `不是 ${targetHour} 点那一场（还差 ${Math.round(gate.ms / 1e3)}s）—— 本次零请求`,
      true
    );
    return finish(
      true,
      `还没到抢兑时间（还差 ${Math.round(gate.ms / 1e3)}s），本次零请求`,
      true
    );
  }
  if (!ensureToken()) {
    return finish(false, "网页会话续期失败 —— 登录凭据可能已过期，去「账号」页重新登录一次");
  }
  try {
    const l = runExchangeList(deps);
    add("清单", l.ok, l.note);
  } catch (e) {
    add("清单", false, `抛异常 ${e instanceof Error ? e.message : String(e)}`);
  }
  const plan = opts.target ?? deps.store.getState(STATE_SCHEDULE_PLAN) ?? deps.store.getState(PLAN_KEY) ?? "";
  if (plan === "") {
    add("下单", true, "没有设定时兑换目标（在「定时抢兑」里选一件之后才会下单）", true);
    return finish(true, "没有设定时兑换目标（在「定时抢兑」里选一件之后才会下单）");
  }
  if (!deps.store.getState("mcloud.exchange.order.step")) {
    const s = startOrder(deps, plan);
    add("起单", s.ok, s.note);
    if (!s.ok) {
      return finish(false, s.note);
    }
  }
  const scopedId = scopedAccountOf(deps.store);
  const allAccounts = loadAccounts(deps.store);
  const accounts = scopedId === void 0 ? allAccounts : allAccounts.filter((a) => a.id === scopedId);
  const multi = accounts.length > 1;
  const snaps = /* @__PURE__ */ new Map();
  const offsets = [];
  let r;
  if (multi) {
    for (const acc of accounts) {
      selectAccount(deps.store, acc.id, deps.nowIso);
      if (!ensureToken()) {
        add(`准备 ${maskAuth(loadAuthRaw(deps.store) ?? "")}`, false, "网页会话续期失败");
        continue;
      }
      clearOrder(deps);
      startOrder(deps, plan);
      const a = drain(runOrderStep, deps, {
        label: "兑换",
        maxSteps: 8,
        deadlineMs: Math.min(DEADLINE_MS, started + 3e4),
        startedAt: started,
        // 停在 `order` 之前 —— 下单那一枪留给阶段 B（那时才压整点）
        more: () => currentStep(deps.store) !== "order" && currentStep(deps.store) !== null
      });
      snaps.set(acc.id, snapshotOrder(deps.store));
      add(
        `准备 ${maskAuth(loadAuthRaw(deps.store) ?? "")}`,
        a.ok,
        `${a.note}（已推到只差下单）`
      );
    }
  }
  const fireStart = Date.now();
  for (const acc of accounts) {
    if (multi) {
      const snap = snaps.get(acc.id);
      if (snap === void 0) continue;
      selectAccount(deps.store, acc.id, deps.nowIso);
      restoreOrder(deps.store, snap, deps.nowIso);
    }
    const one = drain(runOrderStep, deps, {
      label: "兑换",
      maxSteps: 10,
      deadlineMs: DEADLINE_MS,
      startedAt: started,
      // ⚠ 干跑：停在 `order` **之前** —— 一枪都不发
      ...opts.dryRun ? { more: () => currentStep(deps.store) !== "order" && currentStep(deps.store) !== null } : {}
    });
    if (multi) {
      offsets.push(`${maskAuth(loadAuthRaw(deps.store) ?? "")} +${Date.now() - fireStart}ms`);
      add(
        `下单 ${maskAuth(loadAuthRaw(deps.store) ?? "")}`,
        one.ok,
        `${one.note} · 离第一枪 ${Date.now() - fireStart}ms`
      );
    }
    if (!multi) {
      r = one;
      break;
    }
  }
  r ?? (r = { ok: true, idle: true, steps: 0, note: "没有账号（先登录一个）", notes: [] });
  if (multi) {
    add(
      "整点这一批",
      offsets.length > 0,
      offsets.length > 0 ? `串行发出，各账号离第一枪：${offsets.join("、")}` : "没有账号可发"
    );
  }
  for (const line of r.notes) {
    const i = line.indexOf("：");
    if (i < 0) add("兑换", true, line);
    else add(line.slice(0, i), true, line.slice(i + 1));
  }
  return finish(r.ok, `${r.note}（用时 ${((Date.now() - started) / 1e3).toFixed(1)}s）`);
}

// src/entries/exchange.ts
worker_hint("exclusive");
var store = makeStore();
var transport = makeTransport(LONG_REQUEST_TIMEOUT_MS);
if (migrateLegacyPlan(store, (/* @__PURE__ */ new Date()).toISOString())) {
  log("老库迁移：兑换的旧单计划已按账号拆成多条计划");
}
var bootAll = loadPlans(store);
var bootPlans = enabledPlans(bootAll);
var bootCron = bootPlans.length > 0 ? unionCron(bootPlans) : DEFAULT_SCHEDULE_CRON;
var bootLeadMs = maxLeadMs(bootPlans);
register_cron(
  "兑换",
  bootCron,
  (p) => {
    const startedMs = Date.now();
    const at = new Date(startedMs).toISOString();
    const targetMs = Date.parse(p.scheduledAt);
    if (!Number.isFinite(targetMs)) return;
    const plans = duePlans(loadPlans(store), targetMs);
    if (plans.length === 0) return;
    const targetHour = new Date(targetMs).getHours();
    const thisHour = hourKey(targetMs);
    const accounts = loadAccounts(store);
    let acted = 0;
    let skipped = 0;
    for (const plan of plans) {
      const acct = accounts.find((a) => a.id === plan.accountId);
      if (!acct || acct.disabled === true) {
        skipped += 1;
        logBoth(store, "debug", "计划「" + plan.prizeName + "」：账号已删除或停用，这一趟跳过", at);
        continue;
      }
      const untilGate = scheduleUntilAllowed(store, targetMs, plan.until);
      if (!untilGate.ok) {
        skipped += 1;
        continue;
      }
      const done = planRuns(store, plan.id);
      if (plan.maxRuns > 0 && done >= plan.maxRuns) {
        skipped += 1;
        continue;
      }
      const deps = depsForAccount(store, plan.accountId, p, transport);
      if (deps.store.getState(STATE_ORDERED_HOUR) === thisHour) {
        skipped += 1;
        continue;
      }
      const auth = runAuthRefresh(deps);
      if (!auth.skipped) {
        logBoth(store, auth.ok ? "debug" : "warn", `凭据续期：${auth.note}`, at);
      }
      const r = runExchange(deps, {
        maxWaitMs: BUDGET_MS - 1e4,
        targetHour,
        target: planTarget(plan)
      });
      if (r && !r.idle) bumpPlanRuns(store, plan.id, deps.nowIso);
      if (!r || r.idle) {
        store.recordRun({
          task: p.name,
          runs: p.runs,
          scheduledAt: p.scheduledAt,
          firedAt: p.firedAt,
          lateMs: p.lateMs,
          ok: true,
          note: r ? r.note : "这一趟没动手"
        });
        continue;
      }
      acted += 1;
      const who = acct.label || labelOf(store, plan.accountId);
      const note = who + " · " + plan.prizeName + "：" + r.note;
      appendRun(store, {
        task: "exchange",
        acct: plan.accountId,
        who,
        at,
        ms: Date.now() - startedMs,
        ok: r.ok,
        note,
        steps: r.steps
      });
      store.recordRun({
        task: p.name,
        runs: p.runs,
        scheduledAt: p.scheduledAt,
        firedAt: p.firedAt,
        lateMs: p.lateMs,
        ok: r.ok,
        note
      });
      const tag = `[${shortLabel(who)}]`;
      logBoth(store, "start", `${tag} 兑换 · ${plan.prizeName}`, at);
      let prev = "";
      const skippedSteps = [];
      for (const line of r.detail) {
        const shown = lineText(line);
        if (shown === prev) continue;
        prev = shown;
        if (shown.includes("跳过（没有网页会话）")) {
          skippedSteps.push(shown.split("：")[0] ?? shown);
          continue;
        }
        logBoth(store, lineLevel(shown), `${tag} ${shown}`, at);
      }
      if (skippedSteps.length > 0) {
        const head = skippedSteps.slice(0, 3).join("、");
        logBoth(
          store,
          "debug",
          `${tag} 跳过 ${skippedSteps.length} 项（没有网页会话）：${head}${skippedSteps.length > 3 ? " 等" : ""}`,
          at
        );
      }
      const { bad, hard } = stepTally(r.steps);
      const tail = bad === 0 ? `${plan.prizeName}：都成了` : hard === 0 ? `${plan.prizeName}：${bad} 项跳过（没有网页会话）` : `${plan.prizeName}：${hard} 项没成${bad > hard ? `、${bad - hard} 项跳过` : ""}`;
      logBoth(store, hard > 0 ? "warn" : "debug", `${tag} ${tail}`, at);
    }
    if (acted === 0) {
      logBoth(
        store,
        "debug",
        "这一趟没动手 —— " + plans.length + " 条计划都还没到该抢的时候（或这一场已经试过了）" + (skipped > 0 ? "，其中 " + skipped + " 条被跳过" : ""),
        at
      );
    }
  },
  {
    budget_ms: BUDGET_MS,
    // 提前量取所有计划里最大的那个；具体某条计划提前少一点也没关系，
    // `runExchange` 的时间闸会在窗口前等，不会提前发单。
    lead_ms: bootLeadMs,
    // 兑换是**插件页面管理的功能实例**：面板任务列表不显示它，配置全在云盘页。
    surface: "plugin",
    // 新装先暂停：等用户建好计划、点“启用”再跑。已有安装的 paused 由面板保留。
    paused: bootPlans.length === 0,
    manage_url: "#/plugin/mcloud",
    summary: planSummary(store),
    blockers: blockersFor(store, "兑换")
  }
);
debug(
  "mcloud-exchange：任务「兑换」（预算 " + BUDGET_MS / 1e3 + "s，独占线程）—— " + bootPlans.length + " 条计划，cron " + bootCron + "，提前 " + bootLeadMs / 1e3 + "s 醒"
);
