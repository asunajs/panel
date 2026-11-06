//! crypto / node:crypto 的测试。
//!
//! 期望值不是手写的：scripts/gen-vectors-crypto.mjs 用**真的 Node**（node:crypto
//! 与 WebCrypto）跑出 src/jscompat/vectors/crypto.json，这里让 QuickJS 侧的同一批
//! 函数跑同样的输入再逐条比对。这样「面板里跑的和 Node 里跑的」是不是一回事，是
//! **测出来**的，不是看代码看出来的。
//!
//! 向量表的每一条是「函数名 + 参数描述符 + 期望值或期望错误」。参数描述符是纯数据
//! （JSON），harness 用同名的 mk() 把参数重建出来 —— 生成器和这里各写一份 mk/FNS，
//! 是**故意**的：生成器跑的是 Node 的真实现，这里跑的是兼容层，两份必须独立。
//!
//! 只有「错误 name / code / 同步还是 Promise 拒绝」进对比，消息文本不进：
//! Node 的消息里带 Buffer、我们的带中文提示，逐字比没有意义。

use crate::jscompat::crypto::EXPORTS;
use crate::jscompat::with_compat;

const VECTORS: &str = include_str!("vectors/crypto.json");

/// node:crypto 的 CJS 模块体（与生产同一个生成器：jshost::node_module_for_require）。
fn node_crypto_source() -> String {
    crate::jshost::node_module_for_require("node:crypto").expect("node:crypto 是内置模块")
}

/// 把测试脚本里的 `__NODE_CRYPTO_SRC__` 换成 JSON 字符串字面量。
fn with_module(script: &str) -> String {
    script.replace(
        "__NODE_CRYPTO_SRC__",
        &serde_json::to_string(&node_crypto_source()).expect("序列化模块体"),
    )
}

/// 跑一段**同步**测试脚本并解析它的返回值。
///
/// 包一层 try/catch：脚本里任何一处抛错都要**带着 JS 的堆栈**冒出来。直接让
/// ctx.eval 失败时 rquickjs 只给一句 "Exception"，等于没说话。
///
/// 脚本里的 `__NODE_CRYPTO_SRC__` 会先被换成模块体的 JSON 字符串字面量 ——
/// 少了这一步，new Function(__NODE_CRYPTO_SRC__) 只会得到一个 ReferenceError。
fn eval_json(ctx: &rquickjs::Ctx, body: &str) -> serde_json::Value {
    let body = with_module(body);
    let wrapped = format!(
        "(function () {{ try {{ return JSON.stringify((function () {{ {body} }})()); }} \
         catch (e) {{ return '!JSERR:' + (e && e.stack ? e.stack : String(e)); }} }})()"
    );
    let got: String = ctx.eval(wrapped).expect("测试脚本要能跑");
    assert!(!got.starts_with("!JSERR:"), "兼容层脚本抛错:\n{got}");
    serde_json::from_str(&got).expect("测试脚本必须返回 JSON 可表达的值")
}

/// 异步脚本的脚手架：建 Context → 装 BOOTSTRAP + 兼容层 → 启动脚本 →
/// **退出** ctx.with 之后再泵微任务 → 再进去读结果。
///
/// 为什么不能像同步那样直接在 with_compat 的闭包里泵：闭包是在 ctx.with 里跑的，
/// 而 Runtime::execute_pending_job 要再借一次 Context（RefCell）—— 在里面泵会
/// panic "RefCell already borrowed"（实测）。内嵌 QuickJS 没有事件循环，Promise 的
/// 续体必须由宿主显式泵，所以只能分三次进出 Context。
/// （fetch 域将来也要这套东西；等 with_compat 扩成异步版就可以删掉这里。）
fn run_async_raw(starter: &str, reader: &str) -> String {
    let rt = rquickjs::Runtime::new().expect("建 Runtime");
    let ctx = rquickjs::Context::full(&rt).expect("建 Context");
    ctx.with(|ctx| {
        ctx.eval::<(), _>(crate::jshost::BOOTSTRAP).expect("BOOTSTRAP 自身要能跑");
        crate::jscompat::install(&ctx).expect("兼容层自身要能跑");
        ctx.eval::<(), _>(starter).expect("测试脚本要能跑");
    });
    while rt.execute_pending_job().unwrap_or(false) {}
    ctx.with(|ctx| ctx.eval::<String, _>(reader).expect("读结果"))
}

/// 跑一段**异步**测试脚本（body 里可以 await），等微任务跑完再取结果。
fn eval_json_async(body: &str) -> serde_json::Value {
    let body = with_module(body);
    let starter = format!(
        "globalThis.__t = null;\n(async function () {{ try {{ \
           globalThis.__t = JSON.stringify(await (async function () {{ {body} }})()); \
         }} catch (e) {{ globalThis.__t = '!JSERR:' + (e && e.stack ? e.stack : String(e)); }} }})();"
    );
    let got = run_async_raw(
        &starter,
        "globalThis.__t === null ? '!PENDING' : globalThis.__t",
    );
    assert!(!got.starts_with('!'), "测试脚本抛错或没跑完:\n{got}");
    serde_json::from_str(&got).expect("测试脚本必须返回 JSON 可表达的值")
}

