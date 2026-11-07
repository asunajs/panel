import test from 'node:test';
import assert from 'node:assert/strict';

import { compareVersions, panel, parseVersion } from '../panel.mjs';

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
