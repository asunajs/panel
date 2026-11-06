#!/usr/bin/env node
// 生成 src/jscompat/vectors/url.json —— 期望值来自**真的 Node**。
//
// 为什么要有这个脚本：兼容层的意义是「同一份插件在 QuickJS 与 Node 下行为一致」。
// 手写期望值等于把"我以为 Node 会这样"固化进测试里；用 Node 生成，才是把
// **Node 的实际行为**固化进来。改了 url.js / node_url.js 之后重跑它，差异会直接暴露。
//
// 用法：node scripts/gen-vectors-url.mjs
//
// ⚠ 生成结果要提交进仓库：跑测试的机器（CI / 离线构建）不需要装 Node。
//
// 刻意**不收集**的两类输入（见 url.js 文件头的已知差异）：
// 1. IDN 主机名（Node 会 punycode，本实现不做 IDNA）；
// 2. opaque base（mailto: / data: / foo:bar）加带 '#' 的相对地址 —— Node v24
//    在这里有非规范行为（会成功解析），浏览器与 URL 规范都是抛错，本实现按规范。

import { writeFileSync, mkdirSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath, pathToFileURL as nodePathToFileURL, urlToHttpOptions } from 'node:url';
import * as nodeUrl from 'node:url';

const here = dirname(fileURLToPath(import.meta.url));
const outFile = join(here, '..', 'src', 'jscompat', 'vectors', 'url.json');

const BT = String.fromCharCode(96);
const DQ = String.fromCharCode(34);

const FIELDS = ['href', 'origin', 'protocol', 'username', 'password', 'host',
    'hostname', 'port', 'pathname', 'search', 'hash'];

function snapshot(u) {
    const out = {};
    for (const f of FIELDS) out[f] = u[f];
    out.searchParams = u.searchParams.toString();
    return out;
}

function errorTag(e) {
    return e.name + ':' + (e.code || '');
}

// ──────────────────────────────────────────────────────── parse 矩阵

