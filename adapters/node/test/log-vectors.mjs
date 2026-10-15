// 共享向量的 JS 侧测试。
//
// `testdata/redact-cases.json` 与 `testdata/log-format-cases.json` 是
// **Rust / JS / Python 三份实现共用**的向量文件：谁改歪了规则，谁这边的测试就红。
// 这是「同一份插件源码面板内外都能跑」这句话的唯一执行保障 —— 三份实现分散在
// 三种语言里，靠人肉对齐撑不过第三次改动。
//
// 跑法：node --test test/log-vectors.mjs

import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { spawnSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';

import { TYPES, LEVEL_OF, redact, stamp, rfc3339, lineOf, jsonOf } from '../src/log.js';

const HERE = path.dirname(fileURLToPath(import.meta.url));
// adapters/node/test -> 仓库根
const REPO = path.resolve(HERE, '..', '..', '..');
const readVectors = (name) =>
  JSON.parse(fs.readFileSync(path.join(REPO, 'testdata', name), 'utf8'));

test('脱敏规则与共享向量逐字符一致', () => {
  const { cases } = readVectors('redact-cases.json');
  assert.ok(cases.length >= 10, `向量太少（${cases.length}）—— 文件被删了？`);
  for (const [i, c] of cases.entries()) {
    assert.equal(
      redact(c.in),
      c.out,
      `向量 #${i} 不通过（${c.why}）\n  输入: ${JSON.stringify(c.in)}\n  期望: ${JSON.stringify(c.out)}\n  实际: ${JSON.stringify(redact(c.in))}`,
    );
  }
});

test('八档 type 与过滤级别的映射与共享向量一致', () => {
  const { type_level: map } = readVectors('log-format-cases.json');
  assert.equal(Object.keys(map).length, 8, '八档一个都不能少');
  assert.deepEqual([...TYPES].sort(), Object.keys(map).sort(), 'TYPES 的集合要和向量一致');
  for (const [type, level] of Object.entries(map)) {
    assert.equal(LEVEL_OF[type], level, `type \`${type}\` 的过滤级别不对`);
  }
});

test('行格式与 JSONL 字段与共享向量一致', () => {
  const { cases } = readVectors('log-format-cases.json');
  for (const [i, c] of cases.entries()) {
    const now = new Date();
    const entry = {
      ts: stamp(now),
      rfc: rfc3339(now),
      type: c.type,
      scope: c.scope,
      plugin: c.plugin,
      run: c.run,
      msg: redact(c.msg),
    };

    // ① 行：前缀是本地时间 `YYYY-MM-DD HH:MM:SS.mmm `（24 字符含尾空格），
    //    后缀逐字节等于向量里的 line_suffix
    const line = lineOf(entry);
    const suffix = c.line_suffix;
    assert.ok(
      line.endsWith(suffix),
      `向量 #${i} 行后缀不对（${c.why}）\n  实际: ${JSON.stringify(line)}\n  期望后缀: ${JSON.stringify(suffix)}`,
    );
    const prefix = line.slice(0, line.length - suffix.length);
    assert.equal(prefix.length, 24, `向量 #${i} 时间戳前缀长度不对: ${JSON.stringify(prefix)}`);
    assert.match(prefix, /^\d{4}-\d{2}-\d{2} \d{2}:\d{2}:\d{2}\.\d{3} $/);

    // ② JSONL：字段集合严格相等（多一个少一个都算错）
    const obj = jsonOf(entry);
    assert.deepEqual(
      Object.keys(obj).sort(),
      [...c.json_fields].sort(),
      `向量 #${i} JSONL 字段集合不对（${c.why}）`,
    );
    assert.equal(obj.msg, entry.msg);
    assert.equal(obj.scope, entry.scope);
    assert.equal(obj.type, c.type);
    assert.equal(obj.level, LEVEL_OF[c.type]);
    // ts 断言「是带毫秒的 RFC3339」，不断言字符串相等（时区差异不是格式差异）
    assert.match(obj.ts, /^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}\.\d{3}[+-]\d{2}:\d{2}$/);
    assert.equal(Date.parse(obj.ts) > 0, true, `ts 解析不出来: ${obj.ts}`);
  }
});