/// 向量表里的期望值（字符串）。用来给「换一种写法算同一个东西」的测试当基准 ——
/// 基准仍然来自 Node，而不是在这里再手写一遍。
fn vector_value(id: &str) -> String {
    let table: serde_json::Value = serde_json::from_str(VECTORS).expect("向量表是 JSON");
    table["checks"]
        .as_array()
        .expect("checks 是数组")
        .iter()
        .find(|c| c["id"] == id)
        .unwrap_or_else(|| panic!("向量表里没有 {id:?}"))["value"]
        .as_str()
        .unwrap_or_else(|| panic!("{id:?} 的期望值不是字符串"))
        .to_string()
}

/// 向量对拍用的 harness：在 QuickJS 里重建向量表的每一条输入，把结果写回
/// globalThis.__crypto_report（JSON）。
const HARNESS: &str = r#"
(function () {
    var V = __VECTORS__;
    var NODE_CRYPTO_SRC = __NODE_CRYPTO_SRC__;
    var nc = (new Function(NODE_CRYPTO_SRC))();

    function hexBytes(h) {
        var out = new Uint8Array(h.length >> 1);
        for (var i = 0; i < out.length; i++) out[i] = parseInt(h.slice(i * 2, i * 2 + 2), 16);
        return out;
    }
    function unitsToStr(u) {
        var s = '';
        for (var i = 0; i < u.length; i++) s += String.fromCharCode(u[i]);
        return s;
    }
    function bytesHex(view) {
        var b = new Uint8Array(view.buffer, view.byteOffset, view.byteLength);
        var s = '';
        var digits = '0123456789abcdef';
        for (var i = 0; i < b.length; i++) s += digits[(b[i] >> 4) & 15] + digits[b[i] & 15];
        return s;
    }
    function errInfo(e) {
        return {
            name: (e && e.name) ? e.name : String(e),
            code: (e === null || e === undefined || e.code === undefined) ? null : e.code,
        };
    }
    function norm(v) {
        if (v instanceof ArrayBuffer) return { hex: bytesHex(new Uint8Array(v)) };
        if (ArrayBuffer.isView(v)) return { hex: bytesHex(v) };
        return v;
    }
    async function mk(a) {
        switch (a.t) {
            case 'bytes': return hexBytes(a.hex);
            case 'str': return a.v;
            case 'num': return a.v;
            case 'json': return a.v;
            case 'undef': return undefined;
            case 'sub': return hexBytes(a.hex).subarray(a.start, a.end);
            case 'dataview': return new DataView(hexBytes(a.hex).buffer);
            case 'arraybuffer': return hexBytes(a.hex).buffer;
            case 'typed': return new globalThis[a.ctor](a.len);
            case 'key':
                return crypto.subtle.importKey(
                    'raw', hexBytes(a.hex), { name: 'HMAC', hash: { name: a.hash } },
                    a.extractable, a.usages);
            default: throw new Error('未知参数描述: ' + JSON.stringify(a));
        }
    }
    async function mkAll(list) {
        var out = [];
        for (var i = 0; i < list.length; i++) {
            if (list[i].t === 'missing') break;
            out.push(await mk(list[i]));
        }
        return out;
    }

    var FNS = {
        hashHex: function (alg, data) { return nc.createHash(alg).update(data).digest('hex'); },
        hashTextHex: function (alg, u, enc) {
            return nc.createHash(alg).update(unitsToStr(u), enc).digest('hex');
        },
        hashDigest: function (alg, data, enc) { return nc.createHash(alg).update(data).digest(enc); },
        hmacHex: function (alg, key, data) { return nc.createHmac(alg, key).update(data).digest('hex'); },
        hmacTextHex: function (alg, ku, ke, data) {
            return nc.createHmac(alg, unitsToStr(ku), ke).update(data).digest('hex');
        },
        timingSafe: function (a, b) { return nc.timingSafeEqual(a, b); },
        subtleDigest: function () { return crypto.subtle.digest.apply(crypto.subtle, arguments); },
        subtleSign: async function (hashName, keyBytes, data) {
            var key = await crypto.subtle.importKey(
                'raw', keyBytes, { name: 'HMAC', hash: { name: hashName } }, false, ['sign']);
            return crypto.subtle.sign({ name: 'HMAC', hash: { name: hashName } }, key, data);
        },
        subtleSignAlg: async function (alg, hashName, keyBytes, data) {
            var key = await crypto.subtle.importKey(
                'raw', keyBytes, { name: 'HMAC', hash: { name: hashName } }, false, ['sign']);
            return crypto.subtle.sign(alg, key, data);
        },
        subtleVerify: async function (hashName, keyBytes, data, sig) {
            var key = await crypto.subtle.importKey(
                'raw', keyBytes, { name: 'HMAC', hash: { name: hashName } }, false, ['sign', 'verify']);
            return crypto.subtle.verify({ name: 'HMAC' }, key, sig, data);
        },
        subtleImportKey: function () {
            return crypto.subtle.importKey.apply(crypto.subtle, arguments);
        },
        subtleImportKeyInfo: async function (hashName, keyBytes, extractable, usages) {
            var key = await crypto.subtle.importKey(
                'raw', keyBytes, { name: 'HMAC', hash: { name: hashName } }, !!extractable, usages);
            return {
                type: key.type,
                extractable: key.extractable,
                usages: key.usages,
                algorithm: key.algorithm,
                tag: Object.prototype.toString.call(key),
                ownKeys: Object.keys(key),
                json: JSON.stringify(key),
            };
        },
        grvOk: function (arr) {
            var ret = crypto.getRandomValues(arr);
            return { same: ret === arr, length: arr.length, byteLength: arr.byteLength };
        },
        subtleSignRaw: function () { return crypto.subtle.sign.apply(crypto.subtle, arguments); },
        subtleVerifyRaw: function () { return crypto.subtle.verify.apply(crypto.subtle, arguments); },
        getRandomValues: function () { return crypto.getRandomValues.apply(crypto, arguments); },
        cryptoShape: function () {
            return {
                cryptoTag: Object.prototype.toString.call(crypto),
                subtleTag: Object.prototype.toString.call(crypto.subtle),
                instanceofCrypto: crypto instanceof Crypto,
                instanceofSubtle: crypto.subtle instanceof SubtleCrypto,
                cryptoKeys: Object.keys(crypto),
                subtleKeys: Object.keys(crypto.subtle),
                cryptoLength: Crypto.length,
                subtleLength: SubtleCrypto.length,
                keyLength: CryptoKey.length,
                grvLength: crypto.getRandomValues.length,
                uuidLength: crypto.randomUUID.length,
                jsonSubtle: JSON.stringify(crypto.subtle),
            };
        },
        newCrypto: function () { return new Crypto(); },
        newCryptoKey: function () { return new CryptoKey(); },
        createHash: function (alg) { return nc.createHash(alg); },
        createHmac: function (alg, key, enc) { return nc.createHmac(alg, key, enc); },
        hashUpdate: function (data, enc) { return nc.createHash('sha256').update(data, enc); },
        hashDigestTwice: function () {
            var h = nc.createHash('sha256');
            h.update('a').digest('hex');
            return h.digest('hex');
        },
        hashUpdateAfterDigest: function () {
            var h = nc.createHash('sha256');
            h.digest();
            return h.update('a');
        },
        randomBytes: function (size) { return nc.randomBytes(size); },
        randomInt: function () { return nc.randomInt.apply(null, arguments); },
        timingSafeEqual: function (a, b) { return nc.timingSafeEqual(a, b); },
    };

    async function runCheck(c) {
        var fn = FNS[c.fn];
        if (typeof fn !== 'function') return { harnessError: '未知 fn: ' + c.fn };
        var args = await mkAll(c.args || []);
        var r;
        try {
            r = fn.apply(null, args);
        } catch (e) {
            return { error: errInfo(e), sync: true };
        }
        if (r && typeof r.then === 'function') {
            try {
                r = await r;
            } catch (e) {
                return { error: errInfo(e), sync: false };
            }
        }
        return { value: norm(r) };
    }

    globalThis.__crypto_report = null;
    (async function () {
        try {
            var out = [];
            for (var i = 0; i < V.checks.length; i++) {
                var c = V.checks[i];
                var r;
                try {
                    r = await runCheck(c);
                } catch (e) {
                    r = { harnessError: String(e && e.stack ? e.stack : e) };
                }
                r.id = c.id;
                out.push(r);
            }
            globalThis.__crypto_report = JSON.stringify(out);
        } catch (e) {
            globalThis.__crypto_report = '!JSERR:' + String(e && e.stack ? e.stack : e);
        }
    })();
})();
"#;

