//! Buffer 兼容层的测试。
//!
//! 期望值不是手写的：scripts/gen-vectors-buffer.mjs 用 **Node 24** 生成
//! vectors/buffer.json，这里让 QuickJS 跑同一批输入再逐条比对。
//! 这样「面板里跑的和 Node 里跑的」是不是一回事，是**测出来**的，不是看代码看出来的。
//!
//! 四组测试各管一件事：
//! 1. buffer_vectors_match_node —— 编码 / 读写 / 比较 / JSON 的**值**要和 Node 一致；
//! 2. buffer_shape_and_error_codes —— 类型形状（instanceof、toStringTag）与错误 code；
//! 3. buffer_memory_sharing —— 谁是共享内存、谁是复制（最容易做反的一块）；
//! 4. node_buffer_module_matches_the_code —— node:buffer 模块体走**真的**包装器，
//!    导出清单与模块体一一对上，且 Buffer 就是全局那个类。

use crate::jscompat::with_compat;

const VECTORS: &str = include_str!("vectors/buffer.json");

/// 跑一段 JS 并解析成 JSON。
///
/// 包一层 try/catch：脚本里任何一处抛错都要**带着 JS 的堆栈**冒出来 ——
/// 直接 ctx.eval 失败时 rquickjs 只给一句 "Exception"，等于没说话。
fn eval_json(ctx: &rquickjs::Ctx<'_>, script: &str) -> serde_json::Value {
    let wrapped = format!(
        "(function () {{ try {{ return JSON.stringify(({script})); }} catch (e) {{ return '!JSERR:' + (e && e.stack ? e.stack : String(e)); }} }})()"
    );
    let got: String = ctx.eval(wrapped).expect("测试脚本要能跑");
    assert!(!got.starts_with("!JSERR:"), "兼容层脚本抛错: {got}");
    serde_json::from_str(&got).expect("脚本要返回合法 JSON")
}

/// 逐条比对（而不是整块 assert_eq）：失败时要能一眼看出是**哪条**输入，
/// 否则一个三百条的向量表只会回一句 "assertion failed"。
fn assert_vectors(got: &serde_json::Value, want: &serde_json::Value) {
    for (key, expected) in want.as_object().expect("向量文件要是对象") {
        if key == "note" {
            continue;
        }
        let actual = got
            .get(key)
            .unwrap_or_else(|| panic!("脚本没有产出 {key}（生成器与回放脚本对不上？）"));
        if let Some(expected_items) = expected.as_array() {
            let actual_items = actual
                .as_array()
                .unwrap_or_else(|| panic!("{key} 不是数组"));
            assert_eq!(actual_items.len(), expected_items.len(), "{key} 条数不对");
            for (i, want_item) in expected_items.iter().enumerate() {
                assert_eq!(
                    &actual_items[i], want_item,
                    "{key}[{i}] 与 Node 不一致（输入见 vectors/buffer.json）"
                );
            }
        } else if key != "meta" {
            assert_eq!(actual, expected, "{key} 与 Node 不一致");
        }
    }
}

