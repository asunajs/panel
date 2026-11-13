#!/usr/bin/env node
/// 把 mcloud 命令行那套数据搬进面板插件（第 147 轮）。
///
/// 为什么需要：mcloud 的账号/订阅原先存在 `~/mobile/workspace/mcloud/data/*.json`（Bun 那套 CLI），
/// 而面板插件把它们放在面板库的 `state` 表里（键 `mcloud.accounts` + 每号一个 JSON 袋）。
/// 两边字段名不一样，直接拷文件没用 —— 这个脚本做的是**逐字段对拍**。
///
/// 默认**只打印计划不落库**；加 `--apply` 才写。写之前请自己备份面板库。
///
/// 用法：
///   node scripts/migrate-mcloud-data.mjs --db ~/panel-3030/data/panel.db            # 看一眼
///   node scripts/migrate-mcloud-data.mjs --db ~/panel-3030/data/panel.db --apply    # 真写
///
/// 迁什么 / 不迁什么：
///   迁：账号表（凭据、备注、用户域、停用状态）、每个号的「关注奖品」（订阅）
///   不迁：口令池（`kouling-pool.json`）—— 那是「本轮」的临时池子，5 小时就过期，
///         搬过去也立刻被 isStale() 重置；历史（`history.json`）与流水线（`pipelines.json`）
///         是 CLI 独有的概念，插件里没有对应结构；推送规则（CLI 自己发推送）同理。
import { DatabaseSync } from 'node:sqlite';
import { readFileSync, existsSync, statSync } from 'node:fs';
import { homedir } from 'node:os';
import path from 'node:path';

const args = process.argv.slice(2);
const getArg = (name, dflt) => {
  const i = args.indexOf('--' + name);
  return i >= 0 && args[i + 1] ? args[i + 1] : dflt;
};
const from = getArg('from', path.join(homedir(), 'mobile/workspace/mcloud/data'));
const dbPath = getArg('db', '');
const apply = args.includes('--apply');
if (!dbPath) {
  console.error('用法：node scripts/migrate-mcloud-data.mjs --db <panel.db> [--apply] [--from <mcloud/data>]');
  process.exit(2);
}
if (!existsSync(dbPath)) {
  console.error('找不到面板库：' + dbPath);
  process.exit(2);
}

const readJson = (name, dflt) => {
  const p = path.join(from, name);
  if (!existsSync(p)) {
    console.log('（没有 ' + name + '，跳过）');
    return dflt;
  }
  return JSON.parse(readFileSync(p, 'utf8'));
};

const cliAccounts = readJson('accounts.json', []);
const cliSubs = readJson('exchange-subscriptions.json', {});
if (!Array.isArray(cliAccounts) || cliAccounts.length === 0) {
  console.error('accounts.json 里没有账号，什么都不用迁');
  process.exit(1);
}

// CLI 的账号 → 插件的账号记录（字段名对不上，逐个搬）：
//   id           插件用「手机号」当键（`phoneOf(auth)`）；CLI 里另有一个 uuid 当 id
//   auth         插件要的是**一整串** `Basic base64(pc:手机号:token…)`；CLI 存在 auth.raw
//   label        CLI 叫 nickname
//   userDomainId CLI 叫 user_domain_id
//   disabled     等于 !enabled
//   platform     插件只认 'pc' / 'mobile'
const now = new Date().toISOString();
const accounts = [];
const ownerToPhone = new Map();
for (const a of cliAccounts) {
  const auth = a && a.auth && typeof a.auth.raw === 'string' ? a.auth.raw : '';
  const phone = a && a.auth && typeof a.auth.phone === 'string' ? a.auth.phone : '';
  if (!auth) {
    console.log('· 跳过一条没有凭据的记录：' + (a && a.id));
    continue;
  }
  const id = phone || String(a.id);
  const platform = a.auth.platform === 'pc' ? 'pc' : 'mobile';
  const rec = {
    id,
    label: String(a.nickname || phone || a.id || id),
    auth,
    platform,
    userDomainId: a.user_domain_id || undefined,
    disabled: a.enabled === false,
    addedAt: now,
  };
  if (platform === 'pc') rec.pcPlatform = 'win';
  accounts.push(rec);
  if (a.owner) ownerToPhone.set(String(a.owner), id);
}

