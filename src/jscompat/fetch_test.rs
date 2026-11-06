//! fetch / Headers / Request / Response / AbortController 的测试。
//!
//! **完全离线**：一个包都不发。宿主那一层用 `__host_fetch_start` 的桩替换 ——
//! 它只记下调用参数、返回自增 id，然后由测试手动调 `__deliver_fetch` 投递结果。
//! 这正是 engine.rs 里真实投递走的**同一个入口**，所以 fetch.js 里那段
//! 「宿主 JSON → Response」的解析是真在跑，不是被 mock 掉了。
//!
//! 期望值从哪来：都是真的 Node **24.16.0** 跑出来的（见文件末尾「证据」一节，
//! 逐条记了命令）。这个域没有可批量对拍的「值表」（不像 text/crypto 有成千上万的
//! 字节级向量），全是形状与语义断言；唯一那张表是 statusText，它由
//! node:http 的 STATUS_CODES 生成后直接写进 fetch.js。
//!
//! 泵微任务的写法照抄 jshost.rs 的 async 测试：Promise 续体在 job 队列里，
//! 宿主投递完必须 `execute_pending_job()` 才会跑。

use rquickjs::{Context, Runtime};

/// 建一个「BOOTSTRAP + 兼容层」的 Context，并且**允许阶段之间泵微任务**。
///
/// # 为什么不复用 jscompat::with_compat（踩过，写给后来人）
///
/// with_compat 把 Ctx 借给闭包，于是闭包里调 `rt.execute_pending_job()` 时，
/// rquickjs 的 safe_ref 拿不到 runtime 的内部 RefCell，直接 panic：
/// `RefCell already borrowed`（rquickjs-core/src/safe_ref.rs）。
///
/// 纯同步的域（text / url / crypto）用 with_compat 没问题 —— 它们不需要泵 job。
/// 但只要一段 JS **等 Promise 续体**（fetch 的整条链路都是：发起 → 投递 →
/// 泵 → 断言），就必须在每次 ctx.with 之间回到「没有 Ctx 借用」的状态。所以这里
/// 自己持有 Context，按阶段开 ctx.with，pump 放在外面。
///
/// ⚠ 也别想在闭包里再套一层 ctx.with 绕开 —— 同一个 Context 不能嵌套 with。
fn with_fetch_compat<R>(f: impl FnOnce(&Runtime, &Context) -> R) -> R {
    let rt = Runtime::new().expect("建 Runtime");
    let ctx = Context::full(&rt).expect("建 Context");
    ctx.with(|c| {
        c.eval::<(), _>(crate::jshost::BOOTSTRAP)
            .expect("BOOTSTRAP 自身要能跑");
        crate::jscompat::install(&c).expect("兼容层自身要能跑");
    });
    f(&rt, &ctx)
}

/// 宿主桩。必须在**第一次 fetch 之前**装好。
///
/// - `__host_fetch_start(url, opts)`：返回自增 id，并把调用原样记进
///   `__fetch_calls`（插件侧断言「到底发出去了什么」靠它）。真实实现会在这里
///   fork 一条后台线程；测试里我们只关心「参数对不对」。
/// - 定时器表：BOOTSTRAP 的 `setTimeout` 依赖 `__host_timer_set` 返回 id。
///   `AbortSignal.timeout` 用的就是这条路 —— 桩返回自增 id 之后，测试可以调
///   `__fire_timer(id, false)` 手动「到点」（不然一条 10ms 的定时器在单测里
///   要么真等、要么永远不响）。
const HOST_STUBS: &str = r#"
globalThis.__fetch_calls = [];
globalThis.__fetch_seq = 0;
globalThis.__host_fetch_start = function (url, opts) {
    var id = ++globalThis.__fetch_seq;
    globalThis.__fetch_calls.push({ id: id, url: url, opts: JSON.parse(opts) });
    return id;
};
globalThis.__timer_seq = 0;
globalThis.__host_timer_set = function (ms, repeat) {
    globalThis.__timer_seq++;
    return globalThis.__timer_seq;
};
globalThis.__host_timer_clear = function () {};
"#;

/// 泵微任务：fetch 的 Promise 续体排在 job 队列里。
fn pump(rt: &Runtime) {
    while rt.execute_pending_job().unwrap_or(false) {}
}

fn stub_host(ctx: &Context) {
    ctx.with(|c| c.eval::<(), _>(HOST_STUBS))
        .expect("装宿主桩要能跑");
}

fn eval_unit(ctx: &Context, src: &str) {
    ctx.with(|c| c.eval::<(), _>(src))
        .unwrap_or_else(|e| panic!("脚本要能跑: {e}\n{src}"));
}

/// 跑一段 JS，把它的返回值当 JSON 读回来。
///
/// 包一层 try/catch 是**必须**的：直接 ctx.eval 失败时 rquickjs 只给一句
/// "Exception"，等于没说话（text_test 里踩过同样的坑）。
fn eval_json(ctx: &Context, body: &str) -> serde_json::Value {
    let wrapped = format!(
        // ⚠ return 后面必须紧跟左括号：脚本以换行开头时 ASI 会把 return 与下一行拆开，
        // 整个 eval 静默返回 undefined（text_test 里踩过一次）。
        "(function () {{ try {{ return JSON.stringify((function () {{ {body} }})()); }} \
         catch (e) {{ return '!JSERR:' + (e && e.stack ? e.stack : String(e)); }} }})()"
    );
    let got: String = ctx.with(|c| c.eval(wrapped)).expect("测试脚本要能跑");
    assert!(!got.starts_with("!JSERR:"), "被测脚本抛错: {got}\n脚本: {body}");
    serde_json::from_str(&got).unwrap_or_else(|e| panic!("脚本返回值不是 JSON（{e}）: {got}"))
}

/// 手动投递一次宿主结果（== engine.rs 在引擎线程上调的那个入口）。
///
/// payload 用 JSON 再编码一次成 JS 字符串字面量，省得在 Rust 里手写转义。
fn deliver(ctx: &Context, id: i64, ok: bool, payload: &str) {
    let lit = serde_json::to_string(payload).expect("payload 要能序列化");
    let src = format!("__deliver_fetch({id}, {ok}, {lit});");
    ctx.with(|c| c.eval::<(), _>(src.as_str()))
        .unwrap_or_else(|e| panic!("投递 {id} 失败: {e}"));
}

/// 拼一个宿主响应 JSON（do_http_fetch 的输出形状：status / headers / body）。
fn host_response(status: i64, headers: &[(&str, &str)], body: &str) -> String {
    let mut map = serde_json::Map::new();
    for (k, v) in headers {
        map.insert((*k).to_string(), serde_json::Value::String((*v).to_string()));
    }
    serde_json::json!({ "status": status, "headers": map, "body": body }).to_string()
}

// ════════════════════════════════════════════════ fetch 的形状