const parseCases = [
    ['https://example.com/'],
    ['https://example.com'],
    ['https://example.com:443/'],
    ['http://example.com:80/'],
    ['https://example.com:8080/a/b?x=1#frag'],
    ['https://user:pass@example.com/p'],
    ['https://user@example.com/'],
    ['http://user:pass@example.com:8080/a?b=c#d'],
    ['https://user:pa%40ss@example.com/'],
    ['http://u s:p@ss@example.com/'],
    ['https://example.com/a/b/../c'],
    ['https://example.com/a/./b'],
    ['https://example.com/a//b'],
    ['https://example.com/%2e%2E/a'],
    ['https://example.com/a b'],
    ['https://example.com/a/[]{}|^' + BT + DQ + '<>'],
    ['https://example.com/?a b' + DQ + '#<>'],
    ['https://example.com/?a=' + String.fromCharCode(39) + 'b'],
    ['https://example.com/#a b' + DQ + '<>' + BT],
    ['https://example.com/?#f'],
    ['https://example.com/#'],
    ['https://example.com/a?b#c?d#e'],
    ['https://example.com/?q=a+b'],
    ['https://example.com/path%2Fsub'],
    ['https://example.com/path%2e'],
    ['https://example.com/%E4%BD%A0%E5%A5%BD'],
    ['https://example.com/你好'],
    ['https://example.com/?q=%E4%BD%A0'],
    ['https://example.com/café'],
    ['https://example.com/%zz'],
    ['https://EXAMPLE.COM./A'],
    ['https://ex%61mple.com/'],
    ['https://example.com:65535/'],
    ['https://example.com:0/'],
    ['http://127.0.0.1/'],
    ['http://127.0.0.1:8080/a'],
    ['http://127.0.0.1./'],
    ['http://0x7f.1/'],
    ['http://010.0.0.1/'],
    ['http://999999999/'],
    ['http://[::1]/'],
    ['http://[2001:db8::1]:8080/a'],
    ['http://[0:0:0:0:0:0:0:1]/'],
    ['http://[1:0:0:2:0:0:0:3]/'],
    ['http://[1:0:0:2:0:0:3:4]/'],
    ['http://[1:2:3:4:5:6:0:0]/'],
    ['http://[1:0:2:3:4:5:6:7]/'],
    ['ws://example.com/socket'],
    ['wss://example.com:443/socket'],
    ['ftp://example.com/pub/file.txt'],
    ['file:///etc/passwd'],
    ['file://localhost/etc/hosts'],
    ['file:///C:/Windows/System32'],
    ['file://server/share/file'],
    ['file:///a/../../b'],
    ['file://C|/x'],
    ['file:'],
    ['file:/'],
    ['file://'],
    ['foo:bar'],
    ['foo:bar/baz?q=1#f'],
    ['foo:/bar'],
    ['foo://Host/x'],
    ['foo://'],
    ['foo://h'],
    ['foo://h/x'],
    ['foo:/.//not-a-host/'],
    ['web+demo:/path/..//not-a-host/'],
    ['a+b:c'],
    ['mailto:user@example.com'],
    ['mailto:a b@c.d'],
    ['data:text/plain,hello%20world'],
    ['data:text/plain,hi there'],
    ['data:text/plain,hi ?x'],
    ['urn:isbn:9780131103627'],
    ['blob:https://example.com/uuid'],
    ['blob:null/abc'],
    ['https://example.com/?a=~* _-./'],
    ['https://a.com/a/..'],
    ['https://a.com/a/../'],
    ['https://a.com/a/..//b'],
    ['https://a.com/.//x'],
    ['https://a.com/..//x'],
    ['https://a.com//..//x'],
    ['https://a.com/a/%2e%2e'],
    ['https://a.com/a/.%2e'],
    ['https://a.com/a/%2E./b'],
    ['https://a.com/%'],
    ['file:///a%zz'],
    // 相对解析
    ['/a', 'https://x.y/b/c'],
    ['b/c', 'https://a.com/x/y'],
    ['../c', 'https://a.com/a/b/c'],
    ['..', 'https://a.com/a/b/c?q#f'],
    ['.', 'https://a.com/a/b'],
    ['?q=2', 'https://a.com/a/b?q=1#f'],
    ['#f2', 'https://a.com/a/b?q=1#f'],
    ['', 'https://a.com/a/b?q=1#f'],
    ['//other.example/x', 'https://a.com/'],
    ['https://other.example/x', 'https://a.com/'],
    ['//h', 'file:///a/b'],
    ['../a', 'file:///b/c/d'],
    ['C:/x', 'file:///b/c/d'],
    ['/C:/x', 'file:///a/b'],
    ['?q', 'https://a.com/a/b?q=1#f'],
    ['#f', 'mailto:a@b'],
    ['..', 'https://a.com/'],
    ['../../../g', 'https://a.com/a/b'],
    ['http:foo', 'https://a.com/x'],
    ['https:foo', 'https://a.com/x'],
    ['http:/foo'],
    ['http:foo.com'],
    ['http:foo.com', 'https://a.com/'],
    ['https:example.com', 'https://a.com/'],
    ['http://127.1/'],
    ['https://example.com\\a\\b'],
    ['%2e%2e/x', 'https://a.com/a/b/c'],
    ['about:blank'],
    ['blob:https://x/y'],
    ['\\h\\x', 'http://a.com:8080/x'],
    ['http://a/b\\c'],
    ['foo://h/a\\b'],
    ['b', 'foo://h/a'],
    ['/b', 'foo://h/a'],
    ['//x/y', 'foo://h/a'],
    ['../b', 'foo://h/a/c'],
    ['?q', 'foo://h/a?old#f'],
];