/// 兼容层与 Node 的逐条对拍。这是这个域的主测试：184 条输入覆盖 4 种摘要算法、
/// HMAC（含 > block size 的长 key）、各种编码、WebCrypto 的 digest / sign / verify /
/// importKey、getRandomValues 的边界，以及 70 条错误路径的 name / code / 同步性。
#[test]
fn vectors_match_node() {
    let script = HARNESS
        .replace("__VECTORS__", VECTORS)
        .replace(
            "__NODE_CRYPTO_SRC__",
            &serde_json::to_string(&node_crypto_source()).expect("序列化模块体"),
        );
    let raw = run_async_raw(
        &script,
        "globalThis.__crypto_report === null ? '!PENDING' : globalThis.__crypto_report",
    );
    assert!(
        !raw.starts_with("!PENDING"),
        "harness 没有产出结果（Promise 没跑完？）"
    );
    assert!(!raw.starts_with("!JSERR:"), "harness 抛错:\n{raw}");
    let got: serde_json::Value =
        serde_json::from_str(&raw).expect("harness 结果必须是 JSON");

    let want: serde_json::Value = serde_json::from_str(VECTORS).expect("向量表是 JSON");
    let want_checks = want["checks"].as_array().expect("checks 是数组");
    let got_checks = got.as_array().expect("结果是数组");
    assert_eq!(got_checks.len(), want_checks.len(), "跑出来的条数不对");

    // 逐条断（而不是整块 assert_eq）：失败时要能一眼看出是**哪条**输入。
    for (i, exp) in want_checks.iter().enumerate() {
        let id = exp["id"].as_str().expect("id 是字符串");
        let actual = &got_checks[i];
        assert_eq!(actual["id"], exp["id"], "第 {i} 条的顺序/id 对不上");
        if let Some(h) = actual.get("harnessError") {
            panic!("{id}: harness 出错: {h}");
        }
        if let Some(exp_value) = exp.get("value") {
            let got_value = actual
                .get("value")
                .unwrap_or_else(|| panic!("{id}: 期望返回值 {exp_value}，实际抛错 {}", actual["error"]));
            assert_eq!(got_value, exp_value, "{id}: 与 Node 不一致");
        } else {
            let exp_err = &exp["error"];
            let got_err = actual
                .get("error")
                .unwrap_or_else(|| panic!("{id}: 期望抛错 {exp_err}，实际返回 {}", actual["value"]));
            assert_eq!(
                got_err["name"], exp_err["name"],
                "{id}: 错误 name 不一致（实际 {got_err}）"
            );
            assert_eq!(
                got_err["code"], exp_err["code"],
                "{id}: 错误 code 不一致（实际 {got_err}）"
            );
            assert_eq!(
                actual["sync"], exp["sync"],
                "{id}: 同步抛 / Promise 拒绝的边界与 Node 不一致"
            );
        }
    }
}

