//! node:zlib 兼容层的测试。
//!
//! 期望值来自**真的 Node**（v24.16.0）：
//!
//!   node -e "const z=require('node:zlib');const b=z.gzipSync(Buffer.from('x'.repeat(100)));
//!            console.log(b.subarray(0,10).toString('hex'), b.length)"
//!   → 1f8b08000000000000ff ...（第 4–7 字节 mtime = 00000000）
//!
//! 也就是说：gzip 头的 mtime **恒为 0**，而 `gzipSync(buf, {mtime: 0})` 这种写法
//! 在 Node 里只是「多给了一个没人读的选项」—— 这里必须同义。

use crate::jscompat::with_compat;
use rquickjs::{Ctx, Runtime};

/// 把 node:zlib 的模块体装成 `globalThis.__zlib`（走的是**真的**那条注入路径：
/// jshost::node_module_for_require 生成源码 → new Function 执行）。
fn with_zlib<R>(f: impl FnOnce(&Runtime, &Ctx<'_>) -> R) -> R {
    with_compat(|rt, ctx| {
        let src = crate::jshost::node_module_for_require("node:zlib")
            .expect("node:zlib 必须在内置模块表里");
        let lit = serde_json::to_string(&src).expect("模块源码要能序列化");
        ctx.eval::<(), _>(
            format!(
                "globalThis.__zlib = (function () {{ var f = new Function({lit}); return f(); }})();"
            )
            .as_str(),
        )
        .expect("node:zlib 模块体要能执行");
        f(rt, ctx)
    })
}

fn eval_json(ctx: &Ctx<'_>, body: &str) -> serde_json::Value {
    let src = format!(
        "(function () {{ try {{ return JSON.stringify((function () {{ {body} }})()); }} \
         catch (e) {{ return '!JSERR:' + (e && e.stack ? e.stack : String(e)); }} }})()"
    );
    let got: String = ctx.eval(src.as_str()).expect("求值失败");
    assert!(!got.starts_with("!JSERR:"), "被测脚本抛错: {got}\n脚本: {body}");
    serde_json::from_str(&got).expect("返回值要能序列化成 JSON")
}

#[test]
fn gzip_round_trips_and_writes_a_zero_mtime_header() {
    with_zlib(|_, ctx| {
        let out = eval_json(
            ctx,
            r#"
            var input = new Uint8Array(100);
            for (var i = 0; i < input.length; i++) input[i] = i * 7 % 256;
            var gz = __zlib.gzipSync(input);
            var back = __zlib.gunzipSync(gz);
            return {
                isBuffer: Buffer.isBuffer(gz),
                isUint8: gz instanceof Uint8Array,
                head: Array.prototype.slice.call(gz.subarray(0, 10)),
                mtime: Array.prototype.slice.call(gz.subarray(4, 8)),
                sameLength: back.length === input.length,
                sameBytes: Array.prototype.every.call(input, function (b, i) { return back[i] === b; })
            };
            "#,
        );
        assert_eq!(out["isBuffer"], true, "gzipSync 必须返回 Buffer（与 Node 一致）");
        assert_eq!(out["isUint8"], true, "Buffer 是 Uint8Array 子类");
        let head: Vec<u64> = out["head"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_u64().unwrap())
            .collect();
        assert_eq!(&head[..3], &[0x1f, 0x8b, 0x08], "gzip 魔数 + DEFLATE: {head:?}");
        assert_eq!(
            out["mtime"],
            serde_json::json!([0, 0, 0, 0]),
            "mtime 必须是 0（否则同一份输入每次压出来都不一样）"
        );
        assert_eq!(out["sameLength"], true);
        assert_eq!(out["sameBytes"], true, "gunzip 必须逐字节还原");
    });
}

/// `{mtime: 0}` 这种写法在 Node 里是「多给一个没人读的选项」，这里必须同义。
/// 央视频 CLI 版就是这么调的（gzipSync(buf, {mtime: 0})）。
#[test]
fn gzip_accepts_a_string_and_ignores_mtime_like_node() {
    with_zlib(|_, ctx| {
        let out = eval_json(
            ctx,
            r#"
            var a = __zlib.gzipSync('hello');
            var b = __zlib.gzipSync('hello', { mtime: 0 });
            var c = __zlib.gzipSync('hello', { level: 9 });
            return {
                aLen: a.length,
                sameAsB: a.length === b.length && Array.prototype.every.call(a, function (v, i) { return v === b[i]; }),
                text: __zlib.gunzipSync(c).toString('utf8')
            };
            "#,
        );
        assert_eq!(out["text"], "hello", "字符串按 utf8 进、gunzip 出来还是原文");
        assert_eq!(
            out["sameAsB"], true,
            "mtime 选项被忽略（Node 的 zlib 没有这个选项，头一直是 0）"
        );
    });
}

#[test]
fn gunzip_reports_bad_data_with_nodes_error_code() {
    with_zlib(|_, ctx| {
        let out = eval_json(
            ctx,
            r#"
            var bad = null;
            try { __zlib.gunzipSync(new Uint8Array([1, 2, 3, 4])); }
            catch (e) { bad = { name: e.name, code: e.code, message: e.message }; }
            return bad;
            "#,
        );
        assert_eq!(out["name"], "Error", "坏数据在 Node 里是 Error（不是 TypeError）");
        assert_eq!(out["code"], "Z_DATA_ERROR", "Node 给的正是这个 code");
        assert!(
            out["message"].as_str().unwrap().contains("gunzip"),
            "文案要说是 gunzip 失败: {}",
            out["message"]
        );
    });
}

#[test]
fn level_out_of_range_is_a_range_error_like_node() {
    with_zlib(|_, ctx| {
        let out = eval_json(
            ctx,
            r#"
            var got = null;
            try { __zlib.gzipSync('x', { level: 99 }); }
            catch (e) { got = { name: e.name, code: e.code }; }
            return got;
            "#,
        );
        assert_eq!(out["name"], "RangeError");
        assert_eq!(out["code"], "ERR_OUT_OF_RANGE");
    });
}

/// 超过 16 MiB 的输入在**进宿主之前**就被拒（面板是常驻进程，不是沙箱）。
///
/// 解压炸弹那一侧（输出上限）在 Rust 单测里验：那条要构造一个「压缩后很小、
/// 解开超过上限」的输入，而本层的输入闸先把它挡住了 —— 两道闸分开测才对得上。
#[test]
fn oversized_input_is_rejected_before_reaching_the_host() {
    with_zlib(|_, ctx| {
        let out = eval_json(
            ctx,
            r#"
            var big = new Uint8Array(17 * 1024 * 1024);
            var got = { name: null, code: null };
            try { __zlib.gzipSync(big); got.ok = true; }
            catch (e) { got.ok = false; got.name = e.name; got.code = e.code; got.message = e.message; }
            return got;
            "#,
        );
        assert_eq!(out["ok"], false, "17 MiB 必须被拒");
        assert!(
            out["message"].as_str().unwrap().contains("上限"),
            "文案要说清是上限: {}",
            out["message"]
        );
    });
}

#[test]
fn constants_match_nodes_values() {
    with_zlib(|_, ctx| {
        let out = eval_json(
            ctx,
            r#"
            var c = __zlib.constants;
            return [c.Z_NO_COMPRESSION, c.Z_BEST_SPEED, c.Z_BEST_COMPRESSION,
                    c.Z_DEFAULT_COMPRESSION, c.Z_DEFAULT_STRATEGY, c.Z_FINISH,
                    c.Z_SYNC_FLUSH, c.Z_DATA_ERROR, c.Z_STREAM_END];
            "#,
        );
        assert_eq!(
            out,
            serde_json::json!([0, 1, 9, -1, 0, 4, 2, -3, 1]),
            "这些值与 Node 的 zlib.constants 逐项相同"
        );
    });
}