const parse = parseCases.map(([input, base]) => {
    let u;
    try {
        u = new URL(input, base);
    } catch (e) {
        throw new Error('parse 用例本身不合法（应放进 throws）: ' + JSON.stringify(input) + ' -> ' + e.code);
    }
    const item = { input: input, expect: snapshot(u) };
    if (base !== undefined) item.base = base;
    return item;
});

// ──────────────────────────────────────────────────────── 非法输入

const throwCases = [
    ['http://'],
    ['https://a b/'],
    ['not a url'],
    ['/a'],
    ['foo', 'mailto:a@b'],
    ['https://exa%23mple.org/'],
    ['https://[::1/'],
    ['https://[::1]x/'],
    ['https://example.com:99999/'],
    ['https://example.com:abc/'],
    ['https://example.com:65536/'],
    ['http://1.2.3.4.5/'],
    ['http://09.0.0.1/'],
    ['http://[v1.x]/'],
    ['file://%zz'],
    ['http://%ff/'],
    ['http://%c0%af/'],
    ['https://a.com:80x/'],
    ['/a', 'not a url'],
    ['/a', 123],
];

const throws = throwCases.map(([input, base]) => {
    let outcome = null;
    try {
        new URL(input, base);
    } catch (e) {
        outcome = { name: e.name, code: e.code, input: e.input };
    }
    if (outcome === null) throw new Error('该抛错的用例没抛: ' + JSON.stringify(input));
    const item = { input: input, expect: outcome };
    if (base !== undefined) item.base = base;
    return item;
});

// ──────────────────────────────────────────────────────── setter 矩阵

function applyOps(u, ops) {
    let threw = null;
    for (const op of ops) {
        try {
            u[op[0]] = op[1];
        } catch (e) {
            threw = errorTag(e);
            break;
        }
    }
    return threw;
}

const setterCases = [
    ['https://a.com:8080/p', [['host', 'b.com']]],
    ['https://a.com:8080/p', [['host', 'b.com:99']]],
    ['https://a.com:8080/p', [['hostname', 'b.com:99']]],
    ['https://a.com/p', [['hostname', 'a b']]],
    ['https://a.com/p', [['host', '']]],
    ['https://a.com/p', [['host', '%ff']]],
    ['https://a.com/p', [['hostname', '']]],
    ['foo://h/x', [['host', '']]],
    ['https://a.com/p', [['port', '99x']]],
    ['https://a.com/p', [['port', '+99']]],
    ['https://a.com/p', [['port', '099']]],
    ['https://a.com/p', [['port', '65536']]],
    ['https://a.com/p', [['port', '0']]],
    ['https://a.com:8080/p', [['port', '443']]],
    ['https://a.com:8080/p', [['port', '']]],
    ['foo://h:99/x', [['port', '99']]],
    ['http://a.com:80/p', [['protocol', 'https']]],
    ['http://a.com/p', [['protocol', 'mailto']]],
    ['https://a.com/p', [['protocol', '1http']]],
    ['https://a.com/p', [['protocol', 'https:']]],
    ['https://a.com/p', [['protocol', 'file']]],
    ['http://a.com:99/p', [['protocol', 'file']]],
    ['file:///x', [['protocol', 'http']]],
    ['foo:bar', [['protocol', 'foo2']]],
    ['foo://h/x', [['protocol', 'https']]],
    ['https://h/x', [['protocol', 'foo']]],
    ['http://a.com/', [['username', 'a b@c']]],
    ['http://a.com/', [['password', 'p q']]],
    ['http://a.com/', [['username', 'a:b']]],
    ['http://a.com/', [['password', 'p:q']]],
    ['http://a.com/', [['password', 'p/q']]],
    ['http://a.com/', [['username', 'a%20b']]],
    ['file:///a', [['username', 'x']]],
    ['file:///a', [['host', 'x']]],
    ['file:///a/b', [['hostname', 'x']]],
    ['file://server/a', [['host', '']]],
    ['http://[::1]:8080/', [['hostname', '[::1]']]],
    ['http://a.com/', [['hostname', '::1']]],
    ['https://a.com/x/y', [['pathname', '/a/../b']]],
    ['https://a.com/x', [['pathname', '']]],
    ['https://a.com/x', [['pathname', 'a']]],
    ['https://a.com/x', [['pathname', '/a b?c#d']]],
    ['foo:bar', [['pathname', 'a b']]],
    ['foo://h/x', [['pathname', '']]],
    ['foo://', [['pathname', 'x y']]],
    ['file:///a', [['pathname', 'C:/x']]],
    ['file:///a', [['pathname', '/C:/x']]],
    ['https://a.com/x', [['search', '?x=1 2']]],
    ['https://a.com/x', [['search', '?a=' + String.fromCharCode(39) + 'b']]],
    ['https://a.com/x', [['search', '?a=b#c']]],
    ['https://a.com/x?q', [['search', '']]],
    ['https://a.com/x?q', [['search', '?']]],
    ['foo://h/x', [['search', '?a=' + String.fromCharCode(39) + 'b']]],
    ['https://a.com/x', [['hash', ' a b']]],
    ['https://a.com/x', [['hash', 'a b ']]],
    ['https://a.com/x#a', [['hash', '']]],
    ['https://a.com/x#a', [['hash', '#']]],
    ['https://a.com/x', [['hash', '#a#b']]],
    ['https://a.com/x', [['hash', '#' + BT]]],
    ['https://a.com/x', [['search', '?' + BT]]],
    ['https://a.com/x', [['search', '??a']]],
    ['https://a.com/x', [['href', 'https://b.com/y?z#w']]],
    ['https://a.com/x', [['href', 'https://a b/']]],
];