/// EXPORTS 清单与模块体一一对上，且 require 那条路（__builtinCjsModule）能跑通。
///
/// 清单与模块体分开写，最容易出的偏差是「清单里有、模块体没定义」：插件
/// import 到 undefined，报错却发生在插件那边（"xxx is not a function"）。
#[test]
fn node_crypto_exports_and_require_path() {
    let script = with_module(
        r#"
        globalThis.__host_node_cjs = {};
        globalThis.__host_node_cjs['node:crypto'] = __NODE_CRYPTO_SRC__;
        var m = globalThis.__builtinCjsModule('node:crypto');
        var keys = Object.keys(m).sort();
        return {
            keys: keys,
            types: keys.map(function (k) { return typeof m[k]; }),
            cached: globalThis.__builtinCjsModule('node:crypto') === m,
            hashWorks: m.createHash('sha256').update('abc').digest('hex'),
        };
        "#,
    );
    let got = with_compat(|_, ctx| eval_json(ctx, &script));

    let mut want: Vec<String> = EXPORTS.iter().map(|s| s.to_string()).collect();
    want.sort();
    let keys: Vec<String> =
        serde_json::from_value(got["keys"].clone()).expect("keys 是字符串数组");
    assert_eq!(keys, want, "node:crypto 的导出清单与模块体对不上");

    // 名字在、但值是 undefined 也算对不上：插件 import 到的东西不可调用。
    let types: Vec<String> =
        serde_json::from_value(got["types"].clone()).expect("types 是字符串数组");
    for (name, ty) in keys.iter().zip(types.iter()) {
        assert_ne!(ty, "undefined", "导出了 {name} 但模块体里没有它的值");
        let want_ty = if name == "webcrypto" || name == "subtle" { "object" } else { "function" };
        assert_eq!(ty, want_ty, "{name} 的类型不对");
    }
    assert_eq!(got["cached"], serde_json::json!(true), "模块对象必须被缓存");
    assert_eq!(
        got["hashWorks"],
        serde_json::json!(vector_value("hash sha256 abc")),
        "require 出来的 createHash 结果不对"
    );
}

