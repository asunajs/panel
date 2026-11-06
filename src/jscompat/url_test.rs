//! URL / URLSearchParams / node:url 的测试。
//!
//! 期望值不是手写的：scripts/gen-vectors-url.mjs 用**真的 Node**（v24）生成
//! vectors/url.json，这里让 QuickJS 跑同一批输入再逐条比对。
//!
//! 覆盖：绝对/相对解析、点段规整、百分号编码边界、IPv4/IPv6、userinfo、默认端口、
//! file:、opaque scheme、search/hash 编码、setter、searchParams 活绑定、
//! URL.canParse/URL.parse、node:url 的 fileURLToPath/pathToFileURL/urlToHttpOptions。
//!
//! **刻意避开**的两类：IDN 主机名（本实现不做 IDNA，见 url.js 文件头）与
//! opaque base + 带 '#' 的相对地址（Node v24 的非规范行为，本实现按规范抛错）。

use crate::jscompat::with_compat;
use serde_json::Value;

const VECTORS: &str = include_str!("vectors/url.json");

/// 这段脚本在 QuickJS 里重放 vectors/url.json 的操作，最后返回**实际结果**。
/// 结果结构与 JSON 里的 expect 一一对应（同一套 snapshot 字段），比对方能定位到第几条。
const HARNESS: &str = r#"
(function () {
    var V = globalThis.__urlVectors;
    var FIELDS = ['href', 'origin', 'protocol', 'username', 'password', 'host',
        'hostname', 'port', 'pathname', 'search', 'hash'];

    function snapshot(u) {
        var out = {};
        for (var i = 0; i < FIELDS.length; i++) out[FIELDS[i]] = u[FIELDS[i]];
        out.searchParams = u.searchParams.toString();
        return out;
    }

    function errTag(e) {
        return (e && e.name ? e.name : 'Error') + ':' + ((e && e.code) || '');
    }

    function hasBase(item) {
        return Object.prototype.hasOwnProperty.call(item, 'base');
    }

    function makeInit(init) {
        switch (init.kind) {
        case 'none': return new URLSearchParams();
        case 'null': return new URLSearchParams(null);
        case 'string': return new URLSearchParams(init.value);
        case 'number': return new URLSearchParams(init.value);
        case 'object': return new URLSearchParams(init.value);
        case 'pairs': return new URLSearchParams(init.value);
        case 'copy': return new URLSearchParams(new URLSearchParams(init.value));
        default: throw new Error('未知 init kind: ' + init.kind);
        }
    }

    var out = {
        parse: [], throws: [], setters: [], params: [], paramsQueries: [],
        urlParams: [], statics: [],
        nodeUrl: { urlToHttpOptions: [], fileURLToPath: [], pathToFileURL: [] }
    };

    V.parse.forEach(function (item) {
        var u = hasBase(item) ? new URL(item.input, item.base) : new URL(item.input);
        out.parse.push(snapshot(u));
    });

    V.throws.forEach(function (item) {
        var caught = null;
        try {
            if (hasBase(item)) new URL(item.input, item.base);
            else new URL(item.input);
        } catch (e) {
            caught = { name: e.name, code: e.code, input: e.input };
        }
        if (caught === null) caught = { name: '!NO THROW', code: null, input: undefined };
        out.throws.push(caught);
    });

    V.setters.forEach(function (item) {
        var u = new URL(item.input);
        var threw = null;
        for (var i = 0; i < item.ops.length; i++) {
            try {
                u[item.ops[i][0]] = item.ops[i][1];
            } catch (e) {
                threw = errTag(e);
                break;
            }
        }
        var entry = { threw: threw };
        var snap = snapshot(u);
        for (var k in snap) entry[k] = snap[k];
        out.setters.push(entry);
    });

    V.params.forEach(function (item) {
        var p = makeInit(item.init);
        for (var i = 0; i < item.ops.length; i++) {
            var op = item.ops[i];
            p[op[0]].apply(p, op.slice(1));
        }
        out.params.push({
            pairs: Array.from(p.entries()),
            string: p.toString(),
            size: p.size,
        });
    });

    V.paramsQueries.forEach(function (item) {
        var p = makeInit(item.init);
        var values = [];
        for (var i = 0; i < item.calls.length; i++) {
            var call = item.calls[i];
            try {
                var value = p[call[0]].apply(p, call.slice(1));
                values.push(value === undefined ? null : value);
            } catch (e) {
                values.push(errTag(e));
            }
        }
        out.paramsQueries.push(values);
    });

    V.urlParams.forEach(function (item) {
        var u = new URL(item.input);
        var p = u.searchParams;
        var identity = p === u.searchParams;
        for (var i = 0; i < item.ops.length; i++) {
            var op = item.ops[i];
            if (op[0] === 'search' || op[0] === 'hash' || op[0] === 'href') u[op[0]] = op[1];
            else p[op[0]].apply(p, op.slice(1));
        }
        out.urlParams.push({
            identity: identity,
            href: u.href,
            search: u.search,
            params: p.toString(),
            size: p.size,
        });
    });

    V.statics.forEach(function (item) {
        try {
            var value = URL[item.fn](item.input, item.base);
            if (item.fn === 'parse') {
                out.statics.push(value === null ? null : value.href);
            } else {
                out.statics.push(value);
            }
        } catch (e) {
            out.statics.push(errTag(e));
        }
    });

    var mod = new Function(globalThis.__nodeUrlSrc)();

    V.nodeUrl.urlToHttpOptions.forEach(function (item) {
        try {
            out.nodeUrl.urlToHttpOptions.push({ ok: true, value: mod.urlToHttpOptions(new URL(item.input)) });
        } catch (e) {
            out.nodeUrl.urlToHttpOptions.push({ ok: false, name: e.name, code: e.code });
        }
    });

    V.nodeUrl.fileURLToPath.forEach(function (item) {
        try {
            out.nodeUrl.fileURLToPath.push({ ok: true, value: mod.fileURLToPath(item.input) });
        } catch (e) {
            out.nodeUrl.fileURLToPath.push({ ok: false, name: e.name, code: e.code });
        }
    });

    V.nodeUrl.pathToFileURL.forEach(function (item) {
        try {
            out.nodeUrl.pathToFileURL.push({ ok: true, value: mod.pathToFileURL(item.input).href });
        } catch (e) {
            out.nodeUrl.pathToFileURL.push({ ok: false, name: e.name, code: e.code });
        }
    });

    return JSON.stringify(out);
})()
"#;