test('JSONL 落盘：按天切、立刻可见、内容脱敏', () => {
  // 单独起一个子进程：配置（HOST_LOG_DIR 等）只在首次用到时读一次环境变量，
  // 在主测试进程里改会污染别的用例。
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'host-api-log-'));
  const script = `
    import { createLogger, logState } from ${JSON.stringify(path.resolve(HERE, '..', 'src', 'log.js'))};
    const lg = createLogger({ scope: 'demo', plugin: 'demo' });
    lg.start('插件已加载');
    lg.warn('账号 13600136001 失败');
    lg('多参', { a: 1 }, 3);
    process.stderr.write('STATE ' + JSON.stringify(logState()) + '\\n');
  `;
  const r1 = spawnSync(process.execPath, ['--input-type=module', '-e', script], {
    env: { ...process.env, HOST_LOG_DIR: dir, HOST_LOG_COLOR: '0' },
    encoding: 'utf8',
  });
  // ⚠ 适配器的日志走 **stderr**（stdout 留给插件自己的业务输出），别取错
  const out = r1.stderr || '';

  const files = fs.readdirSync(dir);
  assert.equal(files.length, 1, `应当只有一个当天的文件，实际 ${JSON.stringify(files)}`);
  assert.match(files[0], /^plugin-\d{4}-\d{2}-\d{2}\.jsonl$/);

  const rows = fs
    .readFileSync(path.join(dir, files[0]), 'utf8')
    .trim()
    .split('\n')
    .map((l) => JSON.parse(l));
  assert.equal(rows.length, 3);
  assert.equal(rows[0].type, 'start');
  assert.equal(rows[0].level, 'info', 'start 的过滤级别是 info');
  assert.equal(rows[0].scope, 'demo');
  assert.equal(rows[0].plugin, 'demo');
  assert.equal(rows[1].msg, '账号 136****6001 失败', '落盘的必须是脱敏后的');
  assert.equal(rows[2].msg, '多参 {"a":1} 3', '多参按空格拼接，容器走 JSON');
  assert.ok(out.includes('"written":3'), `state 里应当记到写了 3 行: ${out}`);
});

test('HOST_LOG_DIR=off 时不落盘，但终端照常', () => {
  const script = `
    import { createLogger, logState } from ${JSON.stringify(path.resolve(HERE, '..', 'src', 'log.js'))};
    createLogger({ scope: 'x' }).info('只去终端');
    process.stderr.write('STATE ' + JSON.stringify(logState()) + '\\n');
  `;
  const r2 = spawnSync(process.execPath, ['--input-type=module', '-e', script], {
    env: { ...process.env, HOST_LOG_DIR: 'off', HOST_LOG_COLOR: '0' },
    encoding: 'utf8',
  });
  const out = r2.stderr || '';
  assert.ok(out.includes('只去终端'), '终端还是要打的');
  assert.ok(out.includes('"dir":""') || out.includes('"dir": ""'), `dir 应当是空: ${out}`);
});

test('HOST_LOG_LEVEL 过滤：warn 以上才出来', () => {
  const script = `
    import { createLogger } from ${JSON.stringify(path.resolve(HERE, '..', 'src', 'log.js'))};
    const lg = createLogger({ scope: 'x' });
    lg.debug('看不见');
    lg.info('也看不见');
    lg.warn('看得见');
  `;
  const r3 = spawnSync(process.execPath, ['--input-type=module', '-e', script], {
    env: { ...process.env, HOST_LOG_DIR: 'off', HOST_LOG_COLOR: '0', HOST_LOG_LEVEL: 'warn' },
    encoding: 'utf8',
  });
  const out = r3.stderr || '';
  assert.ok(!out.includes('看不见'));
  assert.ok(out.includes('[warn] [x] 看得见'), out);
});

