import test from 'node:test';
import assert from 'node:assert/strict';

import { compareVersions, panel, parseVersion } from '../panel.mjs';
import { installGlobals, sleep } from '../bus.mjs';

// 版本比较必须与 Rust 侧（src/panelversion.rs）同语义：插件用 panel.atLeast()
// 做能力降级时，两个引擎不能给出不同答案。
test('parseVersion pads missing parts and splits prereleases', () => {
  assert.deepEqual(parseVersion('0.3'), { nums: [0, 3, 0], pre: [] });
  assert.deepEqual(parseVersion('v1'), { nums: [1, 0, 0], pre: [] });
  assert.deepEqual(parseVersion('0.3.0-rc.1'), { nums: [0, 3, 0], pre: ['rc', '1'] });
  assert.deepEqual(parseVersion('1.2.3+build.7'), { nums: [1, 2, 3], pre: [] });
  assert.equal(parseVersion('latest'), null);
  assert.equal(parseVersion('1.2.3.4'), null);
  assert.equal(parseVersion(''), null);
});

test('compareVersions follows semver precedence', () => {
  const cmp = (a, b) => compareVersions(parseVersion(a), parseVersion(b));
  assert.equal(cmp('0.3', '0.3.0'), 0);
  assert.equal(cmp('0.2.3', '0.3.0'), -1);
  assert.equal(cmp('10.0.0', '9.9.9'), 1);
  assert.equal(cmp('0.3.0-rc.1', '0.3.0'), -1, 'prerelease sorts first');
  assert.equal(cmp('0.3.0-rc.2', '0.3.0-rc.1'), 1);
  assert.equal(cmp('0.3.0-rc.10', '0.3.0-rc.9'), 1, 'numeric identifiers compare numerically');
  assert.equal(cmp('0.3.0-alpha', '0.3.0-beta'), -1);
  assert.equal(cmp('0.3.0-1', '0.3.0-alpha'), -1, 'numeric sorts before alphanumeric');
});

test('panel.version reads PANEL_VERSION and falls back', () => {
  const saved = process.env.PANEL_VERSION;
  try {
    process.env.PANEL_VERSION = '0.3.0';
    assert.equal(panel.version(), '0.3.0');
    assert.equal(panel.atLeast('0.0.1'), true);
    assert.equal(panel.atLeast('0.3.0'), true);
    assert.equal(panel.atLeast('999.0.0'), false);
    assert.throws(() => panel.atLeast('latest'), /不是合法版本号/);
    delete process.env.PANEL_VERSION;
    assert.equal(panel.version(), '0.0.0');
    assert.equal(panel.atLeast('0.0.1'), false);
  } finally {
    if (saved === undefined) delete process.env.PANEL_VERSION;
    else process.env.PANEL_VERSION = saved;
  }
});
// sleep 是面板扩展（Node 没有）；Node / Bun 两个引擎必须同义，否则同一份插件
// 换个引擎就 ReferenceError。
test('sleep resolves after roughly the requested delay', async () => {
  const t0 = Date.now();
  await sleep(60);
  const dt = Date.now() - t0;
  assert.ok(dt >= 55, `睡了 ${dt}ms，太短`);
  assert.ok(dt < 1000, `睡了 ${dt}ms，太离谱`);
});

test('sleep treats junk and negatives as 0', async () => {
  for (const v of [undefined, null, -5, NaN, 'abc', {}]) {
    const t0 = Date.now();
    await sleep(v);
    assert.ok(Date.now() - t0 < 1000, `sleep(${String(v)}) 卡住了`);
  }
  // 返回值必须是 Promise（不 await 也不能同步抛）
  assert.ok(sleep(0) instanceof Promise);
});

test('sleep is exported from host-api and becomes a global', async () => {
  const mod = await import('../host-api/index.mjs');
  assert.equal(typeof mod.sleep, 'function', 'host-api 具名导出少了 sleep');
  // installGlobals 里那段「裸名补齐」只拷函数 —— sleep 正好是函数，所以它该变成全局。
  installGlobals();
  assert.equal(typeof globalThis.sleep, 'function', '全局 sleep 没装上');
  assert.equal(globalThis.sleep, mod.sleep, '全局与具名导出必须是同一个函数');
});