fn harness_output() -> Value {
    with_compat(|_, ctx| {
        // node:url 的源码从真正的内置模块包装器取（不是另写一份），
        // 这样这里验的就是 jshost 给插件的那条路。
        let module_src =
            crate::jshost::node_module_for_require("node:url").expect("node:url 必须是内置模块");
        let inject = format!(
            "globalThis.__urlVectors = {VECTORS};\nglobalThis.__nodeUrlSrc = {};",
            serde_json::to_string(&module_src).expect("模块源码转 JSON")
        );
        ctx.eval::<(), _>(inject).expect("向量注入要能跑");

        // 包一层 try/catch：脚本里任何一处抛错都要带着 JS 堆栈冒出来，
        // 否则 rquickjs 只会给一句 "Exception"（同 text_test 的坑）。
        let wrapped = format!(
            "(function () {{ try {{ return ({HARNESS}); }} catch (e) {{ return '!JSERR:' + (e && e.stack ? e.stack : String(e)); }} }})()"
        );
        let got: String = ctx.eval(wrapped).expect("测试脚本要能跑");
        assert!(!got.starts_with("!JSERR:"), "兼容层脚本抛错: {got}");
        serde_json::from_str(&got).expect("结果必须是 JSON")
    })
}

fn compare_section(name: &str, want: &Value, got: &Value) {
    let want_items = want
        .get(name)
        .unwrap_or_else(|| panic!("vectors/url.json 缺 {name}"))
        .as_array()
        .unwrap_or_else(|| panic!("{name} 应是数组"));
    let got_items = got
        .get(name)
        .unwrap_or_else(|| panic!("harness 结果缺 {name}"))
        .as_array()
        .unwrap_or_else(|| panic!("{name} 应是数组"));
    assert_eq!(got_items.len(), want_items.len(), "{name} 条数不对");
    for (i, entry) in want_items.iter().enumerate() {
        // 向量表每一条都带 input/init 等"回放参数"，真正要比的是 expect：
        // 把输入也一起比会让失败信息被埋在一堆正常字段里。
        let expected = entry.get("expect").unwrap_or(entry);
        assert_eq!(
            &got_items[i], expected,
            "{name}[{i}] 与 Node 不一致（原始输入见 vectors/url.json）"
        );
    }
}

