/**
 * 面板宿主 API（`panel:host`）的类型契约。
 *
 * 真源：面板仓 `src/hostapi.rs`（函数注册表）+ `docs/host-api.md`（语义与限额）。
 * 用法：插件仓用 `scripts/sync-types.mjs` 拷到 `types/panel-host.d.ts`，再由
 * `tsconfig.node.base.json` 的 `paths` 把 `panel:host` 指过来 —— 于是插件服务端那份 JS
 * 也能跑 `tsc --checkJs`。
 *
 * 口径：这里只声明**能确认的**签名。少数平台专用 op（ws_* / smtp_send / set_job_*）
 * 面板侧参数还在演进，先给 `any` + 注释，别假装精确 —— 宁可少检查，不可给假契约。
 */

declare module 'panel:host' {
  /** `host.fetch` / `http_fetch` 的返回：**body 已是字符串**（不是 Response，没有 .text()）。 */
  export interface HostResponse {
    status: number;
    /** 响应体（文本）。二进制请自行处理编码。 */
    body: string;
    headers?: Record<string, string>;
    /** 上游是否 2xx（部分实现有，判 2xx 请用 status） */
    ok?: boolean;
    /** 失败时宿主可能直接给 error 字符串（而不是抛异常） */
    error?: string;
  }

  export interface FetchOpts {
    method?: string;
    headers?: Record<string, string>;
    /** 单发请求的时间预算（毫秒） */
    timeout_ms?: number;
    body?: string;
  }

  /** `sqlite_query*` 的返回形状（与面板的 query 封装一致） */
  export interface SqliteRows<T = any> {
    rows: T[];
  }

  /** `sqlite_exec*` 的返回形状 */
  export interface SqliteExecResult {
    changes?: number;
    last_insert_rowid?: number;
  }

  export interface Host {
    // ── 路径 / 配置 ─────────────────────────────────────────────
    /** 本插件的数据目录（`plugin-data/<name>/`），插件只该写这里 */
    plugin_data_dir(): Promise<string>;
    /** 本插件代码目录（可能为 null：单文件插件没有目录） */
    plugin_dir(): Promise<string | null>;
    /** 面板给本插件的那份配置（`plugin-data/<name>/config.json` 解析结果） */
    self_config(): Promise<Record<string, unknown> | null>;
    /** 面板级配置（只读；插件不该写它） */
    config(): Promise<Record<string, unknown> | null>;

    // ── 网络 ───────────────────────────────────────────────────
    /** 异步出站请求（有 SSRF 门与预算） */
    fetch(url: string, opts?: FetchOpts): Promise<HostResponse>;
    /** 同步出站请求（同一份实现；在同步上下文里用） */
    http_fetch(url: string, opts?: FetchOpts): HostResponse;

    // ── 存储 ───────────────────────────────────────────────────
    sqlite_query<T = any>(sql: string, params?: unknown[]): Promise<SqliteRows<T>>;
    sqlite_query_sync<T = any>(sql: string, params?: unknown[]): SqliteRows<T>;
    sqlite_exec(sql: string, params?: unknown[]): Promise<SqliteExecResult>;
    sqlite_exec_sync(sql: string, params?: unknown[]): SqliteExecResult;

    // ── 调度 / 任务 ────────────────────────────────────────────
    /** 注册定时任务（**建议**，采纳与删除是用户的事）：`(名称, cron 串, 预算/opts, 参数名?)` */
    register_cron(
      name: string,
      spec: string,
      budgetMsOrOpts?: number | Record<string, unknown>,
      params?: string[] | Record<string, unknown>,
    ): void;
    /** 注册手动任务（任务页上的按钮） */
    register_handler(name: string, fn: (evt?: any) => unknown): void;
    /** 注册插件页面路由（`/plugins/<name>/<rel>`） */
    register_web(fn: (evt: any) => unknown): void;
    /** 侧栏入口 */
    sidebar(opts: { title: string; order?: number }): Promise<void>;
    /** 请面板立刻跑另一个已注册的一次性任务 */
    run_once(name: string, opts?: { params?: unknown; budget_ms?: number }): Promise<{ queued?: boolean; position?: number } | null>;
    sleep(ms: number): Promise<void>;
    /** 任务状态微调（面板侧还在演进，先宽松） */
    set_job_paused(paused: boolean): Promise<void>;
    set_job_summary(text: string): Promise<void>;
    set_job_blockers(list: unknown): Promise<void>;
    set_job_schedule(spec: Record<string, unknown>): Promise<void>;

    // ── 通知 / 日志 ────────────────────────────────────────────
    /** 面板铃铛通知（标题, 正文） */
    notify(title: string, body?: string): Promise<void>;
    /** 结构化日志（面板日志页可见） */
    log(level: 'info' | 'warn' | 'error' | 'debug', msg: string, extra?: unknown): Promise<void> | void;
    info(msg: string, extra?: unknown): Promise<void> | void;
    warn(msg: string, extra?: unknown): Promise<void> | void;
    error(msg: string, extra?: unknown): Promise<void> | void;

    // ── 平台能力 ───────────────────────────────────────────────
    panel_version(): string;
    panel_at_least(version: string): boolean;
    /** 宿主有没有原生 TLS（直播链那条要它） */
    native_tls?: boolean;

    // ── 尚未定型的 op：先 any，别给假契约 ──────────────────────
    /** WebSocket（参数见 docs/host-api.md §出站三件） */
    ws_open(url: string, opts?: any): any;
    ws_send(handle: any, data: any): any;
    ws_recv(handle: any, timeout_ms?: number): any;
    ws_close(handle: any): any;
    /** 发信（SMTP） */
    smtp_send(opts: any): any;
  }

  export const host: Host;
  export default host;
}

/** 面板注入的全局 `panel` 对象（不是 panel:host 的导出）：只有版本等元信息。
 *  插件代码里用 `typeof panel !== 'undefined'` 判存在。 */
declare const panel: {
  version(): string;
  /** 面板注入的其他常量（按需补，别在这瞎猜） */
  [key: string]: unknown;
};

/** 面板注入的全局日志器（插件代码里直接 `log.info(...)`）。 */
declare const log: {
  info(msg: string, extra?: unknown): void;
  warn(msg: string, extra?: unknown): void;
  error(msg: string, extra?: unknown): void;
  debug(msg: string, extra?: unknown): void;
};
