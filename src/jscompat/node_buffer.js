// node:buffer 内置模块（QuickJS 侧）。
//
// 模块体由 new Function(body)() 执行（见 jshost::node_module_for_require），末尾会拼上
// return { ...导出清单 }; —— 所以下面的名字必须是顶层 var / const / function。
//
// 真正的实现只有一份：globalThis.__jscompat.buffer（buffer.js）。这里只做"编织"：
// 拿到全局那份再原样导出，保证 require('node:buffer').Buffer 与全局 Buffer 是**同一个类**
// （Node 就是这样）。再造一个类的话，两个 Buffer 之间的 instanceof 会悄悄变成 false ——
// 那是最难查的一类偏差。
'use strict';

var B = globalThis.__jscompat.buffer;
if (!B) throw new Error('node:buffer 需要先加载 jscompat 的 buffer.js（兼容层没装上？）');

// 用 const 而不是 var：jshost::tests::every_exported_builtin_name_is_actually_defined
// 是靠静态扫描（function NAME( / const NAME = / NAME:）确认"模块体里真有这个名字"的 ——
// 写成 var 会让那条对账测试看不见这些名字，于是一个打错字的名字要等插件 import 到
// undefined 才暴露。
const Buffer = B.Buffer;
const SlowBuffer = B.SlowBuffer;
const isUtf8 = B.isUtf8;
const isAscii = B.isAscii;
const constants = B.constants;
const kMaxLength = B.kMaxLength;
const kStringMaxLength = B.kStringMaxLength;
const INSPECT_MAX_BYTES = B.INSPECT_MAX_BYTES;

// atob / btoa 直接转发全局（text.js 装的那两个）：不另写一份，
// 否则 atob 和 buf.toString('base64') 迟早会在某个边界上对不上。
const atob = globalThis.atob;
const btoa = globalThis.btoa;