/// 向量回放脚本：把生成器做过的操作**原样再做一遍**，只换实现（QuickJS 的 Buffer）。
///
/// 结构与 scripts/gen-vectors-buffer.mjs 一一对应（同样的字段名、同样的 null→undefined
/// 约定）；改了一边就要改另一边，否则这里会以「条数不对 / 字段对不上」红给我看。
const REPLAY: &str = r##"
(function () {
    var V = __VECTORS__;
    var C = globalThis.__jscompat;
    var Buffer = globalThis.Buffer;

    function u2s(u) {
        var s = '';
        for (var i = 0; i < u.length; i++) s += String.fromCharCode(u[i]);
        return s;
    }
    function s2u(s) {
        var a = [];
        for (var i = 0; i < s.length; i++) a.push(s.charCodeAt(i));
        return a;
    }
    function bytes(h) {
        var a = [];
        for (var i = 0; i + 1 < h.length; i += 2) a.push(parseInt(h.slice(i, i + 2), 16));
        return new Uint8Array(a);
    }
    function hex(b) {
        var s = '';
        for (var i = 0; i < b.length; i++) s += ('0' + b[i].toString(16)).slice(-2);
        return s;
    }
    // JSON 里 NaN / Infinity / -0 都不能原样存活，生成器把它们写成了字符串 token。
    function numIn(v) {
        return v === '-0' ? -0
            : v === 'NaN' ? NaN
                : v === 'Infinity' ? Infinity
                    : v === '-Infinity' ? -Infinity : v;
    }
    function numOut(v) { return (typeof v === 'number' && isFinite(v)) ? v : String(v); }
    function undef(v) { return v === null || v === undefined ? undefined : v; }
    function sizeIn(v) { return v === 'NaN' ? NaN : v === 'Infinity' ? Infinity : v; }
    function fillValue(v) {
        if (v.kind === 'number') return v.n;
        if (v.kind === 'bytes') return Buffer.from(bytes(v.bytes));
        return u2s(v.units);
    }

    var out = {};

    out.is_encoding = V.is_encoding.map(function (c) {
        return { enc: c.enc, out: Buffer.isEncoding(c.enc) };
    });
    out.is_encoding_non_string = V.is_encoding_non_string.map(function (c) {
        var v = c.kind === '123' ? 123 : c.kind === 'null' ? null
            : c.kind === 'undefined' ? undefined : {};
        return { kind: c.kind, out: Buffer.isEncoding(v) };
    });

    out.from_string = V.from_string.map(function (c) {
        var o = { units: c.units, encoding: c.encoding };
        try {
            o.hex = hex(Buffer.from(u2s(c.units), undef(c.encoding)));
        } catch (e) { o.throw = e.name; }
        return o;
    });

    out.to_string = V.to_string.map(function (c) {
        var o = { bytes: c.bytes, encoding: c.encoding, start: c.start, end: c.end };
        try {
            o.units = s2u(Buffer.from(bytes(c.bytes)).toString(c.encoding, undef(c.start), undef(c.end)));
        } catch (e) { o.throw = e.name; }
        return o;
    });

    out.byte_length = V.byte_length.map(function (c) {
        var o = { kind: c.kind, units: c.units, encoding: c.encoding };
        try {
            o.len = Buffer.byteLength(u2s(c.units), undef(c.encoding));
        } catch (e) { o.throw = e.name; }
        return o;
    });
    out.byte_length_bytes = V.byte_length_bytes.map(function (c) {
        var o = { kind: c.kind };
        if (c.bytes !== undefined) o.bytes = c.bytes;
        if (c.size !== undefined) o.size = c.size;
        try {
            var v = c.kind === 'bytes' ? Buffer.from(bytes(c.bytes))
                : c.kind === 'arraybuffer' ? new ArrayBuffer(c.size)
                    : new DataView(new ArrayBuffer(c.size));
            o.len = Buffer.byteLength(v);
        } catch (e) { o.throw = e.name; }
        return o;
    });

    out.from_array = V.from_array.map(function (c) {
        return { values: c.values, hex: hex(Buffer.from(c.values)) };
    });

    out.alloc = V.alloc.map(function (c) {
        var o = { size: c.size };
        if (c.fill !== undefined) o.fill = c.fill;
        try {
            var n = sizeIn(c.size);
            var b;
            if (c.fill === undefined) b = Buffer.alloc(n);
            else if (c.fill.kind === 'number') b = Buffer.alloc(n, c.fill.n);
            else if (c.fill.kind === 'bytes') b = Buffer.alloc(n, Buffer.from(bytes(c.fill.bytes)));
            else b = Buffer.alloc(n, u2s(c.fill.units), c.fill.encoding);
            o.hex = hex(b);
        } catch (e) { o.throw = e.name; }
        return o;
    });

    out.fill = V.fill.map(function (c) {
        var o = { init: c.init, value: c.value };
        if (c.start !== undefined) o.start = c.start;
        if (c.end !== undefined) o.end = c.end;
        try {
            var b = Buffer.from(bytes(c.init));
            // 与生成器同样显式给 start/end（原因见 gen-vectors-buffer.mjs 里的注释）。
            var start = c.start === undefined ? 0 : c.start;
            var end = c.end === undefined ? b.length : c.end;
            b.fill(fillValue(c.value), start, end, c.value.encoding);
            o.hex = hex(b);
        } catch (e) { o.throw = e.name; }
        return o;
    });
    out.fill_returns_this = (function () {
        var b = Buffer.alloc(3);
        return b.fill(1) === b;
    })();

    out.fill_encoding = V.fill_encoding.map(function (c) {
        var o = { init: c.init, units: c.units, args: c.args };
        try {
            var b = Buffer.from(bytes(c.init));
            b.fill(u2s(c.units), c.args[0], c.args[1]);
            o.hex = hex(b);
        } catch (e) { o.throw = e.name; }
        return o;
    });

    out.concat = V.concat.map(function (c) {
        var o = { list: c.list, totalLength: c.totalLength };
        try {
            o.hex = hex(Buffer.concat(c.list.map(function (h) { return Buffer.from(bytes(h)); }), undef(c.totalLength)));
        } catch (e) { o.throw = e.name; }
        return o;
    });

    out.write = V.write.map(function (c) {
        var o = { init: c.init, units: c.units, offset: c.offset, length: c.length };
        if (c.encoding !== undefined) o.encoding = c.encoding;
        try {
            var b = Buffer.from(bytes(c.init));
            o.written = b.write(u2s(c.units), c.offset, undef(c.length), c.encoding);
            o.hex = hex(b);
        } catch (e) { o.throw = e.name; }
        return o;
    });

    out.read_int = V.read_int.map(function (c) {
        var o = { bytes: c.bytes, op: c.op, offset: c.offset };
        try {
            var b = Buffer.from(bytes(c.bytes));
            o.value = c.offset === null ? b[c.op]() : b[c.op](c.offset);
        } catch (e) { o.throw = e.name; }
        return o;
    });
    out.read_float = V.read_float.map(function (c) {
        var o = { bytes: c.bytes, op: c.op, offset: c.offset };
        try {
            o.value = numOut(Buffer.from(bytes(c.bytes))[c.op](c.offset));
        } catch (e) { o.throw = e.name; }
        return o;
    });
    out.read_bigint = V.read_bigint.map(function (c) {
        var o = { bytes: c.bytes, op: c.op, offset: c.offset };
        try {
            o.value = String(Buffer.from(bytes(c.bytes))[c.op](c.offset));
        } catch (e) { o.throw = e.name; }
        return o;
    });

    out.write_int = V.write_int.map(function (c) {
        var o = { init: c.init, op: c.op, value: c.value, offset: c.offset };
        try {
            var b = Buffer.from(bytes(c.init));
            o.returned = b[c.op](c.value, c.offset);
            o.hex = hex(b);
        } catch (e) { o.throw = e.name; }
        return o;
    });
    out.write_float = V.write_float.map(function (c) {
        var o = { init: c.init, op: c.op, value: c.value, offset: c.offset };
        try {
            var b = Buffer.from(bytes(c.init));
            b[c.op](numIn(c.value), c.offset);
            o.hex = hex(b);
        } catch (e) { o.throw = e.name; }
        return o;
    });
    out.write_bigint = V.write_bigint.map(function (c) {
        var o = { init: c.init, op: c.op, value: c.value, offset: c.offset };
        try {
            var b = Buffer.from(bytes(c.init));
            b[c.op](BigInt(c.value), c.offset);
            o.hex = hex(b);
        } catch (e) { o.throw = e.name; }
        return o;
    });

    out.compare = V.compare.map(function (c) {
        var a = Buffer.from(bytes(c.a));
        var b = Buffer.from(bytes(c.b));
        return {
            a: c.a, b: c.b,
            cmp: Buffer.compare(a, b),
            equal: a.equals(b),
            instance_cmp: a.compare(b),
        };
    });

    out.copy = V.copy.map(function (c) {
        var o = { init: c.init, targetLength: c.targetLength, targetStart: c.targetStart,
                  sourceStart: c.sourceStart, sourceEnd: c.sourceEnd };
        try {
            var t = Buffer.alloc(c.targetLength);
            o.copied = Buffer.from(bytes(c.init)).copy(t, c.targetStart, undef(c.sourceStart), undef(c.sourceEnd));
            o.hex = hex(t);
        } catch (e) { o.throw = e.name; }
        return o;
    });

    out.index_of = V.index_of.map(function (c) {
        var o = { bytes: c.bytes, value: c.value, offset: c.offset };
        try {
            var v = c.value.kind === 'number' ? c.value.n
                : c.value.kind === 'bytes' ? Buffer.from(bytes(c.value.bytes))
                    : u2s(c.value.units);
            o.index = Buffer.from(bytes(c.bytes)).indexOf(v, undef(c.offset), c.value.encoding);
        } catch (e) { o.throw = e.name; }
        return o;
    });

    out.to_json = V.to_json.map(function (c) {
        return { bytes: c.bytes, json: JSON.stringify(Buffer.from(bytes(c.bytes))) };
    });

    out.is_utf8_ascii = V.is_utf8_ascii.map(function (c) {
        var b = Buffer.from(bytes(c.bytes));
        return { bytes: c.bytes, utf8: C.buffer.isUtf8(b), ascii: C.buffer.isAscii(b) };
    });

    out.meta = {
        poolSize: Buffer.poolSize,
        INSPECT_MAX_BYTES: C.buffer.INSPECT_MAX_BYTES,
        kMaxLength: C.buffer.kMaxLength,
        kStringMaxLength: C.buffer.kStringMaxLength,
        MAX_LENGTH: C.buffer.constants.MAX_LENGTH,
        MAX_STRING_LENGTH: C.buffer.constants.MAX_STRING_LENGTH,
    };

    return out;
})()
"##;

