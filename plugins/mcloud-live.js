// 由 src/entries/live.ts 打包生成（panel-llrt 版）—— **不要手改这个文件**。
//
//   改代码：编辑 src/，然后 `npm run build:panelllrt`
//   mcloud-plugin 0.2.25 · 插件 `mcloud-live`

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
  try {
    void Promise.resolve(host.register_cron(name, expr, budget)).catch((e) => {
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
function ws_open(url, options) {
  return host.ws_open(url, options ?? {});
}
function ws_send(id, text2) {
  return host.ws_send(id, text2);
}
function ws_recv(id, timeout_ms) {
  return host.ws_recv(id, timeout_ms);
}
function ws_close(id) {
  return host.ws_close(id);
}

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
function fromHex(h) {
  const clean = h.replace(/[^0-9a-fA-F]/g, "");
  const out = new Uint8Array(clean.length >> 1);
  for (let i = 0; i < out.length; i += 1) {
    out[i] = parseInt(clean.slice(i * 2, i * 2 + 2), 16);
  }
  return out;
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
  let bits = 0;
  let acc = 0;
  let o = 0;
  for (let i = 0; i < clean.length; i += 1) {
    const v = B64.indexOf(clean[i]);
    if (v < 0) continue;
    acc = acc << 6 | v;
    bits += 6;
    if (bits >= 8) {
      bits -= 8;
      out[o] = acc >> bits & 255;
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
  const bits = bytes * 8;
  return { hi: Math.floor(bits / 4294967296) >>> 0, lo: bits >>> 0 };
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
  const bits = derRead(der, algo.end);
  if (bits.tag !== 3) throw new Error("RSA: BIT STRING 缺失");
  if (der[bits.start] !== 0) throw new Error("RSA: BIT STRING 有未用位");
  const inner = derRead(der, bits.start + 1);
  if (inner.tag !== 48) throw new Error("RSA: 公钥不是 SEQUENCE");
  const intAt3 = (p) => {
    const it = derRead(der, p);
    if (it.tag !== 2) throw new Error("RSA: 期望 INTEGER");
    let v = 0n;
    for (let i = it.start; i < it.end; i += 1) v = v << 8n | BigInt(der[i]);
    return { v, next: it.end };
  };
  const nn = intAt3(inner.start);
  const ee = intAt3(nn.next);
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
var MCLOUD_HOST = "https://m.mcloud.139.com";
var REFERER_SIGN_IN = `${MCLOUD_HOST}/mcloud/sign_in/index.html`;
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
var H5_APP_VERSION = "12.6.0.0";
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
    appversion: H5_APP_VERSION,
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
  const finish2 = (s, ok, note2) => {
    s.step = "done";
    s.finishedAt = deps.nowIso;
    s.ok = ok;
    s.note = note2;
    delete s.ssoToken;
    delete s.ts;
    deps.store.setState(exchangeKey(deps.store, recipe), JSON.stringify(s), deps.nowIso);
    return { ok, note: note2, done: true };
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
    return finish2(st, true, `④ 换到 deviceId（${got.deviceId.length} 字符，B 开头）—— 领取类接口现在能用了`);
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
    return finish2(st, false, "③ 前置状态丢了（SSO token 或时间戳）—— SSO token 不缓存，得从头再来");
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
  return finish2(
    st,
    true,
    `③ 换到 jwttoken（${p.value.jwt.length} 字符${p.value.mobile ? `，回显 ${maskMobile(p.value.mobile)}` : ""}）—— 约 ${Math.round(JWT_TTL_MS / 6e4)} 分钟后该续` + // ⚠ 主动说出"缺 deviceId"：不说的话，用户第一次用 `receive` 会看到
    // 「活动太火爆啦」，然后以为过会儿就好 —— 而它其实永远不会好（是参数问题）。
    ((sess?.deviceId ?? "") === "" && (loadSession(deps.store)?.deviceId ?? "") === "" ? "。⚠ 这份会话**没有 `deviceId`**：领取类接口（receive）会回 615，而它的文案是「活动太火爆啦」—— 那不是在限流，是缺参数。要修：在会话 JSON 里带上 `deviceId`（H5 那一份是 `B` + 88 字符 base64）" : "")
  );
}
function failStep(deps, st, note2) {
  st.step = "done";
  st.finishedAt = deps.nowIso;
  st.ok = false;
  st.note = note2;
  delete st.ssoToken;
  delete st.ts;
  deps.store.setState(exchangeKey(deps.store, st.recipe ?? FALLBACK_RECIPE), JSON.stringify(st), deps.nowIso);
  return { ok: false, note: note2, done: true };
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
  const usable = assertUsable(applied.raw);
  if (!usable.ok) return { ok: false, note: `没存下来：${usable.error}` };
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

// src/mcloud/scheduleCron.ts
var MIN = 0;
var MAX_HOUR = 23;
function parseNum(s, min, max) {
  if (!/^\d+$/.test(s)) throw new Error(`不是数字：${s}`);
  const n = Number(s);
  if (n < min || n > max) throw new Error(`超出范围：${s}`);
  return n;
}
function parseField(field, min, max) {
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
    let end = max;
    if (base !== "*") {
      if (base.includes("-")) {
        const [a, b] = base.split("-");
        start2 = parseNum(a, min, max);
        end = parseNum(b, min, max);
        if (start2 > end) throw new Error(`范围反了：${base}`);
      } else {
        start2 = parseNum(base, min, max);
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
  const hours2 = hoursRaw ?? Array.from({ length: MAX_HOUR + 1 }, (_, i) => i);
  const dom = parseField(parts[2], 1, 31);
  const month = parseField(parts[3], 1, 12);
  const dow = parseField(parts[4], 0, 7);
  return {
    hours: hours2,
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
function enabledPlans(plans) {
  return plans.filter((p) => p.enabled);
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

// src/mcloud/taskReadiness.ts
var H = globalThis.host ?? globalThis.host_api ?? {};
function call(name, ...args) {
  const fn = H[name];
  if (typeof fn !== "function") return;
  try {
    fn(...args);
  } catch {
  }
}
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
function accountLabels(store2, ids) {
  const all = loadAccounts(store2);
  const out = ids.map((id) => all.find((a) => a.id === id)?.label ?? id);
  if (out.length === 0) return "无";
  const head = out.slice(0, 3).join("、");
  return out.length > 3 ? head + " 等" : head;
}
function summaryFor(store2, task) {
  if (task === "兑换") return planSummary(store2);
  const n = enabledAccountCount(store2);
  if (n === 0) return "还没有账号";
  const ids = loadAccounts(store2).filter((a) => a.disabled !== true).map((a) => a.id);
  return n <= 3 ? n + " 个账号（" + accountLabels(store2, ids) + "）" : n + " 个账号";
}
var TASK_KEYS = {
  日常: "mcloud-daily/日常",
  兑换: "mcloud-exchange/兑换",
  直播: "mcloud-live/直播"
};
function setTaskSummary(task, text2) {
  call("set_job_summary", TASK_KEYS[task] ?? task, text2);
}
function setTaskBlockers(task, blockers) {
  call("set_job_blockers", TASK_KEYS[task] ?? task, blockers);
}

// src/mcloud/runLog.ts
var RUN_LOG_KEY = "mcloud.run.log";
var RUNS_PER_TASK = 20;
var MAX_STEPS = 40;
var MAX_NOTE = 300;
function clip(s, max = MAX_NOTE) {
  const t = typeof s === "string" ? s : String(s ?? "");
  return t.length <= max ? t : `…${t.slice(t.length - max + 1)}`;
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
function trimNote(name, note2) {
  let t = note2;
  for (const n of [name, name.split("（")[0] ?? name]) {
    const pre = `${n}：`;
    if (t.startsWith(pre)) {
      t = t.slice(pre.length);
      break;
    }
  }
  const lead = /^([^：]{2,12})：/.exec(t);
  if (lead !== null && name.includes(lead[1] ?? "")) t = t.slice(lead[0].length);
  t = t.replace(/^(第 )?\d+ 步(失败|走完) —— /, "");
  const raw = /^code=(-?\d+)(?: msg=(.*))?$/.exec(t.trim());
  if (raw !== null) {
    const msg = (raw[2] ?? "").trim();
    t = msg === "" ? `服务端没给原因（编号 ${raw[1]}）` : `服务端说：${msg}`;
  }
  t = t.replace(/（服务端回 \{[^）]*\}）/g, "（服务端确认了）");
  return t.trim();
}
var PLUMBING = ["换票", "凭据续期"];
function isPlumbing(name) {
  return PLUMBING.some((p) => name.startsWith(p));
}
var ONCE_HINTS = ["（要先在服务端那边开启）"];
function onceHint(line, seen) {
  for (const h of ONCE_HINTS) {
    if (!line.includes(h)) continue;
    if (seen.has(h)) return line.replace(h, "");
    seen.add(h);
  }
  return line;
}
function userLine(step) {
  if (isPlumbing(step.name) && step.ok) return null;
  const why = trimNote(step.name, step.note);
  if (/^(签到翻倍|复活卡)$/.test(step.name) && /(这个月|本月)/.test(why)) return null;
  if (step.pending === true) return `${step.name}：${why === "" ? "等服务端确认" : why}`;
  if (!step.ok) return `${step.name}：没成${why === "" ? "" : ` —— ${why}`}`;
  return `${step.name}：${why === "" ? "好了" : why}`;
}
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

// src/mcloud/session.ts
var STATE_BASE_URL = "mcloud.base_url";
function loadBaseUrl(store2) {
  const v = store2.getState(STATE_BASE_URL);
  if (!v) return "";
  const t = v.trim();
  if (!/^https?:\/\//.test(t)) return "";
  return t.replace(/\/+$/, "");
}

// src/mcloud/tasks/base.ts
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
function recordError(store2, task, note2, nowIso) {
  store2.setState(`${ERR_PREFIX}${task}`, `${note2} @ ${nowIso}`, nowIso);
}
function clearError(store2, task, nowIso) {
  store2.setState(`${ERR_PREFIX}${task}`, "", nowIso);
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

// src/mcloud/tokenGate.ts
var LoginExpiredError = class extends Error {
  constructor(message = "这一环要的票没被认（90001）") {
    super(message);
    this.name = "LoginExpiredError";
  }
};
var MAX_RESETS = 2;
function exchangeOnce(deps, recipe) {
  const auth = loadAuthRaw(deps.store);
  if (auth === "") return { ok: false, note: "没有可换票的原生凭据 —— 先加一个账号" };
  const started = startExchange(deps.store, auth, deps.nowIso, recipe);
  if (!started.ok) return { ok: false, note: started.error ?? "这串登录信息用不了" };
  const taskDeps3 = {
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
    taskDeps3,
    { label: `换票（${recipe}）`, maxSteps: 4, deadlineMs: 3e4, startedAt: Date.now() }
  );
  return { ok: r.ok && jwtFresh(deps.store, Date.now(), recipe), note: r.note };
}
function withTokenGate(deps, read, recipe = FALLBACK_RECIPE) {
  const doRefresh = deps.refreshToken ?? ((rp) => exchangeOnce(deps, rp));
  if (!jwtFresh(deps.store, Date.now(), recipe)) doRefresh(recipe);
  let resets = 0;
  for (; ; ) {
    try {
      return read();
    } catch (e) {
      if (!(e instanceof LoginExpiredError)) throw e;
      if (resets >= MAX_RESETS) {
        throw new Error("这个号读不到：换了两次票还是不认 —— 多半是登录信息坏了，去「账号」页重新登录一次");
      }
      resets++;
      doRefresh(recipe);
    }
  }
}

// src/mcloud/liveFlower.ts
var LIVE_ROOM_ORIGIN = "https://m.mcloud.139.com";
var USER_INFO_PATH = "/ycloud/liveRoomFeedback/user/info";
var JOURNALING_PATH = "/ycloud/visitlog/journaling";
var FIRST_PARTICIPATE_GIVE_PATH = "/ycloud/liveRoomFeedback/firstParticipate/give";
var DRAW_PATH = "/ycloud/liveRoomFeedback/redFlower/draw";
var QUERY_REVIVAL_FLOWERS_PATH = "/ycloud/liveRoomFeedback/redFlower/queryRevivalFlowers";
var LIVE_ROOM_MARKET_NAME = "National_LiveRoomFeedback";
var LIVE_ROOM_JOURNALING_SOURCE_ID = "1007";
var LIVE_ROOM_ACTIVITY_ID = "sign_in_3";
var LIVE_ROOM_APP_VERSION = "12.6.0.0";
var TO_SOURCE_ID = "001005";
var LIVE_ROOM_USER_AGENT = "Mozilla/5.0 (iPad; CPU OS 18_7 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) MCloudApp/12.6.0 iPad AppLanguage/zh-CN";
var CODE_KOULING_INVALID = 200108;
var CODE_KOULING_ALREADY_DRAWN = 200112;
var CODE_KOULING_EXPIRED = 200118;
var JOURNALING_EVENTS = [
  "National_LiveRoomFeedback_pv",
  "National_LiveRoomFeedback_client",
  "National_LiveRoomFeedback_client_isApp",
  "National_LiveRoomFeedback_login",
  "National_LiveRoomFeedback_flowerBtn"
];
function liveRoomPageReferer() {
  return `${LIVE_ROOM_ORIGIN}/portal/yunClound/index.html?path=${LIVE_ROOM_MARKET_NAME}&sourceid=${LIVE_ROOM_JOURNALING_SOURCE_ID}&targetSourceId=${TO_SOURCE_ID}`;
}
function liveRoomHeaders() {
  return {
    Accept: "*/*",
    activityid: LIVE_ROOM_ACTIVITY_ID,
    appversion: LIVE_ROOM_APP_VERSION,
    Origin: LIVE_ROOM_ORIGIN,
    Referer: liveRoomPageReferer(),
    "User-Agent": LIVE_ROOM_USER_AGENT
  };
}
function liveRoomUrl(path, base) {
  return `${rootOrDefault(base, LIVE_ROOM_ORIGIN)}${path}`;
}
function userInfoRequest(base) {
  return {
    url: liveRoomUrl(USER_INFO_PATH, base),
    method: "POST",
    // ⚠ 不带 content-type：真实现（`live_flower::user_info_request`）就没设它。
    // 模块文档那张表写着 application/json，但**代码才是实际发出去的**。
    headers: liveRoomHeaders(),
    body: "{}"
  };
}
function journalingRequest(optkeyword, base) {
  return {
    url: liveRoomUrl(JOURNALING_PATH, base),
    method: "POST",
    headers: {
      ...liveRoomHeaders(),
      "content-type": "application/x-www-form-urlencoded;charset=UTF-8"
    },
    body: `module=uservisit&optkeyword=${encodeURIComponent(optkeyword)}&sourceid=${LIVE_ROOM_JOURNALING_SOURCE_ID}&marketName=${LIVE_ROOM_MARKET_NAME}`
  };
}
function firstParticipateGiveRequest(base) {
  return {
    url: liveRoomUrl(FIRST_PARTICIPATE_GIVE_PATH, base),
    method: "POST",
    headers: liveRoomHeaders(),
    body: void 0
  };
}
function drawRequest(code, base) {
  return {
    url: `${liveRoomUrl(DRAW_PATH, base)}?code=${encodeURIComponent(code)}`,
    method: "POST",
    headers: liveRoomHeaders(),
    body: void 0
  };
}
function queryRevivalFlowersRequest(base) {
  return {
    url: liveRoomUrl(QUERY_REVIVAL_FLOWERS_PATH, base),
    method: "GET",
    headers: liveRoomHeaders(),
    body: void 0
  };
}
function intAt(o, k) {
  const v = o[k];
  if (typeof v === "number" && Number.isFinite(v)) return Math.trunc(v);
  if (typeof v === "string" && /^-?\d+$/.test(v.trim())) return Number(v.trim());
  return void 0;
}
function strAt(o, k) {
  const v = o[k];
  return typeof v === "string" && v !== "" ? v : void 0;
}
function classifyDraw(env) {
  if (!env || env.code === void 0) {
    return { kind: "unparsable", code: env?.code, message: env?.message ?? "响应解不出来" };
  }
  const base = { code: env.code, message: env.message ?? "" };
  switch (env.code) {
    case 0: {
      const r = env.result;
      if (r === null || typeof r !== "object" || Array.isArray(r)) {
        return { ...base, kind: "unparsable" };
      }
      const o = r;
      return {
        ...base,
        kind: "succeeded",
        result: {
          prizeId: intAt(o, "prizeId") ?? 0,
          flowerNum: intAt(o, "flowerNum") ?? 0,
          prizeName: strAt(o, "prizeName"),
          prizeType: intAt(o, "prizeType") ?? 0
        }
      };
    }
    case CODE_KOULING_ALREADY_DRAWN:
      return { ...base, kind: "already-drawn" };
    case CODE_KOULING_INVALID:
      return { ...base, kind: "invalid-kouling" };
    case CODE_KOULING_EXPIRED:
      return { ...base, kind: "kouling-expired" };
    default:
      return { ...base, kind: "other" };
  }
}
function drawLine(code, out) {
  switch (out.kind) {
    case "succeeded":
      return `口令「${code}」领到 ${out.result?.flowerNum ?? 0} 朵`;
    case "already-drawn":
      return `口令「${code}」没领到：已兑换过`;
    case "invalid-kouling":
      return `口令「${code}」没领到：口令无效`;
    case "kouling-expired":
      return `口令「${code}」没领到：本场已结束（口令是对的）`;
    case "other":
      return `口令「${code}」没领到：服务端回了没见过的码 ${out.code ?? "?"}${out.message ? `（${out.message}）` : ""}`;
    default:
      return `口令「${code}」没领到：响应解不出来`;
  }
}
function parseBalance(env) {
  const r = env?.result;
  if (r === null || typeof r !== "object" || Array.isArray(r)) return void 0;
  return intAt(r, "flowerCnt");
}

// src/mcloud/liveKouling.ts
var XIAOHONGSHU_KOULING_PROFILE_URL = "https://www.xiaohongshu.com/user/profile/568caf13aed7585949773b30";
function xiaohongshuProfileHeaders() {
  return {
    accept: "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8",
    // 空串不是“省一个头”：宿主 HTTP 客户端会自动补 `Accept-Encoding: gzip`，
    // 而小红书只要看到这个头就回 36 KB 空壳（`userPageData:{}`）；
    // 显式给空值才能压掉自动补头，拿到带笔记的完整页。
    "accept-encoding": "",
    "accept-language": "zh-CN,zh;q=0.9",
    referer: "https://www.xiaohongshu.com/",
    "user-agent": "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/128.0.0.0 Safari/537.36"
  };
}
function isKoulingHead(text2) {
  return /(?:复制)?口令\s*\d*\s*[：:]\s*$/.test(text2.trim());
}
function hasKouling(text2) {
  return /口令|小红花/.test(text2);
}
function isKoulingMarker(text2) {
  return /无随机/.test(text2);
}
var TRAILING_PUNCT = /[。．.！!？?~～、,，;；:：'"]+$/;
function trimKoulingTail(code) {
  let out = code.trim();
  for (; ; ) {
    const before = out;
    out = out.replace(TRAILING_PUNCT, "").trim();
    out = out.replace(/\s*无随机口令$/, "");
    out = out.replace(/(?:^|\s+)无随机$/, "");
    out = out.replace(/(?:^|\s+)无$/, "");
    out = out.trim();
    if (out === before) break;
  }
  return out;
}
var TRAILING_PARTICLE = /[呀啊吧呢嘛哦]+$/;
var QUESTION_WORDS = ["什么", "啥", "多少", "几", "哪个", "哪些"];
function isQuestion(code) {
  if (/[?？]/.test(code)) return true;
  const t = code.replace(TRAILING_PARTICLE, "");
  return QUESTION_WORDS.includes(t);
}
var SAME_LINE_PATTERN = /(?:复制)?口令\s*\d*\s*[：:]\s*(.*?)(?=\s+(?:复制)?口令\s*\d*\s*[：:]|无随机|$)/g;
function extractKouling(msg) {
  const result = [];
  const m2 = msg.match(/小红花口令[：:]\s*(.+)/);
  if (m2 && m2[1] !== void 0) result.push(m2[1].trim());
  const lines = msg.split(/\r?\n/);
  for (let i = 0; i < lines.length; i += 1) {
    const line = (lines[i] ?? "").trim();
    if (isKoulingHead(line)) {
      const next = lines[i + 1];
      if (next !== void 0) {
        const content = trimKoulingTail(next.trim());
        if (content && !/^(?:复制)?口令/.test(content)) {
          result.push(content);
        }
      }
      continue;
    }
    for (const match of line.matchAll(SAME_LINE_PATTERN)) {
      const code = trimKoulingTail((match[1] ?? "").trim());
      if (code) result.push(code);
    }
  }
  const m1 = msg.match(/口令是(.+)/);
  if (m1 && m1[1] !== void 0) result.push(m1[1].trim());
  if (result.length === 0 && /无随机/.test(msg)) {
    const parts = msg.replace(/无随机.*/, "").trim().split(/\s+/);
    for (const part of parts) {
      const v = part.match(/^\d+\s*[：:]\s*(.+)/);
      if (v && v[1] !== void 0) {
        const code = v[1].trim();
        if (code) result.push(code);
      }
    }
  }
  return [...new Set(result.map(trimKoulingTail))].filter((code) => code !== "" && !isQuestion(code));
}
var NAMED_ENTITIES = {
  amp: "&",
  apos: "'",
  gt: ">",
  lt: "<",
  nbsp: " ",
  quot: '"'
};
function decodeHtmlEntities(text2) {
  return text2.replace(/&(?:#(\d+)|#x([\da-f]+)|(\w+));/gi, (entity, decimal, hex, named) => {
    if (typeof decimal === "string" && decimal !== "") {
      const n = Number(decimal);
      return safeFromCodePoint(n, entity);
    }
    if (typeof hex === "string" && hex !== "") {
      return safeFromCodePoint(Number.parseInt(hex, 16), entity);
    }
    if (typeof named === "string") {
      return NAMED_ENTITIES[named.toLowerCase()] ?? entity;
    }
    return entity;
  });
}
function safeFromCodePoint(n, fallback) {
  if (!Number.isFinite(n) || n < 0 || n > 1114111) return fallback;
  try {
    return String.fromCodePoint(n);
  } catch {
    return fallback;
  }
}
function decodeJsonString(value) {
  try {
    return JSON.parse(`"${value}"`);
  } catch {
    return value.replace(/\\n/g, "\n").replace(/\\r/g, "\r").replace(/\\t/g, "	").replace(/\\"/g, '"').replace(/\\\\/g, "\\");
  }
}
function htmlToText(html) {
  return decodeHtmlEntities(
    html.replace(/<br\s*\/?\s*>/gi, "\n").replace(/<\/(?:a|article|div|h\d|li|p|section)>/gi, "\n").replace(/<[^>]+>/g, " ")
  ).replace(/[ \t]+/g, " ").replace(/\n\s+/g, "\n").trim();
}
function collectCandidateTexts(html) {
  const candidates = [];
  const seen = /* @__PURE__ */ new Set();
  const add = (value) => {
    const text2 = decodeHtmlEntities(value).replace(/\s+/g, " ").trim();
    if (!text2 || !hasKouling(text2) || seen.has(text2)) return;
    seen.add(text2);
    candidates.push(text2);
  };
  const jsonFieldPattern = /"(?:displayTitle|title|desc)"\s*:\s*"((?:\\.|[^"\\])*)"/g;
  for (const match of html.matchAll(jsonFieldPattern)) add(decodeJsonString(match[1] ?? ""));
  const elementPattern = /<(?:a|article|div|h\d|p)[^>]*>([\s\S]*?)<\/(?:a|article|div|h\d|p)>/gi;
  for (const match of html.matchAll(elementPattern)) add(htmlToText(match[1] ?? ""));
  const metaPattern = /<meta\b[^>]*(?:content=["']([^"']+)["'])[^>]*>/gi;
  for (const match of html.matchAll(metaPattern)) add(match[1] ?? "");
  for (const line of htmlToText(html).split(/\r?\n/)) add(line);
  return candidates;
}
function extractKoulingFromXiaohongshuHtml(html) {
  for (const candidate of collectCandidateTexts(html)) {
    const codes = extractKouling(candidate);
    if (codes.length > 0) return codes;
  }
  return [];
}

// src/mcloud/koulingPool.ts
var CYCLE_TTL_MS = 5 * 60 * 60 * 1e3;
var GATE_CLOSE_GRACE_MS = 10 * 60 * 1e3;
var VALID_ENOUGH = 2;
var MAX_SESSIONS = 12;
var SCRAPE_MIN_INTERVAL_MS = 180 * 1e3;
var XHS_MIN_INTERVAL_MS = 30 * 60 * 1e3;
var MAX_POOL_CODES = 200;
var KOULING_ANCHOR = "中国移动云盘";
var POOL_KEY = "mcloud.kouling.pool";
function emptyPool() {
  return {
    cycleAtMs: 0,
    codes: [],
    claims: {},
    lastLiveAtMs: null,
    gateClosedAtMs: null,
    lastXhsAtMs: null,
    lastScrapeAtMs: null,
    schedule: [],
    scheduleAtMs: null,
    scheduleAnchor: null,
    sessions: []
  };
}
function startCycle(pool, nowMs) {
  pool.cycleAtMs = nowMs;
  pool.codes = [];
  pool.claims = {};
  pool.lastLiveAtMs = null;
  pool.gateClosedAtMs = null;
}
function isStale(pool, nowMs) {
  if (pool.cycleAtMs === 0) {
    return false;
  }
  if (nowMs - pool.cycleAtMs > CYCLE_TTL_MS) return true;
  return pool.gateClosedAtMs !== null && nowMs - pool.gateClosedAtMs > GATE_CLOSE_GRACE_MS;
}
function closeOpenSession(pool) {
  const open = pool.sessions[pool.sessions.length - 1];
  if (open && open.endedAtMs === void 0) {
    open.endedAtMs = pool.lastLiveAtMs ?? pool.cycleAtMs;
  }
}
function normalize(pool, nowMs) {
  if (isStale(pool, nowMs)) {
    closeOpenSession(pool);
    startCycle(pool, nowMs);
  }
  if (pool.cycleAtMs === 0) pool.cycleAtMs = nowMs;
}
function noteProbe(pool, rooms, active, nowMs) {
  pool.schedule = rooms.slice();
  pool.scheduleAtMs = nowMs;
  noteGate(pool, active, nowMs);
  if (active) {
    const last = pool.sessions[pool.sessions.length - 1];
    const open = last !== void 0 && last.endedAtMs === void 0;
    if (!open) {
      pool.sessions.push({ startedAtMs: nowMs });
      while (pool.sessions.length > MAX_SESSIONS) pool.sessions.shift();
    }
  }
}
function noteGate(pool, live, nowMs) {
  if (live) {
    pool.lastLiveAtMs = nowMs;
    pool.gateClosedAtMs = null;
  } else if (pool.gateClosedAtMs === null) {
    pool.gateClosedAtMs = nowMs;
  }
}
function note(pool, values, source, nowMs) {
  normalize(pool, nowMs);
  let added = 0;
  for (const raw of values) {
    const value = raw.trim();
    if (value === "") continue;
    const existing = pool.codes.find((c) => c.value === value);
    if (existing !== void 0) continue;
    if (pool.codes.length >= MAX_POOL_CODES) break;
    pool.codes.push({
      value,
      state: "unknown",
      source,
      firstSeenMs: nowMs
    });
    added += 1;
  }
  return added;
}
function noteClaim(pool, account, code, verdict, nowMs) {
  normalize(pool, nowMs);
  const value = code.trim();
  if (value === "") return;
  const mine = pool.claims[account] ?? {};
  mine[value] = verdict;
  pool.claims[account] = mine;
  let state;
  let reason;
  switch (verdict) {
    case "claimed":
      state = "valid";
      reason = "已验证有效";
      break;
    case "alreadyClaimed":
      state = "valid";
      reason = "已验证有效（至少一个账号领过）";
      break;
    case "invalid":
      state = "invalid";
      reason = "口令无效";
      break;
    case "expired":
      state = "invalid";
      reason = "口令已过期";
      break;
    case "other":
      break;
  }
  if (state === void 0) return;
  const slot = pool.codes.find((c) => c.value === value);
  if (slot !== void 0) {
    const downgrade = slot.state === "valid" && state === "invalid";
    if (!downgrade) slot.state = state;
    slot.reason = reason;
    if (state === "valid" && slot.validAtMs === void 0) slot.validAtMs = nowMs;
    return;
  }
  if (pool.codes.length >= MAX_POOL_CODES) return;
  pool.codes.push({
    value,
    state,
    source: "manual",
    reason,
    firstSeenMs: nowMs,
    ...state === "valid" ? { validAtMs: nowMs } : {}
  });
}
function validCount(pool) {
  return pool.codes.filter((c) => c.state === "valid").length;
}
function stopScraping(pool) {
  return validCount(pool) >= VALID_ENOUGH;
}
function accountClaimed(pool, account, code) {
  const mine = pool.claims[account];
  return mine !== void 0 && Object.prototype.hasOwnProperty.call(mine, code.trim());
}
function codeState(pool, code) {
  return pool.codes.find((c) => c.value === code.trim())?.state;
}
function pickFor(pool, account) {
  const mine = pool.claims[account];
  return pool.codes.filter((c) => c.state !== "invalid").filter((c) => mine === void 0 || !Object.prototype.hasOwnProperty.call(mine, c.value)).map((c) => c.value);
}
function xhsReady(pool, nowMs) {
  return pool.lastXhsAtMs === null || nowMs - pool.lastXhsAtMs >= XHS_MIN_INTERVAL_MS;
}
function noteXhs(pool, nowMs) {
  pool.lastXhsAtMs = nowMs;
}
function xhsWaitMs(pool, nowMs) {
  if (pool.lastXhsAtMs === null) return 0;
  return Math.max(0, XHS_MIN_INTERVAL_MS - (nowMs - pool.lastXhsAtMs));
}
function accountKey(phone) {
  const p = phone.trim();
  if (p === "") return "default";
  return toHex(sha256(utf8(`mcloud.kouling.account:${p}`))).slice(0, 16);
}
function parsePool(raw) {
  if (!raw) return { pool: emptyPool(), reset: false };
  let v;
  try {
    v = JSON.parse(raw);
  } catch {
    return { pool: emptyPool(), reset: true };
  }
  if (v === null || typeof v !== "object" || Array.isArray(v)) {
    return { pool: emptyPool(), reset: true };
  }
  const o = v;
  const pool = emptyPool();
  pool.cycleAtMs = num(o["cycleAtMs"]) ?? 0;
  pool.codes = codeList(o["codes"]);
  pool.claims = claimsOf(o["claims"]);
  pool.lastLiveAtMs = num(o["lastLiveAtMs"]) ?? null;
  pool.gateClosedAtMs = num(o["gateClosedAtMs"]) ?? null;
  pool.lastXhsAtMs = num(o["lastXhsAtMs"]) ?? null;
  pool.lastScrapeAtMs = num(o["lastScrapeAtMs"]) ?? null;
  pool.schedule = roomsOf(o["schedule"]);
  pool.scheduleAtMs = num(o["scheduleAtMs"]) ?? null;
  pool.scheduleAnchor = typeof o["scheduleAnchor"] === "string" ? o["scheduleAnchor"] : null;
  pool.sessions = sessionsOf(o["sessions"]);
  return { pool, reset: false };
}
function serializePool(pool) {
  return JSON.stringify(pool);
}
function loadPoolFrom(store2) {
  return parsePool(store2.getState(POOL_KEY));
}
function loadFreshPool(store2, nowMs) {
  const got = loadPoolFrom(store2);
  normalize(got.pool, nowMs);
  return got;
}
function savePoolTo(store2, pool, nowIso) {
  store2.setState(POOL_KEY, serializePool(pool), nowIso);
}
function num(v) {
  return typeof v === "number" && Number.isFinite(v) ? Math.trunc(v) : void 0;
}
function str(v) {
  return typeof v === "string" ? v : "";
}
function codeList(v) {
  if (!Array.isArray(v)) return [];
  const out = [];
  for (const item of v) {
    if (item === null || typeof item !== "object") continue;
    const o = item;
    const value = str(o["value"]);
    if (value === "") continue;
    const state = o["state"];
    const code = {
      value,
      // 认不出的状态按 **unknown**（不是 valid）：保守方向是"还要再验一次"，
      // 而"当成有效"会让 stopScraping 提前为真。
      state: state === "valid" || state === "invalid" ? state : "unknown",
      source: sourceOf(o["source"]),
      firstSeenMs: num(o["firstSeenMs"]) ?? 0
    };
    const reason = str(o["reason"]);
    if (reason !== "") code.reason = reason;
    const validAt = num(o["validAtMs"]);
    if (validAt !== void 0) code.validAtMs = validAt;
    out.push(code);
  }
  return out;
}
function sourceOf(v) {
  return v === "danmaku" || v === "xiaohongshu" || v === "manual" || v === "auto" ? v : "auto";
}
function verdictOf(v) {
  return v === "claimed" || v === "alreadyClaimed" || v === "invalid" || v === "expired" || v === "other" ? v : void 0;
}
function claimsOf(v) {
  const out = {};
  if (v === null || typeof v !== "object" || Array.isArray(v)) return out;
  for (const [account, codes] of Object.entries(v)) {
    if (codes === null || typeof codes !== "object" || Array.isArray(codes)) continue;
    const mine = {};
    for (const [code, verdict] of Object.entries(codes)) {
      const v2 = verdictOf(verdict);
      if (v2 !== void 0) mine[code] = v2;
    }
    out[account] = mine;
  }
  return out;
}
function roomsOf(v) {
  if (!Array.isArray(v)) return [];
  const out = [];
  for (const item of v) {
    if (item === null || typeof item !== "object") continue;
    const o = item;
    out.push({
      roomId: num(o["roomId"]) ?? 0,
      title: str(o["title"]),
      status: num(o["status"]) ?? 0,
      expectStartTime: str(o["expectStartTime"]),
      anchorNickname: str(o["anchorNickname"])
    });
  }
  return out;
}
function sessionsOf(v) {
  if (!Array.isArray(v)) return [];
  const out = [];
  for (const item of v) {
    if (item === null || typeof item !== "object") continue;
    const o = item;
    const started = num(o["startedAtMs"]);
    if (started === void 0) continue;
    const s = { startedAtMs: started };
    const ended = num(o["endedAtMs"]);
    if (ended !== void 0) s.endedAtMs = ended;
    out.push(s);
  }
  return out;
}

// src/mcloud/tasks/liveRoomRules.ts
function accountOf(store2) {
  const raw = loadAuthRaw(store2);
  if (raw === "") return accountKey("");
  const seg = parseAuthorization(raw);
  return accountKey(seg?.phone ?? "");
}
function mergeKouling(sources) {
  const out = [];
  for (const raw of sources) {
    const code = raw.trim();
    if (code === "" || out.includes(code)) continue;
    out.push(code);
  }
  return out;
}
function claimVerdictOf(kind) {
  switch (kind) {
    case "succeeded":
      return "claimed";
    case "already-drawn":
      return "alreadyClaimed";
    case "invalid-kouling":
      return "invalid";
    case "kouling-expired":
      return "expired";
    default:
      return "other";
  }
}
function planDrawCodes(pool, account, humanCodes) {
  const picked = pickFor(pool, account);
  const out = [];
  for (const code of mergeKouling([...humanCodes, ...picked])) {
    if (codeState(pool, code) === "invalid") continue;
    if (accountClaimed(pool, account, code)) continue;
    out.push(code);
  }
  return out;
}

// src/mcloud/tasks/liveRoom.ts
var STATE_READING = "mcloud.live.reading";
var STATE_CODES = "mcloud.live.codes";
var STATE_LIVE_URL = "mcloud.live.url";
function loadLiveUrl(store2) {
  const v = store2.getState(STATE_LIVE_URL);
  return v && v.trim() ? v.trim() : XIAOHONGSHU_KOULING_PROFILE_URL;
}
var EMPTY_SHELL_HINT_CHARS = 5e3;
function runLiveRoom(deps) {
  const url = loadLiveUrl(deps.store);
  const nowMs = Date.parse(deps.nowIso);
  const loaded = loadPoolFrom(deps.store);
  const pool = loaded.pool;
  normalize(pool, nowMs);
  if (stopScraping(pool)) {
    return {
      ok: true,
      idle: true,
      note: `已经有 ${validCount(pool)} 条能用的口令了，不用再抓`
    };
  }
  if (!xhsReady(pool, nowMs)) {
    const mins = Math.ceil(xhsWaitMs(pool, nowMs) / 6e4);
    return {
      ok: true,
      idle: true,
      // 原因留给代码注释，日志只说"什么时候再看"
      note: `刚看过，${mins} 分钟后再看`
    };
  }
  noteXhs(pool, nowMs);
  savePoolTo(deps.store, pool, deps.nowIso);
  const res = callRawText(deps.transport, {
    url,
    method: "GET",
    headers: xiaohongshuProfileHeaders(),
    body: void 0
  });
  if (!res.ok) {
    const note3 = `抓小红书主页失败：${res.error ?? "未知失败"}`;
    recordError(deps.store, "live-room", note3, deps.nowIso);
    return { ok: false, note: note3 };
  }
  const codes = extractKoulingFromXiaohongshuHtml(res.text);
  const added = note(pool, codes, "xiaohongshu", nowMs);
  savePoolTo(deps.store, pool, deps.nowIso);
  const guess = res.chars < EMPTY_SHELL_HINT_CHARS;
  const reset = loaded.reset ? "（⚠ 盘上那份池解不出来，已按空池重开 —— 这一场的领取记录丢了）" : "";
  const poolLine = `池：新增 ${added} 条，有效 ${validCount(pool)} 条${stopScraping(pool) ? "（已够 2 条 ⇒ 停止抓取）" : ""}`;
  const note2 = codes.length ? `小红书主页：${codes.length} 条口令（${codes.join("、")}）· ${poolLine}` : `小红书主页：没读到口令（页面 ${res.chars} 字符）· ${poolLine}` + (guess ? ` —— 页面小得可疑，**猜**是被挡成了空壳（不是判据）` : ` —— 也可能是这期没发：空壳页与"没发"在解析层长得一样`) + reset;
  const reading = {
    at: deps.nowIso,
    url,
    codes,
    chars: res.chars,
    tookMs: res.tookMs,
    note: note2
  };
  deps.store.setState(STATE_READING, JSON.stringify(reading), deps.nowIso);
  clearError(deps.store, "live-room", deps.nowIso);
  return { ok: true, note: note2 };
}
function loadLiveCodes(store2) {
  return parseJson(store2.getState(STATE_CODES));
}
function clearLiveCodes(store2, nowIso) {
  store2.setState(STATE_CODES, "", nowIso);
}
function parseJson(raw) {
  if (!raw) return null;
  try {
    return JSON.parse(raw);
  } catch {
    return null;
  }
}

// src/mcloud/tasks/liveDraw.ts
var STATE_DRAW = "mcloud.live.draw";
function collectCodes(store2, nowMs = Date.now()) {
  const human = loadLiveCodes(store2)?.codes ?? [];
  const { pool } = loadFreshPool(store2, nowMs);
  return planDrawCodes(pool, accountOf(store2), human);
}
function loadDrawState(store2) {
  const raw = store2.getState(STATE_DRAW);
  if (!raw) return null;
  try {
    const v = JSON.parse(raw);
    return typeof v?.step === "string" ? v : null;
  } catch {
    return null;
  }
}
function stepRequest(s, base) {
  switch (s.step) {
    case "balance-before":
      return { request: userInfoRequest(base), label: "① 读余额（领之前）" };
    case "journaling": {
      const ev = JOURNALING_EVENTS[s.journalingSent];
      if (ev === void 0) return null;
      return { request: journalingRequest(ev, base), label: `② 埋点 ${s.journalingSent + 1}/5` };
    }
    case "first-participate":
      return { request: firstParticipateGiveRequest(base), label: "③ 首次参与惊喜奖" };
    case "draw": {
      const code = s.codes[s.drawIndex];
      if (code === void 0) return null;
      return {
        request: drawRequest(code, base),
        label: `④ 领口令「${code}」（${s.drawIndex + 1}/${s.codes.length}）`
      };
    }
    case "revival":
      return { request: queryRevivalFlowersRequest(base), label: "⑤ 复活花" };
    case "balance-after":
      return { request: userInfoRequest(base), label: "⑥ 再读余额（核对进账）" };
    default:
      return null;
  }
}
function runLiveDraw(deps, opts = {}) {
  const TASK = "live-room";
  void opts;
  let s = loadDrawState(deps.store);
  const codes = opts.codes ? [...opts.codes] : collectCodes(deps.store, Date.parse(deps.nowIso));
  if (!s || s.step === "done") {
    const done = s?.codes ?? [];
    const { pool } = loadFreshPool(deps.store, Date.parse(deps.nowIso));
    const account = accountOf(deps.store);
    const allClaimed = done.length > 0 && done.every((c) => accountClaimed(pool, account, c));
    if (codes.length === 0) {
      const claimCount = Object.keys(pool.claims[account] ?? {}).length;
      const why = `池里 ${pool.codes.length} 条 / 这个号能领 ${codes.length} 条 / 上一轮 ${done.length} 条 / 这个号已领过 ${claimCount} 条`;
      return allClaimed ? { ok: true, idle: true, note: `这一轮已跑完（${s?.summary ?? ""}）；口令没变，不重复领（${why}）` } : { ok: true, idle: true, note: `没有口令可领（${why}）—— 先抓主页或把口令填进输入框` };
    }
    if (s?.step === "done" && sameList(done, codes) && s.account === account) {
      return {
        ok: true,
        idle: true,
        note: `口令没变（${codes.length} 条都是这个号领过的），本次没再发请求；上次那轮：${s.summary ?? ""}`
      };
    }
    s = {
      step: "balance-before",
      journalingSent: 0,
      drawIndex: 0,
      codes,
      account,
      results: [],
      startedAt: deps.nowIso
    };
    deps.store.setState(STATE_DRAW, JSON.stringify(s), deps.nowIso);
  }
  const session = loadSession(deps.store);
  if (!session || !session.jwt) {
    const note2 = "领小红花要先有票 —— 上面「换票」那一步说了原因";
    recordError(deps.store, TASK, note2, deps.nowIso);
    return { ok: false, note: note2 };
  }
  const step = stepRequest(s, deps.baseUrl);
  if (!step) {
    s.step = "balance-after";
    deps.store.setState(STATE_DRAW, JSON.stringify(s), deps.nowIso);
    return { ok: true, note: "步骤下标越界，已跳到收尾" };
  }
  const read = () => {
    const cur = loadSession(deps.store);
    if (!cur || !cur.jwt) throw new LoginExpiredError("领小红花要先有票 —— 上面「换票」那一步说了原因");
    const out = callOnce(deps.transport, withSession(step.request, cur.jwt));
    if (out.env?.code === 90001) throw new LoginExpiredError();
    return out;
  };
  let res;
  try {
    res = withTokenGate(deps, read);
  } catch (e) {
    const note2 = e instanceof Error ? e.message : String(e);
    recordError(deps.store, TASK, note2, deps.nowIso);
    return { ok: false, note: note2 };
  }
  const before = s.results.length;
  const advance = judge(s, step.label, res, deps.nowIso);
  deps.store.setState(STATE_DRAW, JSON.stringify(advance.state), deps.nowIso);
  if (s.step === "draw" && advance.state.results.length > before) {
    const row = advance.state.results[advance.state.results.length - 1];
    if (row !== void 0) {
      const loaded = loadFreshPool(deps.store, Date.parse(deps.nowIso));
      noteClaim(loaded.pool, accountOf(deps.store), row.code, claimVerdictOf(row.kind), Date.parse(deps.nowIso));
      savePoolTo(deps.store, loaded.pool, deps.nowIso);
    }
  }
  if (!advance.ok) {
    recordError(deps.store, TASK, advance.note, deps.nowIso);
    return { ok: false, note: advance.note };
  }
  clearError(deps.store, TASK, deps.nowIso);
  return { ok: true, note: advance.note };
}
function withSession(req, jwt) {
  return {
    ...req,
    headers: { ...req.headers, showloading: "true", jwttoken: jwt }
  };
}
function judge(s, label, res, nowIso) {
  const st = { ...s, results: [...s.results] };
  if (res.env === void 0) {
    const why = res.error ?? "未知失败";
    return { state: st, ok: false, note: `${label} 失败：${why}` };
  }
  const biz = () => `${label}：服务端回 ${res.env?.code ?? "?"}${res.env?.message ? `（${res.env.message}）` : ""}`;
  switch (s.step) {
    case "balance-before": {
      if (res.env.code !== 0) return { state: st, ok: false, note: biz() };
      const v = parseBalance(res.env);
      if (v === void 0) {
        return { state: st, ok: false, note: `${label}：响应里没有 result.flowerCnt（余额）` };
      }
      st.balanceBefore = v;
      st.step = "journaling";
      return { state: st, ok: true, note: `${label}：${v} 朵 → 开始领` };
    }
    case "journaling": {
      st.journalingSent = s.journalingSent + 1;
      if (st.journalingSent >= JOURNALING_EVENTS.length) st.step = "first-participate";
      return {
        state: st,
        ok: true,
        note: res.env.code === 0 ? `${label} 完成` : `${biz()}（埋点失败不拦领取）`
      };
    }
    case "first-participate": {
      const code = res.env.code;
      st.firstParticipate = code === 0 ? "领到首次参与惊喜奖" : code === 200110 ? "已参与过（正常）" : `服务端回 ${code ?? "?"}`;
      st.firstParticipateNum = code === 0 ? classifyDraw(res.env).result?.flowerNum ?? 0 : void 0;
      st.firstParticipateCode = code;
      st.firstParticipateMessage = res.env.message ?? "";
      st.step = "draw";
      return { state: st, ok: true, note: `${label}：${st.firstParticipate}` };
    }
    case "draw": {
      const code = s.codes[s.drawIndex] ?? "";
      const out = classifyDraw(res.env);
      st.results.push({
        code,
        kind: out.kind,
        note: drawLine(code, out),
        flowerNum: out.result?.flowerNum ?? 0,
        prizeName: out.result?.prizeName,
        message: out.message,
        envCode: out.code
      });
      st.drawIndex = s.drawIndex + 1;
      if (st.drawIndex >= st.codes.length) st.step = "revival";
      return { state: st, ok: true, note: drawLine(code, out) };
    }
    case "revival": {
      const r = res.env.result;
      st.revival = typeof r === "number" ? r : void 0;
      st.step = "balance-after";
      return {
        state: st,
        ok: true,
        note: res.env.code === 0 ? `${label}：${st.revival ?? "?"} 朵（**不是**小红花余额）` : biz()
      };
    }
    case "balance-after": {
      if (res.env.code !== 0) return { state: st, ok: false, note: biz() };
      const v = parseBalance(res.env);
      if (v === void 0) {
        return { state: st, ok: false, note: `${label}：响应里没有 result.flowerCnt` };
      }
      st.balanceAfter = v;
      st.step = "done";
      st.finishedAt = nowIso;
      const gained = (st.balanceAfter ?? 0) - (st.balanceBefore ?? 0);
      const okCount = st.results.filter((r) => r.kind === "succeeded").length;
      st.summary = `${st.results.length} 条口令领到 ${okCount} 条，进账 ${gained} 朵（${st.balanceBefore} → ${st.balanceAfter}）`;
      return { state: st, ok: true, note: st.summary };
    }
    default:
      return { state: st, ok: true, note: "没有可推进的步骤" };
  }
}
function sameList(a, b) {
  return a.length === b.length && a.every((v, i) => v === b[i]);
}

// src/mcloud/liveGate.ts
var BASE_CMLIVE = "https://v.cmcc-cs.cn/cmlive";
var LIVE_ORIGIN = "https://v.cmcc-cs.cn";
var LIVE_USER_AGENT = "Mozilla/5.0 (iPad; CPU OS 18_5 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) MCloudApp/12.6.0 iPad AppLanguage/zh-CN";
var LIVE_CHANNEL_ID = "134";
var LIVE_SOURCE_ID = "017027";
var LIVE_TAB_ID_BENEFITS = 127;
var LIVE_TAB_ID_REPLAY = 129;
var STATUS_LIVE = 1;
var GATE_TAB_IDS = [LIVE_TAB_ID_BENEFITS, LIVE_TAB_ID_REPLAY];
function parseCmlive(body) {
  let v;
  try {
    v = JSON.parse(body);
  } catch (e) {
    return { ok: false, error: `直播侧响应不是 JSON：${e instanceof Error ? e.message : String(e)}` };
  }
  const code = v["rtnCode"];
  if (typeof code !== "string") {
    return {
      ok: false,
      error: `直播侧的 rtnCode 应当是**字符串**，实际是 ${code === void 0 ? "缺字段" : typeof code}（这一族用字符串码；服务端若真改成了数字码，这里必须报出来而不是兼容掉）`
    };
  }
  const msg = typeof v["rtnMsg"] === "string" ? v["rtnMsg"] : void 0;
  const env = { rtnCode: code, object: v["object"] };
  if (msg !== void 0) env.rtnMsg = msg;
  return { ok: true, value: env };
}
function cmliveOk(env) {
  return env.rtnCode === "0";
}
function cmliveLine(env) {
  return `rtnCode=${env.rtnCode}${env.rtnMsg ? `（${env.rtnMsg}）` : ""}`;
}
function objectOf(env, what) {
  if (!cmliveOk(env)) {
    return { ok: false, error: `${what}失败：${cmliveLine(env)}` };
  }
  const o = env.object;
  if (o === null || typeof o !== "object" || Array.isArray(o)) {
    return { ok: false, error: `${what}：object 不是对象（实际 ${o === null ? "null" : typeof o}）` };
  }
  return { ok: true, value: o };
}
function cmliveHeaders(jwt) {
  const h = {
    accept: "application/json, text/plain, */*",
    "x-requested-with": "XMLHttpRequest",
    "accept-language": "zh-CN,zh-Hans;q=0.9",
    origin: LIVE_ORIGIN,
    "user-agent": LIVE_USER_AGENT
  };
  if (jwt !== void 0 && jwt !== "") h["authorization"] = jwt;
  return h;
}
function gateSpecTokenRequest(authorization, base) {
  const req = specTokenRequest({ toSourceId: LIVE_SOURCE_ID }, authorization, base);
  return req;
}
function cmpassLoginRequest(ssoToken, base) {
  return {
    url: `${rootOrDefault(base, BASE_CMLIVE)}/api/user/auth/cmpassLoginByCmToken`,
    method: "POST",
    headers: { ...cmliveHeaders(), "content-type": "application/json" },
    body: JSON.stringify({ cmpassToken: ssoToken }),
    // ⚠ `v.cmcc-cs.cn` **只给 CBC 套件**（2026-10-14 实测：`ECDHE-RSA-AES128-SHA`）——
    //    rustls 不实现 CBC ⇒ 必须走平台 TLS。见 [`BuiltRequest::tls`] 与 F-030/F-031。
    tls: "native"
  };
}
function parseCmpassLogin(env) {
  if (!cmliveOk(env)) return { ok: false, error: `直播侧登录失败：${cmliveLine(env)}` };
  const raw = env.object;
  if (typeof raw !== "string" || raw === "") {
    return {
      ok: false,
      error: `cmpassLoginByCmToken 的 object 不是非空字符串（应当就是那串 JWT 本身），实际 ${typeof raw}`
    };
  }
  return { ok: true, value: raw };
}
function tabContentListRequest(tabId, jwt, base) {
  return {
    url: `${rootOrDefault(base, BASE_CMLIVE)}/api/channel/channel/tabContentList?tabId=${tabId}&contentType=0&pageNo=1&channelId=${LIVE_CHANNEL_ID}`,
    method: "GET",
    headers: cmliveHeaders(jwt),
    body: void 0,
    // 同一个主机（`v.cmcc-cs.cn`）⇒ 同一条理由：只给 CBC，必须平台 TLS。
    tls: "native"
  };
}
function userTokenRequest(jwt, base) {
  return {
    url: `${rootOrDefault(base, BASE_CMLIVE)}/api/user/customer/userToken`,
    method: "POST",
    headers: { ...cmliveHeaders(jwt), "content-type": "application/json" },
    body: JSON.stringify({}),
    tls: "native"
  };
}
function parseUserToken(body) {
  const env = parseCmlive(body);
  if (!env.ok) return { ok: false, error: env.error ?? "直播侧响应解不出来" };
  if (env.value === void 0) return { ok: false, error: "直播侧响应是空的" };
  const object = env.value.object;
  if (typeof object !== "object" || object === null || Array.isArray(object)) {
    return { ok: false, error: "userToken 的 object 不是对象" };
  }
  const o = object;
  const token = o["access_token"];
  if (typeof token !== "string" || token === "") {
    return { ok: false, error: "userToken 响应缺少 object.access_token" };
  }
  return {
    ok: true,
    value: {
      accessToken: token,
      expiresIn: typeof o["expires_in"] === "number" ? o["expires_in"] : 0
    }
  };
}
function roomBeatRequest(roomId, jwt, base) {
  return {
    url: `${rootOrDefault(base, BASE_CMLIVE)}/api/room/front/roomBeat`,
    method: "POST",
    headers: { ...cmliveHeaders(jwt), "content-type": "application/json" },
    body: JSON.stringify({ roomId: String(roomId), channelId: LIVE_CHANNEL_ID, sourceApp: 0 }),
    // 同一个主机（`v.cmcc-cs.cn`）⇒ 见 [`BuiltRequest.tls`]：只给 CBC，必须平台 TLS。
    tls: "native"
  };
}
function parseRoomBeat(body) {
  const env = parseCmlive(body);
  if (!env.ok) return { ok: false, error: env.error ?? "直播侧响应解不出来" };
  if (env.value === void 0) return { ok: false, error: "直播侧响应是空的" };
  const object = env.value.object;
  if (typeof object !== "object" || object === null || Array.isArray(object)) {
    return { ok: false, error: "roomBeat 的 object 不是对象" };
  }
  const o = object;
  const anchor = o["anchor"];
  const nick = typeof anchor === "object" && anchor !== null ? anchor["nickname"] ?? "" : "";
  const rawChat = o["chatRoomId"];
  return {
    ok: true,
    value: {
      status: typeof o["status"] === "number" ? o["status"] : -1,
      title: typeof o["title"] === "string" ? o["title"] : "",
      expectStartTime: typeof o["expectStartTime"] === "string" ? o["expectStartTime"] : "",
      anchorNickname: typeof nick === "string" ? nick : "",
      // ⚠ **空串也当"没有"**：空串进不了 WS（会拼出一个空的 `cmlive_…`）。
      chatRoomId: typeof rawChat === "string" && rawChat !== "" ? rawChat : void 0
    }
  };
}
function parseTabContentList(env) {
  const o = objectOf(env, "查房间列表");
  if (!o.ok || o.value === void 0) return { ok: false, error: o.error ?? "查房间列表失败" };
  const list = o.value["dataList"];
  if (list === null || list === void 0) return { ok: true, value: [] };
  if (!Array.isArray(list)) {
    return { ok: false, error: `dataList 不是数组（实际 ${typeof list}）` };
  }
  const rooms = [];
  for (const entry of list) {
    if (entry === null || typeof entry !== "object") continue;
    const room = entry["room"];
    if (room === null || typeof room !== "object" || Array.isArray(room)) continue;
    const r = room;
    rooms.push({
      id: intAt2(r, "id") ?? 0,
      title: strAt2(r, "title"),
      status: intAt2(r, "status") ?? -1,
      channelId: intAt2(r, "channelId") ?? 0,
      expectStartTime: strAt2(r, "expectStartTime"),
      anchorNickname: r["anchor"] !== null && typeof r["anchor"] === "object" ? strAt2(r["anchor"], "nickname") : ""
    });
  }
  return { ok: true, value: rooms };
}
function mergeRooms(groups) {
  const seen = /* @__PURE__ */ new Set();
  const out = [];
  for (const group of groups) {
    for (const room of group) {
      if (seen.has(room.id)) continue;
      seen.add(room.id);
      out.push(room);
    }
  }
  return out;
}
function koulingRooms(rooms) {
  return rooms.filter((r) => r.status === STATUS_LIVE && r.anchorNickname === KOULING_ANCHOR);
}
function toProbeRooms(rooms) {
  return rooms.map((r) => ({
    roomId: r.id,
    title: r.title,
    status: r.status,
    expectStartTime: r.expectStartTime,
    anchorNickname: r.anchorNickname
  }));
}
function gateReadingLine(rooms) {
  const live = rooms.filter((r) => r.status === STATUS_LIVE);
  const hit = koulingRooms(rooms);
  if (hit.length > 0) {
    return `正在听：${KOULING_ANCHOR} 开播了（${hit.map((r) => r.title || `#${r.id}`).join("、")}）`;
  }
  if (live.length === 0) return `现在没在听：谁都没开播（看到 ${rooms.length} 间房，全是预告/回放）`;
  return `现在没在听：有 ${live.length} 间在播，但不是「${KOULING_ANCHOR}」的（${live.map((r) => r.anchorNickname || "无名").join("、")}）`;
}
function intAt2(o, k) {
  const v = o[k];
  if (typeof v === "number" && Number.isFinite(v)) return Math.trunc(v);
  if (typeof v === "string" && /^-?\d+$/.test(v.trim())) return Number(v.trim());
  return void 0;
}
function strAt2(o, k) {
  const v = o[k];
  return typeof v === "string" ? v : "";
}

// src/mcloud/tasks/liveGateTask.ts
var STATE_GATE = "mcloud.live.gate";
var STATE_READING2 = "mcloud.live.gate.reading";
var STATE_GATE_BASE = "mcloud.live.gate_base";
function loadGateBase(store2) {
  const v = store2.getState(STATE_GATE_BASE);
  return v && v.trim() ? v.trim().replace(/\/+$/, "") : void 0;
}
function loadGateState(store2) {
  return parseJson2(store2.getState(STATE_GATE));
}
function readGateReading(store2) {
  return parseJson2(store2.getState(STATE_READING2));
}
function runLiveGate(deps) {
  const base = loadGateBase(deps.store);
  const nowMs = Date.parse(deps.nowIso);
  const prev = loadGateState(deps.store);
  const st = prev && prev.step !== "done" ? prev : { step: "handshake", startedAt: deps.nowIso };
  if (st.step === "handshake") return handshake(deps, st, base);
  return roomsStep(deps, st, base, nowMs);
}
function handshake(deps, st, base) {
  const TASK = "live-gate";
  const auth = loadAuthRaw(deps.store);
  if (auth === "") {
    const note2 = "这个号还没有原生凭据 —— 先在账号页把它加上";
    recordError(deps.store, TASK, note2, deps.nowIso);
    return { ok: false, note: note2 };
  }
  const r1 = callRawText(deps.transport, gateSpecTokenRequest(auth, base));
  if (!r1.ok) return stepFail(deps, TASK, `① 换 SSO token 失败：${r1.error ?? "未知失败"}`);
  const sso = parseSsoToken(r1.text);
  if (!sso.ok || sso.value === void 0) {
    return stepFail(deps, TASK, `① 换 SSO token 失败：${sso.error ?? "响应里没有 data.token"}`);
  }
  const r2 = callRawText(deps.transport, cmpassLoginRequest(sso.value, base));
  if (!r2.ok) return stepFail(deps, TASK, `② 换直播侧 JWT 失败：${r2.error ?? "未知失败"}`);
  const env = parseCmlive(r2.text);
  if (!env.ok || env.value === void 0) {
    return stepFail(deps, TASK, `② 换直播侧 JWT 失败：${env.error ?? "响应解不出来"}`);
  }
  const jwt = parseCmpassLogin(env.value);
  if (!jwt.ok || jwt.value === void 0) {
    return stepFail(deps, TASK, `② 换直播侧 JWT 失败：${jwt.error ?? "object 不是 JWT"}`);
  }
  st.jwt = jwt.value;
  st.step = "rooms";
  deps.store.setState(STATE_GATE, JSON.stringify(st), deps.nowIso);
  clearError(deps.store, TASK, deps.nowIso);
  return {
    ok: true,
    // 日志里一行一步，这里只说结果（怎么换的、留不留盘是代码的事，见文件头）
    // 这只是**第一步**（换票）；下一步才去看有没有开播 —— 说清楚，别让人以为已经看过了
    note: "票换好了，下一步去看有没有开播"
  };
}
function roomsStep(deps, st, base, nowMs) {
  const TASK = "live-gate";
  const jwt = st.jwt;
  if (jwt === void 0 || jwt === "") {
    st.step = "handshake";
    deps.store.setState(STATE_GATE, JSON.stringify(st), deps.nowIso);
    return { ok: true, idle: true, note: "这一轮没有 JWT ⇒ 退回第一步（下一趟重新握手）" };
  }
  const groups = [];
  for (const tabId of GATE_TAB_IDS) {
    const res = callRawText(deps.transport, tabContentListRequest(tabId, jwt, base));
    if (!res.ok) {
      return dropJwtAndFail(deps, st, `③ 页签 ${tabId} 失败：${res.error ?? "未知失败"}`);
    }
    const env = parseCmlive(res.text);
    if (!env.ok || env.value === void 0) {
      return dropJwtAndFail(deps, st, `③ 页签 ${tabId} 失败：${env.error ?? "响应解不出来"}`);
    }
    const parsed = parseTabContentList(env.value);
    if (!parsed.ok || parsed.value === void 0) {
      return dropJwtAndFail(deps, st, `③ 页签 ${tabId} 失败：${parsed.error ?? "房间列表解不出来"}`);
    }
    groups.push(parsed.value);
  }
  const rooms = mergeRooms(groups);
  const hit = koulingRooms(rooms);
  const note2 = gateReadingLine(rooms);
  const { pool } = loadPoolFrom(deps.store);
  normalize(pool, nowMs);
  noteProbe(pool, toProbeRooms(rooms), hit.length > 0, nowMs);
  savePoolTo(deps.store, pool, deps.nowIso);
  const reading = {
    at: deps.nowIso,
    active: hit.length > 0,
    liveCount: rooms.filter((r) => r.status === 1).length,
    rooms: toProbeRooms(rooms),
    note: note2,
    target: KOULING_ANCHOR
  };
  deps.store.setState(STATE_READING2, JSON.stringify(reading), deps.nowIso);
  delete st.jwt;
  st.rooms = rooms;
  st.step = "done";
  st.finishedAt = deps.nowIso;
  st.note = note2;
  deps.store.setState(STATE_GATE, JSON.stringify(st), deps.nowIso);
  clearError(deps.store, TASK, deps.nowIso);
  return { ok: true, note: `${note2}（页签 ${GATE_TAB_IDS.join("/")}，共 ${rooms.length} 间房）` };
}
function dropJwtAndFail(deps, st, note2) {
  st.step = "handshake";
  delete st.jwt;
  deps.store.setState(STATE_GATE, JSON.stringify(st), deps.nowIso);
  return stepFail(deps, "live-gate", `${note2}（已丢弃 JWT，下一趟重新握手）`);
}
function stepFail(deps, task, note2) {
  recordError(deps.store, task, note2, deps.nowIso);
  return { ok: false, note: note2 };
}
function parseJson2(raw) {
  if (!raw) return null;
  try {
    return JSON.parse(raw);
  } catch {
    return null;
  }
}

// src/mcloud/msync.ts
var CMD_ACK = 0;
var CMD_HEARTBEAT = 1;
var CMD_DANMAKU = 2;
var CMD_PROVISION = 3;
var LIVE_APP_KEY = "cmos#ngvlcs";
var LIVE_DOMAIN = "cmosim.com";
var LIVE_AUTH_PREFIX = "$t$";
var LIVE_RESOURCE_PREFIX = "webim_";
var LIVE_DEVICE_INFO_ASCII = "830652084dda69b6b957e2c3a7bd9b18db65922e29051b1b342ccf15bebc0fdbd9c03439bcd8fff7ea316192960629f2";
var LIVE_IDENTIFICATION_PREFIX = "cmos_im2";
var PROVISION_OS_TYPE_JS = 7;
var PROVISION_VERSION = "23.0.2";
var PROVISION_APP_SIGN = "cmlive_cmos_js_sdk";
var PROVISION_DEVICE_NAME = "webim";
var WIRE_VARINT = 0;
var WIRE_FIXED64 = 1;
var WIRE_BYTES = 2;
var WIRE_FIXED32 = 5;
function readVarint(data, pos) {
  let value = 0n;
  let shift = 0n;
  for (let i = pos; i < data.length; i += 1) {
    const b = data[i];
    const idx = i - pos;
    if (idx >= 10) throw new Error(`varint 超过 10 字节（第 ${idx + 1} 字节 = 0x${b.toString(16)}）`);
    if (idx === 9 && b > 1) throw new Error(`varint 溢出（第 10 字节 = 0x${b.toString(16)}）`);
    value |= BigInt(b & 127) << shift;
    if ((b & 128) === 0) return { value, next: i + 1 };
    shift += 7n;
  }
  throw new Error("varint 被截断");
}
function scanProtoFields(data) {
  const out = [];
  let i = 0;
  while (i < data.length) {
    const key = readVarint(data, i);
    i = key.next;
    const num2 = Number(key.value >> 3n);
    const wire = Number(key.value & 7n);
    if (num2 === 0) throw new Error(`字段号为 0（key = ${key.value.toString()}）`);
    if (!Number.isSafeInteger(num2)) throw new Error(`字段号超出安全整数（key = ${key.value.toString()}）`);
    let value = 0n;
    let raw = new Uint8Array(0);
    if (wire === WIRE_VARINT) {
      const v = readVarint(data, i);
      i = v.next;
      value = v.value;
    } else if (wire === WIRE_BYTES) {
      const len = readVarint(data, i);
      i = len.next;
      const size = Number(len.value);
      if (!Number.isSafeInteger(size) || size < 0) throw new Error(`字段 ${num2} 的声明长度超出范围`);
      if (data.length - i < size) {
        throw new Error(`字段 ${num2} 声明长度 ${size}，实际只剩 ${data.length - i} 字节`);
      }
      raw = new Uint8Array(data.subarray(i, i + size));
      i += size;
    } else if (wire === WIRE_FIXED64) {
      if (data.length - i < 8) throw new Error(`字段 ${num2} 的 64 位定长值被截断`);
      i += 8;
    } else if (wire === WIRE_FIXED32) {
      if (data.length - i < 4) throw new Error(`字段 ${num2} 的 32 位定长值被截断`);
      i += 4;
    } else {
      throw new Error(`不支持的 wire type ${wire}（字段 ${num2}）`);
    }
    out.push({ num: num2, wire, value, raw });
  }
  return out;
}
function fieldsOf(data) {
  try {
    return scanProtoFields(data);
  } catch {
    return [];
  }
}
function lastBytesField(fields, num2) {
  let found;
  for (const f of fields) if (f.num === num2 && f.wire === WIRE_BYTES) found = f.raw;
  return found;
}
function lastVarintField(fields, num2) {
  let found;
  for (const f of fields) if (f.num === num2 && f.wire === WIRE_VARINT) found = f.value;
  return found;
}
function putVarint(out, value) {
  let v = value;
  while (v >= 0x80n) {
    out.push(Number(v & 0x7fn) | 128);
    v >>= 7n;
  }
  out.push(Number(v));
}
function putTag(out, num2, wire) {
  putVarint(out, BigInt(num2) << 3n | BigInt(wire));
}
function putScalar(out, num2, value) {
  if (value === 0n) return;
  putTag(out, num2, WIRE_VARINT);
  putVarint(out, value);
}
function putScalarForced(out, num2, value) {
  putTag(out, num2, WIRE_VARINT);
  putVarint(out, value);
}
function putString(out, num2, value) {
  if (value.length === 0) return;
  putBytesRaw(out, num2, utf8(value));
}
function putStringKeepingEmpty(out, num2, value) {
  putBytesRaw(out, num2, utf8(value));
}
function putBytesRaw(out, num2, body) {
  putTag(out, num2, WIRE_BYTES);
  putVarint(out, BigInt(body.length));
  for (let i = 0; i < body.length; i += 1) out.push(body[i]);
}
function putBytes(out, num2, body) {
  if (body.length === 0) return;
  putBytesRaw(out, num2, body);
}
function putMessage(out, num2, body) {
  putBytesRaw(out, num2, body);
}
function be64(value) {
  const out = new Uint8Array(8);
  let v = value;
  for (let i = 7; i >= 0; i -= 1) {
    out[i] = Number(v & 0xffn);
    v >>= 8n;
  }
  return out;
}
function secondsBytes(nowMs) {
  if (!Number.isFinite(nowMs)) throw new Error(`nowMs 必须是有限数：${nowMs}`);
  const sec = Math.floor(nowMs / 1e3);
  if (sec < 0 || sec > 4294967295) {
    throw new Error(`nowMs=${nowMs} 换算成秒后超出 4 字节（${sec}）—— 本协议只给 32 位秒`);
  }
  return new Uint8Array([sec >>> 24 & 255, sec >>> 16 & 255, sec >>> 8 & 255, sec & 255]);
}
function identificationBytes(nowMs, identTail) {
  const prefix = utf8(LIVE_IDENTIFICATION_PREFIX);
  if (prefix.length !== 8) {
    throw new Error(`identification 前缀必须是 8 字节 ASCII，实际 ${prefix.length} 字节`);
  }
  const tail = identTail ?? be64(BigInt(Math.floor(nowMs)));
  if (tail.length !== 8) throw new Error(`identification 的后半段必须是 8 字节，实际 ${tail.length}`);
  const out = new Uint8Array(16);
  out.set(prefix, 0);
  out.set(tail, 8);
  return out;
}
function encodeJid(phone, nowMs) {
  const out = [];
  putString(out, 1, LIVE_APP_KEY);
  putString(out, 2, `cmlive_c_${phone}`);
  putString(out, 3, LIVE_DOMAIN);
  putString(out, 4, `${LIVE_RESOURCE_PREFIX}${Math.floor(nowMs)}`);
  return new Uint8Array(out);
}
function encodeProvisionPayload(input, auth) {
  const out = [];
  putScalar(out, 1, BigInt(PROVISION_OS_TYPE_JS));
  putString(out, 2, PROVISION_VERSION);
  putString(out, 4, PROVISION_APP_SIGN);
  putScalarForced(out, 5, 0n);
  putScalarForced(out, 6, 0n);
  putStringKeepingEmpty(out, 9, input.deviceUuid);
  putString(out, 12, PROVISION_DEVICE_NAME);
  putString(out, 13, input.resource);
  putString(out, 14, auth);
  return new Uint8Array(out);
}
function encodeProvision(input, nowMs, identTail) {
  const auth = `${LIVE_AUTH_PREFIX}${input.accessToken}`;
  const out = [];
  putMessage(out, 2, encodeJid(input.phone, nowMs));
  putString(out, 3, auth);
  putScalarForced(out, 8, BigInt(CMD_PROVISION));
  putMessage(out, 9, encodeProvisionPayload(input, auth));
  putScalarForced(out, 10, 0n);
  putScalarForced(out, 11, 0n);
  putBytes(out, 13, utf8(LIVE_DEVICE_INFO_ASCII));
  putBytes(out, 14, secondsBytes(nowMs));
  putBytes(out, 15, identificationBytes(nowMs, identTail));
  return new Uint8Array(out);
}
function encodeHeartbeat(nowMs, identTail) {
  const out = [];
  putScalarForced(out, 8, BigInt(CMD_HEARTBEAT));
  putScalarForced(out, 11, 0n);
  putBytes(out, 13, utf8(LIVE_DEVICE_INFO_ASCII));
  putBytes(out, 14, secondsBytes(nowMs));
  putBytes(out, 15, identificationBytes(nowMs, identTail));
  return new Uint8Array(out);
}
function encodeAck(msgId, nowMs, identTail) {
  if (!/^(0|[1-9][0-9]*)$/.test(msgId)) {
    throw new Error(`encodeAck 的 msgId 必须是十进制串（无前导零），收到 ${JSON.stringify(msgId)}`);
  }
  const out = [];
  putScalarForced(out, 8, BigInt(CMD_ACK));
  putMessage(out, 9, ackPayload(BigInt(msgId)));
  putScalarForced(out, 11, 0n);
  putBytes(out, 13, utf8(LIVE_DEVICE_INFO_ASCII));
  putBytes(out, 14, secondsBytes(nowMs));
  putBytes(out, 15, identificationBytes(nowMs, identTail));
  return new Uint8Array(out);
}
function ackPayload(msgId) {
  const inner = [];
  putScalarForced(inner, 1, msgId);
  const out = [];
  putMessage(out, 1, new Uint8Array(inner));
  return new Uint8Array(out);
}
function decodeMsync(bytes) {
  let fields;
  try {
    fields = scanProtoFields(bytes);
  } catch {
    return { command: 0, msgId: void 0, texts: [], isMsync: false };
  }
  if (fields.length === 0) return { command: 0, msgId: void 0, texts: [], isMsync: false };
  const command = Number(lastVarintField(fields, 8) ?? 0n) & 255;
  const payload = lastBytesField(fields, 9);
  if (command === CMD_DANMAKU && payload !== void 0) {
    return { command, ...danmakuOf(payload), isMsync: true };
  }
  if (command === CMD_ACK && payload !== void 0) {
    return { command, msgId: ackMsgIdOf(payload), texts: [], isMsync: true };
  }
  return { command, msgId: void 0, texts: [], isMsync: true };
}
function danmakuOf(payload) {
  const texts = [];
  let msgId;
  for (const meta of fieldsOf(payload)) {
    if (meta.num !== 2) continue;
    if (meta.wire === WIRE_VARINT) {
      msgId = meta.value.toString();
      continue;
    }
    if (meta.wire !== WIRE_BYTES) continue;
    const inner = fieldsOf(meta.raw);
    for (const body of inner) {
      if (body.num !== 6 || body.wire !== WIRE_BYTES) continue;
      for (const content of fieldsOf(body.raw)) {
        if (content.num !== 4 || content.wire !== WIRE_BYTES) continue;
        const text2 = lastBytesField(fieldsOf(content.raw), 2);
        if (text2 !== void 0) texts.push(fromUtf8(text2));
      }
    }
    const id = msgIdOf(inner);
    if (id !== void 0) msgId = id;
  }
  return { msgId, texts };
}
function msgIdOf(messageFields) {
  const v = lastVarintField(messageFields, 1);
  if (v !== void 0) return v.toString();
  const s = lastBytesField(messageFields, 1);
  if (s === void 0) return void 0;
  const asText = fromUtf8(s);
  return /^[0-9]+$/.test(asText) ? asText : void 0;
}
function ackMsgIdOf(payload) {
  const fields = fieldsOf(payload);
  const direct = lastVarintField(fields, 2);
  if (direct !== void 0) return direct.toString();
  const one = lastBytesField(fields, 1);
  if (one === void 0) return void 0;
  const inner = lastVarintField(fieldsOf(one), 1);
  return inner !== void 0 && inner > 0n ? inner.toString() : void 0;
}

// src/mcloud/liveIm.ts
var BASE_VIDEO_IM = "https://videoim.cmcc-cs.cn:20103";
var LIVE_APPLY_VERSION = "v23.0.2";
function timestampSuffix10(timestamp) {
  return timestamp.length <= 10 ? timestamp : timestamp.slice(timestamp.length - 10);
}
var md5HexBytes = (bytes) => toHex(md5(bytes));
function systemMd5(deviceInfoHex, timestamp) {
  const device = fromHex(deviceInfoHex);
  const ts = utf8(timestampSuffix10(timestamp));
  const payload = new Uint8Array(device.length + ts.length);
  payload.set(device, 0);
  payload.set(ts, device.length);
  return md5HexBytes(payload);
}
function tokenMd5(accessToken, timestamp) {
  const head = accessToken.length > 10 ? accessToken.slice(0, 10) : accessToken;
  const tail = accessToken.length > 10 ? accessToken.slice(10) : "";
  return md5HexBytes(utf8(`Bearer ${head}${timestampSuffix10(timestamp)}${tail}`));
}
function applyRoomRequest(chatRoomId, userName, invoker, accessToken, timestamp, opts = {}) {
  const deviceInfoHex = opts.deviceInfoHex ?? LIVE_DEVICE_INFO_ASCII;
  return {
    url: `${rootOrDefault(opts.base, BASE_VIDEO_IM)}/v3/room/member/apply`,
    method: "POST",
    headers: {
      accept: "*/*",
      version: LIVE_APPLY_VERSION,
      timestamp,
      authorization: `Bearer ${accessToken}`,
      origin: LIVE_ORIGIN,
      "user-agent": LIVE_USER_AGENT,
      deviceinfo: deviceInfoHex,
      systemmd5: systemMd5(deviceInfoHex, timestamp),
      tokenmd5: tokenMd5(accessToken, timestamp),
      "content-type": "application/json"
    },
    body: JSON.stringify({ invoker, message: "", roomId: chatRoomId, userName }),
    // ⚠ 这个主机与直播侧**同一类问题**：2026-10-14 实测只给 CBC 套件
    //   （`ECDSA/ECDHE-RSA-AES128-SHA`），rustls 谈不下来 ⇒ 必须平台 TLS。
    tls: "native"
  };
}
function parseApplyRoom(body) {
  let v;
  try {
    v = JSON.parse(body);
  } catch (e) {
    return { ok: false, error: `进聊天室的响应不是 JSON：${e instanceof Error ? e.message : String(e)}` };
  }
  const code = v["code"];
  if (code !== 0) {
    const msg = typeof v["msg"] === "string" ? v["msg"] : "";
    return { ok: false, error: `进聊天室失败：code=${JSON.stringify(code)} msg=${msg || "(无)"}` };
  }
  const data = typeof v["data"] === "object" && v["data"] !== null ? v["data"] : {};
  return {
    ok: true,
    value: {
      appId: typeof data["appId"] === "string" ? data["appId"] : "",
      appKey: typeof data["appKey"] === "string" ? data["appKey"] : "",
      roomId: typeof data["roomId"] === "string" ? data["roomId"] : "",
      serverTimestamp: typeof v["timestamp"] === "number" ? v["timestamp"] : 0
    }
  };
}

// src/mcloud/liveAuto.ts
var ONE_HOUR_MS = 60 * 60 * 1e3;
var DAY_MS = 24 * ONE_HOUR_MS;
var BEIJING_OFFSET_MS = 8 * ONE_HOUR_MS;
function planDue(plan, nowMs) {
  return plan !== null && nowMs >= plan.wakeAtMs;
}
function planInWindow(plan, nowMs) {
  return plan !== null && nowMs >= plan.wakeAtMs && nowMs < plan.wakeAtMs + plan.windowMs;
}

// src/mcloud/tasks/liveAutoTask.ts
var STATE_WAKE_PLAN = "mcloud.live.wake_plan";
var SCHEDULE_TRUST_MS = 6 * ONE_HOUR_MS;
function loadWakePlan(store2) {
  const raw = store2.getState(STATE_WAKE_PLAN);
  if (!raw) return null;
  try {
    const v = JSON.parse(raw);
    return typeof v?.wakeAtMs === "number" && typeof v?.wakeAt === "string" ? v : null;
  } catch {
    return null;
  }
}
function wakeSleepMs(store2, nowMs, windowMs) {
  const plan = loadWakePlan(store2);
  if (plan === null) return void 0;
  if (!planInWindow(plan, nowMs) && planDue(plan, nowMs)) {
    return void 0;
  }
  if (planDue(plan, nowMs)) return void 0;
  void windowMs;
  return plan.wakeAtMs - nowMs;
}

// src/mcloud/liveWs.ts
var SOCKJS_OPEN = "o";
var SOCKJS_HEARTBEAT = "h";
var SOCKJS_CLOSE_PREFIX = "c[";
var SOCKJS_DATA_PREFIX = "a[";
function sockjsSendFrame(msync) {
  return `["${toBase64(msync)}"]`;
}
function sockjsDataFrames(text2) {
  return splitDataFrameJson(text2).map((b64) => fromBase64(b64));
}
function splitDataFrameJson(text2) {
  const json = text2.slice(SOCKJS_DATA_PREFIX.length - 1);
  let parsed;
  try {
    parsed = JSON.parse(json);
  } catch (e) {
    throw new Error(`SockJS 数据帧不是合法 JSON 数组：${String(e)}`);
  }
  if (!Array.isArray(parsed)) {
    throw new Error("SockJS 数据帧不是 JSON 数组");
  }
  const out = [];
  for (const item of parsed) {
    if (typeof item !== "string") {
      throw new Error("SockJS 数据帧的数组元素必须是 base64 字符串");
    }
    out.push(item);
  }
  return out;
}
var DEFAULT_HEARTBEAT_MS = 15e3;
var DEFAULT_MALFORMED_LIMIT = 8;
var CLOSE_REASON_SERVER = "服务端关闭帧 c[";
var CLOSE_REASON_MALFORMED = "连续 ";
var CLOSE_REASON_SOCKJS_HEARTBEAT = "收到 SockJS 心跳帧 h";
function createListener(o) {
  const heartbeatMs = o.heartbeatMs ?? DEFAULT_HEARTBEAT_MS;
  const malformedLimit = o.malformedLimit ?? DEFAULT_MALFORMED_LIMIT;
  const st = {
    openSeen: false,
    provisionSent: false,
    provisionAckSeen: false,
    heartbeatCount: 0,
    lastHeartbeatAtMs: void 0,
    consecutiveMalformed: 0,
    closeReason: void 0
  };
  const giveUp = (reason) => {
    st.closeReason = reason;
    return { outbound: [], texts: [], closed: reason };
  };
  const heartbeatFrame = (nowMs) => {
    st.heartbeatCount += 1;
    st.lastHeartbeatAtMs = nowMs;
    return sockjsSendFrame(encodeHeartbeat(nowMs));
  };
  const onFrameInner = (text2, nowMs) => {
    if (text2 === SOCKJS_HEARTBEAT) {
      if (o.sockjsHeartbeatReply !== void 0) {
        return {
          kind: "heartbeat",
          outbound: [o.sockjsHeartbeatReply],
          texts: [],
          closed: void 0
        };
      }
      return {
        kind: "heartbeat",
        outbound: [],
        texts: [],
        closed: `${CLOSE_REASON_SOCKJS_HEARTBEAT}：抓包 §四 4.1 全程零样本（业务帧太密，从未静默满 25 秒）⇒「h 该回什么」判为**未判定**，本层不猜、不回帧；请调用方决定（回 "h" / 回 [""] / 忽略并断开）`
      };
    }
    if (text2.startsWith(SOCKJS_CLOSE_PREFIX)) {
      return {
        kind: "closed",
        outbound: [],
        texts: [],
        closed: `${CLOSE_REASON_SERVER}…]：服务端关闭了这条连接，不再往上写任何东西`
      };
    }
    if (text2 === SOCKJS_OPEN) {
      st.openSeen = true;
      if (st.provisionSent) {
        return { kind: "open", outbound: [], texts: [], closed: void 0 };
      }
      const resource = `webim_${nowMs}`;
      const frame = sockjsSendFrame(
        encodeProvision(
          { accessToken: o.accessToken, resource, deviceUuid: o.deviceUuid, phone: o.phone },
          nowMs
        )
      );
      st.provisionSent = true;
      return { kind: "open", outbound: [frame], texts: [], closed: void 0 };
    }
    if (!text2.startsWith(SOCKJS_DATA_PREFIX)) {
      return { kind: "ignored", outbound: [], texts: [], closed: void 0 };
    }
    let pieces;
    try {
      pieces = splitDataFrameJson(text2);
    } catch {
      return { kind: "malformed", outbound: [], texts: [], closed: void 0 };
    }
    const outbound = [];
    const texts = [];
    let kind = "silent";
    let malformed = false;
    for (const piece of pieces) {
      const decoded = decodeMsync(fromBase64(piece));
      if (!decoded.isMsync) {
        malformed = true;
        continue;
      }
      const command = decoded.command;
      const msgId = decoded.msgId;
      const frameTexts = decoded.texts;
      switch (command) {
        case CMD_PROVISION: {
          kind = "provision_ack";
          st.provisionAckSeen = true;
          if (st.lastHeartbeatAtMs === void 0) {
            outbound.push(heartbeatFrame(nowMs));
          }
          break;
        }
        case CMD_DANMAKU: {
          kind = "danmaku";
          if (frameTexts.length > 0) {
            texts.push(...frameTexts);
          }
          if (msgId === void 0) {
            break;
          }
          outbound.push(sockjsSendFrame(encodeAck(msgId, nowMs)));
          break;
        }
        case CMD_ACK:
          break;
        case CMD_HEARTBEAT:
          break;
        default:
          break;
      }
    }
    if (malformed && kind === "silent") {
      kind = "malformed";
    }
    return { kind, outbound, texts, closed: void 0 };
  };
  const onFrame = (text2, nowMs) => {
    if (st.closeReason !== void 0) {
      return { outbound: [], texts: [], closed: st.closeReason };
    }
    const r = onFrameInner(text2, nowMs);
    if (r.kind === "malformed") {
      st.consecutiveMalformed += 1;
    } else {
      st.consecutiveMalformed = 0;
    }
    if (r.kind === "malformed" && malformedLimit !== 0 && st.consecutiveMalformed >= malformedLimit) {
      return giveUp(
        `${CLOSE_REASON_MALFORMED}${st.consecutiveMalformed} 帧解不开（上限 ${malformedLimit} 帧）：这条连接还在通，但说的不是我们的协议 ⇒ 请收工（别挂到超时）`
      );
    }
    if (r.kind === "closed") {
      st.closeReason = r.closed;
    }
    return { outbound: r.outbound, texts: r.texts, closed: r.closed };
  };
  const dueHeartbeat = (nowMs) => {
    if (st.closeReason !== void 0) {
      return [];
    }
    if (!st.provisionAckSeen) {
      return [];
    }
    if (st.lastHeartbeatAtMs !== void 0 && nowMs - st.lastHeartbeatAtMs < heartbeatMs) {
      return [];
    }
    return [heartbeatFrame(nowMs)];
  };
  const state = () => ({
    provisionSent: st.provisionSent,
    provisionAckSeen: st.provisionAckSeen,
    // 判据照 Rust：收到过 `o` **并且**收到过 PROVISION 的响应（cmd=3）才算进过房间。
    enteredRoom: st.openSeen && st.provisionAckSeen,
    heartbeatCount: st.heartbeatCount,
    lastHeartbeatAtMs: st.lastHeartbeatAtMs,
    closed: st.closeReason !== void 0
  });
  return { onFrame, state, dueHeartbeat };
}

// src/mcloud/tasks/liveListen.ts
var STATE_LISTEN = "mcloud.live.listen";
var STATE_WS_BASE = "mcloud.live.ws_base";
var STATE_WS_HEARTBEAT = "mcloud.live.ws_heartbeat_reply";
var STATE_LISTEN_ROOM = "mcloud.live.listen_room";
function loadListenRoom(store2) {
  const v = (store2.getState(STATE_LISTEN_ROOM) ?? "").trim();
  if (v === "") return void 0;
  const n = Number.parseInt(v, 10);
  return Number.isFinite(n) && n > 0 ? n : void 0;
}
var DEFAULT_WS_HEARTBEAT = "h";
function loadWsHeartbeat(store2) {
  const v = store2.getState(STATE_WS_HEARTBEAT);
  return v === null || v === void 0 ? DEFAULT_WS_HEARTBEAT : v;
}
var BASE_VIDEO_IM_WS = "wss://videoim.cmcc-cs.cn:20102";
var DEFAULT_LISTEN_SECONDS = 600;
var MIN_TASK_LISTEN_SECONDS = 5;
var MAX_TASK_LISTEN_SECONDS = 1800;
var IDLE_RECHECK_MS = 5 * 60 * 1e3;
var MAX_IDLE_RECHECK_MS = 12 * 60 * 60 * 1e3;
function idleCooldownMs(deps, nowMs) {
  const sleep = wakeSleepMs(deps.store, nowMs, loadListenSeconds(deps.store) * 1e3);
  if (sleep === void 0 || sleep <= 0) return IDLE_RECHECK_MS;
  return Math.min(sleep, MAX_IDLE_RECHECK_MS);
}
var READ_BURST_MS = 1600;
var RECV_SLICE_MS = 700;
var WS_OPEN_TIMEOUT_MS = 8e3;
var parseJson3 = (raw) => {
  if (!raw) return null;
  try {
    return JSON.parse(raw);
  } catch {
    return null;
  }
};
function loadListenState(store2) {
  return parseJson3(store2.getState(STATE_LISTEN));
}
function loadWsBase(store2) {
  const v = store2.getState(STATE_WS_BASE);
  return v && v.trim() ? v.trim().replace(/\/+$/, "") : BASE_VIDEO_IM_WS;
}
function liveWsUrl(base, serverId, sessionId) {
  return `${base.replace(/\/+$/, "")}/ws/${serverId}/${sessionId}/websocket`;
}
function sockjsSessionId(rand = Math.random) {
  const alpha = "abcdefghijklmnopqrstuvwxyz0123456789";
  let out = "";
  for (let i = 0; i < 8; i += 1) out += alpha[Math.floor(rand() * alpha.length)] ?? "a";
  return out;
}
function randomServerId(rand = Math.random) {
  return Math.floor(rand() * 1e3);
}
function wsHandshakeHeaders() {
  return { origin: LIVE_ORIGIN, "user-agent": LIVE_USER_AGENT };
}
function listenerIdle(st, nowMs) {
  if (st.nextCheckAtMs !== void 0 && nowMs < st.nextCheckAtMs) {
    const d = new Date(st.nextCheckAtMs);
    const pad = (n) => String(n).padStart(2, "0");
    const at = `${pad(d.getHours())}:${pad(d.getMinutes())}`;
    return `现在没在听，${at} 之后再去看`;
  }
  return void 0;
}
var current;
function hbPolicyOf(v) {
  return v === "h" ? "reply" : "none";
}
function parseHeartbeatReply(v) {
  return hbPolicyOf(v) === "reply" ? v : void 0;
}
function listenerFor(deps, st, nowMs, opts) {
  const handle = st.handle ?? -1;
  if (current !== void 0 && current.handle === handle && current.windowStartMs === st.windowStartMs && current.chatRoomId === opts.chatRoomId) {
    return current.listener;
  }
  const listener = createListener({
    accessToken: opts.accessToken,
    chatRoomId: opts.chatRoomId,
    phone: opts.phone,
    // ⚠ 这一格照抓包给**空串**（§5.5：字段在、长度为 0），不是毫秒串。
    deviceUuid: "",
    // ⚠⚠ `h`（SockJS 心跳）抓包**零样本**（§四 4.1），所以这一格是**实机试出来的**：
    //   2026-10-14 那场房间里安静满 25 秒 ⇒ 服务端真的发了 `h`；回一帧 `h`
    //   之后连接继续可用（读数与判据见 docs/findings.md F-032）。
    //   回 `[""]` 是**错的**：那是 SockJS 的**关闭**帧（抓包 §二 末帧）。
    sockjsHeartbeatReply: parseHeartbeatReply(loadWsHeartbeat(deps.store))
  });
  current = {
    handle,
    windowStartMs: st.windowStartMs,
    chatRoomId: opts.chatRoomId,
    listener
  };
  return listener;
}
function runLiveListen(deps) {
  const nowMs = Date.parse(deps.nowIso);
  const prev = loadListenState(deps.store);
  const windowMs = loadListenSeconds(deps.store) * 1e3;
  if (prev !== null && prev.step === "done") {
    const idle = listenerIdle(prev, nowMs);
    if (idle !== void 0) return { ok: true, note: idle };
  }
  const inWindow = prev !== null && (prev.step === "listen" || prev.step === "open");
  const stale = prev === null || prev.step === "done" || !inWindow && nowMs - prev.windowStartMs > windowMs;
  const st = !stale ? prev : {
    step: "handshake",
    windowStartMs: nowMs,
    frames: 0,
    danmakuFrames: 0,
    acks: 0,
    codes: [],
    startedAt: deps.nowIso
  };
  switch (st.step) {
    case "handshake":
      return handshakeStep(deps, st);
    case "rooms":
      return roomsStep2(deps, st, nowMs);
    case "userToken":
      return userTokenStep(deps, st);
    case "roomBeat":
      return roomBeatStep(deps, st, nowMs);
    case "apply":
      return applyStep(deps, st, nowMs);
    case "open":
      return openStep(deps, st, nowMs);
    case "listen":
      return listenStep(deps, st, nowMs, windowMs);
    default:
      return finish(deps, st, "窗口已结束");
  }
}
var STATE_LISTEN_SECONDS = "mcloud.live.listen_seconds";
var STATE_LISTEN_SECONDS_TASK = "mcloud.live.listen_seconds.task";
function loadListenSeconds(store2) {
  const taskRaw = store2.getState(STATE_LISTEN_SECONDS_TASK);
  const task = taskRaw ? Number.parseInt(taskRaw, 10) : Number.NaN;
  if (Number.isFinite(task) && task >= MIN_TASK_LISTEN_SECONDS) {
    return Math.min(task, MAX_TASK_LISTEN_SECONDS);
  }
  const raw = store2.getState(STATE_LISTEN_SECONDS);
  const n = raw ? Number.parseInt(raw, 10) : Number.NaN;
  return Number.isFinite(n) && n > 0 ? Math.min(n, 3600) : DEFAULT_LISTEN_SECONDS;
}
function persist(deps, st) {
  deps.store.setState(STATE_LISTEN, JSON.stringify(st), deps.nowIso);
}
function stepFail2(deps, st, note2) {
  recordError(deps.store, "live-listen", note2, deps.nowIso);
  persist(deps, st);
  return { ok: false, note: note2 };
}
function handshakeStep(deps, st) {
  const base = loadGateBaseOf(deps.store);
  const auth = loadAuthRaw(deps.store);
  if (auth === "") {
    return stepFail2(deps, st, "听弹幕要**原生凭据**（`Authorization`）：先去界面贴一份");
  }
  const r1 = callRawText(deps.transport, gateSpecTokenRequest(auth, base));
  if (!r1.ok) return stepFail2(deps, st, `① 换 SSO token 失败：${r1.error ?? "未知失败"}`);
  const sso = parseSsoToken(r1.text);
  if (!sso.ok || sso.value === void 0) {
    return stepFail2(deps, st, `① 换 SSO token 失败：${sso.error ?? "响应里没有 data.token"}`);
  }
  const r2 = callRawText(deps.transport, cmpassLoginRequest(sso.value, base));
  if (!r2.ok) return stepFail2(deps, st, `② 换直播侧 JWT 失败：${r2.error ?? "未知失败"}`);
  const env = parseCmlive(r2.text);
  if (!env.ok || env.value === void 0) {
    return stepFail2(deps, st, `② 换直播侧 JWT 失败：${env.error ?? "响应解不出来"}`);
  }
  const jwt = parseCmpassLogin(env.value);
  if (!jwt.ok || jwt.value === void 0) {
    return stepFail2(deps, st, `② 换直播侧 JWT 失败：${jwt.error ?? "object 不是 JWT"}`);
  }
  st.jwt = jwt.value;
  st.step = "rooms";
  persist(deps, st);
  clearError(deps.store, "live-listen", deps.nowIso);
  return { ok: true, note: "开始听了" };
}
function roomsStep2(deps, st, nowMs) {
  const base = loadGateBaseOf(deps.store);
  const jwt = st.jwt ?? "";
  if (jwt === "") return stepFail2(deps, st, "③ 没有票（上一拍的 JWT 不在）");
  const groups = [];
  for (const tabId of GATE_TAB_IDS) {
    const r = callRawText(deps.transport, tabContentListRequest(tabId, jwt, base));
    if (!r.ok) return stepFail2(deps, st, `③ 页签 ${tabId} 失败：${r.error ?? "未知失败"}`);
    const env = parseCmlive(r.text);
    if (!env.ok || env.value === void 0) {
      return stepFail2(deps, st, `③ 页签 ${tabId} 失败：${env.error ?? "响应解不出来"}`);
    }
    const parsed = parseTabContentList(env.value);
    if (!parsed.ok || parsed.value === void 0) {
      return stepFail2(deps, st, `③ 页签 ${tabId} 失败：${parsed.error ?? "房间列表解不出来"}`);
    }
    groups.push([...parsed.value]);
  }
  const merged = mergeRooms(groups);
  void nowMs;
  const forced = loadListenRoom(deps.store);
  const chosen = forced !== void 0 ? forced : koulingRooms(merged)[0]?.id;
  if (chosen === void 0) {
    st.step = "done";
    const coolMs = idleCooldownMs(deps, Date.parse(deps.nowIso));
    st.nextCheckAtMs = Date.parse(deps.nowIso) + coolMs;
    st.closedReason = "那个主播没在播";
    st.finishedAt = deps.nowIso;
    st.probedAt = deps.nowIso;
    st.note = forced !== void 0 ? `指定的房间 ${forced} 不在这次探到的 ${merged.length} 间房里（诊断口子）` : coolMs > IDLE_RECHECK_MS ? `现在没在听（看到 ${merged.length} 间房，但没有「${KOULING_ANCHOR}」的）—— 到下一场之后再去看（${hours(coolMs)}）` : `现在没在听（看到 ${merged.length} 间房，但没有「${KOULING_ANCHOR}」的）—— 隔 ${hours(coolMs)}再看（上次真探 ${deps.nowIso.slice(11, 19)}）`;
    persist(deps, st);
    return { ok: true, note: humanNote(st.note) };
  }
  st.roomId = chosen;
  st.step = "userToken";
  st.probedAt = deps.nowIso;
  persist(deps, st);
  return {
    ok: true,
    note: forced === void 0 ? `找到了：房间 ${st.roomId} 在播，接着去听` : `诊断口子：直接听指定的房间 ${st.roomId}，接着取 WS 凭据`
  };
}
function humanNote(note2) {
  return note2.replace(/门关着（合并后 (\d+) 间房）/g, "现在没在听（看到 $1 间房）").replace(/门关着/g, "现在没在听").replace(/门开着/g, "正在听").replace(/（(\d+) 分钟）/g, (_m, n) => `（${hours(Number(n) * 6e4)}）`);
}
function hours(ms) {
  const min = Math.round(ms / 6e4);
  if (min < 90) return `${min} 分钟`;
  const h = Math.round(min / 60);
  return `${h} 小时`;
}
function userTokenStep(deps, st) {
  const base = loadGateBaseOf(deps.store);
  const jwt = st.jwt ?? "";
  if (jwt === "") return stepFail2(deps, st, "③ 没有票（上一拍的 JWT 不在）");
  const r = callRawText(deps.transport, userTokenRequest(jwt, base));
  if (!r.ok) return stepFail2(deps, st, `③ 取 WS 凭据失败：${r.error ?? "未知失败"}`);
  const parsed = parseUserToken(r.text);
  if (!parsed.ok || parsed.value === void 0) {
    return stepFail2(deps, st, `③ 取 WS 凭据失败：${parsed.error ?? "响应里没有 access_token"}`);
  }
  st.accessToken = parsed.value.accessToken;
  st.step = "roomBeat";
  persist(deps, st);
  return { ok: true, note: `③ 取到 WS 凭据（${parsed.value.accessToken.length} 字符，${parsed.value.expiresIn}s）` };
}
function roomBeatStep(deps, st, nowMs) {
  void nowMs;
  const base = loadGateBaseOf(deps.store);
  const jwt = st.jwt ?? "";
  if (st.roomId === void 0 || jwt === "") {
    return stepFail2(deps, st, "④ 缺房间号或票（上一拍的状态不完整）");
  }
  const r = callRawText(deps.transport, roomBeatRequest(st.roomId, jwt, base));
  if (!r.ok) return stepFail2(deps, st, `④ roomBeat 失败：${r.error ?? "未知失败"}`);
  const parsed = parseRoomBeat(r.text);
  if (!parsed.ok || parsed.value === void 0) {
    return stepFail2(deps, st, `④ roomBeat 失败：${parsed.error ?? "响应解不出来"}`);
  }
  const beat = parsed.value;
  if (beat.chatRoomId === void 0) {
    st.step = "done";
    st.nextCheckAtMs = Date.parse(deps.nowIso) + idleCooldownMs(deps, Date.parse(deps.nowIso));
    st.closedReason = `房间 ${st.roomId} 没有聊天室（status=${beat.status}）`;
    st.finishedAt = deps.nowIso;
    st.note = st.closedReason;
    persist(deps, st);
    return { ok: true, note: st.note };
  }
  st.chatRoomId = beat.chatRoomId;
  st.step = "apply";
  persist(deps, st);
  return {
    ok: true,
    note: `房间 ${st.roomId}（${beat.title || "无标题"}）聊天室 ${st.chatRoomId}，接着进房间`
  };
}
function applyStep(deps, st, nowMs) {
  const auth = loadAuthRaw(deps.store);
  const parsed = parseAuthorization(auth);
  if (parsed === null) return stepFail2(deps, st, "⑤ 进房间要手机号（`userName` 用 `cmlive_c_<手机>`）");
  if (st.chatRoomId === void 0 || st.accessToken === void 0) {
    return stepFail2(deps, st, "⑤ 进房间缺聊天室号或 WS 凭据（上一拍的状态不完整）");
  }
  const userName = `cmlive_c_${parsed.phone}`;
  const req = applyRoomRequest(st.chatRoomId, userName, userName, st.accessToken, String(nowMs), {
    // 没配口子 ⇒ `applyRoomRequest` 自己用真主机（`BASE_VIDEO_IM`）
    base: loadImBase(deps.store)
  });
  const r = callRawText(deps.transport, req);
  if (!r.ok) return stepFail2(deps, st, `⑤ 进房间失败：${r.error ?? "未知失败"}`);
  const out = parseApplyRoom(r.text);
  if (!out.ok) return stepFail2(deps, st, `⑤ 进房间失败：${out.error}`);
  st.applied = true;
  st.step = "open";
  persist(deps, st);
  return {
    ok: true,
    note: `⑤ 进房间成功（appId=${out.value.appId || "?"}，回显聊天室 ${out.value.roomId || "?"}）`
  };
}
function openStep(deps, st, nowMs) {
  const auth = loadAuthRaw(deps.store);
  const parsed = parseAuthorization(auth);
  if (parsed === null) {
    return stepFail2(deps, st, "WS 那一步要手机号（PROVISION 的 `guid.name` 用它）—— 凭据解不出来");
  }
  if (st.chatRoomId === void 0 || st.jwt === void 0) {
    return stepFail2(deps, st, "开 WS 缺聊天室号或票（上一拍的状态不完整）");
  }
  const url = liveWsUrl(loadWsBase(deps.store), randomServerId(), sockjsSessionId());
  const opts = {
    headers: wsHandshakeHeaders(),
    timeout_ms: WS_OPEN_TIMEOUT_MS
  };
  if (url.startsWith("wss:")) opts.tls = "native";
  let handle;
  try {
    handle = deps.host.wsOpen(url, opts);
  } catch (e) {
    return stepFail2(deps, st, `开 WS 失败：${e instanceof Error ? e.message : String(e)}`);
  }
  st.handle = handle;
  const listener = listenerFor(deps, st, nowMs, {
    accessToken: st.jwt,
    chatRoomId: st.chatRoomId,
    phone: parsed.phone
  });
  let first;
  try {
    first = deps.host.wsRecv(handle, RECV_SLICE_MS);
  } catch (e) {
    try {
      deps.host.wsClose(handle);
    } catch {
    }
    current = void 0;
    st.handle = void 0;
    st.step = "done";
    st.closedReason = `句柄失效（${e instanceof Error ? e.message : String(e)}）`;
    st.finishedAt = deps.nowIso;
    persist(deps, st);
    return { ok: false, note: `WS 句柄失效：${st.closedReason}` };
  }
  if (first.type === "close" || first.type === "error") {
    deps.host.wsClose(handle);
    st.handle = void 0;
    st.step = "done";
    st.closedReason = first.type === "close" ? first.reason : first.message;
    st.finishedAt = deps.nowIso;
    persist(deps, st);
    return { ok: false, note: `WS 刚连上就断了：${st.closedReason ?? ""}` };
  }
  let sent = 0;
  if (first.type === "text") {
    const actions = listener.onFrame(first.data, nowMs);
    sent += flush(deps, handle, actions.outbound);
    st.frames += 1;
  }
  if (sent === 0) {
    persist(deps, st);
    return { ok: true, note: "WS 连上了，还没等到 SockJS 的 `o`（下一拍接着等）" };
  }
  st.lastHeartbeatAtMs = nowMs;
  st.step = "listen";
  persist(deps, st);
  return { ok: true, note: `WS 连上了（句柄 ${handle}），PROVISION 已发 —— 开始听弹幕` };
}
function listenStep(deps, st, nowMs, windowMs) {
  const handle = st.handle;
  if (handle === void 0) {
    current = void 0;
    st.step = "handshake";
    persist(deps, st);
    return { ok: true, note: "WS 句柄不在了（面板重启过？）⇒ 重新走一遍门" };
  }
  const auth = loadAuthRaw(deps.store);
  const parsed = parseAuthorization(auth);
  if (parsed === null || st.jwt === void 0 || st.chatRoomId === void 0) {
    return stepFail2(deps, st, "听的那一步缺凭据/票/聊天室号");
  }
  const listener = listenerFor(deps, st, nowMs, {
    accessToken: st.jwt,
    chatRoomId: st.chatRoomId,
    phone: parsed.phone
  });
  const deadline = Date.now() + READ_BURST_MS;
  const found = [];
  let endReason;
  while (Date.now() < deadline) {
    const sliceMs = Math.min(RECV_SLICE_MS, Math.max(120, deadline - Date.now()));
    let frame;
    try {
      frame = deps.host.wsRecv(handle, sliceMs);
    } catch (e) {
      endReason = `句柄失效（${e instanceof Error ? e.message : String(e)}）`;
      break;
    }
    if (frame.type === "close") {
      endReason = frame.reason;
      break;
    }
    if (frame.type === "error") {
      endReason = frame.message;
      break;
    }
    if (frame.type !== "text") continue;
    st.frames += 1;
    if (frame.data === "h") st.sockjsHb = (st.sockjsHb ?? 0) + 1;
    try {
      for (const bytes of sockjsDataFrames(frame.data)) {
        const cmd = decodeMsync(bytes).command;
        const k = String(cmd);
        st.kinds = { ...st.kinds ?? {}, [k]: ((st.kinds ?? {})[k] ?? 0) + 1 };
        if (cmd === CMD_ACK) st.serverAcks = (st.serverAcks ?? 0) + 1;
      }
    } catch {
      st.decodeFail = (st.decodeFail ?? 0) + 1;
    }
    const actions = listener.onFrame(frame.data, Date.now());
    flush(deps, handle, actions.outbound);
    if (actions.outbound.length > 0) st.acks += 1;
    if (actions.texts.length > 0) {
      st.danmakuFrames += 1;
      for (const text2 of actions.texts) {
        deps.onDanmaku?.(text2.replace(/\r?\n/g, "\\n").slice(0, 200));
        if (st.pendingHead === true) {
          const t = text2.trim();
          if (t !== "" && !isKoulingHead(t) && !isKoulingMarker(t)) {
            found.push(t);
          }
          st.pendingHead = false;
        }
        if (hasKouling(text2)) {
          for (const code of extractKouling(text2)) found.push(code);
        }
        if (isKoulingHead(text2)) st.pendingHead = true;
      }
    }
    if (actions.closed !== void 0) {
      if (actions.closed.includes("SockJS 心跳帧")) continue;
      endReason = actions.closed;
      break;
    }
  }
  if (found.length > 0) {
    const { pool } = loadFreshPool(deps.store, Date.now());
    note(pool, found, "danmaku", Date.now());
    savePoolTo(deps.store, pool, deps.nowIso);
    for (const code of found) if (!st.codes.includes(code)) st.codes.push(code);
  }
  const due = listener.dueHeartbeat(Date.now());
  if (due.length > 0) {
    flush(deps, handle, due);
    st.lastHeartbeatAtMs = Date.now();
  }
  if (endReason !== void 0) {
    try {
      deps.host.wsClose(handle);
    } catch {
    }
    current = void 0;
    st.handle = void 0;
    st.step = "done";
    st.closedReason = endReason;
    st.finishedAt = deps.nowIso;
    st.note = `弹幕连接结束：${endReason}（这一段里收到 ${st.frames} 帧、弹幕 ${st.danmakuFrames} 条）`;
    persist(deps, st);
    return { ok: true, note: st.note };
  }
  if (Date.now() - st.windowStartMs >= windowMs) {
    try {
      deps.host.wsClose(handle);
    } catch {
    }
    current = void 0;
    st.handle = void 0;
    st.step = "done";
    st.closedReason = "窗口到点";
    st.finishedAt = deps.nowIso;
    st.note = `窗口到点，主动断开（收到 ${st.frames} 帧、弹幕 ${st.danmakuFrames} 条、口令 ${st.codes.length} 条）`;
    persist(deps, st);
    return { ok: true, note: st.note };
  }
  persist(deps, st);
  const got = found.length > 0 ? `，本轮口令 ${found.join(" / ")}` : "";
  return {
    ok: true,
    // 与 `listenReadingLine` 同一套说法：只报弹幕/口令，帧数是排查用的
    note: `正在听（收到 ${st.danmakuFrames} 条弹幕，捡到 ${st.codes.length} 条口令${got}）`
  };
}
function flush(deps, handle, outbound) {
  let n = 0;
  for (const text2 of outbound) {
    try {
      deps.host.wsSend(handle, text2);
      n += 1;
    } catch {
      break;
    }
  }
  return n;
}
function finish(deps, st, note2) {
  current = void 0;
  if (st.handle !== void 0) {
    try {
      deps.host.wsClose(st.handle);
    } catch {
    }
    st.handle = void 0;
  }
  st.step = "done";
  st.finishedAt = deps.nowIso;
  st.note = note2;
  persist(deps, st);
  return { ok: true, note: note2 };
}
var STATE_IM_BASE = "mcloud.live.im_base";
function loadImBase(store2) {
  const v = store2.getState(STATE_IM_BASE);
  return v && v.trim() ? v.trim().replace(/\/+$/, "") : void 0;
}
function loadGateBaseOf(store2) {
  const v = store2.getState("mcloud.live.gate_base");
  return v && v.trim() ? v.trim().replace(/\/+$/, "") : void 0;
}

// src/mcloud/hostCap.ts
var STATE_CAP = "mcloud.host.native_tls";
var CAP_TTL_MS = 60 * 60 * 1e3;
function probeNativeTls(transport2) {
  const r = transport2({
    url: "https://127.0.0.1:9/",
    method: "GET",
    headers: {},
    body: void 0,
    tls: "native"
  });
  if (isFetchError(r)) {
    const msg = r.error ?? "";
    return { verdict: /legacy-tls/.test(msg) ? "no" : "yes", why: msg };
  }
  return { verdict: "yes", why: "" };
}
function hostNativeTls(store2, nowIso, transport2) {
  const raw = store2.getState(STATE_CAP);
  if (raw !== null && raw !== "") {
    try {
      const c = JSON.parse(raw);
      const at = Date.parse(c.at);
      const now = Date.parse(nowIso);
      if ((c.verdict === "yes" || c.verdict === "no") && Number.isFinite(at) && Number.isFinite(now) && now - at < CAP_TTL_MS) {
        return { verdict: c.verdict, why: typeof c.why === "string" ? c.why : "", fresh: true };
      }
    } catch {
    }
  }
  const p = probeNativeTls(transport2);
  const v = { at: nowIso, verdict: p.verdict, why: p.why };
  store2.setState(STATE_CAP, JSON.stringify(v), nowIso);
  return { ...p, fresh: false };
}
function capNote(verdict) {
  return verdict === "yes" ? "" : "这个面板二进制缺直播要的 TLS 支持（构建漏了 `legacy-tls`，它本该是默认带的）—— 这是构建缺陷，不是配置问题；换一个正常的面板构建。";
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
  let host3 = m[1].toLowerCase();
  const at = host3.lastIndexOf("@");
  if (at >= 0) host3 = host3.slice(at + 1);
  const colon = host3.lastIndexOf(":");
  if (colon >= 0) host3 = host3.slice(0, colon);
  return host3;
}
function sendsAuthorizationTo(rawUrl) {
  const host3 = hostOf(rawUrl);
  if (host3 === "") return false;
  return AUTH_DOMAINS.some((suffix) => host3.endsWith(suffix));
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
function makeHost() {
  return {
    wsOpen: (url, opts) => ws_open(url, opts),
    wsSend: (id, text2) => ws_send(id, text2),
    wsRecv: (id, timeoutMs) => ws_recv(id, timeoutMs),
    wsClose: (id) => ws_close(id)
  };
}
function forEachTaskAccount(base, task, payload, transport2, fn) {
  const mode = payload.manual === true || payload.lateMs < 0 ? "manual" : "auto";
  const targets = resolveTaskAccounts(rawStoreOf(base), task, mode);
  const out = [];
  for (const [i, acctId] of targets.entries()) {
    const one = scoped(rawStoreOf(base), acctId);
    const deps = taskDeps(one, transport2, payload);
    out.push({ acctId, result: fn(deps, acctId, i, targets.length) });
  }
  return out;
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

// src/entries/live.ts
var store = makeStore();
var transport = makeTransport(LONG_REQUEST_TIMEOUT_MS);
var host2 = makeHost();
var BUDGET_MS = 6e5;
function listenDeps(deps) {
  return {
    ...deps,
    host: host2,
    // 照 mcloud-sign：`[WS] 弹幕: ...` 只放 debug —— 用户能看到听到了什么，
    // 又不至于把正常日志刷成 info。
    onDanmaku: (text2) => logBoth(store, "debug", `弹幕：${text2}`, (/* @__PURE__ */ new Date()).toISOString())
  };
}
function collectOnce(deps, runStartMs) {
  const steps = [];
  const step = (name, fn) => {
    try {
      const r = fn();
      steps.push({ name, ok: r.ok, note: r.note, idle: r.idle === true });
      return r;
    } catch (e) {
      const note2 = `抛异常 ${e instanceof Error ? e.message : String(e)}`;
      steps.push({ name, ok: false, note: note2, idle: false });
      return { ok: false, note: note2 };
    }
  };
  const done = (ok, note2, stopped = false) => ({
    steps,
    detail: steps.map((s) => userLine(s)).filter((x) => x !== null),
    ok,
    note: note2,
    stopped
  });
  logBoth(store, "start", "直播口令：开始查开播", deps.nowIso);
  const auth = step("凭据续期", () => runAuthRefresh(deps));
  debug(`凭据续期：${auth.note}`);
  const cap = hostNativeTls(deps.store, deps.nowIso, deps.transport);
  if (cap.verdict === "no") {
    steps.push({ name: "这个面板能不能直播", ok: false, note: capNote(cap.verdict), idle: false });
    return done(false, capNote(cap.verdict), true);
  }
  const gateOut = drain(runLiveGate, deps, {
    label: "有没有开播",
    maxSteps: 3,
    deadlineMs: 4e4,
    startedAt: runStartMs
  });
  const gate = readGateReading(deps.store);
  const liveCount = gate === null ? 0 : gate.liveCount;
  if (!gateOut.ok) {
    steps.push({ name: "有没有开播", ok: false, note: `${gateOut.note}（继续试一次）`, idle: false });
  } else if (gate === null || gate.active !== true) {
    steps.push({
      name: "有没有开播",
      ok: true,
      idle: true,
      note: liveCount === 0 ? "今天没有直播（谁都没在播）" : `有 ${liveCount} 间在播，但没有一间是「${KOULING_ANCHOR}」的`
    });
    return done(true, "没在播 —— 不听，也不看小红书", true);
  } else {
    steps.push({
      name: "有没有开播",
      ok: true,
      idle: false,
      note: `「${KOULING_ANCHOR}」开播了（${liveCount} 间在播）`
    });
    logBoth(
      store,
      "start",
      `开播了，先看小红书，再听弹幕（窗口 ${loadListenSeconds(deps.store)} 秒）`,
      deps.nowIso
    );
  }
  logBoth(store, "start", "小红书：开始看口令发布方主页", deps.nowIso);
  const xhsAt = Date.now();
  const xhs = step("小红书主页口令", () => runLiveRoom(deps));
  logBoth(store, "debug", `小红书：这一步用了 ${Math.round((Date.now() - xhsAt) / 1e3)} 秒`, deps.nowIso);
  const poolNow = loadPoolFrom(deps.store).pool;
  const enough = stopScraping(poolNow);
  if (enough) {
    steps.push({
      name: "听弹幕",
      ok: true,
      idle: true,
      note: `手里已经有 ${validCount(poolNow)} 条能用的口令，不用再听`
    });
    return done(true, "口令够了，没再听");
  }
  let frames = 0;
  let listenNote = "";
  let listenPushed = false;
  let listenStop = "";
  const verified = /* @__PURE__ */ new Set();
  const ld = listenDeps(deps);
  const listenDeadline = runStartMs + Math.min(loadListenSeconds(deps.store) * 1e3, BUDGET_MS - 2e4);
  while (Date.now() < listenDeadline) {
    let r;
    try {
      r = runLiveListen(ld);
    } catch (e) {
      const note2 = `抛异常 ${e instanceof Error ? e.message : String(e)}`;
      steps.push({ name: "听弹幕", ok: false, note: note2, idle: false });
      listenPushed = true;
      break;
    }
    listenNote = r.note;
    if (!r.ok || r.idle) {
      steps.push({ name: "听弹幕", ok: r.ok, note: r.note, idle: r.idle === true });
      listenPushed = true;
      break;
    }
    frames++;
    const tickPet = loadPoolFrom(deps.store).pool;
    if (stopScraping(tickPet)) {
      listenStop = `已经有 ${validCount(tickPet)} 条能用的口令，不再听`;
      break;
    }
    const fresh = tickPet.codes.find(
      (c) => c.state !== "invalid" && c.state !== "valid" && c.firstSeenMs >= runStartMs && !verified.has(c.value)
    );
    if (fresh !== void 0) {
      verified.add(fresh.value);
      logBoth(store, "start", `听到口令「${fresh.value}」—— 立刻去验证（不等窗口）`, deps.nowIso);
      const v = drain((d) => runLiveDraw(d, { codes: [fresh.value] }), deps, {
        label: `验证「${fresh.value}」`,
        maxSteps: 8,
        // 一条口令的验证 = 读余额 → 领 → 回读余额，给 25 秒足够
        deadlineMs: 25e3,
        startedAt: Date.now(),
        gap: true
      });
      const nowPet = loadPoolFrom(deps.store).pool;
      const thisOne = nowPet.codes.find((c) => c.value === fresh.value);
      logBoth(
        store,
        v.ok ? "info" : "warn",
        `验证「${fresh.value}」：${thisOne?.state === "valid" ? "有效" : thisOne?.state === "invalid" ? "无效" : "没结论"} —— ${v.note}`,
        deps.nowIso
      );
      continue;
    }
    if (frames === 1 || frames % 5 === 0) {
      logBoth(
        store,
        "debug",
        `听弹幕中：第 ${frames} 拍（已听约 ${Math.round((Date.now() - runStartMs) / 1e3)} 秒）`,
        deps.nowIso
      );
    }
  }
  if (frames > 0 && !listenPushed) {
    steps.push({
      name: "听弹幕",
      ok: true,
      note: listenStop !== "" ? `${listenStop}（听了 ${frames} 拍）` : listenNote || `听了 ${frames} 轮`,
      idle: false
    });
  }
  const after = loadPoolFrom(deps.store).pool;
  const valid = validCount(after);
  const candidates = after.codes.filter((c) => c.state !== "invalid");
  if (candidates.length === 0) {
    return done(false, `没捡到能用的口令（听了 ${frames} 拍）`, true);
  }
  return done(
    true,
    valid > 0 ? `池里现在有 ${valid} 条已验证的口令` : `池里现在有 ${candidates.length} 条待验证口令（拿去领就会验）`
  );
}
function drawOnce(deps, runStartMs, onlyCodes) {
  const auth = runAuthRefresh(deps);
  const before = loadDrawState(deps.store);
  const r = drain(
    (d) => runLiveDraw(d, onlyCodes ? { codes: onlyCodes } : {}),
    deps,
    {
      label: "领小红花",
      maxSteps: 12,
      // 一次抽奖 = 一步，连着抽就是连发 ⇒ 步与步之间随机隔 0.5–2 秒（`deps.gapMs`）
      gap: true,
      deadlineMs: Math.max(15e3, runStartMs + BUDGET_MS - Date.now() - 3e3),
      startedAt: runStartMs
    }
  );
  const st = loadDrawState(deps.store);
  const freshRound = st !== null && st.startedAt === deps.nowIso;
  const rows = freshRound ? st?.results ?? [] : [];
  const steps = [
    {
      name: "凭据续期",
      ok: auth.ok,
      note: auth.note,
      idle: auth.idle === true || auth.skipped === true
    }
  ];
  steps.push(...rows.map((row) => {
    const head = `口令「${row.code}」`;
    const detail = row.note.startsWith(head) ? row.note.slice(head.length).trim() : row.note;
    return {
      name: head,
      ok: row.kind === "succeeded",
      note: detail,
      idle: false
    };
  }));
  const finalNote = freshRound && st?.step === "done" && st.summary ? st.summary : freshRound ? r.note : `本次没领 —— ${r.note}`;
  const noClaim = /没开|跳过|没有可|不用领|无需/.test(finalNote);
  steps.push({
    name: "领小红花",
    ok: r.ok,
    note: finalNote,
    // 没真跑一轮 ⇒ `idle`（界面上是灰色 info，不是成功绿灯）
    idle: freshRound ? r.idle === true && noClaim : true,
    pending: r.pending === true
  });
  return {
    steps,
    detail: steps.map((s) => userLine(s)).filter((x) => x !== null),
    ok: r.ok,
    note: finalNote
  };
}
register_cron(
  "直播",
  // 用户口径（2026-10-17）：「太频繁了，每天上午 10:30 / 下午 15:30 两次」。
  // 每 5 分钟一跳太密 —— 而开播时刻是整点粒度，两次足够；没在播时第一次
  // 调用就早退（零请求），见下面的「先验开播再决定要不要听」。
  "30 10,15 * * *",
  (p) => {
    const startedMs = Date.now();
    const at = new Date(startedMs).toISOString();
    const manualCodes = (loadLiveCodes(store)?.codes ?? []).map((c2) => c2.trim()).filter(Boolean);
    const manual = manualCodes.length > 0;
    if (manual) clearLiveCodes(store, at);
    let collected = null;
    let stopAll = false;
    const runs = forEachTaskAccount(store, "直播", p, transport, (deps, _acct, index) => {
      if (index === 0 && !manual) {
        const one = collectOnce(deps, startedMs);
        collected = one;
        if (one.stopped) stopAll = true;
      }
      if (stopAll) return { steps: [], detail: [], ok: true, note: "没在播，跳过" };
      return drawOnce(deps, startedMs, manual ? manualCodes : null);
    });
    if (runs.length === 0) return;
    const c = collected;
    const capOnly = c !== null && c.steps.length === 1 && !c.ok && c.steps[0]?.name === "这个面板能不能直播";
    if (c !== null && c.stopped) {
      const reason = c.note;
      if (capOnly) {
        logBoth(store, "error", c.detail[0] ?? c.note, at);
      } else {
        logBoth(store, "info", `这一趟不领 —— ${reason}；跳过 ${runs.length} 个账号`, at);
      }
      store.recordRun({
        task: p.name,
        runs: p.runs,
        scheduledAt: p.scheduledAt,
        firedAt: p.firedAt,
        lateMs: p.lateMs,
        ok: c.ok && !capOnly,
        note: c.note
      });
      setTaskBlockers("直播", blockersFor(store, "直播"));
      setTaskSummary("直播", summaryFor(store, "直播"));
      return;
    }
    const onceHints = /* @__PURE__ */ new Set();
    if (c !== null && c.detail.length > 0) {
      if (capOnly) {
        logBoth(store, "error", c.detail[0] ?? c.note, at);
      } else {
        logBoth(store, "start", "口令", at);
        let prev = "";
        for (const line of c.detail) {
          const shown = lineText(onceHint(line, onceHints));
          if (shown === prev) continue;
          prev = shown;
          logBoth(store, lineLevel(shown), shown, at);
        }
        const cBad = c.steps.filter((x) => !x.ok).length;
        logBoth(store, !c.ok || cBad > 0 ? "warn" : "success", `口令：${c.note}`, at);
      }
    }
    logBoth(store, "start", `直播 · ${runs.length} 个账号`, at);
    for (const one of runs) {
      const r = one.result;
      const who = labelOf(store, one.acctId);
      const note2 = runs.length > 1 ? `${who}：${r.note}` : r.note;
      appendRun(store, {
        task: "live",
        acct: one.acctId,
        who,
        at,
        ms: Date.now() - startedMs,
        ok: r.ok,
        note: note2,
        steps: r.steps
      });
      store.recordRun({
        task: p.name,
        runs: p.runs,
        scheduledAt: p.scheduledAt,
        firedAt: p.firedAt,
        lateMs: p.lateMs,
        ok: r.ok,
        note: note2
      });
      const tag = `[${shortLabel(who)}]`;
      logBoth(store, "start", tag, at);
      let prev = "";
      const skippedSteps = [];
      for (const line of r.detail) {
        const shown = lineText(onceHint(line, onceHints));
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
      const allIdle = r.steps.length > 0 && r.steps.every((s) => s.idle === true);
      const tail = bad === 0 ? allIdle ? `跳过：${r.steps[0]?.note ?? "这一步没做"}` : "都成了" : hard === 0 ? `${bad} 项跳过（没有网页会话）` : `${hard} 项没成${bad > hard ? `、${bad - hard} 项跳过` : ""}`;
      logBoth(store, hard > 0 ? "warn" : "debug", `${tag} ${tail}`, at);
    }
    const okCount = runs.filter((x) => x.result.ok).length;
    const idleAll = runs.length > 0 && runs.every(
      (x) => x.result.ok && x.result.steps.length > 0 && x.result.steps.every((s) => s.idle === true)
    );
    logBoth(
      store,
      okCount === runs.length && !idleAll ? "success" : "warn",
      idleAll ? `直播：${runs.length} 个账号都没领 —— ${runs[0]?.result.note ?? ""}` : `直播完成：${okCount}/${runs.length} 个账号好了`,
      at
    );
    setTaskBlockers("直播", blockersFor(store, "直播"));
    setTaskSummary("直播", summaryFor(store, "直播"));
  },
  {
    budget_ms: BUDGET_MS,
    // 直播也是环境型任务：面板任务列表里显示；没账号时“未就绪”。
    surface: "panel",
    summary: summaryFor(store, "直播"),
    blockers: blockersFor(store, "直播")
  }
);
debug(`mcloud-live：任务「直播」（预算 ${BUDGET_MS / 1e3}s，共享粗粒度 lane）`);