// 每个号的「关注奖品」：CLI 的 {owner: [{prize_id,name,…}]} → 插件袋里的 [{id,name}]
const subsByPhone = new Map();
for (const [owner, list] of Object.entries(cliSubs || {})) {
  const phone = ownerToPhone.get(owner);
  if (!phone || !Array.isArray(list)) continue;
  const out = [];
  for (const s of list) {
    if (!s || typeof s.name !== 'string' || s.name.trim() === '') continue;
    const id = typeof s.prize_id === 'number' ? Math.trunc(s.prize_id) : undefined;
    out.push(id === undefined ? { name: s.name.trim() } : { id, name: s.name.trim() });
  }
  if (out.length) subsByPhone.set(phone, out);
}

console.log('来源：' + from);
console.log('目标：' + dbPath + '（' + (statSync(dbPath).size / 1024).toFixed(0) + ' KB）');
console.log('账号 ' + accounts.length + ' 个：');
for (const a of accounts) {
  console.log('  · ' + a.label + '（id=' + a.id + '，' + a.platform + (a.disabled ? '，已停用' : '') + '）');
}
if (subsByPhone.size) {
  const n = [...subsByPhone.values()].reduce((s, v) => s + v.length, 0);
  console.log('关注奖品 ' + n + ' 条，分布在 ' + subsByPhone.size + ' 个账号上');
}

const db = new DatabaseSync(dbPath);
// 表可能还不存在（新库、插件还没跑过）：建一份和插件里一样的 schema
db.exec(`CREATE TABLE IF NOT EXISTS state (
           key   TEXT PRIMARY KEY,
           value TEXT NOT NULL,
           updated_at TEXT NOT NULL
         )`);
const existing = db.prepare('SELECT value FROM state WHERE key = ?1');
const upsert = db.prepare(`INSERT INTO state (key, value, updated_at) VALUES (?1, ?2, ?3)
  ON CONFLICT(key) DO UPDATE SET value = ?2, updated_at = ?3`);

const oldRaw = existing.get('mcloud.accounts');
let oldList = [];
try {
  oldList = oldRaw ? JSON.parse(oldRaw.value) : [];
} catch {
  oldList = [];
}
if (Array.isArray(oldList) && oldList.length) {
  console.log('⚠ 目标库里已经有 ' + oldList.length + ' 个账号：同名（同 id）的会被覆盖，其余保留');
}
const merged = Array.isArray(oldList) ? oldList.slice() : [];
for (const a of accounts) {
  const i = merged.findIndex((x) => x && x.id === a.id);
  if (i >= 0) merged[i] = a;
  else merged.push(a);
}

if (!apply) {
  console.log('');
  console.log('（只打印计划。要真写就加 --apply；写之前先备份面板库）');
  db.close();
  process.exit(0);
}

upsert.run('mcloud.accounts', JSON.stringify(merged), now);
for (const [phone, subs] of subsByPhone) {
  const bagKey = 'mcloud.acct.' + phone;
  const cur = existing.get(bagKey);
  let bag = {};
  try {
    bag = cur ? JSON.parse(cur.value) : {};
  } catch {
    bag = {};
  }
  if (typeof bag !== 'object' || bag === null || Array.isArray(bag)) bag = {};
  bag['mcloud.exchange.subs'] = JSON.stringify(subs);
  upsert.run(bagKey, JSON.stringify(bag), now);
}
db.close();
console.log('');
console.log('写完：账号 ' + merged.length + ' 个，袋子 ' + subsByPhone.size + ' 个');
console.log('接下来：重启面板（launchctl kickstart -k gui/$(id -u)/dev.panel），插件重载时会读到这些键');