const setters = setterCases.map(([input, ops]) => {
    const u = new URL(input);
    const threw = applyOps(u, ops);
    const expect = { threw: threw };
    Object.assign(expect, snapshot(u));
    return { input: input, ops: ops, expect: expect };
});

// ──────────────────────────────────────────────────────── URLSearchParams

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

const paramCases = [
    { init: { kind: 'string', value: 'a=1&b=2' }, ops: [] },
    { init: { kind: 'string', value: '?a=1' }, ops: [] },
    { init: { kind: 'string', value: '' }, ops: [] },
    { init: { kind: 'string', value: '?' }, ops: [] },
    { init: { kind: 'string', value: 'a&b&c=1' }, ops: [] },
    { init: { kind: 'string', value: 'a=1+2&b=%20' }, ops: [] },
    { init: { kind: 'string', value: 'a=%zz' }, ops: [] },
    { init: { kind: 'string', value: 'a=1&a=2' }, ops: [] },
    { init: { kind: 'string', value: 'a=你好&b=%E4%BD%A0' }, ops: [] },
    { init: { kind: 'number', value: 5 }, ops: [] },
    { init: { kind: 'null' }, ops: [] },
    { init: { kind: 'none' }, ops: [] },
    { init: { kind: 'object', value: { a: '1', b: 2, 2: 'x', 1: 'y' } }, ops: [] },
    { init: { kind: 'pairs', value: [['a', '1'], ['b', '2']] }, ops: [] },
    { init: { kind: 'copy', value: 'a=1' }, ops: [] },
    { init: { kind: 'string', value: 'a=1' }, ops: [['append', 'a', '2']] },
    { init: { kind: 'string', value: 'a=1&b=2' }, ops: [['append', 'b', 'x y'], ['append', '', '']] },
    { init: { kind: 'string', value: 'a=1&a=2' }, ops: [['set', 'a', '9']] },
    { init: { kind: 'string', value: 'a=1&b=2&a=3' }, ops: [['set', 'a', '9']] },
    { init: { kind: 'string', value: 'a=1&a=2&b=3' }, ops: [['delete', 'a']] },
    { init: { kind: 'string', value: 'a=1&a=2&b=3' }, ops: [['delete', 'a', '2']] },
    { init: { kind: 'string', value: 'b=2&a=1&a=0&c=3' }, ops: [['sort']] },
    { init: { kind: 'string', value: 'a=1&b=2&a=0' }, ops: [['sort']] },
    { init: { kind: 'string', value: 'a=~* _-./' }, ops: [['set', 'b', String.fromCharCode(39)], ['append', 'c', '你好']] },
    { init: { kind: 'string', value: '' }, ops: [['append', 'a', '1'], ['delete', 'a']] },
    { init: { kind: 'string', value: 'a=1' }, ops: [['sort'], ['sort']] },
];

