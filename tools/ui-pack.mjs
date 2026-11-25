#!/usr/bin/env node
// 界面包打包。
//
// 为什么单独一个脚本：面板的 `web/` 是**编进二进制**的（`include_str!`），所以「只改前端」
// 过去只能靠重编译 + 重发整个面板。界面包把同一份字节打成 ~300 KB 的 zip，
// 装上即生效、不换二进制、不升面板版本。
//
// 用法：
//   node tools/ui-pack.mjs                # 打包到 dist/panel-ui-<ver>.<n>.zip
//   node tools/ui-pack.mjs --check        # 只校验（门禁用：清单与 web.rs 白名单一致、可复现）
//   node tools/ui-pack.mjs --seq 3        # 指定序号（默认 1）
//   PANEL_UI_OUT=/tmp/x node tools/ui-pack.mjs
//
// 产物可复现：固定 DOS 时间戳 + 路径排序（同 scripts/zip.mjs 的做法），
// 同一份 web/ 反复打出来 sha256 相同。
import { createHash } from 'node:crypto';
import { execFileSync } from 'node:child_process';
import { existsSync, mkdirSync, readFileSync, readdirSync, rmSync, statSync, writeFileSync } from 'node:fs';
import { dirname, join, relative, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { deflateRawSync } from 'node:zlib';

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const WEB = join(ROOT, 'web');
const CHECK = process.argv.includes('--check');
const seqArg = process.argv.indexOf('--seq');
const SEQ = seqArg >= 0 ? String(process.argv[seqArg + 1] || '1') : '1';
const OUT = process.env.PANEL_UI_OUT ? resolve(process.env.PANEL_UI_OUT) : join(ROOT, 'dist');

const die = (msg) => {
  console.error(`❌ ${msg}`);
  process.exit(1);
};

/** 面板版本：唯一真源是 Cargo.toml。 */
function panelVersion() {
  const toml = readFileSync(join(ROOT, 'Cargo.toml'), 'utf8');
  const m = /^\[package\][\s\S]*?^version\s*=\s*"([^"]+)"/m.exec(toml);
  if (!m) die('读不出 Cargo.toml 的 version');
  return m[1];
}

/**
 * 白名单：与 `src/web.rs` 里的 `include_str!` 资源 + `ui_pack::ALLOWED` 一一对应。
 * 不手写死 —— 从 `src/ui_pack.rs` 里把 ALLOWED 抠出来，两边就不可能漂。
 */
function allowedFiles() {
  const src = readFileSync(join(ROOT, 'src/ui_pack.rs'), 'utf8');
  const block = /pub const ALLOWED: &\[&str\] = &\[([\s\S]*?)\];/.exec(src);
  if (!block) die('src/ui_pack.rs 里找不到 ALLOWED');
  const names = [...block[1].matchAll(/"([^"]+)"/g)].map((m) => m[1]);
  if (names.length === 0) die('ALLOWED 是空的');
  return names;
}

/** 必需清单同样从源码读（缺一个整个包装不上，所以打包时就要齐）。 */
function requiredFiles() {
  const src = readFileSync(join(ROOT, 'src/ui_pack.rs'), 'utf8');
  const block = /pub const REQUIRED: &\[&str\] = &\[([\s\S]*?)\];/.exec(src);
  if (!block) die('src/ui_pack.rs 里找不到 REQUIRED');
  return [...block[1].matchAll(/"([^"]+)"/g)].map((m) => m[1]);
}

// 最小 zip 写出器
// 与 scripts/zip.mjs 同源：固定时间戳，产物可复现。
const CRC_TABLE = (() => {
  const t = new Uint32Array(256);
  for (let i = 0; i < 256; i += 1) {
    let c = i;
    for (let k = 0; k < 8; k += 1) c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1;
    t[i] = c >>> 0;
  }
  return t;
})();
const crc32 = (buf) => {
  let c = 0xffffffff;
  for (const b of buf) c = CRC_TABLE[(c ^ b) & 0xff] ^ (c >>> 8);
  return (c ^ 0xffffffff) >>> 0;
};

