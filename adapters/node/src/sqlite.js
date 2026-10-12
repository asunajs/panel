// SQLite 能力 —— 用 Node 22+ 内置的 node:sqlite（DatabaseSync），零第三方依赖。
//
//   sqlite_query(sql, params = []) -> list[dict]                  （SELECT 用）
//   sqlite_exec(sql, params = [])  -> {changes, lastInsertRowid}
//
// 占位符用 `?1 ?2`（按位置数组绑定），和 SQLite 原生语义一致。
// 数据库路径取环境变量 PANEL_DB，默认 `:memory:`。

import { DatabaseSync } from 'node:sqlite';

let db = null;
let openedPath = null;

function open() {
  if (db) return db;
  openedPath = process.env.PANEL_DB || ':memory:';
  db = new DatabaseSync(openedPath);
  db.exec('PRAGMA busy_timeout = 5000');
  return db;
}

/// node:sqlite 只接受 number / bigint / string / null / Uint8Array。
///
/// 注意：node:sqlite 会把 JS number 一律按 **double** 绑定（`typeof(?1)` 是 'real'），
/// 和 Rust 宿主 rusqlite 的 i64 绑定不一致（整数插进 TEXT 列会变成 '1.0' 而不是 '1'）。
/// 所以这里把整数值转成 bigint，浮点保持 number —— 两种运行时的落库结果就一致了。
function bindable(v) {
  if (typeof v === 'boolean') return v ? 1n : 0n; // SQLite 没有布尔类型
  if (v === undefined) return null;
  if (typeof v === 'number') {
    if (!Number.isFinite(v)) return null; // NaN / Infinity
    return Number.isInteger(v) && Math.abs(v) <= Number.MAX_SAFE_INTEGER ? BigInt(v) : v;
  }
  return v;
}

function toPlainRow(row) {
  const out = {};
  for (const key of Object.keys(row)) {
    const v = row[key];
    if (typeof v === 'bigint') {
      // 超出安全整数范围时退化成字符串，避免静默丢精度
      out[key] =
        v >= BigInt(Number.MIN_SAFE_INTEGER) && v <= BigInt(Number.MAX_SAFE_INTEGER) ? Number(v) : v.toString();
    } else {
      out[key] = v;
    }
  }
  return out;
}

function checkArgs(sql, params) {
  if (typeof sql !== 'string' || sql.trim() === '') throw new Error('sql 必须是非空字符串');
  if (params === undefined || params === null) return [];
  if (!Array.isArray(params)) throw new Error('params 必须是数组（按位置绑定 ?1 ?2）');
  return params.map(bindable);
}

export function sqlite_query(sql, params = []) {
  const args = checkArgs(sql, params);
  const stmt = open().prepare(sql);
  return stmt.all(...args).map(toPlainRow);
}

export function sqlite_exec(sql, params = []) {
  const args = checkArgs(sql, params);
  const conn = open();
  // 无参数且是多语句脚本（DDL 建表之类）时走 exec，允许一次跑多条
  if (args.length === 0 && /;\s*\S/.test(sql)) {
    conn.exec(sql);
    return { changes: 0, lastInsertRowid: 0 };
  }
  const info = conn.prepare(sql).run(...args);
  return {
    changes: Number(info.changes ?? 0),
    lastInsertRowid: Number(info.lastInsertRowid ?? 0),
  };
}

/// 内部用：当前数据库路径 / 关库（测试与运行器退出时用）
export function dbPath() {
  return db ? openedPath : null;
}

export function closeDb() {
  if (db) {
    db.close();
    db = null;
    openedPath = null;
  }
}