const params = paramCases.map(({ init, ops }) => {
    const p = makeInit(init);
    for (const op of ops) p[op[0]].apply(p, op.slice(1));
    return {
        init: init,
        ops: ops,
        expect: {
            pairs: Array.from(p.entries()),
            string: p.toString(),
            size: p.size,
        },
    };
});

const paramQueryCases = [
    { init: { kind: 'string', value: 'a=1&a=2&b=3' }, calls: [['get', 'a'], ['getAll', 'a'], ['has', 'a'], ['has', 'a', '2'], ['has', 'a', '9'], ['get', 'zz']] },
    { init: { kind: 'string', value: 'a=1&a=2&b=3' }, calls: [['delete', 'a', '2']] },
    { init: { kind: 'string', value: 'a=1&a=2' }, calls: [['delete', 'a']] },
    { init: { kind: 'string', value: 'a=1' }, calls: [['get']] },
    { init: { kind: 'string', value: 'a=1' }, calls: [['append', 'a']] },
    { init: { kind: 'string', value: 'a=1' }, calls: [['set', 'a']] },
    { init: { kind: 'string', value: 'a=1' }, calls: [] },
];

const paramsQueries = paramQueryCases.map(({ init, calls }) => {
    const p = makeInit(init);
    const result = calls.map((call) => {
        try {
            const value = p[call[0]].apply(p, call.slice(1));
            return value === undefined ? null : value;
        } catch (e) {
            return errorTag(e);
        }
    });
    return { init: init, calls: calls, expect: result };
});

const urlParamCases = [
    ['https://a.com/?a=1', [['set', 'a', '1'], ['append', 'a', '2']]],
    ['https://a.com/x', [['set', 'a', '1']]],
    ['https://a.com/x', [['set', 'a', '1'], ['delete', 'a']]],
    ['https://a.com/?', [['delete', 'a']]],
    ['https://a.com/?b=2&a=1&a=0&c=3', [['sort']]],
    ['https://a.com/?a=%20&b=+', [['append', 'c', ' ']]],
    ['https://a.com/x#h', [['set', 'a', '1']]],
    ['https://a.com/?a=1', [['search', '?b=2']]],
    ['https://a.com/?a=1', [['href', 'https://b.com/?c=3']]],
    ['https://a.com/?a=1', [['hash', 'x']]],
    ['mailto:a@b?x=1', [['set', 'y', '2']]],
    ['foo://h/x', [['set', 'a b', 'c d']]],
];

const urlParams = urlParamCases.map(([input, ops]) => {
    const u = new URL(input);
    const p = u.searchParams;
    const identity = p === u.searchParams;
    for (const op of ops) {
        if (op[0] === 'search' || op[0] === 'hash' || op[0] === 'href') u[op[0]] = op[1];
        else p[op[0]].apply(p, op.slice(1));
    }
    return {
        input: input,
        ops: ops,
        expect: {
            identity: identity,
            href: u.href,
            search: u.search,
            params: p.toString(),
            size: p.size,
        },
    };
});

// ──────────────────────────────────────────────────────── 静态方法