test('彩色行与共享向量逐字符一致（八档）', () => {
  const { line_colored_template: tpl, colors } = readVectors('log-format-cases.json');
  const script = `
    import { createLogger, TYPES } from ${JSON.stringify(path.resolve(HERE, '..', 'src', 'log.js'))};
    const lg = createLogger({ scope: 'demo' });
    for (const t of TYPES) lg[t]('消息');
  `;
  const r = spawnSync(process.execPath, ['--input-type=module', '-e', script], {
    env: { ...process.env, HOST_LOG_DIR: 'off', HOST_LOG_COLOR: '1' },
    encoding: 'utf8',
  });
  const lines = (r.stderr || '').trimEnd().split('\n');
  assert.equal(lines.length, TYPES.length, `八档应当八行: ${r.stderr}`);
  TYPES.forEach((type, i) => {
    const want = tpl
      .replace('{color}', colors[type])
      .replace('{type}', type)
      .replace('{reset}', colors.reset)
      .replace('{local}', '<TIME>')
      .replace('{scope}', 'demo')
      .replace('{msg}', '消息');
    const got = lines[i].replace(/\d{4}-\d{2}-\d{2} \d{2}:\d{2}:\d{2}\.\d{3}/, '<TIME>');
    assert.equal(got, want, `type=${type}`);
  });
});

test('颜色只在 tty 上开，且只给 [type] 上色（与宿主一致）', () => {
  const script = `
    import { createLogger } from ${JSON.stringify(path.resolve(HERE, '..', 'src', 'log.js'))};
    createLogger({ scope: 'x' }).warn('彩色');
  `;
  const r4 = spawnSync(process.execPath, ['--input-type=module', '-e', script], {
    env: { ...process.env, HOST_LOG_DIR: 'off', HOST_LOG_COLOR: '1' },
    encoding: 'utf8',
  });
  const out = r4.stderr || '';
  // 只有 [warn] 这一段上色，时间戳与 scope 不带转义序列
  assert.match(out, /\x1b\[33m\[warn\]\x1b\[0m \d{4}-\d{2}-\d{2} \d{2}:\d{2}:\d{2}\.\d{3} \[x\] 彩色/);
  assert.equal((out.match(/\x1b\[/g) || []).length, 2, '只该有两处转义（上色 + 复位）');
});

test('HOST_LOG_LEVEL 认 warning 这个别名（与 Rust 的 Level::parse 一致）', () => {
  const script = `
    import { createLogger, logState } from ${JSON.stringify(path.resolve(HERE, '..', 'src', 'log.js'))};
    createLogger({ scope: 'x' }).debug('看不见');
    createLogger({ scope: 'x' }).warn('看得见');
    process.stderr.write('MIN ' + logState().min + '\\n');
  `;
  const r = spawnSync(process.execPath, ['--input-type=module', '-e', script], {
    env: { ...process.env, HOST_LOG_DIR: 'off', HOST_LOG_COLOR: '0', HOST_LOG_LEVEL: 'warning' },
    encoding: 'utf8',
  });
  const out = r.stderr || '';
  assert.ok(out.includes('MIN warn'), `warning 应当解析成 warn: ${out}`);
  assert.ok(!out.includes('看不见') && out.includes('看得见'), out);
});

test('HOST_LOG_KEEP_DAYS 负数不会把当天的文件删掉', () => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'host-api-keep-'));
  const script = `
    import { createLogger } from ${JSON.stringify(path.resolve(HERE, '..', 'src', 'log.js'))};
    createLogger({ scope: 'x' }).info('应当留下');
  `;
  spawnSync(process.execPath, ['--input-type=module', '-e', script], {
    env: { ...process.env, HOST_LOG_DIR: dir, HOST_LOG_COLOR: '0', HOST_LOG_KEEP_DAYS: '-3' },
    encoding: 'utf8',
  });
  const files = fs.readdirSync(dir);
  assert.equal(files.length, 1, `当天的文件不该被删: ${JSON.stringify(files)}`);
  assert.ok(fs.readFileSync(path.join(dir, files[0]), 'utf8').includes('应当留下'));
});