/// 全局安装的形态：可写、可配置、不可枚举（与 text.js 的 TextEncoder 一致），
/// 且 toStringTag / instanceof / Illegal constructor 都与 Node 同形。
#[test]
fn crypto_globals_are_writable_configurable_not_enumerable() {
    let got = with_compat(|_, ctx| {
        eval_json(
            ctx,
            r#"
            function errName(fn) {
                try { fn(); return 'no-throw'; } catch (e) { return e.name + '/' + (e.code === undefined ? 'null' : e.code); }
            }
            var d = Object.getOwnPropertyDescriptor(globalThis, 'crypto');
            return {
                writable: d.writable,
                configurable: d.configurable,
                enumerable: d.enumerable,
                tag: Object.prototype.toString.call(crypto),
                subtleTag: Object.prototype.toString.call(crypto.subtle),
                instances: [crypto instanceof Crypto, crypto.subtle instanceof SubtleCrypto],
                illegalCrypto: errName(function () { return new Crypto(); }),
                illegalSubtle: errName(function () { return new SubtleCrypto(); }),
                illegalKey: errName(function () { return new CryptoKey(); }),
                hasMethods: ['digest', 'importKey', 'sign', 'verify'].every(function (n) {
                    return typeof crypto.subtle[n] === 'function';
                }),
            };
            "#,
        )
    });
    assert_eq!(got["writable"], serde_json::json!(true));
    assert_eq!(got["configurable"], serde_json::json!(true));
    assert_eq!(got["enumerable"], serde_json::json!(false), "Node 那边可枚举，但本项目约定不可枚举");
    assert_eq!(got["tag"], serde_json::json!("[object Crypto]"));
    assert_eq!(got["subtleTag"], serde_json::json!("[object SubtleCrypto]"));
    assert_eq!(got["instances"], serde_json::json!([true, true]));
    assert_eq!(got["illegalCrypto"], serde_json::json!("TypeError/ERR_ILLEGAL_CONSTRUCTOR"));
    assert_eq!(got["illegalSubtle"], serde_json::json!("TypeError/ERR_ILLEGAL_CONSTRUCTOR"));
    assert_eq!(got["illegalKey"], serde_json::json!("TypeError/ERR_ILLEGAL_CONSTRUCTOR"));
    assert_eq!(got["hasMethods"], serde_json::json!(true));
}

/// 兼容层是 IIFE，不许把临时变量漏到全局（那会污染插件的作用域，
/// 更糟的是能让插件改掉兼容层的内部状态）。
#[test]
fn crypto_layer_leaks_no_temporaries() {
    let got = with_compat(|_, ctx| {
        eval_json(
            ctx,
            r#"
            var internals = ['C', 'K', 'KEY_INFO', 'KEY_TOKEN', 'SUBTLE_TOKEN', 'CRYPTO_TOKEN',
                'HEX_VALUE', 'KNOWN_ALGS', 'VALID_USAGES', 'HASH_LABEL', 'asPromise',
                'normEncoding', 'normDigestName', 'digestHex', 'hmacHex', 'hexToBytes',
                'decodeHexStrict', 'bufferSource', 'SUBTLE', 'CRYPTO', 'randomBytes',
                'b64encode', 'utf8Encode', 'utf8Decode'];
            return internals.filter(function (n) { return typeof globalThis[n] !== 'undefined'; });
            "#,
        )
    });
    assert_eq!(got, serde_json::json!([]), "有内部名字漏到了全局作用域");
}

/// 随机数必须来自宿主的系统 CSPRNG，**绝不能**是 Math.random —— 这里直接把
/// Math.random 换成会抛的版本，随机 API 仍要照常工作。顺带验三条 WebCrypto 的规矩：
/// 返回同一个数组、按视图边界填、字节数而不是元素个数。
#[test]
fn get_random_values_uses_csprng_and_respects_view_bounds() {
    let got = with_compat(|_, ctx| {
        eval_json(
            ctx,
            r#"
            var nc = (new Function(__NODE_CRYPTO_SRC__))();
            var original = Math.random;
            var csprngOk;
            Math.random = function () { throw new Error('用了 Math.random'); };
            try {
                var a = new Uint8Array(16);
                crypto.getRandomValues(a);
                var u = crypto.randomUUID();
                var b = nc.randomBytes(8);
                var n = nc.randomInt(0, 1000);
                csprngOk = [a.length, u.length, b.length, typeof n];
            } finally {
                Math.random = original;
            }

            var x = new Uint8Array(32);
            var y = new Uint8Array(32);
            crypto.getRandomValues(x);
            crypto.getRandomValues(y);
            var differs = false;
            for (var i = 0; i < 32; i++) if (x[i] !== y[i]) differs = true;

            var buf = new Uint8Array(8);
            for (var j = 0; j < 8; j++) buf[j] = 0xaa;
            var view = buf.subarray(2, 6);
            var ret = crypto.getRandomValues(view);
            var sentinels = [buf[0] === 0xaa, buf[1] === 0xaa, buf[6] === 0xaa, buf[7] === 0xaa];

            var u32 = new Uint32Array(4);
            crypto.getRandomValues(u32);

            var sample = new Uint8Array(1024);
            crypto.getRandomValues(sample);
            var seen = {};
            for (var k = 0; k < sample.length; k++) seen[sample[k]] = 1;

            return {
                csprngOk: csprngOk,
                identity: ret === view,
                differs: differs,
                sentinels: sentinels,
                u32Bytes: u32.byteLength,
                distinctBytes: Object.keys(seen).length,
            };
            "#,
        )
    });
    assert_eq!(got["csprngOk"], serde_json::json!([16, 36, 8, "number"]), "随机源不工作");
    assert_eq!(got["identity"], serde_json::json!(true), "必须返回同一个数组");
    assert_eq!(got["differs"], serde_json::json!(true), "两次 32 字节随机数不该全同");
    assert_eq!(got["sentinels"], serde_json::json!([true, true, true, true]), "填到了视图外面");
    assert_eq!(got["u32Bytes"], serde_json::json!(16));
    // 1024 次抽样里 256 个字节值几乎必然都出现过；低于 200 说明随机源有问题。
    let distinct = got["distinctBytes"].as_u64().expect("数字");
    assert!(distinct >= 200, "随机字节分布太窄：只有 {distinct} 种");
}

