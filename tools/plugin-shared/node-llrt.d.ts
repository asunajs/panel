/**
 * LLRT 里**存在**的 Node 内置模块的最小子集（给插件服务端的 `tsc --checkJs` 用）。
 *
 * 真源：面板仓 `docs/engine.md` 的模块表（LLRT 只实现了 Node 的一部分）。
 *
 * ⚠ 这份文件是**契约**，不是补丁：只声明 LLRT 真的有的模块与成员。
 *   `node:http` / `node:tls` / `node:sqlite` / `node:worker_threads` **故意不声明** ——
 *   插件里一旦 import 它们，类型检查当场报错，而不是等运行时 `not a function`
 *   （2026-11-20 cmcc 用了不存在的 `fs.existsSync`，整页 502、日志里连栈都没有）。
 *
 * 另注：CLI（跑在真 Node 上）不要用这份 —— 它该用 @types/node 或各仓自己的 CLI shim。
 */

declare module 'node:fs' {
  export function readFileSync(p: string): any;
  export function readFileSync(p: string, enc: 'utf8' | 'utf-8'): string;
  export function writeFileSync(p: string, data: string | Uint8Array, opts?: { mode?: number }): void;
  export function renameSync(a: string, b: string): void;
  export function chmodSync(p: string, mode: number): void;
  export function mkdirSync(p: string, opts?: { recursive?: boolean }): void;
  export function mkdtempSync(prefix: string): string;
  export function readdirSync(p: string, opts?: any): any[];
  export function rmSync(p: string, opts?: { recursive?: boolean; force?: boolean }): void;
  export function rmdirSync(p: string, opts?: { recursive?: boolean }): void;
  export function statSync(p: string): { isDirectory(): boolean; isFile(): boolean; mtimeMs?: number; size?: number; mode?: number };
  export function lstatSync(p: string): { isDirectory(): boolean; isFile(): boolean; mtimeMs?: number; size?: number; mode?: number };
  export function accessSync(p: string, mode?: number): void;
  export function symlinkSync(target: string, path: string): void;
  export const constants: Record<string, number>;
  declare const fs: {
    readFileSync: typeof readFileSync;
    writeFileSync: typeof writeFileSync;
    renameSync: typeof renameSync;
    chmodSync: typeof chmodSync;
    mkdirSync: typeof mkdirSync;
    mkdtempSync: typeof mkdtempSync;
    readdirSync: typeof readdirSync;
    rmSync: typeof rmSync;
    rmdirSync: typeof rmdirSync;
    statSync: typeof statSync;
    lstatSync: typeof lstatSync;
    accessSync: typeof accessSync;
    symlinkSync: typeof symlinkSync;
    constants: typeof constants;
  };
  export default fs;
}

declare module 'node:fs/promises' {
  export function readFile(p: string, enc?: 'utf8' | 'utf-8'): Promise<any>;
  export function writeFile(p: string, data: string | Uint8Array, opts?: { mode?: number }): Promise<void>;
  export function mkdir(p: string, opts?: { recursive?: boolean }): Promise<any>;
  export function rm(p: string, opts?: { recursive?: boolean; force?: boolean }): Promise<void>;
  export function readdir(p: string, opts?: any): Promise<any[]>;
  export function stat(p: string): Promise<{ isDirectory(): boolean; isFile(): boolean; size?: number }>;
  export function rename(a: string, b: string): Promise<void>;
  export function chmod(p: string, mode: number): Promise<void>;
  const fsp: Record<string, unknown>;
  export default fsp;
}

declare module 'node:path' {
  export function join(...p: string[]): string;
  export function dirname(p: string): string;
  export function basename(p: string, ext?: string): string;
  export function extname(p: string): string;
  export function resolve(...p: string[]): string;
  export function isAbsolute(p: string): boolean;
  export function normalize(p: string): string;
  export function relative(from: string, to: string): string;
  export function parse(p: string): { root: string; dir: string; base: string; ext: string; name: string };
  export function format(o: { root?: string; dir?: string; base?: string; ext?: string; name?: string }): string;
  export const sep: string;
  declare const path: {
    join: typeof join;
    dirname: typeof dirname;
    basename: typeof basename;
    extname: typeof extname;
    resolve: typeof resolve;
    isAbsolute: typeof isAbsolute;
    normalize: typeof normalize;
    relative: typeof relative;
    parse: typeof parse;
    format: typeof format;
    sep: typeof sep;
  };
  export default path;
}

declare module 'node:url' {
  export function fileURLToPath(u: string | URL): string;
  export function pathToFileURL(p: string): URL;
}

declare module 'node:crypto' {
  export function createHash(alg: string): any;
  export function createHmac(alg: string, key: any): any;
  export function createCipheriv(alg: string, key: any, iv: any): any;
  export function createDecipheriv(alg: string, key: any, iv: any): any;
  export function randomBytes(n: number): any;
  export function randomUUID(): string;
  export function pbkdf2Sync(...a: any[]): any;
  export const webcrypto: any;
  const c: Record<string, unknown>;
  export default c;
}

declare module 'node:zlib' {
  export function gzipSync(b: Uint8Array, o?: { mtime?: number }): Uint8Array;
  export function gunzipSync(b: Uint8Array): Uint8Array;
  export function inflateSync(b: Uint8Array): Uint8Array;
  export function inflateRawSync(b: Uint8Array): Uint8Array;
  export function deflateSync(b: Uint8Array): Uint8Array;
  export function brotliDecompressSync(b: Uint8Array): Uint8Array;
}

declare module 'node:os' {
  export function homedir(): string;
  export function tmpdir(): string;
  export function platform(): string;
  export function EOL(): string;
}

declare module 'node:buffer' {
  export const Buffer: any;
  export type Buffer = any;
}

declare module 'node:util' {
  export function format(...a: unknown[]): string;
  export function inspect(o: unknown, opts?: unknown): string;
  export function promisify(f: any): any;
}

declare module 'node:events' {
  export class EventEmitter {
    on(ev: string, fn: (...a: any[]) => void): this;
    once(ev: string, fn: (...a: any[]) => void): this;
    emit(ev: string, ...a: any[]): boolean;
    off(ev: string, fn: (...a: any[]) => void): this;
  }
}

declare module 'node:stream' {
  export class Readable {
    static from(x: any): Readable;
    on(ev: string, fn: (...a: any[]) => void): this;
    pipe(dest: any): any;
  }
  export class Writable {
    write(chunk: any): boolean;
    end(): void;
    on(ev: string, fn: (...a: any[]) => void): this;
  }
  const s: Record<string, unknown>;
  export default s;
}

// ── 定时器（LLRT 有 timers；插件里用 sleep/间隔都走它）──────────
declare function setTimeout(fn: (...args: any[]) => void, ms?: number, ...args: any[]): any;
declare function clearTimeout(handle: any): void;
declare function setInterval(fn: (...args: any[]) => void, ms?: number, ...args: any[]): any;
declare function clearInterval(handle: any): void;

// ── 全局 ───────────────────────────────────────────────────────
declare var process: {
  env: Record<string, string | undefined>;
  argv: string[];
  exit(code?: number): never;
  cwd(): string;
  platform: string;
  version: string;
};
declare var console: { log(...a: unknown[]): void; error(...a: unknown[]): void };
declare var Buffer: any;
declare type Buffer = any;
declare var TextDecoder: any;
declare var TextEncoder: any;
declare var URL: any;
declare var atob: (s: string) => string;
declare var btoa: (s: string) => string;