function makeZip(entries) {
  const locals = [];
  const centrals = [];
  let offset = 0;
  const DOS_TIME = 0;
  const DOS_DATE = 0x0021; // 1980-01-01
  for (const e of entries) {
    const name = Buffer.from(e.name, 'utf8');
    const crc = crc32(e.data);
    const deflated = deflateRawSync(e.data, { level: 9 });
    const useDeflate = deflated.length < e.data.length;
    const body = useDeflate ? deflated : e.data;
    const method = useDeflate ? 8 : 0;

    const lh = Buffer.alloc(30);
    lh.writeUInt32LE(0x04034b50, 0);
    lh.writeUInt16LE(20, 4);
    lh.writeUInt16LE(0, 6);
    lh.writeUInt16LE(method, 8);
    lh.writeUInt16LE(DOS_TIME, 10);
    lh.writeUInt16LE(DOS_DATE, 12);
    lh.writeUInt32LE(crc, 14);
    lh.writeUInt32LE(body.length, 18);
    lh.writeUInt32LE(e.data.length, 22);
    lh.writeUInt16LE(name.length, 26);
    lh.writeUInt16LE(0, 28);
    locals.push(lh, name, body);

    const ch = Buffer.alloc(46);
    ch.writeUInt32LE(0x02014b50, 0);
    ch.writeUInt16LE(20, 4);
    ch.writeUInt16LE(20, 6);
    ch.writeUInt16LE(0, 8);
    ch.writeUInt16LE(method, 10);
    ch.writeUInt16LE(DOS_TIME, 12);
    ch.writeUInt16LE(DOS_DATE, 14);
    ch.writeUInt32LE(crc, 16);
    ch.writeUInt32LE(body.length, 20);
    ch.writeUInt32LE(e.data.length, 24);
    ch.writeUInt16LE(name.length, 28);
    ch.writeUInt16LE(0, 30);
    ch.writeUInt16LE(0, 32);
    ch.writeUInt16LE(0, 34);
    ch.writeUInt16LE(0, 36);
    ch.writeUInt32LE(0, 38);
    ch.writeUInt32LE(offset, 42);
    centrals.push(ch, name);

    offset += lh.length + name.length + body.length;
  }
  const cdSize = centrals.reduce((n, b) => n + b.length, 0);
  const end = Buffer.alloc(22);
  end.writeUInt32LE(0x06054b50, 0);
  end.writeUInt16LE(0, 4);
  end.writeUInt16LE(0, 6);
  end.writeUInt16LE(entries.length, 8);
  end.writeUInt16LE(entries.length, 10);
  end.writeUInt32LE(cdSize, 12);
  end.writeUInt32LE(offset, 16);
  end.writeUInt16LE(0, 20);
  return Buffer.concat([...locals, ...centrals, end]);
}

// 主流程
const panel = panelVersion();
const version = `${panel}.${SEQ}`;
const allowed = allowedFiles();
const required = requiredFiles();

// ① 先重建压缩产物（改了源文件忘了重建，包里就会是旧字节 —— 与面板发版时的口径一致）
if (!CHECK) {
  execFileSync(process.execPath, [join(ROOT, 'tools/web/build.mjs')], { stdio: 'inherit' });
}

// ② 收集文件：白名单里的都收（缺了必需文件直接失败）
const entries = [];
const missing = [];
for (const name of allowed) {
  const p = join(WEB, name);
  if (!existsSync(p)) {
    if (required.includes(name)) missing.push(name);
    continue;
  }
  entries.push({ name, data: readFileSync(p) });
}
if (missing.length) die(`缺必需文件：${missing.join('、')}（web/ 里没有）`);
entries.sort((a, b) => (a.name < b.name ? -1 : a.name > b.name ? 1 : 0));

// ③ 清单：文件名 → sha256（面板装载时会逐字节核对）
const files = {};
for (const e of entries) files[e.name] = createHash('sha256').update(e.data).digest('hex');
const manifest = {
  name: 'panel-ui',
  version,
  panel,
  built_at: new Date().toISOString().replace(/\.\d+Z$/, 'Z'),
  files,
};
const manifestBuf = Buffer.from(`${JSON.stringify(manifest, null, 2)}\n`);
if (CHECK) {
  // 校验模式不打时间戳（否则每次都"不一致"），只核对清单形状与可复现性
  const a = makeZip([...entries, { name: 'ui.json', data: Buffer.from('{}\n') }]);
  const b = makeZip([...entries, { name: 'ui.json', data: Buffer.from('{}\n') }]);
  if (!a.equals(b)) die('同样的输入两次打包结果不同（不可复现）');
  const total = entries.reduce((n, e) => n + e.data.length, 0);
  console.log(`ok: 界面包清单与 web/ 一致（${entries.length} 个文件，${(total / 1024).toFixed(1)} KB，version ${version}）`);
  const extra = allowed.filter((n) => !entries.some((e) => e.name === n));
  if (extra.length) console.log(`  （白名单里这些 web/ 中不存在，已跳过：${extra.join('、')}）`);
  process.exit(0);
}

const zip = makeZip([...entries, { name: 'ui.json', data: manifestBuf }]);
const sha = createHash('sha256').update(zip).digest('hex');
mkdirSync(OUT, { recursive: true });
const zipName = `panel-ui-${version}.zip`;
const zipPath = join(OUT, zipName);
rmSync(zipPath, { force: true });
writeFileSync(zipPath, zip);
writeFileSync(`${zipPath}.sha256`, `${sha}  ${zipName}\n`);
writeFileSync(join(OUT, 'UI-SHA256SUMS'), `${sha}  ${zipName}\n`);

const total = entries.reduce((n, e) => n + e.data.length, 0);
console.log(`✅ ${zipPath}`);
console.log(`   面板 ${panel} · 界面包 ${version} · ${entries.length} 个文件`);
console.log(`   解压后 ${(total / 1024).toFixed(1)} KB，zip ${(zip.length / 1024).toFixed(1)} KB`);
console.log(`   sha256 ${sha}`);
console.log('   装上：面板「设置 → 界面」上传，或 POST /api/ui/install（body 就是这个 zip）');
void statSync;
void readdirSync;
void relative;