/// randomUUID 的格式是 RFC 4122 v4（版本位 4、变体位 10xx），并且不重复。
#[test]
fn random_uuid_is_rfc4122_v4() {
    let got = with_compat(|_, ctx| {
        eval_json(
            ctx,
            r#"
            var nc = (new Function(__NODE_CRYPTO_SRC__))();
            var re = /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/;
            var seen = {};
            var bad = 0;
            var unique = 0;
            var variantOk = true;
            for (var i = 0; i < 200; i++) {
                var u = crypto.randomUUID();
                if (!re.test(u)) bad++;
                if (u.charAt(14) !== '4') bad++;
                if ('89ab'.indexOf(u.charAt(19)) < 0) variantOk = false;
                if (!seen[u]) { seen[u] = 1; unique++; }
            }
            var fromModule = nc.randomUUID();
            return {
                bad: bad,
                unique: unique,
                variantOk: variantOk,
                moduleFormat: re.test(fromModule) && fromModule.length === 36,
            };
            "#,
        )
    });
    assert_eq!(got["bad"], serde_json::json!(0), "UUID 格式/版本位不对");
    assert_eq!(got["unique"], serde_json::json!(200), "200 个 UUID 里出现了重复");
    assert_eq!(got["variantOk"], serde_json::json!(true), "变体位不是 10xx");
    assert_eq!(got["moduleFormat"], serde_json::json!(true));
}

/// randomBytes / randomInt 的范围与边界。randomInt 是属性断言（均匀性没法用
/// 固定向量表达）：取值必须落在半开区间里，且区间里每个值都出现过。
#[test]
fn random_bytes_and_random_int_ranges() {
    let got = with_compat(|_, ctx| {
        eval_json(
            ctx,
            r#"
            var nc = (new Function(__NODE_CRYPTO_SRC__))();
            var sizes = [0, 1, 16, 65536, 70000];
            var lens = sizes.map(function (n) { return nc.randomBytes(n).length; });
            var a = nc.randomBytes(32);
            var b = nc.randomBytes(32);
            var differs = false;
            for (var i = 0; i < 32; i++) if (a[i] !== b[i]) differs = true;

            var draws = [];
            for (var j = 0; j < 2000; j++) draws.push(nc.randomInt(5, 10));
            var outside = 0;
            var hits = {};
            for (var k = 0; k < draws.length; k++) {
                var v = draws[k];
                if (v < 5 || v > 9 || Math.floor(v) !== v) outside++;
                hits[v] = 1;
            }
            var zero = [];
            for (var m = 0; m < 5; m++) zero.push(nc.randomInt(1));
            var big = nc.randomInt(281474976710655);

            return {
                lens: lens,
                differs: differs,
                outside: outside,
                hitCount: Object.keys(hits).length,
                zero: zero,
                bigInRange: big >= 0 && big < 281474976710655 && Math.floor(big) === big,
            };
            "#,
        )
    });
    assert_eq!(
        got["lens"],
        serde_json::json!([0, 1, 16, 65536, 70000]),
        "randomBytes 长度不对（70000 跨过了宿主单次 64 KiB 的上限）"
    );
    assert_eq!(got["differs"], serde_json::json!(true));
    assert_eq!(got["outside"], serde_json::json!(0), "randomInt 越界");
    assert_eq!(got["hitCount"], serde_json::json!(5), "randomInt 有值没被抽到过（区间不对？）");
    assert_eq!(got["zero"], serde_json::json!([0, 0, 0, 0, 0]), "randomInt(1) 只能是 0");
    assert_eq!(got["bigInRange"], serde_json::json!(true));
}

/// 多次 update 与一次给同样字节必须等价；update 之后改原 buffer 不能影响结果
/// （Node 在 update 那一刻就把字节并进状态了）。
#[test]
fn hash_accumulates_and_copies_input() {
    let abc = vector_value("hash sha256 abc");
    let hmac = vector_value("hmac sha256 key");
    let got = with_compat(|_, ctx| {
        eval_json(
            ctx,
            r#"
            var nc = (new Function(__NODE_CRYPTO_SRC__))();
            var h = nc.createHash('sha256');
            h.update('a').update(new Uint8Array([0x62])).update('63', 'hex');
            var multi = h.digest('hex');

            var buf = new Uint8Array([0x61, 0x62, 0x63]);
            var h2 = nc.createHash('sha256').update(buf);
            buf[0] = 0xff;
            var copied = h2.digest('hex');

            var m = nc.createHmac('sha256', 'key');
            m.update('da').update('ta');
            return { multi: multi, copied: copied, hmacSplit: m.digest('hex') };
            "#,
        )
    });
    assert_eq!(got["multi"], serde_json::json!(abc), "多次 update 不等于一次给");
    assert_eq!(got["copied"], serde_json::json!(abc), "update 之后改 buffer 居然影响了结果");
    assert_eq!(got["hmacSplit"], serde_json::json!(hmac), "HMAC 多次 update 不等于一次给");
}