#[test]
fn fetch_get_goes_through_the_host_pipeline_and_back() {
    with_fetch_compat(|rt, ctx| {
        stub_host(ctx);
        eval_unit(ctx, r#"
            globalThis.__out = null;
            fetch('http://x.test/a').then(function (r) {
                __out = {
                    status: r.status, ok: r.ok, statusText: r.statusText,
                    type: r.type, url: r.url, redirected: r.redirected,
                    ct: r.headers.get('content-type'), xa: r.headers.get('x-a'),
                    tag: Object.prototype.toString.call(r)
                };
                return r.text();
            }).then(function (t) {
                __out.text = t;
            }, function (e) {
                __out = { err: e.name, message: e.message };
            });
        "#);

        // 请求侧：确实只通过 __host_fetch_start 发出去，且 GET 不带 body
        let calls = eval_json(ctx, "return __fetch_calls;");
        assert_eq!(calls[0]["url"], "http://x.test/a", "URL 要原样交给宿主");
        assert_eq!(calls[0]["opts"]["method"], "GET", "默认方法必须是 GET");
        assert_eq!(
            calls[0]["opts"]["headers"].as_object().map(|m| m.len()),
            Some(0),
            "没有头就不该凭空造头：{}",
            calls[0]["opts"]
        );
        assert!(
            calls[0]["opts"].get("body").is_none(),
            "GET 不该带 body 字段：{}",
            calls[0]["opts"]
        );
        assert!(
            calls[0]["opts"].get("timeout_ms").is_none(),
            "没给 timeout_ms 就不要传（宿主自己有默认值）"
        );

        let id = calls[0]["id"].as_i64().expect("id 是数字");
        deliver(
            ctx,
            id,
            true,
            &host_response(200, &[("Content-Type", "application/json"), ("x-a", "b")], r#"{"ok":true}"#),
        );
        pump(rt);

        let out = eval_json(ctx, "return __out;");
        assert_eq!(out["status"], 200);
        assert_eq!(out["ok"], true);
        assert_eq!(out["statusText"], "OK", "200 的标准短语");
        assert_eq!(out["type"], "basic", "网络响应与 Node 一样是 basic");
        assert_eq!(out["url"], "http://x.test/a");
        assert_eq!(out["redirected"], false);
        assert_eq!(out["ct"], "application/json");
        assert_eq!(out["xa"], "b");
        assert_eq!(out["tag"], "[object Response]");
        assert_eq!(out["text"], r#"{"ok":true}"#);
    });
}

#[test]
fn fetch_sends_method_headers_body_and_timeout_through_the_host() {
    with_fetch_compat(|rt, ctx| {
        stub_host(ctx);
        eval_unit(ctx, r#"
            globalThis.__out = null;
            fetch('http://x.test/p', {
                method: 'post',
                headers: [['X-A', 'b'], ['x-a', 'c']],
                body: 'hello',
                timeout_ms: 2500
            }).then(function () { __out = { ok: true }; }, function (e) { __out = { err: e.message }; });
        "#);

        let calls = eval_json(ctx, "return __fetch_calls;");
        let opts = &calls[0]["opts"];
        assert_eq!(opts["method"], "POST", "方法要按 Node 的规矩大写");
        assert_eq!(opts["body"], "hello");
        assert_eq!(opts["timeout_ms"], 2500, "timeout_ms 必须原样透传给宿主");
        // 同名头在宿主那边只能有一个值（JSON 对象），按 ', ' 合并 —— 与 Node 发请求一致
        assert_eq!(opts["headers"]["x-a"], "b, c");
        // Node 会给文本 body 补 content-type；宿主不会，所以这层必须补
        assert_eq!(opts["headers"]["content-type"], "text/plain;charset=UTF-8");

        let id = calls[0]["id"].as_i64().unwrap();
        deliver(ctx, id, true, &host_response(201, &[], "made"));
        pump(rt);
        let out = eval_json(ctx, "return __out;");
        assert_eq!(out["ok"], true, "{out}");
    });
}

#[test]
fn fetch_wraps_host_failures_in_fetch_failed_with_a_cause() {
    with_fetch_compat(|rt, ctx| {
        stub_host(ctx);
        eval_unit(ctx, r#"
            globalThis.__out = null;
            fetch('http://x.test/boom').then(function () {
                __out = { ok: true };
            }, function (e) {
                __out = {
                    name: e.name, message: e.message,
                    isTypeError: e instanceof TypeError,
                    cause: String(e.cause && e.cause.message)
                };
            });
        "#);
        let id = eval_json(ctx, "return __fetch_calls[0].id;").as_i64().unwrap();
        deliver(ctx, id, false, "请求失败: 连不上 127.0.0.1:1");
        pump(rt);

        let out = eval_json(ctx, "return __out;");
        assert_eq!(out["name"], "TypeError", "Node 的网络错误就是 TypeError: {out}");
        assert_eq!(out["isTypeError"], true);
        assert_eq!(out["message"], "fetch failed", "文案必须与 Node 一样");
        assert!(
            out["cause"].as_str().unwrap().contains("连不上"),
            "宿主的原因必须挂在 cause 上（不是摊平进 message）: {out}"
        );
    });
}

#[test]
fn fetch_reports_the_concurrency_cap_as_fetch_failed() {
    with_fetch_compat(|rt, ctx| {
        stub_host(ctx);
        // Host 侧单 worker 最多 32 条在途；到顶时 __host_fetch_start 返回 0，
        // fetch_async 立刻 reject。fetch 这一层必须把它包成 Node 的 fetch failed。
        eval_unit(ctx, r#"
            globalThis.__host_fetch_start = function () { return 0; };
            globalThis.__out = null;
            fetch('http://x.test/a').then(function () {
                __out = { ok: true };
            }, function (e) {
                __out = { name: e.name, message: e.message, cause: String(e.cause && e.cause.message) };
            });
        "#);
        pump(rt);
        let out = eval_json(ctx, "return __out;");
        assert_eq!(out["name"], "TypeError");
        assert_eq!(out["message"], "fetch failed");
        assert!(
            out["cause"].as_str().unwrap().contains("并发请求太多"),
            "要保留宿主那句「并发请求太多」，否则看不出是限流: {out}"
        );
    });
}

#[test]
fn fetch_removes_its_abort_listener_when_the_request_settles() {
    with_fetch_compat(|rt, ctx| {
        stub_host(ctx);
        // 泄漏检查：每发一次带 signal 的请求就挂一个监听器，settle 后不摘掉的话，
        // 一个长命的 AbortSignal（比如插件级、给所有请求共用的那个）会越积越多。
        eval_unit(ctx, r#"
            globalThis.__out = {};
            globalThis.__c = new AbortController();
            fetch('http://x.test/a', { signal: __c.signal }).then(function (r) {
                __out.status = r.status;
            }, function (e) { __out.err = e.message; });
            __out.waiting = __c.signal._listeners.length;
        "#);
        assert_eq!(eval_json(ctx, "return __out.waiting;"), 1, "等待期间应该挂着 1 个 abort 监听器");
        let id = eval_json(ctx, "return __fetch_calls[0].id;").as_i64().unwrap();
        deliver(ctx, id, true, &host_response(200, &[], "ok"));
        pump(rt);
        let out = eval_json(ctx, "return { status: __out.status, listeners: __c.signal._listeners.length };");
        assert_eq!(out["status"], 200);
        assert_eq!(out["listeners"], 0, "settle 之后必须摘掉监听器");
    });
}

#[test]
fn fetch_rejects_invalid_urls_without_touching_the_host() {
    with_fetch_compat(|rt, ctx| {
        stub_host(ctx);
        // Node: fetch('not a url') 是 **reject**（不是同步 throw），
        // 且 message 以 'Failed to parse URL from ' 开头，cause.code = ERR_INVALID_URL
        eval_unit(ctx, r#"
            globalThis.__out = null;
            var p;
            try { p = fetch('not a url'); }
            catch (e) { __out = { sync: true, name: e.name }; }
            if (!__out) {
                p.then(function () { __out = { ok: true }; }, function (e) {
                    __out = {
                        name: e.name, message: e.message,
                        code: e.cause && e.cause.code,
                        input: e.cause && e.cause.input
                    };
                });
            }
        "#);
        pump(rt);
        let out = eval_json(ctx, "return __out;");
        assert_eq!(out["name"], "TypeError");
        assert_eq!(out["message"], "Failed to parse URL from not a url");
        assert_eq!(out["code"], "ERR_INVALID_URL");
        assert_eq!(out["input"], "not a url");
        let calls = eval_json(ctx, "return __fetch_calls;");
        assert_eq!(calls.as_array().map(|a| a.len()), Some(0), "URL 非法就不该发请求");
    });
}

#[test]
fn binary_request_bodies_are_rejected_with_an_explanation() {
    with_fetch_compat(|rt, ctx| {
        stub_host(ctx);
        eval_unit(ctx, r#"
            globalThis.__out = { sync: [], async: [] };
            [new Uint8Array([1, 2]), new ArrayBuffer(2), new DataView(new ArrayBuffer(1))].forEach(function (b) {
                try { new Request('http://x.test/', { method: 'POST', body: b }); __out.sync.push('no throw'); }
                catch (e) { __out.sync.push([e.name, e.message]); }
            });
            fetch('http://x.test/', { method: 'POST', body: new Uint8Array([1]) }).then(function () {
                __out.async.push('no throw');
            }, function (e) {
                __out.async.push([e.name, e.message]);
            });
        "#);
        pump(rt);
        let out = eval_json(ctx, "return __out;");
        let sync = out["sync"].as_array().unwrap();
        assert_eq!(sync.len(), 3);
        for item in sync {
            let pair = item.as_array().unwrap();
            assert_eq!(pair[0], "TypeError", "二进制 body 必须是 TypeError");
            let msg = pair[1].as_str().unwrap();
            assert!(msg.contains("只支持文本"), "文案要讲清楚为什么: {msg}");
            assert!(msg.contains("静默损坏"), "要说清后果（宿主会静默损坏）: {msg}");
        }
        let async_first = &out["async"][0];
        assert_eq!(async_first[0], "TypeError", "fetch 侧同样要 reject（不是 throw）");
        let calls = eval_json(ctx, "return __fetch_calls;");
        assert_eq!(calls.as_array().map(|a| a.len()), Some(0), "二进制 body 绝不能发出去");
    });
}

#[test]
fn get_and_head_with_a_body_match_nodes_error() {
    with_fetch_compat(|rt, ctx| {
        stub_host(ctx);
        eval_unit(ctx, r#"
            globalThis.__out = { sync: null, async: null };
            try { new Request('http://x.test/', { method: 'GET', body: 'a' }); }
            catch (e) { __out.sync = [e.name, e.message]; }
            fetch('http://x.test/', { method: 'HEAD', body: 'a' }).then(function () {
                __out.async = 'no throw';
            }, function (e) {
                __out.async = [e.name, e.message];
            });
        "#);
        pump(rt);
        let out = eval_json(ctx, "return __out;");
        assert_eq!(out["sync"][0], "TypeError");
        assert_eq!(out["sync"][1], "Request with GET/HEAD method cannot have body.");
        assert_eq!(out["async"][0], "TypeError", "fetch 侧 reject 而不是 throw");
        assert_eq!(out["async"][1], "Request with GET/HEAD method cannot have body.");
        let calls = eval_json(ctx, "return __fetch_calls;");
        assert_eq!(calls.as_array().map(|a| a.len()), Some(0));
    });
}

#[test]
fn redirect_only_supports_follow_and_says_why() {
    with_fetch_compat(|rt, ctx| {
        stub_host(ctx);
        eval_unit(ctx, r#"
            globalThis.__out = { manual: null, error: null, follow: null };
            fetch('http://x.test/r', { redirect: 'manual' }).then(function () {
                __out.manual = 'no throw';
            }, function (e) { __out.manual = [e.name, e.message]; });
            fetch('http://x.test/r', { redirect: 'error' }).then(function () {
                __out.error = 'no throw';
            }, function (e) { __out.error = [e.name, e.message]; });
            fetch('http://x.test/r', { redirect: 'follow' }).then(function (r) {
                __out.follow = r.status;
            }, function (e) { __out.follow = 'rejected'; });
        "#);
        let calls = eval_json(ctx, "return __fetch_calls;");
        assert_eq!(
            calls.as_array().map(|a| a.len()),
            Some(1),
            "只有 follow 该真的发出去；manual/error 在参数校验阶段就拒绝"
        );
        let id = calls[0]["id"].as_i64().unwrap();
        deliver(ctx, id, true, &host_response(200, &[], "ok"));
        pump(rt);

        let out = eval_json(ctx, "return __out;");
        for key in ["manual", "error"] {
            assert_eq!(out[key][0], "TypeError", "{key} 必须是 TypeError");
            let msg = out[key][1].as_str().unwrap();
            assert!(msg.contains("只支持 'follow'"), "{key} 的文案要讲清楚限制: {msg}");
        }
        assert_eq!(out["follow"], 200, "follow 是默认，必须能用");
    });
}

// ════════════════════════════════════════════════ abort

#[test]
fn fetching_with_an_already_aborted_signal_never_reaches_the_host() {
    with_fetch_compat(|rt, ctx| {
        stub_host(ctx);
        eval_unit(ctx, r#"
            globalThis.__out = null;
            var c = new AbortController();
            c.abort();
            fetch('http://x.test/a', { signal: c.signal }).then(function () {
                __out = { ok: true };
            }, function (e) {
                __out = {
                    name: e.name, message: e.message, code: e.code,
                    isDOMException: e instanceof DOMException
                };
            });
        "#);
        pump(rt);

        let out = eval_json(ctx, "return __out;");
        assert_eq!(out["name"], "AbortError", "插件就是靠 e.name === 'AbortError' 判的");
        assert_eq!(out["message"], "This operation was aborted");
        assert_eq!(out["code"], 20, "AbortError 的 DOMException code 是 20");
        assert_eq!(out["isDOMException"], true);
        let calls = eval_json(ctx, "return __fetch_calls;");
        assert_eq!(calls.as_array().map(|a| a.len()), Some(0), "已经 abort 了就不该发请求");
    });
}

#[test]
fn aborting_mid_flight_rejects_and_ignores_the_late_result() {
    with_fetch_compat(|rt, ctx| {
        stub_host(ctx);
        eval_unit(ctx, r#"
            globalThis.__out = null;
            globalThis.__c = new AbortController();
            fetch('http://x.test/slow', { signal: __c.signal }).then(function () {
                __out = { ok: true };
            }, function (e) {
                __out = { name: e.name, message: e.message };
            });
        "#);
        let id = eval_json(ctx, "return __fetch_calls[0].id;").as_i64().unwrap();
        eval_unit(ctx, "__c.abort();");
        pump(rt);
        assert_eq!(eval_json(ctx, "return __out;")["name"], "AbortError");

        // 宿主请求不可取消：结果迟到是**常态**，必须被安静吃掉，
        // 不能翻转已经 settle 的 Promise，也不能把 __out 改掉。
        deliver(ctx, id, true, &host_response(200, &[], "late"));
        pump(rt);
        let out = eval_json(ctx, "return __out;");
        assert_eq!(out["name"], "AbortError", "迟到的结果不能覆盖已经拒绝的状态: {out}");
        // 不存在的 id：宿主据此只记 debug，不能当成"投递成功"
        let handled: bool = ctx.with(|c| c.eval("__deliver_fetch(999999, true, '{}')")).unwrap();
        assert!(!handled, "不存在的 id 应该返回 false（宿主据此只记 debug）");
    });
}

#[test]
fn abort_controller_and_signal_follow_the_dom_shape() {
    with_fetch_compat(|_, ctx| {
        eval_unit(ctx, r#"
            globalThis.__out = {};
            var c = new AbortController();
            var log = [];
            var removed = function () { log.push('removed'); };
            c.signal.addEventListener('abort', removed);
            c.signal.removeEventListener('abort', removed);
            c.signal.addEventListener('abort', function (e) {
                log.push(['listener', e.type, this === c.signal, Object.prototype.toString.call(e)]);
            });
            c.signal.addEventListener('abort', function () { log.push('once'); }, { once: true });
            c.signal.onabort = function (e) { log.push(['onabort', e.type]); };
            __out.before = [c.signal.aborted, c.signal.reason === undefined, c.signal.reason];
            c.abort();
            c.abort();   // 幂等：只有一个事件
            __out.log = log;
            __out.after = [c.signal.aborted, c.signal.reason.name, c.signal.reason.code];
            // abort 之后再加的监听器不该被调用
            var late = 0;
            c.signal.addEventListener('abort', function () { late++; });
            __out.lateListeners = late;
            // throwIfAborted：抛的是 reason 本身
            try { c.signal.throwIfAborted(); __out.threw = 'no throw'; }
            catch (e) { __out.threw = [e.name, e.message]; }
            try { new AbortController().signal.throwIfAborted(); __out.threwClean = 'ok'; }
            catch (e) { __out.threwClean = 'threw'; }
            __out.custom = AbortSignal.abort('why').reason;
            __out.customThrows = (function () {
                try { AbortSignal.abort('why').throwIfAborted(); } catch (e) { return e; }
                return 'no throw';
            })();
        "#);
        let out = eval_json(ctx, "return __out;");
        assert_eq!(out["before"][0], false);
        assert_eq!(out["before"][1], true, "没 abort 时 reason 应该是 undefined");
        let log = out["log"].as_array().unwrap();
        let flat: Vec<String> = log.iter().map(|v| v.to_string()).collect();
        assert!(
            !flat.iter().any(|s| s.contains("removed")),
            "removeEventListener 之后不该再被调用: {flat:?}"
        );
        assert!(flat.iter().any(|s| s.contains("listener") && s.contains("[object Event]")));
        assert!(flat.iter().any(|s| s.contains("once")));
        assert!(flat.iter().any(|s| s.contains("onabort")));
        assert_eq!(log.len(), 3, "只该有 3 条（listener / once / onabort），重复 abort 不重发: {flat:?}");
        assert_eq!(out["after"][0], true);
        assert_eq!(out["after"][1], "AbortError");
        assert_eq!(out["after"][2], 20);
        assert_eq!(out["lateListeners"], 0);
        assert_eq!(out["threw"][0], "AbortError");
        assert_eq!(out["threwClean"], "ok");
        assert_eq!(out["custom"], "why", "自定义 reason 原样保留");
        assert_eq!(out["customThrows"], "why", "throwIfAborted 抛的就是 reason 本身");
    });
}

#[test]
fn abort_signal_timeout_rides_on_the_host_timer_table() {
    with_fetch_compat(|_rt, ctx| {
        stub_host(ctx);
        eval_unit(ctx, r#"
            globalThis.__out = {};
            globalThis.__timed = AbortSignal.timeout(10);
            globalThis.__timerId = __timer_seq;
            __out.before = [__timed.aborted, __timed.reason === undefined];
            __out.tag = Object.prototype.toString.call(__timed);
        "#);
        // 手动「到点」：桩不会自己响，宿主真实环境里是 fire_timers 调过来的
        eval_unit(ctx, "__fire_timer(__timerId, false);");
        eval_unit(ctx, "globalThis.__out.after = [__timed.aborted, __timed.reason.name, __timed.reason.message];");
        let out = eval_json(ctx, "return __out;");
        assert_eq!(out["before"][0], false);
        assert_eq!(out["tag"], "[object AbortSignal]");
        assert_eq!(out["after"][0], true);
        assert_eq!(out["after"][1], "TimeoutError", "Node 的 AbortSignal.timeout 用 TimeoutError");
        assert_eq!(out["after"][2], "The operation was aborted due to timeout");
    });
}

#[test]
fn a_throwing_abort_listener_does_not_break_abort() {
    with_fetch_compat(|_, ctx| {
        stub_host(ctx);
        // Node 把监听器里的异常当「未捕获异常」异步报出来，abort() 本身不抛
        // （实测：abort() 返回后再在 nextTick 里冒）。
        eval_unit(ctx, r#"
            globalThis.__out = {};
            var c = new AbortController();
            c.signal.addEventListener('abort', function () { throw new Error('listener boom'); });
            try { c.abort(); __out.threw = false; } catch (e) { __out.threw = true; }
            __out.aborted = c.signal.aborted;
        "#);
        let out = eval_json(ctx, "return __out;");
        assert_eq!(out["threw"], false, "abort() 不能被监听器的异常带崩");
        assert_eq!(out["aborted"], true);
    });
}

// ════════════════════════════════════════════════ Headers

#[test]
fn headers_are_case_insensitive_joined_deduped_and_sorted() {
    with_fetch_compat(|_, ctx| {
        eval_unit(ctx, r#"
            globalThis.__out = {};
            var h = new Headers([['b', '2'], ['A', '1'], ['a', '3']]);
            var keys = [], entries = [], seen = [];
            for (var k of h.keys()) keys.push(k);
            for (var pair of h.entries()) entries.push(pair);
            h.forEach(function (v, k, obj) { seen.push([k, v, obj === h]); });
            __out.keys = keys;
            __out.entries = entries;
            __out.seen = seen;
            __out.getA = h.get('a');
            __out.getMissing = h.get('nope');
            __out.hasUpper = h.has('B');
            __out.hasMissing = h.has('zzz');
            __out.spread = (function () { var out = []; for (var p of h) out.push(p); return out; })();
            // set 保留原位（undici：[[a,1],[b,2]] set a 之后顺序还是 a,b）
            var h2 = new Headers([['a', '1'], ['b', '2'], ['a', '3']]);
            h2.set('a', '9');
            __out.afterSet = (function () { var out = []; for (var p of h2) out.push(p); return out; })();
            var h3 = new Headers([['a', '1'], ['b', '2']]);
            h3.delete('a'); h3.append('a', '9');
            __out.afterDeleteAppend = (function () { var out = []; for (var p of h3) out.push(p); return out; })();
            var h4 = new Headers({ a: ['1', '2'] });
            __out.recordArray = h4.get('a');
            var h5 = new Headers(new Headers({ x: 'y' }));
            __out.fromHeaders = h5.get('x');
            var h6 = new Headers(new Map([['m', 'n']]));
            __out.fromMap = h6.get('m');
            __out.count = (function () { var n = 0; for (var p of h) n++; return n; })();
        "#);
        let out = eval_json(ctx, "return __out;");
        assert_eq!(out["keys"], serde_json::json!(["a", "b"]), "迭代要按名字字典序且**去重**");
        assert_eq!(
            out["entries"],
            serde_json::json!([["a", "1, 3"], ["b", "2"]]),
            "同名多值用 ', ' 合并成一个条目"
        );
        assert_eq!(out["seen"], serde_json::json!([["a", "1, 3", true], ["b", "2", true]]));
        assert_eq!(out["getA"], "1, 3");
        assert_eq!(out["getMissing"], serde_json::Value::Null, "缺失要返回 null 而不是 undefined");
        assert_eq!(out["hasUpper"], true, "头名大小写不敏感");
        assert_eq!(out["hasMissing"], false);
        assert_eq!(out["spread"], out["entries"], "[Symbol.iterator] 就是 entries()");
        assert_eq!(out["afterSet"], serde_json::json!([["a", "9"], ["b", "2"]]), "set 保留首次出现的位置");
        assert_eq!(out["afterDeleteAppend"], serde_json::json!([["a", "9"], ["b", "2"]]));
        assert_eq!(out["recordArray"], "1,2", "对象里的数组值按 undici 的规矩用 ',' 连");
        assert_eq!(out["fromHeaders"], "y");
        assert_eq!(out["fromMap"], "n");
        assert_eq!(out["count"], 2);
    });
}

#[test]
fn headers_reject_invalid_names_and_values_with_nodes_messages() {
    with_fetch_compat(|_, ctx| {
        eval_unit(ctx, r#"
            globalThis.__out = {};
            function grab(fn) {
                try { fn(); return 'no throw'; }
                catch (e) { return [e.name, e.message]; }
            }
            __out.badName = grab(function () { new Headers([['bad name', 'v']]); });
            __out.badValue = grab(function () { new Headers({ x: 'a\nb' }); });
            __out.getBad = grab(function () { new Headers().get('bad name'); });
            __out.setBad = grab(function () { new Headers().set('x@y', 'v'); });
            __out.deleteBad = grab(function () { new Headers().delete('a b'); });
        "#);
        let out = eval_json(ctx, "return __out;");
        for key in ["badName", "badValue", "getBad", "setBad", "deleteBad"] {
            assert_eq!(out[key][0], "TypeError", "{key} 必须是 TypeError: {out}");
            let msg = out[key][1].as_str().unwrap();
            assert!(
                msg.contains("is an invalid header name.") || msg.contains("is an invalid header value."),
                "{key} 的文案要照 Node: {msg}"
            );
        }
        assert_eq!(out["getBad"][1], r#"Headers.get: "bad name" is an invalid header name."#);
        assert_eq!(out["setBad"][1], r#"Headers.set: "x@y" is an invalid header name."#);
    });
}

// ════════════════════════════════════════════════ Response

#[test]
fn response_readers_return_bytes_text_and_json() {
    with_fetch_compat(|rt, ctx| {
        eval_unit(ctx, r#"
            globalThis.__out = {};
            var r1 = new Response('h\u00e9llo');
            r1.text().then(function (t) {
                __out.text = t;
                __out.usedAfterText = r1.bodyUsed;
                return new Response('{"a":1}').json();
            }).then(function (v) {
                __out.json = v;
                return new Response('hi').bytes();
            }).then(function (b) {
                __out.bytes = [b.length, b[0], b[1]];
                return new Response('hi').arrayBuffer();
            }).then(function (ab) {
                __out.abLength = ab.byteLength;
                return new Response(null).text();
            }).then(function (t) {
                __out.empty = t;
                return new Response(new Uint8Array([1, 2])).bytes();
            }).then(function (b) {
                __out.fromBytes = [b.length, b[0], b[1]];
                __out.done = true;
            }, function (e) {
                __out.err = [e.name, e.message];
            });
        "#);
        pump(rt);
        let out = eval_json(ctx, "return __out;");
        assert_eq!(out["text"], "héllo", "UTF-8 往返要正确");
        assert_eq!(out["usedAfterText"], true);
        assert_eq!(out["json"], serde_json::json!({"a": 1}));
        assert_eq!(out["bytes"], serde_json::json!([2, 104, 105]));
        assert_eq!(out["abLength"], 2);
        assert_eq!(out["empty"], "");
        assert_eq!(out["fromBytes"], serde_json::json!([2, 1, 2]), "Response 侧允许字节 body（不经宿主）");
        assert_eq!(out["done"], true, "{out}");
    });
}

#[test]
fn response_body_can_only_be_read_once() {
    with_fetch_compat(|rt, ctx| {
        eval_unit(ctx, r#"
            globalThis.__out = {};
            var r = new Response('x');
            r.text().then(function () {
                return r.text().then(function () { return 'no throw'; },
                    function (e) { return [e.name, e.message]; });
            }).then(function (second) {
                __out.second = second;
                var r2 = new Response('y');
                return r2.text().then(function () {
                    try { r2.clone(); return 'no throw'; }
                    catch (e) { return [e.name, e.message]; }
                });
            }).then(function (cloneErr) {
                __out.clone = cloneErr;
                __out.done = true;
            }, function (e) {
                __out.err = [e.name, e.message];
            });
        "#);
        pump(rt);
        let out = eval_json(ctx, "return __out;");
        assert_eq!(out["second"][0], "TypeError");
        assert_eq!(out["second"][1], "Body is unusable: Body has already been read", "与 Node 同一句");
        assert_eq!(out["clone"][0], "TypeError");
        assert_eq!(out["clone"][1], "Response.clone: Body has already been consumed.");
        assert_eq!(out["done"], true, "{out}");
    });
}

#[test]
fn response_clone_reads_independently() {
    with_fetch_compat(|rt, ctx| {
        eval_unit(ctx, r#"
            globalThis.__out = {};
            var r = new Response('hi', { headers: { 'x-a': '1' } });
            var c = r.clone();
            __out.beforeUsed = [r.bodyUsed, c.bodyUsed];
            __out.sameHeaders = r.headers === c.headers;
            Promise.all([r.text(), c.text()]).then(function (v) {
                __out.texts = v;
                __out.after = [r.bodyUsed, c.bodyUsed];
                __out.cloneHeader = c.headers.get('x-a');
            }, function (e) { __out.err = e.message; });
        "#);
        pump(rt);
        let out = eval_json(ctx, "return __out;");
        assert_eq!(out["beforeUsed"], serde_json::json!([false, false]));
        assert_eq!(out["sameHeaders"], false, "clone 的头是独立对象");
        assert_eq!(out["texts"], serde_json::json!(["hi", "hi"]));
        assert_eq!(out["after"], serde_json::json!([true, true]));
        assert_eq!(out["cloneHeader"], "1");
    });
}

#[test]
fn response_headers_are_read_only() {
    with_fetch_compat(|_, ctx| {
        eval_unit(ctx, r#"
            globalThis.__out = {};
            function grab(fn) {
                try { fn(); return 'no throw'; }
                catch (e) { return [e.name, e.message]; }
            }
            var r = new Response('x');
            __out.set = grab(function () { r.headers.set('a', 'b'); });
            __out.append = grab(function () { r.headers.append('a', 'b'); });
            __out.del = grab(function () { r.headers.delete('a'); });
            __out.readStillWorks = r.headers.get('content-type');
            // Request 的头是**可改**的（Node 如此），别把两边写成一样
            var rq = new Request('http://x.test/');
            rq.headers.set('a', 'b');
            __out.requestMutable = rq.headers.get('a');
        "#);
        let out = eval_json(ctx, "return __out;");
        for key in ["set", "append", "del"] {
            assert_eq!(out[key][0], "TypeError", "{key}: {out}");
            assert_eq!(out[key][1], "immutable", "undici 的文案就是一个 immutable");
        }
        assert_eq!(out["readStillWorks"], "text/plain;charset=UTF-8");
        assert_eq!(out["requestMutable"], "b");
    });
}

#[test]
fn response_json_static_method_builds_a_json_response() {
    with_fetch_compat(|rt, ctx| {
        eval_unit(ctx, r#"
            globalThis.__out = {};
            var r = Response.json({ a: 1 });
            __out.status = r.status;
            __out.ct = r.headers.get('content-type');
            var r2 = Response.json({ b: 2 }, { status: 201, headers: { 'x-a': 'b' } });
            __out.status2 = r2.status;
            __out.xa = r2.headers.get('x-a');
            r.text().then(function (t) { __out.text = t; __out.done = true; });
            try { Response.json(undefined); __out.undef = 'no throw'; }
            catch (e) { __out.undef = [e.name, e.message]; }
        "#);
        pump(rt);
        let out = eval_json(ctx, "return __out;");
        assert_eq!(out["status"], 200);
        assert_eq!(out["ct"], "application/json");
        assert_eq!(out["status2"], 201);
        assert_eq!(out["xa"], "b");
        assert_eq!(out["text"], r#"{"a":1}"#);
        assert_eq!(out["undef"], serde_json::json!(["TypeError", "Value is not JSON serializable"]));
        assert_eq!(out["done"], true);
    });
}

#[test]
fn response_constructor_validates_status_and_status_text() {
    with_fetch_compat(|_, ctx| {
        eval_unit(ctx, r#"
            globalThis.__out = {};
            function grab(fn) {
                try { return ['ok', fn()]; }
                catch (e) { return [e.name, e.message]; }
            }
            __out.low = grab(function () { new Response('', { status: 99 }); });
            __out.high = grab(function () { new Response('', { status: 600 }); });
            __out.coerced = new Response('', { status: '200' }).status;
            __out.noBody204 = grab(function () { new Response('x', { status: 204 }); });
            __out.noBody304 = grab(function () { new Response('x', { status: 304 }); });
            __out.nullBody204 = grab(function () { return new Response(null, { status: 204 }).status; });
            __out.badStatusText = grab(function () { new Response('', { status: 200, statusText: 'a\nb' }); });
            __out.defaults = [new Response('').status, new Response('').statusText, new Response('').type,
                              new Response('').url, new Response('').redirected];
            var e = Response.error();
            __out.err = [e.status, e.type, e.ok];
        "#);
        let out = eval_json(ctx, "return __out;");
        assert_eq!(out["low"][0], "RangeError");
        assert_eq!(out["low"][1], "init[\"status\"] must be in the range of 200 to 599, inclusive.");
        assert_eq!(out["high"][0], "RangeError");
        assert_eq!(out["coerced"], 200, "status 是字符串时按数字转（Node 如此）");
        assert_eq!(out["noBody204"][1], "Response constructor: Invalid response status code 204");
        assert_eq!(out["noBody304"][1], "Response constructor: Invalid response status code 304");
        assert_eq!(out["nullBody204"], serde_json::json!(["ok", 204]), "204 配 null body 是合法的");
        assert_eq!(out["badStatusText"][1], "Invalid statusText");
        assert_eq!(
            out["defaults"],
            serde_json::json!([200, "", "default", "", false]),
            "自己 new 出来的 Response：statusText 是 ''（Node 实测），type 是 default"
        );
        assert_eq!(out["err"], serde_json::json!([0, "error", false]));
    });
}

#[test]
fn network_status_text_comes_from_the_node_table() {
    with_fetch_compat(|rt, ctx| {
        stub_host(ctx);
        eval_unit(ctx, r#"
            globalThis.__rows = [];
            function grab(url) {
                return fetch(url).then(function (r) { __rows.push([r.status, r.statusText, r.ok]); });
            }
            grab('http://x.test/1'); grab('http://x.test/2');
            grab('http://x.test/3'); grab('http://x.test/4');
        "#);
        let ids: Vec<i64> = eval_json(ctx, "return __fetch_calls.map(function (c) { return c.id; });")
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_i64().unwrap())
            .collect();
        assert_eq!(ids.len(), 4);
        // 状态码来自 Node 的 http.STATUS_CODES；418 是 "I'm a Teapot"，599 不在表里
        for (id, status) in ids.iter().zip([200i64, 404, 418, 599]) {
            deliver(ctx, *id, true, &host_response(status, &[], "x"));
        }
        pump(rt);
        let rows = eval_json(ctx, "return __rows;");
        assert_eq!(rows[0], serde_json::json!([200, "OK", true]));
        assert_eq!(rows[1], serde_json::json!([404, "Not Found", false]));
        assert_eq!(rows[2], serde_json::json!([418, "I'm a Teapot", false]), "418 的短语照 Node 表（4xx 所以 ok=false）");
        assert_eq!(rows[3], serde_json::json!([599, "", false]), "表里没有的码给 ''");
    });
}

// ════════════════════════════════════════════════ Request

#[test]
fn request_normalises_method_headers_body_and_signal() {
    with_fetch_compat(|_, ctx| {
        eval_unit(ctx, r#"
            globalThis.__out = {};
            var a = new Request('http://x.test/p', {
                method: 'post', body: 'hi', headers: { 'X-A': 'b' }
            });
            var b = new Request(a);
            __out.a = [a.method, a.url, a.body, a.headers.get('content-type'), a.headers.get('x-a')];
            __out.b = [b.method, b.url, b.body, b.headers.get('x-a')];
            __out.sameSignal = a.signal === b.signal;
            __out.tag = Object.prototype.toString.call(a);
            var c = a.clone();
            __out.clone = [c.method, c.body, c.headers === a.headers];
            __out.defaults = [new Request('http://x.test/').method, new Request('http://x.test/').body];
            // URLSearchParams 的鸭子类型：url.js 不一定在（单测里就不在），
            // 靠 Symbol.toStringTag 认出来；content-type 要换成 form 的那条
            var usp = { toString: function () { return 'a=1&b=2'; } };
            usp[Symbol.toStringTag] = 'URLSearchParams';
            var d = new Request('http://x.test/form', { method: 'POST', body: usp });
            __out.usp = [d.body, d.headers.get('content-type')];
            // input 可以是任何带 href 的东西（URL 实例的形状）
            __out.hrefInput = new Request({ href: 'http://x.test/href' }).url;
        "#);
        let out = eval_json(ctx, "return __out;");
        assert_eq!(
            out["a"],
            serde_json::json!(["POST", "http://x.test/p", "hi", "text/plain;charset=UTF-8", "b"])
        );
        assert_eq!(
            out["b"],
            serde_json::json!(["POST", "http://x.test/p", "hi", "b"]),
            "从 Request 复制要带上头和 body"
        );
        assert_eq!(out["sameSignal"], true, "Node 的 Request 复制共享同一个 signal");
        assert_eq!(out["tag"], "[object Request]");
        assert_eq!(out["clone"], serde_json::json!(["POST", "hi", false]));
        assert_eq!(out["defaults"], serde_json::json!(["GET", null]));
        assert_eq!(
            out["usp"],
            serde_json::json!(["a=1&b=2", "application/x-www-form-urlencoded;charset=UTF-8"])
        );
        assert_eq!(out["hrefInput"], "http://x.test/href");
    });
}

#[test]
fn fetch_accepts_a_request_object_and_url_like_input() {
    with_fetch_compat(|rt, ctx| {
        stub_host(ctx);
        eval_unit(ctx, r#"
            globalThis.__out = null;
            globalThis.__out2 = null;
            var rq = new Request('http://x.test/q', {
                method: 'POST', body: 'payload', headers: { 'x-a': 'b' }
            });
            fetch(rq).then(function (r) { __out = r.status; }, function (e) { __out = { err: e.message }; });
            fetch({ href: 'http://x.test/href' }).then(function (r) { __out2 = r.status; },
                                                       function (e) { __out2 = { err: e.message }; });
        "#);
        let calls = eval_json(ctx, "return __fetch_calls;");
        assert_eq!(calls[0]["url"], "http://x.test/q");
        assert_eq!(calls[0]["opts"]["method"], "POST");
        assert_eq!(calls[0]["opts"]["body"], "payload");
        assert_eq!(calls[1]["url"], "http://x.test/href", "带 href 的对象要能用（URL 实例的形状）");
        assert_eq!(calls[1]["opts"]["method"], "GET");

        deliver(ctx, calls[0]["id"].as_i64().unwrap(), true, &host_response(200, &[], "one"));
        deliver(ctx, calls[1]["id"].as_i64().unwrap(), true, &host_response(204, &[], ""));
        pump(rt);
        assert_eq!(eval_json(ctx, "return __out;"), 200);
        assert_eq!(eval_json(ctx, "return __out2;"), 204);
    });
}

#[test]
fn fetch_works_with_the_real_url_and_urlsearchparams_objects() {
    // 集成点：url.js 与 fetch.js 是两个人分别实现的，这条测试盯的是**两者拼得上**。
    // 只用公有形状（URL 实例的 href、URLSearchParams 的 toString），不碰内部字段。
    with_fetch_compat(|rt, ctx| {
        stub_host(ctx);
        eval_unit(ctx, r#"
            globalThis.__out = null;
            var u = new URL('http://x.test/api?q=1');
            var q = new URLSearchParams({ a: '1', b: '2' });
            var rq = new Request(u, { method: 'POST', body: q });
            __out = [rq.url, rq.body, rq.headers.get('content-type')];
            fetch(u, { method: 'POST', body: q }).then(function (r) { __out.push(r.status); },
                                                        function (e) { __out.push('ERR:' + e.message); });
        "#);
        let calls = eval_json(ctx, "return __fetch_calls;");
        assert_eq!(calls[0]["url"], "http://x.test/api?q=1", "URL 实例要按 href 取");
        assert_eq!(calls[0]["opts"]["body"], "a=1&b=2", "URLSearchParams 要序列化后发出");
        assert_eq!(
            calls[0]["opts"]["headers"]["content-type"],
            "application/x-www-form-urlencoded;charset=UTF-8"
        );
        let out = eval_json(ctx, "return __out;");
        assert_eq!(out[0], "http://x.test/api?q=1");
        assert_eq!(out[1], "a=1&b=2");
        assert_eq!(out[2], "application/x-www-form-urlencoded;charset=UTF-8");

        deliver(ctx, calls[0]["id"].as_i64().unwrap(), true, &host_response(201, &[], "created"));
        pump(rt);
        assert_eq!(eval_json(ctx, "return __out[3];"), 201, "整条链要能走通");
    });
}

// ════════════════════════════════════════════════ 全局安装

#[test]
fn compat_globals_are_writable_configurable_but_not_enumerable() {
    with_fetch_compat(|_, ctx| {
        let out = eval_json(ctx, r#"
            var names = ['Headers', 'Request', 'Response', 'AbortController', 'AbortSignal', 'fetch'];
            var out = { missing: [], enumerable: [], writable: [], configurable: [], tags: [], types: [] };
            var tagSamples = {
                Headers: new Headers(),
                Request: new Request('http://x.test/'),
                Response: new Response(''),
                AbortController: new AbortController(),
                AbortSignal: new AbortController().signal
            };
            names.forEach(function (n) {
                var d = Object.getOwnPropertyDescriptor(globalThis, n);
                if (!d) { out.missing.push(n); return; }
                out.enumerable.push(d.enumerable);
                out.writable.push(d.writable);
                out.configurable.push(d.configurable);
                out.types.push(typeof globalThis[n]);
                if (tagSamples[n]) out.tags.push(Object.prototype.toString.call(tagSamples[n]));
                if (Object.keys(globalThis).indexOf(n) >= 0) out.missing.push('enum:' + n);
            });
            out.fetchType = typeof fetch;
            out.constructorType = typeof Headers;
            return out;
        "#);
        assert_eq!(out["missing"], serde_json::json!([]), "六个全局都要装上，且不能出现在 Object.keys 里");
        assert_eq!(out["enumerable"], serde_json::json!([false, false, false, false, false, false]));
        assert_eq!(out["writable"], serde_json::json!([true, true, true, true, true, true]));
        assert_eq!(out["configurable"], serde_json::json!([true, true, true, true, true, true]));
        assert_eq!(
            out["tags"],
            serde_json::json!(["[object Headers]", "[object Request]", "[object Response]",
                               "[object AbortController]", "[object AbortSignal]"]),
            "与 Node 的 Object.prototype.toString 一致"
        );
        assert_eq!(out["fetchType"], "function");
        assert_eq!(out["constructorType"], "function");
    });
}

// ════════════════════════════════════════════════ 证据（期望值怎么来的）
//
// 全部在真的 Node **24.16.0** 上跑出来的（node --version → v24.16.0）。这个域
// 没有可批量对拍的向量表，所以没生成 vectors/fetch.json —— 断言逐条对应下面这些
// 探针输出（探针脚本是临时的，不进仓库；下面是命令与关键输出，方便复核）：
//
//   # 1) statusText 表（fetch.js 里的 STATUS_TEXT 就是它的输出）
//   node -e "console.log(JSON.stringify(require('node:http').STATUS_CODES))"
//   → {"200":"OK", ... "404":"Not Found", ... "418":"I'm a Teapot", ...}
//     注意 599 不在表里 → statusText 为 ''
//
//   # 2) 网络响应的形状（本地起 127.0.0.1 的 http 服务，不联网）
//     200 → {status:200, statusText:'OK', ok:true, type:'basic', url:<请求 URL>,
//             redirected:false, tag:'[object Response]'}
//     404 → statusText:'Not Found'
//     GET/HEAD + body → TypeError('Request with GET/HEAD method cannot have body.')
//     fetch('not a url') → reject TypeError('Failed to parse URL from not a url')
//                          （cause: TypeError code 'ERR_INVALID_URL', input 'not a url'）
//     网络错误 → reject TypeError('fetch failed')，cause 是底层错误
//     abort（之前/之中）→ reject DOMException name 'AbortError' code 20,
//                          message 'This operation was aborted'
//     AbortSignal.timeout(1) → reject name 'TimeoutError',
//                          message 'The operation was aborted due to timeout'
//
//   # 3) Headers
//   new Headers([['b','2'],['A','1'],['a','3']]) 迭代 → [['a','1, 3'],['b','2']]
//   get('nope') → null；{a:['1','2']}.get('a') → '1,2'
//   set 保留原位：[['a','1'],['b','2'],['a','3']] set('a','9') → [['a','9'],['b','2']]
//   非法名 → TypeError('Headers.append: "bad name" is an invalid header name.')
//   非法值 → TypeError('Headers.append: "a\nb" is an invalid header value.')
//   Response 的头 → TypeError('immutable')
//
//   # 4) Response
//   new Response('') → statusText ''（自己 new 的没有短语）、type 'default'、
//                      url ''、redirected false、content-type 'text/plain;charset=UTF-8'
//   读两次 → TypeError('Body is unusable: Body has already been read')
//   读后 clone → TypeError('Response.clone: Body has already been consumed.')
//   status 99/600 → RangeError('init["status"] must be in the range of 200 to 599, inclusive.')
//   204 带 body → TypeError('Response constructor: Invalid response status code 204')
//   statusText 带换行 → TypeError('Invalid statusText')
//   Response.json(undefined) → TypeError('Value is not JSON serializable')
//   Response.error() → status 0 / type 'error' / ok false
//
//   # 5) Request
//   method 'post' → 'POST'；' post ' → TypeError("' post ' is not a valid HTTP method.")
//   字符串 body → content-type 'text/plain;charset=UTF-8'
//   URLSearchParams body → content-type 'application/x-www-form-urlencoded;charset=UTF-8'
//   new Request(另一个 Request) → 方法/URL/body/头都复制，signal 共享
//   默认 method 'GET'、body null
//
//   # 6) AbortSignal / AbortController
//   controller.abort() → reason: DOMException AbortError code 20,
//                        message 'This operation was aborted'
//   abort 两次 → 事件只发一次；abort 之后加监听器不会被调用
//   throwIfAborted() 抛的是 reason 本身（自定义 reason 就抛那个值）
//   监听器里抛错 → abort() 不抛（Node 异步报未捕获异常）
//   onabort 收到的事件：[object Event]，e.type 'abort'，this === signal
//
//   # 7) 全局标签
//   Object.prototype.toString.call: Headers/Request/Response/AbortController/AbortSignal
//   → [object Headers] / [object Request] / [object Response] /
//      [object AbortController] / [object AbortSignal]
//
// 与 Node **不一致**的地方（都在 fetch.js 文件头「已知差异」里，这里只列测试覆盖到的）：
//   · 二进制请求体：Node 支持，这里 TypeError（宿主只收文本）
//     —— binary_request_bodies_are_rejected_with_an_explanation
//   · redirect 'manual'/'error'：Node 支持，这里 TypeError（宿主已经跟完了）
//     —— redirect_only_supports_follow_and_says_why
//   · response.redirected 恒 false、response.url 是请求 URL（拿不到最终 URL）
//   · statusText 是查表（服务器自定义短语看不到）；自己 new 的 Response 与 Node 一样是 ''
//   · response.headers 一律只读（Node 24 对 new 出来的 Response 允许改）
//   · request.body 是字符串（Node 是 ReadableStream）
