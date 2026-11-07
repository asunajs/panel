// panel version helpers for the **external** Node/Bun runtime (0.3.0).
//
// The gate itself lives in Rust (src/panelversion.rs) and runs before this
// process is spawned: a plugin that needs a newer panel never gets here. This
// module only provides the runtime query plugins use for capability
// degradation (panel.version() / panel.atLeast('0.3.0')).
//
// The comparison mirrors the Rust one on purpose: a leading 'v' is ignored,
// missing parts are 0 ('0.3' === '0.3.0'), and a prerelease sorts before its
// release ('0.3.0-rc.1' < '0.3.0'). Keep the two in sync -- if they disagree,
// a plugin would take a different branch depending on the engine.

function isNumeric(s) {
  if (s === '') return false;
  for (const ch of s) {
    if (ch < '0' || ch > '9') return false;
  }
  return true;
}

export function parseVersion(raw) {
  let s = String(raw).trim();
  if (s.startsWith('v')) s = s.slice(1);
  let core = s;
  let pre = '';
  const plus = core.indexOf('+');
  if (plus >= 0) core = core.slice(0, plus);
  const dash = core.indexOf('-');
  if (dash >= 0) {
    pre = core.slice(dash + 1);
    core = core.slice(0, dash);
  }
  const parts = core.split('.');
  if (parts.length === 0 || parts.length > 3) return null;
  const nums = [];
  for (const p of parts) {
    if (!isNumeric(p)) return null;
    nums.push(Number(p));
  }
  while (nums.length < 3) nums.push(0);
  return { nums, pre: pre === '' ? [] : pre.split('.') };
}

function comparePre(a, b) {
  // No prerelease sorts AFTER any prerelease (1.0.0 > 1.0.0-rc.1).
  if (a.length === 0 && b.length === 0) return 0;
  if (a.length === 0) return 1;
  if (b.length === 0) return -1;
  const n = Math.max(a.length, b.length);
  for (let i = 0; i < n; i++) {
    if (i >= a.length) return -1;
    if (i >= b.length) return 1;
    const x = a[i];
    const y = b[i];
    const xn = isNumeric(x);
    const yn = isNumeric(y);
    if (xn && yn) {
      const d = Number(x) - Number(y);
      if (d !== 0) return d < 0 ? -1 : 1;
    } else if (xn !== yn) {
      // Numeric identifiers sort before alphanumeric ones.
      return xn ? -1 : 1;
    } else if (x !== y) {
      return x < y ? -1 : 1;
    }
  }
  return 0;
}

export function compareVersions(a, b) {
  for (let i = 0; i < 3; i++) {
    if (a.nums[i] !== b.nums[i]) return a.nums[i] < b.nums[i] ? -1 : 1;
  }
  return comparePre(a.pre, b.pre);
}

export const panel = {
  version() {
    const v = typeof process !== 'undefined' && process.env ? process.env.PANEL_VERSION : '';
    return v || '0.0.0';
  },
  atLeast(want) {
    const cur = parseVersion(panel.version());
    const target = parseVersion(want);
    if (!cur) return false;
    if (!target) {
      throw new Error('"' + want + '" 不是合法版本号（要 semver，例如 "0.3.0"）');
    }
    return compareVersions(cur, target) >= 0;
  },
};