function staticCall(fn, input, base) {
    try {
        const value = URL[fn](input, base);
        if (fn === 'parse') return value === null ? null : value.href;
        return value;
    } catch (e) {
        return errorTag(e);
    }
}

const staticCases = [
    ['canParse', 'https://x/'],
    ['canParse', '/a'],
    ['canParse', '/a', 'https://x/'],
    ['canParse', 'bad'],
    ['canParse', undefined],
    ['canParse', ''],
    ['canParse', 123],
    ['canParse', 'http://a/', 123],
    ['parse', '/a', 'https://x.y/b/c'],
    ['parse', 'https://a b/'],
    ['parse', undefined],
    ['parse', 'http://a/', 123],
    ['parse', '//h/x', 'file:///a/b'],
];

const statics = staticCases.map(([fn, input, base]) => {
    const item = { fn: fn, input: input, expect: staticCall(fn, input, base) };
    if (base !== undefined) item.base = base;
    return item;
});

// ──────────────────────────────────────────────────────── node:url 助手

function outcome(fn) {
    try {
        return { ok: true, value: fn() };
    } catch (e) {
        return { ok: false, name: e.name, code: e.code };
    }
}

const urlToHttpOptionsCases = [
    'https://u:p@x.y:8080/a?b#c',
    'http://x.y/',
    'file:///a',
    'file://server/x',
    'http://u@a/',
    'http://:@a/',
    'https://[::1]:99/x',
    'foo://h:12/p',
    'mailto:a@b',
    'http://a:80/',
];

const fileURLToPathCases = [
    'file:///tmp/a%20b',
    'file:///tmp/%E4%BD%A0',
    'file://localhost/x',
    'file://server/x',
    'http://x/',
    'not a url',
    'file:///a%00b',
    'file:///a%2Fb',
    'file:///a%5Cb',
    'file:///a+b',
    'file:///a/../b',
    'file:relative',
    'file://',
    'file:///C:/x',
];

const pathToFileURLCases = [
    '/',
    '/tmp/a b',
    '/tmp/a%b',
    '/tmp/a\\b',
    '/tmp/?#[]{}|^' + BT + DQ + String.fromCharCode(39) + '<>;:=@&+$,~!()*_-.',
    '/tmp/a\tb',
    '/tmp/a\nb',
    '/tmp/../a',
    '/tmp//a',
    '/tmp/a/',
    '/tmp/.',
    '/tmp/a\u0000b',
    '/a/./b/../c',
    '/..',
    '/tmp/a b/',
    '/tmp/[]',
    '/tmp/~x',
];

const nodeUrlVectors = {
    urlToHttpOptions: urlToHttpOptionsCases.map((input) => ({
        input: input,
        expect: outcome(() => urlToHttpOptions(new URL(input))),
    })),
    fileURLToPath: fileURLToPathCases.map((input) => ({
        input: input,
        expect: outcome(() => nodeUrl.fileURLToPath(input)),
    })),
    pathToFileURL: pathToFileURLCases.map((input) => ({
        input: input,
        expect: outcome(() => nodePathToFileURL(input).href),
    })),
};

// ──────────────────────────────────────────────────────── 输出

if (parse.length < 60) throw new Error('parse 矩阵少于 60 条');

const out = {
    note: 'generated by scripts/gen-vectors-url.mjs on node ' + process.version,
    parse: parse,
    throws: throws,
    setters: setters,
    params: params,
    paramsQueries: paramsQueries,
    urlParams: urlParams,
    statics: statics,
    nodeUrl: nodeUrlVectors,
};

mkdirSync(dirname(outFile), { recursive: true });
writeFileSync(outFile, JSON.stringify(out, null, 2) + '\n');
console.log('wrote', outFile, '—', Object.entries(out)
    .filter(([k]) => k !== 'note')
    .map(([k, v]) => k + ':' + (Array.isArray(v) ? v.length : Object.values(v).map((x) => x.length).join('/')))
    .join(' '));