#[test]
fn url_vectors_match_node() {
    let got = harness_output();
    let want: Value = serde_json::from_str(VECTORS).expect("向量 JSON");
    for key in [
        "parse",
        "throws",
        "setters",
        "params",
        "paramsQueries",
        "urlParams",
        "statics",
    ] {
        compare_section(key, &want, &got);
    }
    // node:url 的几组助手在 JSON 里是嵌套对象。
    for sub in ["urlToHttpOptions", "fileURLToPath", "pathToFileURL"] {
        compare_section(sub, &want["nodeUrl"], &got["nodeUrl"]);
    }
}

/// 形状与错误：这些是「插件作者会直接摸」的细节，单独立一条测试盯住。
#[test]
fn url_globals_and_errors_match_node() {
    let script = r#"
    (function () {
        var out = {};
        try { new URL('http://a b/'); } catch (e) {
            out.invalid = { name: e.name, code: e.code, input: e.input, message: e.message };
        }
        var d1 = Object.getOwnPropertyDescriptor(globalThis, 'URL');
        var d2 = Object.getOwnPropertyDescriptor(globalThis, 'URLSearchParams');
        out.globalUrl = [d1.writable, d1.enumerable, d1.configurable];
        out.globalUsp = [d2.writable, d2.enumerable, d2.configurable];
        out.tags = [
            Object.prototype.toString.call(new URL('http://a/')),
            Object.prototype.toString.call(new URLSearchParams())
        ];
        out.keysUrl = Object.keys(URL.prototype);
        out.keysUsp = Object.keys(URLSearchParams.prototype);
        out.lengths = [URL.length, URLSearchParams.length, URL.canParse.length, URL.parse.length];
        out.methodLengths = ['append', 'delete', 'get', 'getAll', 'has', 'set', 'sort', 'forEach',
            'entries', 'keys', 'values', 'toString'].map(function (n) {
            return URLSearchParams.prototype[n].length;
        });
        out.iteratorDescriptor = (function () {
            var d = Object.getOwnPropertyDescriptor(URLSearchParams.prototype, Symbol.iterator);
            return [typeof d.value, d.writable, d.enumerable, d.configurable];
        })();
        out.staticKeys = [Object.keys(URL), Object.keys(URLSearchParams)];

        var missing = {};
        try { new URL(); } catch (e) { missing.url = [e.name, e.code]; }
        try { URL.parse(); } catch (e) { missing.parse = [e.name, e.code]; }
        try { URL.canParse(); } catch (e) { missing.canParse = [e.name, e.code]; }
        try { new URLSearchParams('a=1').get(); } catch (e) { missing.get = [e.name, e.code]; }
        try { new URLSearchParams('a=1').set('a'); } catch (e) { missing.set = [e.name, e.code]; }
        try { new URLSearchParams([['a']]); } catch (e) { missing.tuple = [e.name, e.code]; }
        out.missing = missing;

        var u = new URL('https://a.com/');
        var p = u.searchParams;
        out.identity = p === u.searchParams;
        p.set('a', '1');
        out.afterSet = u.href;
        u.search = '?x=1 2';
        out.afterSearch = u.href;
        out.boundValue = p.get('x');
        out.removeLast = (function () { p.delete('x'); return u.href; })();
        out.relative = new URL('/a', 'https://x.y/b/c').href;
        out.canParse = [URL.canParse('/a'), URL.canParse('/a', 'https://x/')];
        out.parseHref = URL.parse('/a', 'https://x.y/b/c').href;
        out.parseFail = URL.parse('https://a b/');
        out.undefinedValue = (function () {
            var q = new URLSearchParams('a=1');
            var had = q.has('a', undefined);
            q.delete('a', undefined);
            return [had, q.toString()];
        })();
        return JSON.stringify(out);
    })()
    "#;
    let got: String = with_compat(|_, ctx| {
        let wrapped = format!(
            "(function () {{ try {{ return ({script}); }} catch (e) {{ return '!JSERR:' + (e && e.stack ? e.stack : String(e)); }} }})()"
        );
        let s: String = ctx.eval(wrapped).expect("测试脚本要能跑");
        assert!(!s.starts_with("!JSERR:"), "兼容层脚本抛错: {s}");
        s
    });
    let v: Value = serde_json::from_str(&got).unwrap();

    assert_eq!(v["invalid"]["name"], "TypeError");
    assert_eq!(v["invalid"]["code"], "ERR_INVALID_URL");
    assert_eq!(v["invalid"]["input"], "http://a b/");
    assert_eq!(v["invalid"]["message"], "Invalid URL");
    assert_eq!(v["globalUrl"], serde_json::json!([true, false, true]));
    assert_eq!(v["globalUsp"], serde_json::json!([true, false, true]));
    assert_eq!(
        v["tags"],
        serde_json::json!(["[object URL]", "[object URLSearchParams]"])
    );
    assert_eq!(v["lengths"], serde_json::json!([1, 0, 1, 1]));
    assert_eq!(
        v["methodLengths"],
        serde_json::json!([2, 1, 1, 1, 1, 2, 0, 1, 0, 0, 0, 0])
    );
    assert_eq!(
        v["iteratorDescriptor"],
        serde_json::json!(["function", true, false, true])
    );
    // 静态方法不可枚举（Node：Object.keys(URL) 为空）。
    assert_eq!(v["staticKeys"], serde_json::json!([[], []]));

    let keys_url = v["keysUrl"].as_array().unwrap();
    for name in [
        "toString",
        "href",
        "origin",
        "protocol",
        "username",
        "password",
        "host",
        "hostname",
        "port",
        "pathname",
        "search",
        "searchParams",
        "hash",
        "toJSON",
    ] {
        assert!(
            keys_url.iter().any(|k| k == name),
            "URL.prototype 缺少可枚举属性 {name}（Node 里 Object.keys 能看到）"
        );
    }
    let keys_usp = v["keysUsp"].as_array().unwrap();
    for name in [
        "size", "append", "delete", "get", "getAll", "has", "set", "sort", "entries", "forEach",
        "keys", "values", "toString",
    ] {
        assert!(
            keys_usp.iter().any(|k| k == name),
            "URLSearchParams.prototype 缺少 {name}"
        );
    }

    assert_eq!(
        v["missing"]["url"],
        serde_json::json!(["TypeError", "ERR_MISSING_ARGS"])
    );
    assert_eq!(
        v["missing"]["parse"],
        serde_json::json!(["TypeError", "ERR_MISSING_ARGS"])
    );
    assert_eq!(
        v["missing"]["canParse"],
        serde_json::json!(["TypeError", "ERR_MISSING_ARGS"])
    );
    assert_eq!(
        v["missing"]["get"],
        serde_json::json!(["TypeError", "ERR_MISSING_ARGS"])
    );
    assert_eq!(
        v["missing"]["set"],
        serde_json::json!(["TypeError", "ERR_MISSING_ARGS"])
    );
    assert_eq!(
        v["missing"]["tuple"],
        serde_json::json!(["TypeError", "ERR_INVALID_TUPLE"])
    );

    assert_eq!(v["identity"], serde_json::json!(true));
    assert_eq!(v["afterSet"], "https://a.com/?a=1");
    assert_eq!(v["afterSearch"], "https://a.com/?x=1%202");
    assert_eq!(v["boundValue"], "1 2");
    assert_eq!(v["removeLast"], "https://a.com/");
    assert_eq!(v["relative"], "https://x.y/a");
    assert_eq!(v["canParse"], serde_json::json!([false, true]));
    assert_eq!(v["parseHref"], "https://x.y/a");
    assert_eq!(v["parseFail"], Value::Null);
    // WebIDL：可选参数显式传 undefined 等于「没给」——Node 的
    // delete('a', undefined) 删同名参数、has('a', undefined) 为 true。
    assert_eq!(v["undefinedValue"], serde_json::json!([true, ""]));
}