/// digest 之后再动 hash 对象：与 Node 一样抛 ERR_CRYPTO_HASH_FINALIZED
/// （Node 的 Hmac 第二次 digest 会**静默返回空串** —— 那条差异见 node_crypto.js 文件头，
/// 这里统一成抛错）。
#[test]
fn hash_state_errors_match_node() {
    let empty_hash = vector_value("hash sha256 empty");
    let empty_hmac = vector_value("hmac sha256 empty");
    let got = with_compat(|_, ctx| {
        eval_json(
            ctx,
            r#"
            var nc = (new Function(__NODE_CRYPTO_SRC__))();
            function errName(fn) {
                try { fn(); return 'no-throw'; } catch (e) { return e.name + '/' + (e.code === undefined ? 'null' : e.code); }
            }
            var h = nc.createHash('sha256');
            h.update('a');
            h.digest('hex');
            var m = nc.createHmac('sha256', 'k');
            m.update('d');
            m.digest('hex');
            return {
                digestTwice: errName(function () { h.digest('hex'); }),
                updateAfterDigest: errName(function () { h.update('b'); }),
                hmacDigestTwice: errName(function () { m.digest('hex'); }),
                emptyHash: nc.createHash('sha256').digest('hex'),
                emptyHmac: nc.createHmac('sha256', '').update('data').digest('hex'),
            };
            "#,
        )
    });
    assert_eq!(got["digestTwice"], serde_json::json!("Error/ERR_CRYPTO_HASH_FINALIZED"));
    assert_eq!(got["updateAfterDigest"], serde_json::json!("Error/ERR_CRYPTO_HASH_FINALIZED"));
    assert_eq!(got["hmacDigestTwice"], serde_json::json!("Error/ERR_CRYPTO_HASH_FINALIZED"));
    assert_eq!(got["emptyHash"], serde_json::json!(empty_hash));
    assert_eq!(got["emptyHmac"], serde_json::json!(empty_hmac));
}

/// **与 Node 的已知差异**（都写在 node_crypto.js 文件头）：
/// 未知编码 / md5 / 非法 hex、base64 / 非整数 size 在 Node 那边会静默降级或直接支持，
/// 这里必须显式报错 —— 静默降级会让人拿到一个错的哈希还看不出来。
#[test]
fn known_differences_from_node_are_loud() {
    let got = with_compat(|_, ctx| {
        eval_json(
            ctx,
            r#"
            var nc = (new Function(__NODE_CRYPTO_SRC__))();
            function errName(fn) {
                try { fn(); return 'no-throw'; } catch (e) { return e.name + '/' + (e.code === undefined ? 'null' : e.code); }
            }
            return {
                unknownInputEncoding: errName(function () { nc.createHash('sha256').update('x', 'nope'); }),
                unknownOutputEncoding: errName(function () { nc.createHash('sha256').update('x').digest('nope'); }),
                md5: errName(function () { nc.createHash('md5'); }),
                sha3: errName(function () { nc.createHash('sha3-256'); }),
                hmacMd5: errName(function () { nc.createHmac('md5', 'k'); }),
                hexOddLength: errName(function () { nc.createHash('sha256').update('616', 'hex'); }),
                hexBadChar: errName(function () { nc.createHash('sha256').update('zz', 'hex'); }),
                base64Bad: errName(function () { nc.createHash('sha256').update('!!!!', 'base64'); }),
                randomBytesFloat: errName(function () { nc.randomBytes(1.5); }),
                randomBytesHuge: errName(function () { nc.randomBytes(17 * 1024 * 1024); }),
            };
            "#,
        )
    });
    assert_eq!(got["unknownInputEncoding"], serde_json::json!("TypeError/ERR_UNKNOWN_ENCODING"));
    assert_eq!(got["unknownOutputEncoding"], serde_json::json!("TypeError/ERR_UNKNOWN_ENCODING"));
    assert_eq!(got["md5"], serde_json::json!("TypeError/ERR_CRYPTO_INVALID_DIGEST"));
    assert_eq!(got["sha3"], serde_json::json!("TypeError/ERR_CRYPTO_INVALID_DIGEST"));
    assert_eq!(got["hmacMd5"], serde_json::json!("TypeError/ERR_CRYPTO_INVALID_DIGEST"));
    assert_eq!(got["hexOddLength"], serde_json::json!("TypeError/ERR_INVALID_ARG_VALUE"));
    assert_eq!(got["hexBadChar"], serde_json::json!("TypeError/ERR_INVALID_ARG_VALUE"));
    assert_eq!(got["base64Bad"], serde_json::json!("TypeError/ERR_INVALID_ARG_VALUE"));
    assert_eq!(got["randomBytesFloat"], serde_json::json!("RangeError/ERR_OUT_OF_RANGE"));
    assert_eq!(got["randomBytesHuge"], serde_json::json!("RangeError/ERR_OUT_OF_RANGE"));
}