#[test]
fn buffer_vectors_match_node() {
    let script = REPLAY.replace("__VECTORS__", VECTORS);
    with_compat(|_, ctx| {
        let got = eval_json(ctx, &script);
        let want: serde_json::Value = serde_json::from_str(VECTORS).expect("向量文件要是合法 JSON");
        assert_vectors(&got, &want);
    });
}

// ──────────────────────────────────────────────────────── 形状与错误码

#[test]
fn buffer_shape_and_error_codes() {
    with_compat(|_, ctx| {
        let got = eval_json(
            ctx,
            r##"
        (function () {
            var Buffer = globalThis.Buffer;
            function code(fn) {
                try { fn(); return 'NO_THROW'; } catch (e) {
                    return (e.name || '?') + ':' + (e.code === undefined ? 'NO_CODE' : e.code);
                }
            }
            var out = {};
            var b = Buffer.from('x');
            out.bufferIsFunction = typeof Buffer === 'function';
            out.constructorName = Buffer.name;
            out.instanceofBuffer = b instanceof Buffer;
            out.instanceofUint8 = b instanceof Uint8Array;
            out.constructorOf = b.constructor === Buffer;
            out.tag = Object.prototype.toString.call(b);
            out.hasOwnToStringTag = Object.prototype.hasOwnProperty.call(Buffer.prototype, Symbol.toStringTag);
            out.isBuffer = [Buffer.isBuffer(b), Buffer.isBuffer(new Uint8Array(1)), Buffer.isBuffer(null),
                            Buffer.isBuffer(Buffer.alloc(0))];
            out.json = JSON.stringify(Buffer.from([1, 2]));
            out.jsonEmpty = JSON.stringify(Buffer.from([]));
            out.jsonView = JSON.stringify(Buffer.from([1, 2, 3]).subarray(1));
            out.jsonNested = JSON.stringify({ b: Buffer.from([1]) });
            out.poolSize = Buffer.poolSize;
            out.isEncoding = ['utf8', 'utf-8', 'HEX', 'base64url', 'binary', 'ucs2', 'utf-16le', 'nope', '']
                .map(function (e) { return Buffer.isEncoding(e); });
            out.allocUnsafeZeroed = Buffer.allocUnsafe(256).every(function (v) { return v === 0; });
            out.allocUnsafeSlowZeroed = Buffer.allocUnsafeSlow(128).every(function (v) { return v === 0; });
            out.allocFill = Buffer.alloc(4, 'ab').toString('hex');
            out.allocTruncates = Buffer.alloc(1.7).length;
            out.byteLength = [
                Buffer.byteLength('abc'), Buffer.byteLength('你好'),
                Buffer.byteLength('6162', 'hex'), Buffer.byteLength('YWJj', 'base64'),
                Buffer.byteLength(new ArrayBuffer(5)), Buffer.byteLength(new Uint8Array(3)),
                Buffer.byteLength(new DataView(new ArrayBuffer(7)))
            ];
            out.statics = ['from', 'alloc', 'allocUnsafe', 'allocUnsafeSlow', 'isBuffer', 'isEncoding',
                           'byteLength', 'concat', 'compare'].map(function (k) { return typeof Buffer[k]; });
            var methods = ['toString', 'toJSON', 'equals', 'compare', 'copy', 'fill', 'write',
                           'subarray', 'slice', 'indexOf', 'includes',
                           'readUInt8', 'readInt8', 'readUInt16BE', 'readUInt16LE', 'readInt16BE', 'readInt16LE',
                           'readUInt32BE', 'readUInt32LE', 'readInt32BE', 'readInt32LE',
                           'readFloatBE', 'readFloatLE', 'readDoubleBE', 'readDoubleLE',
                           'readBigInt64BE', 'readBigInt64LE', 'readBigUInt64BE', 'readBigUInt64LE',
                           'writeUInt8', 'writeInt8', 'writeUInt16BE', 'writeUInt16LE',
                           'writeInt16BE', 'writeInt16LE', 'writeUInt32BE', 'writeUInt32LE',
                           'writeInt32BE', 'writeInt32LE', 'writeFloatBE', 'writeFloatLE',
                           'writeDoubleBE', 'writeDoubleLE',
                           'writeBigInt64BE', 'writeBigInt64LE', 'writeBigUInt64BE', 'writeBigUInt64LE'];
            out.methods = methods.map(function (k) { return typeof Buffer.prototype[k]; });
            out.errors = {
                fromNumber: code(function () { Buffer.from(123); }),
                fromNull: code(function () { Buffer.from(null); }),
                unknownEncoding: code(function () { Buffer.from('x', 'nope'); }),
                unknownEncodingToString: code(function () { Buffer.from('x').toString('nope'); }),
                unknownEncodingWrite: code(function () { Buffer.alloc(2).write('x', 0, 1, 'nope'); }),
                readOutOfBounds: code(function () { Buffer.alloc(1).readUInt16BE(0); }),
                readOffsetNotInteger: code(function () { Buffer.alloc(2).readUInt8(0.5); }),
                readOffsetNotNumber: code(function () { Buffer.alloc(2).readUInt8('0'); }),
                writeOutOfBounds: code(function () { Buffer.alloc(1).writeUInt8(1, 1); }),
                writeValueOutOfRange: code(function () { Buffer.alloc(1).writeUInt8(256); }),
                bigIntOutOfBounds: code(function () { Buffer.alloc(4).writeBigInt64BE(1n); }),
                fromArrayBufferOutOfBounds: code(function () { Buffer.from(new ArrayBuffer(2), 0, 9); }),
                fillNegative: code(function () { Buffer.alloc(2).fill(1, -1); }),
                byteLengthNumber: code(function () { Buffer.byteLength(123); }),
                equalsString: code(function () { Buffer.from([1]).equals('a'); }),
                copyString: code(function () { Buffer.from([1]).copy('x'); }),
                bigIntFromNumber: code(function () { Buffer.alloc(8).writeBigInt64BE(1); }),
                allocString: code(function () { Buffer.alloc('4'); }),
                allocNegative: code(function () { Buffer.alloc(-1); }),
                allocNaN: code(function () { Buffer.alloc(NaN); }),
                toStringNumberEncoding: code(function () { Buffer.from('x').toString(1); }),
                concatNonArray: code(function () { Buffer.concat('x'); }),
                concatBadItem: code(function () { Buffer.concat([1]); })
            };
            return out;
        })()
        "##,
        );

        assert_eq!(got["bufferIsFunction"], true);
        assert_eq!(got["constructorName"], "Buffer");
        assert_eq!(
            got["instanceofBuffer"], true,
            "Buffer.from 的返回值必须是 Buffer"
        );
        assert_eq!(
            got["instanceofUint8"], true,
            "Buffer 必须真的是 Uint8Array 子类"
        );
        assert_eq!(got["constructorOf"], true);
        assert_eq!(
            got["tag"], "[object Uint8Array]",
            "不能自己加 Symbol.toStringTag —— Node 这里就是 [object Uint8Array]"
        );
        assert_eq!(got["hasOwnToStringTag"], false);
        assert_eq!(
            got["isBuffer"],
            serde_json::json!([true, false, false, true])
        );
        assert_eq!(got["json"], r#"{"type":"Buffer","data":[1,2]}"#);
        assert_eq!(got["jsonEmpty"], r#"{"type":"Buffer","data":[]}"#);
        assert_eq!(got["jsonView"], r#"{"type":"Buffer","data":[2,3]}"#);
        assert_eq!(got["jsonNested"], r#"{"b":{"type":"Buffer","data":[1]}}"#);
        assert_eq!(got["poolSize"], 8192);
        assert_eq!(
            got["isEncoding"],
            serde_json::json!([true, true, true, true, true, true, true, false, false])
        );
        assert_eq!(
            got["allocUnsafeZeroed"], true,
            "allocUnsafe 故意清零（见 buffer.js 文件头）"
        );
        assert_eq!(got["allocUnsafeSlowZeroed"], true);
        assert_eq!(got["allocFill"], "61626162");
        assert_eq!(
            got["allocTruncates"], 1,
            "Node 会把 alloc(1.7) 截断成 1 字节"
        );
        assert_eq!(got["byteLength"], serde_json::json!([3, 6, 2, 3, 5, 3, 7]));
        for (i, v) in got["statics"].as_array().unwrap().iter().enumerate() {
            assert_eq!(v, "function", "第 {i} 个静态方法不是函数");
        }
        for (i, v) in got["methods"].as_array().unwrap().iter().enumerate() {
            assert_eq!(
                v, "function",
                "原型上第 {i} 个方法不是函数（清单见 buffer_test.rs）"
            );
        }
        let errors = got["errors"].as_object().unwrap();
        let expected = [
            ("fromNumber", "TypeError:ERR_INVALID_ARG_TYPE"),
            ("fromNull", "TypeError:ERR_INVALID_ARG_TYPE"),
            ("unknownEncoding", "TypeError:ERR_UNKNOWN_ENCODING"),
            ("unknownEncodingToString", "TypeError:ERR_UNKNOWN_ENCODING"),
            ("unknownEncodingWrite", "TypeError:ERR_UNKNOWN_ENCODING"),
            ("readOutOfBounds", "RangeError:ERR_OUT_OF_RANGE"),
            ("readOffsetNotInteger", "RangeError:ERR_OUT_OF_RANGE"),
            ("readOffsetNotNumber", "TypeError:ERR_INVALID_ARG_TYPE"),
            ("writeOutOfBounds", "RangeError:ERR_OUT_OF_RANGE"),
            ("writeValueOutOfRange", "RangeError:ERR_OUT_OF_RANGE"),
            ("bigIntOutOfBounds", "RangeError:ERR_OUT_OF_RANGE"),
            ("fromArrayBufferOutOfBounds", "RangeError:ERR_OUT_OF_RANGE"),
            ("fillNegative", "RangeError:ERR_OUT_OF_RANGE"),
            ("byteLengthNumber", "TypeError:ERR_INVALID_ARG_TYPE"),
            ("equalsString", "TypeError:ERR_INVALID_ARG_TYPE"),
            ("copyString", "TypeError:ERR_INVALID_ARG_TYPE"),
            ("bigIntFromNumber", "TypeError:ERR_INVALID_ARG_TYPE"),
            ("allocString", "TypeError:ERR_INVALID_ARG_TYPE"),
            ("allocNegative", "RangeError:ERR_OUT_OF_RANGE"),
            ("allocNaN", "RangeError:ERR_OUT_OF_RANGE"),
            ("toStringNumberEncoding", "TypeError:ERR_UNKNOWN_ENCODING"),
            ("concatNonArray", "TypeError:ERR_INVALID_ARG_TYPE"),
            ("concatBadItem", "TypeError:ERR_INVALID_ARG_TYPE"),
        ];
        for (key, want) in expected {
            assert_eq!(errors[key], want, "错误码不对: {key}");
        }
    });
}

// ──────────────────────────────────────────────────────── 共享 vs 复制

/// 内存关系：谁是共享、谁是复制。
///
/// 这一块做反了**不会报错** —— 插件会"看起来正常"，直到某天改了子视图，父 Buffer
/// 神秘地跟着变（或者反过来，本该跟着变的没变）。所以逐条钉死。
#[test]
fn buffer_memory_sharing_matches_node() {
    with_compat(|_, ctx| {
        let got = eval_json(
            ctx,
            r##"
        (function () {
            var Buffer = globalThis.Buffer;
            function hex(b) {
                var s = '';
                for (var i = 0; i < b.length; i++) s += ('0' + b[i].toString(16)).slice(-2);
                return s;
            }
            var out = {};

            // 1) ArrayBuffer → **共享**（改 Buffer 看得见原 buffer，反之亦然）
            var ab = new ArrayBuffer(8);
            var whole = Buffer.from(ab);
            whole[0] = 1;
            new Uint8Array(ab)[1] = 2;
            out.arrayBufferShares = [new Uint8Array(ab)[0] === 1, whole[1] === 2, whole.buffer === ab];

            // 2) Buffer.from(arrayBuffer, offset, length)：视图 + 共享
            var view = Buffer.from(ab, 2, 3);
            out.arrayBufferView = [view.byteOffset, view.length, view.buffer === ab];
            view[0] = 9;
            out.arrayBufferViewShares = new Uint8Array(ab)[2] === 9;

            // 3) TypedArray → **复制**
            var u8 = new Uint8Array([1, 2]);
            var fromU8 = Buffer.from(u8);
            var fromU8Hex = hex(fromU8);          // 先留底：下面要改它，验证"改的是副本"
            fromU8[0] = 9;
            out.typedArrayCopied = [u8[0] === 1, fromU8.buffer !== u8.buffer, fromU8Hex];

            // 4) DataView → 复制它覆盖的那段（不是整个底层 buffer，也不共享）
            var dvBuf = new Uint8Array([1, 2, 3, 4]).buffer;
            var fromDv = Buffer.from(new DataView(dvBuf, 1, 2));
            out.dataViewCopy = [fromDv.length, hex(fromDv)];
            fromDv[0] = 9;
            out.dataViewStaysIndependent = new Uint8Array(dvBuf)[1] === 2;

            // 5) Buffer → 复制
            var src = Buffer.from([1, 2, 3]);
            var copy = Buffer.from(src);
            copy[0] = 9;
            out.bufferCopied = [src[0] === 1, copy.buffer !== src.buffer];

            // 6) subarray / slice → **共享**
            var parent = Buffer.from([1, 2, 3, 4]);
            var child = parent.subarray(1, 3);
            out.subarrayShape = [child.length, child.byteOffset, child.buffer === parent.buffer,
                                 child instanceof Buffer];
            child[0] = 9;
            out.subarrayShares = parent[1] === 9;
            parent[2] = 7;
            out.parentToChild = child[1] === 7;

            var sliced = parent.slice(1);
            sliced[0] = 5;
            out.sliceShares = [parent[1] === 5, sliced.buffer === parent.buffer, sliced instanceof Buffer];

            // 7) 视图套视图，偏移要累加
            var nested = child.subarray(1);
            out.nestedOffset = nested.byteOffset === 2;

            // 8) copy() 是真复制：目标变，源不变
            var target = Buffer.alloc(2);
            var n = parent.copy(target);
            out.copyCount = n;
            target[0] = 0;
            out.copyIsRealCopy = parent[0] === 1;

            // 9) 长度与底层 buffer 长度是两件事
            var sliceOfBig = Buffer.from(new ArrayBuffer(16), 4, 4);
            out.viewLengths = [sliceOfBig.length, sliceOfBig.byteLength, sliceOfBig.byteOffset,
                               sliceOfBig.buffer.byteLength];
            return out;
        })()
        "##,
        );
        assert_eq!(
            got["arrayBufferShares"],
            serde_json::json!([true, true, true])
        );
        assert_eq!(got["arrayBufferView"], serde_json::json!([2, 3, true]));
        assert_eq!(got["arrayBufferViewShares"], true);
        assert_eq!(
            got["typedArrayCopied"],
            serde_json::json!([true, true, "0102"])
        );
        assert_eq!(got["dataViewCopy"], serde_json::json!([2, "0203"]));
        assert_eq!(got["dataViewStaysIndependent"], true);
        assert_eq!(got["bufferCopied"], serde_json::json!([true, true]));
        assert_eq!(got["subarrayShape"], serde_json::json!([2, 1, true, true]));
        assert_eq!(got["subarrayShares"], true);
        assert_eq!(got["parentToChild"], true);
        assert_eq!(got["sliceShares"], serde_json::json!([true, true, true]));
        assert_eq!(got["nestedOffset"], true);
        assert_eq!(got["copyCount"], 2);
        assert_eq!(got["copyIsRealCopy"], true);
        assert_eq!(got["viewLengths"], serde_json::json!([4, 4, 4, 16]));
    });
}

// ──────────────────────────────────────────────────────── node:buffer 模块体

/// node:buffer 走**真的**包装器（jshost::node_module_for_require），而不是测试里
/// 另拼一份 —— 另拼的字符串验的就不是面板真正跑的那条路了。
///
/// 两件事必须成立：
/// 1. 导出清单与模块体**双向**对得上（清单多了 → import 到 undefined；模块体多了 →
///    require 拿得到、import 拿不到）；
/// 2. require 来的 Buffer 与全局 Buffer 是**同一个类**（否则两边的 instanceof 会
///    悄悄变成 false —— 这正是"两份实现"最难查的症状）。
#[test]
fn node_buffer_module_exports_the_same_class() {
    use crate::jscompat::buffer::EXPORTS;

    let src = crate::jshost::node_module_for_require("node:buffer")
        .expect("jshost::node_module_parts 里要有 node:buffer（接线由 Lead 负责）");
    let body = crate::jscompat::buffer::BODY;
    assert!(src.starts_with(body), "CJS 包装器要先把模块体原样放前面");

    // ESM 包装器只是"从同一个 CJS 对象上取名字"，不重新实现。
    let esm =
        crate::jshost::node_module_for_import("node:buffer").expect("node:buffer 要有 ESM 视图");
    assert!(esm.contains("__builtinCjsModule(\"node:buffer\")"), "{esm}");
    assert!(esm.contains("export default M;"));
    for name in EXPORTS {
        assert!(
            esm.contains(&format!("export const {name} = M.{name};")),
            "ESM 视图少了 {name}"
        );
    }

    // 导出清单就是 Node 的那 10 个名字（多了少了都会让插件在某处拿到 undefined）。
    let mut names: Vec<&str> = EXPORTS.to_vec();
    names.sort_unstable();
    assert_eq!(
        names,
        vec![
            "Buffer",
            "INSPECT_MAX_BYTES",
            "SlowBuffer",
            "atob",
            "btoa",
            "constants",
            "isAscii",
            "isUtf8",
            "kMaxLength",
            "kStringMaxLength",
        ],
        "node:buffer 的导出清单要正好是这 10 个"
    );

    let script = MODULE_SCRIPT.replace("__SRC__", &serde_json::to_string(&src).unwrap());
    with_compat(|_, ctx| {
        let got = eval_json(ctx, &script);
        assert_eq!(
            got["sameObject"], true,
            "模块对象要缓存成同一个实例（require 两次）"
        );
        assert_eq!(
            got["bufferIsGlobal"], true,
            "模块导出的 Buffer 必须就是全局那一个类"
        );
        assert_eq!(
            got["keys"],
            serde_json::json!([
                "Buffer",
                "INSPECT_MAX_BYTES",
                "SlowBuffer",
                "atob",
                "btoa",
                "constants",
                "isAscii",
                "isUtf8",
                "kMaxLength",
                "kStringMaxLength"
            ]),
            "模块体导出的名字与清单不一致（多一个/少一个都会让 import 出问题）"
        );
        assert_eq!(got["slowBufferLen"], 4);
        assert_eq!(got["slowBufferZeroed"], true, "SlowBuffer 同样清零");
        assert_eq!(got["atobSame"], true, "atob 必须转发全局（同一份实现）");
        assert_eq!(got["btoaSame"], true);
        assert_eq!(
            got["constants"],
            serde_json::json!({
                "MAX_LENGTH": 9007199254740991_i64,
                "MAX_STRING_LENGTH": 536870888_i64
            })
        );
        assert_eq!(got["kMaxLength"], 9007199254740991_i64);
        assert_eq!(got["kStringMaxLength"], 536870888);
        assert_eq!(got["inspectMaxBytes"], 50);
        assert_eq!(got["isUtf8"], serde_json::json!([true, false]));
        assert_eq!(got["isAscii"], serde_json::json!([true, false]));
        assert_eq!(got["isBuffer"], true);
        assert_eq!(got["fromHex"], "abc");
        assert_eq!(got["subarrayShares"], true);
    });
}

const MODULE_SCRIPT: &str = r##"
(function () {
    var Buffer = globalThis.Buffer;
    // 模拟 jshost::install_builtin_node_modules 的注入：把 node:buffer 的模块体挂上去，
    // 之后走的就是 BOOTSTRAP 里那个 __builtinCjsModule（与 require('node:buffer') 同路）。
    globalThis.__host_node_cjs = { 'node:buffer': __SRC__ };
    var M = globalThis.__builtinCjsModule('node:buffer');
    var M2 = globalThis.__builtinCjsModule('node:buffer');
    var keys = Object.keys(M);
    keys.sort();
    var sb = M.SlowBuffer(4);
    var parent = M.Buffer.from([1, 2, 3]);
    var child = parent.subarray(1);
    child[0] = 9;
    return {
        sameObject: M === M2,
        bufferIsGlobal: M.Buffer === Buffer,
        keys: keys,
        slowBufferLen: sb.length,
        slowBufferZeroed: sb.every(function (v) { return v === 0; }),
        atobSame: M.atob === globalThis.atob,
        btoaSame: M.btoa === globalThis.btoa,
        constants: M.constants,
        kMaxLength: M.kMaxLength,
        kStringMaxLength: M.kStringMaxLength,
        inspectMaxBytes: M.INSPECT_MAX_BYTES,
        isUtf8: [M.isUtf8(new Uint8Array([0x61])), M.isUtf8(new Uint8Array([0xff]))],
        isAscii: [M.isAscii(new Uint8Array([0x61])), M.isAscii(new Uint8Array([0x80]))],
        isBuffer: M.Buffer.isBuffer(M.Buffer.from('x')),
        fromHex: M.Buffer.from('616263', 'hex').toString(),
        subarrayShares: parent[1] === 9
    };
})()
"##;

// ──────────────────────────────────────────────────────── 继承来的 TypedArray 行为

/// Buffer 是 Uint8Array 子类，所以 map / filter / sort / copyWithin / join / 迭代
/// 这些**不是**我们写的 —— 它们来自 Uint8Array.prototype，但会走 SpeciesConstructor
/// （内部要 new Buffer(len)）。QuickJS 在这里如果和 Node 不一样，插件会在最意想不到的
/// 地方拿到 Uint8Array 而不是 Buffer（buf.map(...) instanceof Buffer === false）。
#[test]
fn buffer_behaves_like_a_real_typedarray() {
    with_compat(|_, ctx| {
        let got = eval_json(
            ctx,
            r##"
        (function () {
            var Buffer = globalThis.Buffer;
            function hex(b) {
                var s = '';
                for (var i = 0; i < b.length; i++) s += ('0' + b[i].toString(16)).slice(-2);
                return s;
            }
            var mapped = Buffer.from([1, 2, 3]).map(function (x) { return x * 2; });
            var filtered = Buffer.from([1, 2, 3]).filter(function (x) { return x > 1; });
            var setBuf = Buffer.from([1, 2, 3]);
            setBuf.set([9], 0);
            var src = Buffer.from([1, 2, 3]);
            var forOf = 0;
            for (var v of src) forOf += v;
            return {
                map: [hex(mapped), mapped instanceof Buffer],
                filter: [hex(filtered), filtered instanceof Buffer],
                reverse: hex(Buffer.from([1, 2]).reverse()),
                sort: hex(Buffer.from([3, 1, 2]).sort()),
                copyWithin: hex(Buffer.from([1, 2, 3]).copyWithin(0, 1)),
                copyWithinNegative: hex(Buffer.from([1, 2, 3, 4]).copyWithin(-2)),
                set: hex(setBuf),
                join: Buffer.from([1, 2, 3]).join('-'),
                spread: Array.prototype.slice.call(Buffer.from([1, 2])),
                forOf: forOf,
                keys: Array.from(Buffer.from([7, 8]).keys()),
                entries: Array.from(Buffer.from([7, 8]).entries()).map(function (e) { return e.join(':'); }),
                fillNumber: hex(Buffer.from([1, 2, 3]).fill(9, 1)),
                sliceInstance: Buffer.from([1, 2]).slice(0) instanceof Buffer,
                toStringOnPlainU8: Buffer.prototype.toString.call(new Uint8Array([97, 98])),
                jsonArray: JSON.stringify([Buffer.from([1]), Buffer.from([2, 3])]),
                concatUint8: hex(Buffer.concat([new Uint8Array([1]), Buffer.from([2])])),
                writeShorthand: (function () {
                    var b = Buffer.alloc(4);
                    var n = b.write('abcd', 'hex');
                    return [n, hex(b)];
                })(),
                subarrayNegative: Array.prototype.slice.call(Buffer.from([1, 2, 3, 4]).subarray(-2)),
                isView: ArrayBuffer.isView(Buffer.from([1])),
                allocByteOffset: Buffer.alloc(4).byteOffset
            };
        })()
        "##,
        );

        assert_eq!(
            got["map"],
            serde_json::json!(["020406", true]),
            "map 应该返回 Buffer"
        );
        assert_eq!(
            got["filter"],
            serde_json::json!(["0203", true]),
            "filter 应该返回 Buffer"
        );
        assert_eq!(got["reverse"], "0201");
        assert_eq!(got["sort"], "010203");
        assert_eq!(got["copyWithin"], "020303");
        assert_eq!(got["copyWithinNegative"], "01020102");
        assert_eq!(got["set"], "090203");
        assert_eq!(got["join"], "1-2-3");
        assert_eq!(got["spread"], serde_json::json!([1, 2]));
        assert_eq!(got["forOf"], 6);
        assert_eq!(got["keys"], serde_json::json!([0, 1]));
        assert_eq!(got["entries"], serde_json::json!(["0:7", "1:8"]));
        assert_eq!(got["fillNumber"], "010909");
        assert_eq!(got["sliceInstance"], true);
        assert_eq!(got["toStringOnPlainU8"], "ab");
        assert_eq!(
            got["jsonArray"],
            r#"[{"type":"Buffer","data":[1]},{"type":"Buffer","data":[2,3]}]"#
        );
        assert_eq!(got["concatUint8"], "0102");
        assert_eq!(got["writeShorthand"], serde_json::json!([2, "abcd0000"]));
        assert_eq!(got["subarrayNegative"], serde_json::json!([3, 4]));
        assert_eq!(got["isView"], true);
        assert_eq!(got["allocByteOffset"], 0);
    });
}
