//! TextEncoder / TextDecoder / atob / btoa 的测试。
//!
//! 期望值不是手写的：`scripts/gen-vectors-text.mjs` 用 **Node 24** 生成
//! `vectors/text.json`，这里让 QuickJS 跑同一批输入再逐条比对。
//! 这样「面板里跑的和 Node 里跑的」是不是一回事，是**测出来**的，不是看代码看出来的。

use crate::jscompat::with_compat;

const VECTORS: &str = include_str!("vectors/text.json");

/// 内部工具 `C.utf8Decode` 的回归测试。
///
/// 为什么单独测它：`url.js` / `crypto.js` / `fetch.js` 都靠这条内部约定做字节→字符串，
/// 而它此前**一次都没被跑到** —— 参数顺序写反（state 与 bytes 互换）时，TextDecoder 类
/// 那条路照样全绿，只有这条会红。fetch-dev 在 Node 冒烟里撞到了它（任何输入都返回一个 U+FFFD）。
#[test]
fn internal_utf8_helpers_round_trip() {
    with_compat(|_, ctx| {
        let got: String = ctx
            .eval(
                r#"
                (function () {
                    var C = globalThis.__jscompat;
                    var text = '你好😀 end';
                    return JSON.stringify([
                        C.utf8Decode(C.utf8Encode(text), {}) === text,
                        C.utf8Decode(new Uint8Array([65]), {}) === 'A',
                        C.utf8Decode(new Uint8Array(0), {}) === '',
                        C.utf8Decode(new Uint8Array([0xef, 0xbb, 0xbf, 65]), {}) === 'A',
                        C.utf8Decode(new Uint8Array([0xef, 0xbb, 0xbf, 65]), { ignoreBOM: true }).length === 2,
                        C.hexOf(C.utf8Encode('abc')) === '616263',
                        C.b64decode(C.b64encode(C.utf8Encode('abc'))).length === 3
                    ]);
                })()
                "#,
            )
            .unwrap();
        assert_eq!(got, "[true,true,true,true,true,true,true]", "内部字节工具不自洽");
    });
}

#[test]
fn text_encoder_decoder_match_node() {
    with_compat(|_, ctx| {
        let script = format!(
            r#"
            (function () {{
                var V = {VECTORS};
                function u2s(u) {{
                    var s = '';
                    for (var i = 0; i < u.length; i++) s += String.fromCharCode(u[i]);
                    return s;
                }}
                function s2u(s) {{
                    var a = [];
                    for (var i = 0; i < s.length; i++) a.push(s.charCodeAt(i));
                    return a;
                }}
                function hex(bytes) {{
                    var s = '';
                    for (var i = 0; i < bytes.length; i++) {{
                        s += ('0' + bytes[i].toString(16)).slice(-2);
                    }}
                    return s;
                }}
                var out = {{ encode: [], decode: [], encode_into: [], btoa: [], atob: [],
                             btoa_throws: [], atob_throws: [], fatal_throws: [] }};
                var enc = new TextEncoder();
                V.encode.forEach(function (c) {{
                    out.encode.push({{ units: c.units, hex: hex(enc.encode(u2s(c.units))) }});
                }});
                V.decode.forEach(function (c) {{
                    var bytes = [];
                    for (var i = 0; i < c.bytes.length; i += 2) bytes.push(parseInt(c.bytes.slice(i, i + 2), 16));
                    var d = new TextDecoder('utf-8', {{ fatal: !!c.fatal, ignoreBOM: !!c.ignoreBOM }});
                    out.decode.push({{ bytes: c.bytes, fatal: !!c.fatal, ignoreBOM: !!c.ignoreBOM,
                        units: s2u(d.decode(new Uint8Array(bytes))) }});
                }});
                V.encode_into.forEach(function (c) {{
                    var buf = new Uint8Array(c.size);
                    var r = enc.encodeInto(u2s(c.units), buf);
                    out.encode_into.push({{ units: c.units, size: c.size, read: r.read, written: r.written, hex: hex(buf) }});
                }});
                V.btoa.forEach(function (c) {{ out.btoa.push({{ units: c.units, out: btoa(u2s(c.units)) }}); }});
                V.atob.forEach(function (c) {{
                    var s = atob(c.input);
                    var a = [];
                    for (var i = 0; i < s.length; i++) a.push(s.charCodeAt(i));
                    out.atob.push({{ input: c.input, units: a }});
                }});
                V.btoa_throws.forEach(function (c) {{
                    var name = null;
                    try {{ btoa(u2s(c.units)); }} catch (e) {{ name = e.name; }}
                    out.btoa_throws.push({{ units: c.units, name: name }});
                }});
                V.atob_throws.forEach(function (c) {{
                    var name = null;
                    try {{ atob(c.input); }} catch (e) {{ name = e.name; }}
                    out.atob_throws.push({{ input: c.input, name: name }});
                }});
                V.fatal_throws.forEach(function (c) {{
                    var bytes = [];
                    for (var i = 0; i < c.bytes.length; i += 2) bytes.push(parseInt(c.bytes.slice(i, i + 2), 16));
                    var d = new TextDecoder('utf-8', {{ fatal: true }});
                    var name = null;
                    try {{ d.decode(new Uint8Array(bytes)); }} catch (e) {{ name = e.name; }}
                    out.fatal_throws.push({{ bytes: c.bytes, name: name }});
                }});
                return JSON.stringify(out);
            }})()
            "#
        );
        // 包一层 try/catch：脚本里任何一处抛错都要**带着 JS 的堆栈**冒出来。
        // 直接 ctx.eval 失败时 rquickjs 只给一句 "Exception"，等于没说话。
        let wrapped = format!(
            // ⚠ return 后面必须紧跟左括号：脚本以换行开头，JS 的 ASI 会把 `return` 与
        // 下一行的 `(function…` 拆开，于是整个 eval 静默返回 undefined（踩过一次）。
        "(function () {{ try {{ return ({script}); }} catch (e) {{ return '!JSERR:' + (e && e.stack ? e.stack : String(e)); }} }})()"
        );
        let got: String = ctx.eval(wrapped).expect("测试脚本要能跑");
        assert!(!got.starts_with("!JSERR:"), "兼容层脚本抛错: {got}");
        let got: serde_json::Value = serde_json::from_str(&got).unwrap();
        let want: serde_json::Value = serde_json::from_str(VECTORS).unwrap();

        // 逐条断（而不是整块 assert_eq）：失败时要能一眼看出是**哪条**输入，
        // 否则一个 200 条的向量表只会回一句 "assertion failed"。
        for (key, items) in want.as_object().unwrap() {
            if key == "note" {
                continue;
            }
            let actual = got.get(key).unwrap_or_else(|| panic!("缺 {key}"));
            let expected = items.as_array().unwrap();
            let actual = actual.as_array().unwrap();
            assert_eq!(actual.len(), expected.len(), "{key} 条数不对");
            for (i, w) in expected.iter().enumerate() {
                assert_eq!(&actual[i], w, "{key}[{i}] 与 Node 不一致（输入见 vectors/text.json）");
            }
        }
    });
}