/// 没实现的算法 / 接口要**明确**：Node 有而面板没有的东西一律是 undefined
/// （调用时 "not a function"，比给个能 catch 的 NotSupportedError 更难误用），
/// 而算法名层面的不支持是 NotSupportedError —— 与 Node 的错误类型一致。
#[test]
fn unsupported_crypto_surface_is_absent_or_rejects() {
    let script = with_module(
        r#"
        var nc = (new Function(__NODE_CRYPTO_SRC__))();
        var aes = await crypto.subtle.importKey(
            'raw', new Uint8Array(16), { name: 'AES-GCM' }, true, ['encrypt']).then(
            function () { return 'resolved'; },
            function (e) { return e.name + '/' + e.code; });
        var ecdsa = await crypto.subtle.sign({ name: 'ECDSA' }, {}, new Uint8Array(1)).then(
            function () { return 'resolved'; },
            function (e) { return e.name + '/' + e.code; });
        var md5;
        try { nc.createHash('md5'); md5 = 'no-throw'; }
        catch (e) { md5 = e.name + '/' + e.code; }
        return {
            generateKey: typeof crypto.subtle.generateKey,
            exportKey: typeof crypto.subtle.exportKey,
            encrypt: typeof crypto.subtle.encrypt,
            deriveBits: typeof crypto.subtle.deriveBits,
            pbkdf2: typeof nc.pbkdf2,
            createSign: typeof nc.createSign,
            createCipheriv: typeof nc.createCipheriv,
            importAes: aes,
            signEcdsa: ecdsa,
            md5: md5,
            webcryptoAlias: nc.webcrypto === globalThis.crypto,
            subtleAlias: nc.subtle === globalThis.crypto.subtle,
        };
        "#,
    );
    let got = eval_json_async(&script);
    for key in [
        "generateKey", "exportKey", "encrypt", "deriveBits", "pbkdf2", "createSign", "createCipheriv",
    ] {
        assert_eq!(got[key], serde_json::json!("undefined"), "{key} 不该存在");
    }
    assert_eq!(got["importAes"], serde_json::json!("NotSupportedError/9"));
    assert_eq!(got["signEcdsa"], serde_json::json!("NotSupportedError/9"));
    assert_eq!(got["md5"], serde_json::json!("TypeError/ERR_CRYPTO_INVALID_DIGEST"));
    assert_eq!(got["webcryptoAlias"], serde_json::json!(true), "webcrypto 必须是全局 crypto 本尊");
    assert_eq!(got["subtleAlias"], serde_json::json!(true));
}

/// timingSafeEqual：相等/不等、视图偏移、长度不等与类型错误。
/// 「常量时间」没法用单元测试证明，实现里不提前 return（见 node_crypto.js）。
#[test]
fn timing_safe_equal_semantics() {
    let got = with_compat(|_, ctx| {
        eval_json(
            ctx,
            r#"
            var nc = (new Function(__NODE_CRYPTO_SRC__))();
            function errName(fn) {
                try { fn(); return 'no-throw'; } catch (e) { return e.name + '/' + (e.code === undefined ? 'null' : e.code); }
            }
            var a = new Uint8Array([1, 2, 3, 4]);
            var b = new Uint8Array([1, 2, 3, 4]);
            var c = new Uint8Array([1, 2, 3, 5]);
            var view = new Uint8Array([9, 1, 2, 3, 4, 9]).subarray(1, 5);
            return {
                equal: nc.timingSafeEqual(a, b),
                unequal: nc.timingSafeEqual(a, c),
                view: nc.timingSafeEqual(view, a),
                empty: nc.timingSafeEqual(new Uint8Array(0), new Uint8Array(0)),
                dataView: nc.timingSafeEqual(new DataView(a.buffer), b),
                lengthMismatch: errName(function () { nc.timingSafeEqual(a, new Uint8Array(3)); }),
                wrongType: errName(function () { nc.timingSafeEqual('abcd', 'abcd'); }),
            };
            "#,
        )
    });
    assert_eq!(got["equal"], serde_json::json!(true));
    assert_eq!(got["unequal"], serde_json::json!(false));
    assert_eq!(got["view"], serde_json::json!(true), "必须按 byteOffset/byteLength 比");
    assert_eq!(got["empty"], serde_json::json!(true));
    assert_eq!(got["dataView"], serde_json::json!(true));
    assert_eq!(got["lengthMismatch"], serde_json::json!("RangeError/ERR_CRYPTO_TIMING_SAFE_EQUAL_LENGTH"));
    assert_eq!(got["wrongType"], serde_json::json!("TypeError/ERR_INVALID_ARG_TYPE"));
}