/// 迭代器与 forEach：这些行为（快照、拿到 thisArg、Symbol.iterator 自指）
/// 只靠向量表里的 pairs 结果看不出来，单独验一遍。
#[test]
fn urlsearchparams_iteration_matches_node() {
    let script = r#"
    (function () {
        var p = new URLSearchParams('b=2&a=1&a=0');
        var out = {};
        out.keys = Array.from(p.keys());
        out.values = Array.from(p.values());
        out.entries = Array.from(p.entries());
        out.spread = Array.from(p);
        out.sameIterator = p[Symbol.iterator] === p.entries;
        var it = p.entries();
        out.next = [it.next(), it.next(), it.next(), it.next()];
        var seen = [];
        p.forEach(function (v, k) { seen.push([k, v]); });
        out.forEach = seen;
        var ctx = { mark: 1 };
        var gotThis = null;
        p.forEach(function () { gotThis = this; }, ctx);
        out.thisArg = gotThis === ctx;
        out.callbackGuard = (function () {
            try { p.forEach(5); return null; } catch (e) { return e.name; }
        })();
        return JSON.stringify(out);
    })()
    "#;
    let got: String = with_compat(|_, ctx| {
        let wrapped = format!(
            "(function () {{ try {{ return ({script}); }} catch (e) {{ return '!JSERR:' + (e && e.stack ? e.stack : String(e)); }} }})()"
        );
        let s: String = ctx.eval(wrapped).expect("测试脚本要能跑");
        assert!(!s.starts_with("!JSERR:"), "兼容层脚本抛错: {s}");
        s
    });
    let v: Value = serde_json::from_str(&got).unwrap();
    assert_eq!(v["keys"], serde_json::json!(["b", "a", "a"]));
    assert_eq!(v["values"], serde_json::json!(["2", "1", "0"]));
    assert_eq!(
        v["entries"],
        serde_json::json!([["b", "2"], ["a", "1"], ["a", "0"]])
    );
    assert_eq!(
        v["spread"],
        serde_json::json!([["b", "2"], ["a", "1"], ["a", "0"]])
    );
    assert_eq!(v["sameIterator"], serde_json::json!(true));
    assert_eq!(v["next"][0]["value"], serde_json::json!(["b", "2"]));
    assert_eq!(v["next"][0]["done"], serde_json::json!(false));
    assert_eq!(v["next"][3]["done"], serde_json::json!(true));
    assert_eq!(
        v["forEach"],
        serde_json::json!([["b", "2"], ["a", "1"], ["a", "0"]])
    );
    assert_eq!(v["thisArg"], serde_json::json!(true));
    assert_eq!(v["callbackGuard"], "TypeError");
}

