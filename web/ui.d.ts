/**
 * 面板提供给插件页的 JS 面（真源：同目录 `ui.js`）。
 *
 * 为什么放在面板仓：这是**面板的契约**，不是某个插件的私货。插件用
 * `scripts/sync-types.mjs` 把它拷进自己仓的 `types/` 再跑 `tsc --checkJs`，
 * 这样「面板加了新成员、插件没用上/写错名」能在类型检查阶段暴露。
 *
 * 维护约定：改 `ui.js` 的 `window.PanelUI` 成员时，同步改这里；两边不一致由插件的
 * `sync-types --check` 抓出来（它比的是文件内容，不是语义）。
 */

/** 插件页里 `$('id')` 拿到的元素：面板控件把常用成员直接挂在自身上。 */
interface PanelEl extends HTMLElement {
  /** `<p-sms-code>` / `<input>`：当前值 */
  value: string;
  /** `<p-button>`：按钮文字（写它不要碰内部 `_btn`） */
  label: string;
  /** `<p-button>` / `<p-sms-code>`：禁用态 */
  disabled: boolean;
  /** `<input type="checkbox">` / `<p-switch>`：勾选态 */
  checked: boolean;
  /** `<p-sms-code>`：发码在途 */
  busy: boolean;
  /** `<p-sms-code>`：起倒计时 */
  startResend(seconds?: number): void;
  /** `<p-sms-code>`：关弹窗时停掉倒计时 */
  stopResend(): void;
  /** `<select>` / `<p-select>`：选中项 */
  selectedOptions: HTMLCollectionOf<HTMLOptionElement>;
  /** `<button>` / `<input>`：type 属性 */
  type: string;
  /** `<input type="number">`：上下限（属性值，字符串） */
  min: string;
  max: string;
}

type PanelHintKind = 'error' | 'err' | 'warn' | 'warning' | '' | null;

declare const PanelUI: {
  /** 'light' | 'dark' */
  theme(): string;
  setTheme(theme: 'light' | 'dark'): void;
  /** 不要用版本号做特性判断；要看某个成员在不在。 */
  readonly version: string;

  /** 确认框：取消回 false。传字符串等价于 `{ text }`。 */
  confirm(options?: string | { text: string; title?: string; okLabel?: string; danger?: boolean }): Promise<boolean>;
  /** 输入框：取消回 null（空串是有效答案）。与 confirm 共用 `text / okLabel / cancelLabel`。 */
  prompt(options: {
    title?: string;
    label?: string;
    value?: string;
    placeholder?: string;
    text?: string;
    okLabel?: string;
    cancelLabel?: string;
  }): Promise<string | null>;

  /** 留头尾、中间打码；空串/短串原样返回。 */
  mask(value: unknown, head?: number, tail?: number): string;
  /** HTML 转义（拼 innerHTML 时用）。 */
  esc(value: unknown): string;
  /** 时分秒：`09:05:07`。 */
  fmtTime(ms?: number): string;
  /** 月-日 + 时分秒：`11-21 09:05:07`。 */
  fmtLogTime(ms?: number): string;
  /** 提示行：写文案 + 上色；传空串清空并去色。 */
  hint(el: Element | string | null, text: unknown, kind?: PanelHintKind): void;
  /** 忙碌态：`on=true` 禁用并换文案，`on=false` 还原原文案。 */
  busy(btn: Element | null, on: boolean, label?: string): void;

  /**
   * 调自己后端的统一入口。语义定死：2xx → 解析后的 JSON；非 2xx → 抛 `PanelUI.ApiError`
   * （message 取后端 `{error}`，附 `status` / `data`）。body 传对象，别传字符串（会 415）。
   */
  api<T = any>(
    path: string,
    opts?: {
      method?: string;
      body?: unknown;
      /** 写操作要带 `x-panel: 1` —— 传 true 即可 */
      write?: boolean;
      headers?: Record<string, string>;
      timeoutMs?: number;
    },
  ): Promise<T>;

  /** `api()` 的失败形状。 */
  ApiError: new (message: string, status?: number, data?: unknown) => Error & { status: number; data: unknown };
};

interface Window {
  PanelUI: typeof PanelUI;
}
