#!/usr/bin/env node
/// 把**老面板**里 mcloud 的 state 搬进新面板（第 148 轮）。
///
/// 为什么要单独一个脚本：老项目（`mcloud-plugin` / `mcloud-sign`）跑在旧面板里，插件状态存在那个库的
/// `state` 表（键 `mcloud.*`，每个号还有一个 JSON 袋）—— 而新面板是另一个库。两边 schema 一样，
/// 直接把键搬过去就行；但**不能整库拷**（新库还有新面板自己的 runs/logs/tasks）。
///
/// 用法：
///   node scripts/migrate-mcloud-state.mjs --from <老panel.db> --db ~/panel-3030/data/panel.db
///   … 加 --apply 才真写；再加 --prune-orphan-bags 顺手删掉不在账号表里的空袋子
///
/// 搬什么：`state` 里所有 `mcloud.*` 键（含账号表、每号的状态袋、口令池、日志环、计划/任务账号表）
///         `mcloud_runs` 表的行（运行历史，id 重新分配避免撞）
/// 不搬：`plugin.loaded_at` 这类面板自己的键；别的插件的键一律不碰。
import { DatabaseSync } from 'node:sqlite';
import { existsSync, statSync } from 'node:fs';

const args = process.argv.slice(2);
const getArg = (n) => {
  const i = args.indexOf('--' + n);
  return i >= 0 && args[i + 1] ? args[i + 1] : '';
};
const fromPath = getArg('from');
const dbPath = getArg('db');
const apply = args.includes('--apply');
const prune = args.includes('--prune-orphan-bags');
if (!fromPath || !dbPath || !existsSync(fromPath) || !existsSync(dbPath)) {
  console.error('用法：node scripts/migrate-mcloud-state.mjs --from <老panel.db> --db <新panel.db> [--apply]');
  process.exit(2);
}

const src = new DatabaseSync(fromPath, { readOnly: true });
const dst = new DatabaseSync(dbPath);
dst.exec('CREATE TABLE IF NOT EXISTS state (key TEXT PRIMARY KEY, value TEXT NOT NULL, updated_at TEXT NOT NULL)');;
const hasRuns = (db) =>
  !!db.prepare("SELECT name FROM sqlite_master WHERE type='table' AND name='mcloud_runs'").get();

const rows = src
  .prepare("SELECT key, value, updated_at FROM state WHERE key LIKE 'mcloud.%' ORDER BY key")
  .all();
if (!rows.length) {
  console.error('来源库里没有 mcloud.* 的键 —— 是不是选错库了？');
  process.exit(1);
}

// 账号表里有哪些号（决定哪些袋子是孤儿）
const accRow = rows.find((r) => r.key === 'mcloud.accounts');
let accIds = [];
try {
  const list = accRow ? JSON.parse(accRow.value) : [];
  if (Array.isArray(list)) accIds = list.map((a) => a && a.id).filter((x) => typeof x === 'string');
} catch { /* 账号表坏了就当没有 */ }

const bags = rows.filter((r) => r.key.startsWith('mcloud.acct.')).map((r) => r.key.slice('mcloud.acct.'.length));
const orphans = bags.filter((id) => !accIds.includes(id) && id !== 'default');

const runsSrc = hasRuns(src) ? src.prepare('SELECT count(*) AS n FROM mcloud_runs').get().n : 0;
const runsDst = hasRuns(dst) ? dst.prepare('SELECT count(*) AS n FROM mcloud_runs').get().n : 0;

console.log('来源：' + fromPath + '（' + (statSync(fromPath).size / 1024).toFixed(0) + ' KB）');
console.log('目标：' + dbPath + '（' + (statSync(dbPath).size / 1024).toFixed(0) + ' KB）');
console.log('state 键：' + rows.length + ' 个 mcloud.* 键');
for (const r of rows) console.log('  · ' + r.key.padEnd(38) + String(r.value.length).padStart(7) + ' B');
console.log('账号：' + (accIds.length ? accIds.join(', ') : '（账号表里没有 id）'));
console.log('状态袋：' + bags.length + ' 个' + (orphans.length ? '，其中孤儿 ' + orphans.join(', ') : ''));
console.log('mcloud_runs：来源 ' + runsSrc + ' 行，目标已有 ' + runsDst + ' 行');

if (!apply) {
  console.log('');
  console.log('（只打印计划。加 --apply 才写；写之前先备份新库）');
  process.exit(0);
}

const now = new Date().toISOString();
const upsert = dst.prepare(`INSERT INTO state (key, value, updated_at) VALUES (?1, ?2, ?3)
  ON CONFLICT(key) DO UPDATE SET value = ?2, updated_at = ?3`);
let wrote = 0;
for (const r of rows) {
  upsert.run(r.key, r.value, r.updated_at || now);
  wrote += 1;
}

// 运行历史：整段照搬，id 交给目标库自己分配（来源 id 可能与目标的撞）
let runsCopied = 0;
if (runsSrc && hasRuns(dst)) {
  const srcRows = src.prepare('SELECT task, runs, scheduled_at, fired_at, late_ms, ok, note FROM mcloud_runs').all();
  const ins = dst.prepare(`INSERT INTO mcloud_runs (task, runs, scheduled_at, fired_at, late_ms, ok, note)
    VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)`);
  for (const r of srcRows) {
    ins.run(r.task, r.runs, r.scheduled_at, r.fired_at, r.late_ms, r.ok, r.note ?? null);
    runsCopied += 1;
  }
}

let pruned = 0;
if (prune) {
  // ⚠ 按**目标库**现有的袋子来算：来源里没有、但目标里残留的（比如上一个脚本写进去的）
  // 同样是孤儿，只按来源算会漏掉它们。
  const live = dst
    .prepare("SELECT key FROM state WHERE key LIKE 'mcloud.acct.%'")
    .all()
    .map((r) => r.key.slice('mcloud.acct.'.length));
  const del = dst.prepare('DELETE FROM state WHERE key = ?1');
  for (const id of live) {
    if (id === 'default' || accIds.includes(id)) continue;
    del.run('mcloud.acct.' + id);
    pruned += 1;
  }
}

console.log('');
console.log('写完：state ' + wrote + ' 个键、mcloud_runs ' + runsCopied + ' 行' + (pruned ? '、清掉孤儿袋 ' + pruned + ' 个' : ''));
console.log('接下来重启面板：launchctl kickstart -k gui/$(id -u)/dev.panel');
src.close();
dst.close();