/// node:url 的模块体 + 包装器：导出必须真的能调用，而不是只有名字。
#[test]
fn node_url_module_exports_work() {
    let script = r#"
    (function () {
        var M = new Function(globalThis.__nodeUrlSrc)();
        var out = {};
        out.keys = Object.keys(M).sort();
        out.sameClass = M.URL === URL && M.URLSearchParams === URLSearchParams;
        out.path = M.fileURLToPath('file:///tmp/a%20b');
        out.pathFromUrl = M.fileURLToPath(new URL('file:///tmp/%E4%BD%A0'));
        out.hostError = (function () {
            try { M.fileURLToPath('file://server/x'); return null; } catch (e) { return [e.name, e.code]; }
        })();
        out.schemeError = (function () {
            try { M.fileURLToPath('http://x/'); return null; } catch (e) { return [e.name, e.code]; }
        })();
        out.wrongType = (function () {
            try { M.fileURLToPath(5); return null; } catch (e) { return [e.name, e.code]; }
        })();
        out.pathUrl = M.pathToFileURL('/tmp/a b').href;
        out.pathUrlRelative = (function () {
            try { return M.pathToFileURL('a/b').href; } catch (e) { return [e.name, e.code]; }
        })();
        out.options = M.urlToHttpOptions(new URL('https://u:p@x.y:8080/a?b#c'));
        out.options6 = M.urlToHttpOptions(new URL('https://[::1]:99/x'));
        return JSON.stringify(out);
    })()
    "#;
    let got: String = with_compat(|_, ctx| {
        let module_src =
            crate::jshost::node_module_for_require("node:url").expect("node:url 必须是内置模块");
        ctx.eval::<(), _>(format!(
            "globalThis.__nodeUrlSrc = {};",
            serde_json::to_string(&module_src).unwrap()
        ))
        .expect("模块源码注入要能跑");
        let wrapped = format!(
            "(function () {{ try {{ return ({script}); }} catch (e) {{ return '!JSERR:' + (e && e.stack ? e.stack : String(e)); }} }})()"
        );
        let s: String = ctx.eval(wrapped).expect("测试脚本要能跑");
        assert!(!s.starts_with("!JSERR:"), "兼容层脚本抛错: {s}");
        s
    });
    let v: Value = serde_json::from_str(&got).unwrap();

    assert_eq!(
        v["keys"],
        serde_json::json!([
            "URL",
            "URLSearchParams",
            "fileURLToPath",
            "pathToFileURL",
            "urlToHttpOptions"
        ])
    );
    assert_eq!(v["sameClass"], serde_json::json!(true));
    assert_eq!(v["path"], "/tmp/a b");
    assert_eq!(v["pathFromUrl"], "/tmp/你");
    assert_eq!(
        v["hostError"],
        serde_json::json!(["TypeError", "ERR_INVALID_FILE_URL_HOST"])
    );
    assert_eq!(
        v["schemeError"],
        serde_json::json!(["TypeError", "ERR_INVALID_URL_SCHEME"])
    );
    assert_eq!(
        v["wrongType"],
        serde_json::json!(["TypeError", "ERR_INVALID_ARG_TYPE"])
    );
    assert_eq!(v["pathUrl"], "file:///tmp/a%20b");
    assert_eq!(
        v["pathUrlRelative"],
        serde_json::json!(["TypeError", "ERR_INVALID_ARG_VALUE"])
    );
    assert_eq!(v["options"]["protocol"], "https:");
    assert_eq!(v["options"]["hostname"], "x.y");
    assert_eq!(v["options"]["port"], 8080);
    assert_eq!(v["options"]["auth"], "u:p");
    assert_eq!(v["options"]["path"], "/a?b");
    assert_eq!(v["options6"]["hostname"], "::1");
    assert_eq!(v["options6"]["port"], 99);
}
