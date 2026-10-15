// dialog.js — modal & drawer overlays on the native <dialog> element. Zero-dep,
// no build, CSP-safe (layout/animation via CSSOM inline styles — no injected
// <style>; theme the .dlg-* classes and ::backdrop from your app stylesheet).
//
// Built on showModal() so the browser gives us, for free: top-layer stacking,
// focus trap, Esc-to-close, and (with closedby="any") backdrop light-dismiss. A
// fallback reproduces light-dismiss on browsers without `closedby` (Safari).
//
//   import { dialog, drawer } from "./vendor/puredashboard/dialog.js";
//
//   const d = dialog({
//     title: "Add node",
//     content: (body) => { body.append(myForm); },   // fn | Node | string(text)
//     footer: (foot) => { foot.append(saveBtn); },    // optional, same shape as content
//     onClose: (value) => { if (value === "ok") save(); },
//   }).show();
//   // later: d.close("ok");  →  d.closed resolves to "ok"
//
//   // Actions: buttons rendered by the library (<puredashboard-button>) in a footer row, no hand-built DOM.
//   dialog({ title: "Delete?", actions: [
//     { label: "Cancel", value: "cancel" },
//     { label: "Delete", value: "ok", variant: "primary", danger: true },       // closes with that value
//     { label: "Save", variant: "primary", onclick: (ev, d) => save(d) },       // or run your own handler
//   ] }).show();                                                                // d.actions = [button elements]
//
//   drawer({ position: "right", title: "Filters", content: (b) => {…} }).show();
//
// SIZE: a modal is a fixed width, not shrink-to-content — `size: "sm" | "md" | "lg" |
// "xl" | "full"` (default "md", 560px), a number (px) or a CSS length, or "auto" to fit
// the content. Drawers take the same names for their width (left/right) or height
// (top/bottom). Pick it from the CONTENT: md = a short form, lg = a two-column form or a
// table, xl = a wide table / side-by-side panes, full = an editor or a big preview.
//
// Layout: the dialog is a flex column — the header and the optional footer stay
// pinned, and only the body scrolls (overflow:auto) once the content exceeds the
// dialog's max-height. Put actions (button rows) in `footer` so they stay visible.
//
// Contract: `content` (and `footer`) is a function (el) => void | a DOM Node | a
// plain string (inserted as TEXT — safe). For trusted markup, pass a function and
// render into the given element yourself (e.g. el.appendChild(node) or html``).

let _seq = 0;

// Size presets (px). Each is themable per app via --puredashboard-dialog-w-<name> (modal
// width) / --puredashboard-dialog-drawer-w-<name> / --puredashboard-dialog-drawer-h-<name>.
const MODAL_W = { sm: 400, md: 560, lg: 800, xl: 1140 };
const DRAWER_W = { sm: 320, md: 400, lg: 560, xl: 720 };
const DRAWER_H = { sm: "30vh", md: "45vh", lg: "65vh", xl: "80vh" };
const isPreset = (size, table) => typeof size === "string" && Object.prototype.hasOwnProperty.call(table, size);
// A size → a CSS length: a preset (through its themable var), a number (px), or a CSS
// length string as-is. null = "use the default".
function sizeLength(size, table, varName) {
  if (size == null || size === "" || size === "auto") return null;
  if (typeof size === "number" && Number.isFinite(size)) return size + "px";
  if (isPreset(size, table)) { const v = table[size]; return `var(${varName}-${size}, ${typeof v === "number" ? v + "px" : v})`; }
  return String(size);
}

/**
 * Open a modal dialog (or, with a side `position`, a drawer) at a FIXED width you pick
 * with `size` — `"sm"` 400 · `"md"` 560 (default) · `"lg"` 800 · `"xl"` 1140 · `"full"` ·
 * px · CSS length · `"auto"` — on the native `<dialog>`: top layer, focus trap, Esc and
 * backdrop light-dismiss. Imperative — call it, `.show()` the returned controller,
 * await `.closed`.
 *
 * SIZE IT FROM THE CONTENT — a modal is a fixed width (`size`, default `"md"` = 560px),
 * not shrink-to-content: `"sm"` (400) a message or one field · `"md"` (560) a short form
 * · `"lg"` (800) a two-column form or a table · `"xl"` (1140) a wide table or side-by-side
 * panes · `"full"` (viewport minus 16px each side) an editor or a large preview · a
 * number (px) or CSS length · `"auto"` to fit the content (old behaviour). Every size is
 * capped to the viewport. Drawers use the same names for their width (left/right:
 * 320/400/560/720px) or height (top/bottom: 30/45/65/80vh); without `size` a drawer is
 * 380px / 45vh. Height grows with the content up to `--puredashboard-dialog-max-h`
 * (85vh); only the body scrolls, header and footer stay put.
 *
 * @function dialog
 * @param {Object} opts - `{ title?, content?, footer?, actions?, size?, position?, className?, dismissable?, closeButton?, ariaLabel?, onClose? }`. `content` / `footer`: `(el) => void` | Node | string (inserted as TEXT). `actions`: `[{ label, value?, variant?, danger?, disabled?, attrs?, onclick?(ev, ctrl) }]` → library-rendered footer buttons; a click closes with `value` unless `onclick` is given. `size`: `"sm"|"md"|"lg"|"xl"|"full"|"auto"` | number (px) | CSS length — default `"md"`. `position`: `"center"` (default) | `"right"|"left"|"top"|"bottom"` (a drawer). `dismissable` (default true): Esc + backdrop close. `closeButton` (default true): the × in the header. `onClose(value)` runs after it closes.
 * @returns {Object} Controller `{ el, show(), close(value?), closed: Promise<value>, actions: Element[] }`. `close("ok")` resolves `closed` with `"ok"`; Esc / backdrop / × resolve it with `"close"` or `undefined`.
 *
 * @example
 * import { dialog, drawer } from "LIB/dialog.js";
 * const d = dialog({
 *   title: "Edit node", size: "lg",                       // a two-column form → lg
 *   content: (body) => body.append(form),
 *   actions: [{ label: "Cancel", value: "cancel" }, { label: "Save", value: "save", variant: "primary" }],
 * }).show();
 * if ((await d.closed) === "save") save(form);
 * drawer({ title: "Filters", size: "md", content: (b) => b.append(filters) }).show();   // 400px wide
 */
export function dialog(opts = {}) {
  const position = opts.position || "center";       // center = modal; sides = drawer
  const isDrawer = position !== "center";
  const dismissable = opts.dismissable !== false;
  const seq = ++_seq;

  const el = document.createElement("dialog");
  el.className = "puredashboard-dialog puredashboard-dialog--" + position + (opts.className ? " " + opts.className : "");
  // Modal width: a fixed size (default md), unless "auto". The value goes through a var
  // so a className / app stylesheet can still override it.
  if (!isDrawer) {
    const size = opts.size === undefined ? "md" : opts.size;
    const w = size === "full" ? "100vw" : sizeLength(size, MODAL_W, "--puredashboard-dialog-w");
    if (w) {
      el.classList.add("puredashboard-dialog--sized", "puredashboard-dialog--size-" + (size === "full" || isPreset(size, MODAL_W) ? size : "custom"));
      el.style.setProperty("--puredashboard-dialog-w", w);
    }
  }
  if (dismissable) el.setAttribute("closedby", "any");   // Esc + backdrop light-dismiss

  // ---- header (title + close button) — accessible name via aria-labelledby ----
  if (opts.title != null || opts.closeButton !== false) {
    const head = document.createElement("div");
    head.className = "puredashboard-dialog__head";
    if (opts.title != null) {
      const h = document.createElement("h2");
      h.className = "puredashboard-dialog__title"; h.id = "puredashboard-dialog__title-" + seq; h.textContent = opts.title;
      head.appendChild(h);
      el.setAttribute("aria-labelledby", h.id);
    } else if (opts.ariaLabel) {
      el.setAttribute("aria-label", opts.ariaLabel);
    }
    if (opts.closeButton !== false) {
      const b = document.createElement("button");
      b.type = "button"; b.className = "puredashboard-dialog__close"; b.setAttribute("aria-label", "Close"); b.textContent = "×";
      b.addEventListener("click", () => ctrl.close("close"));
      head.appendChild(b);
    }
    el.appendChild(head);
  } else if (opts.ariaLabel) {
    el.setAttribute("aria-label", opts.ariaLabel);
  }

  // ---- body content -----------------------------------------------------------
  const body = document.createElement("div");
  body.className = "puredashboard-dialog__body";
  const c = opts.content;
  if (typeof c === "function") c(body);
  else if (c instanceof Node) body.appendChild(c);
  else if (typeof c === "string") body.textContent = c;
  el.appendChild(body);

  // ---- footer (optional) — pinned below the scrolling body --------------------
  // Same shape as `content` (function | Node | string). The dialog is a flex column
  // (see dialog.css): head + footer stay fixed, only the body scrolls when the
  // content overflows the dialog's max-height.
  let foot = null;
  const fc = opts.footer;
  if (fc != null) {
    foot = document.createElement("div");
    foot.className = "puredashboard-dialog__footer";
    if (typeof fc === "function") fc(foot);
    else if (fc instanceof Node) foot.appendChild(fc);
    else if (typeof fc === "string") foot.textContent = fc;
    el.appendChild(foot);
  }

  // ---- actions (optional) — footer buttons rendered by the library --------------
  // `actions: [{ label, value?, variant?, danger?, disabled?, attrs?, onclick? }]`. Each becomes a <puredashboard-button> in a
  // `.puredashboard-dialog__actions` row appended to the footer (created when `footer` is absent). A click calls `onclick(ev, ctrl)`
  // if given, otherwise closes the dialog with `value`. The elements are returned as `ctrl.actions` (same order).
  const actionEls = [];
  if (Array.isArray(opts.actions) && opts.actions.length) {
    // The buttons are <puredashboard-button>s: make sure that element is defined, or an app that never imported button.js
    // would get unknown elements (no focus, no keyboard activation, no button role). Loaded only when actions are used, so
    // a dialog without them stays dependency-free; the elements created below upgrade in place once it is defined.
    if (typeof customElements !== "undefined" && !customElements.get("puredashboard-button")) import("./button.js");
    if (!foot) {
      foot = document.createElement("div");
      foot.className = "puredashboard-dialog__footer";
      el.appendChild(foot);
    }
    const row = document.createElement("div");
    row.className = "puredashboard-dialog__actions";
    for (const a of opts.actions) {
      const b = document.createElement("puredashboard-button");
      if (a.variant) b.setAttribute("variant", a.variant);
      if (a.danger) b.setAttribute("danger", "");
      if (a.disabled) b.setAttribute("disabled", "");
      for (const [k, v] of Object.entries(a.attrs || {})) b.setAttribute(k, v);
      b.textContent = a.label;
      b.addEventListener("click", (e) => { if (a.onclick) a.onclick(e, ctrl); else ctrl.close(a.value); });
      row.appendChild(b);
      actionEls.push(b);
    }
    foot.appendChild(row);
  }

  document.body.appendChild(el);

  // ---- drawer layout + slide (inline, CSP-safe) -------------------------------
  // Structural layout is inline (so a drawer works with no CSS), but the size is a
  // CSS var → the app themes width/height via --puredashboard-dialog-drawer-w / --puredashboard-dialog-drawer-h.
  const offscreen = { right: "translateX(100%)", left: "translateX(-100%)", top: "translateY(-100%)", bottom: "translateY(100%)" }[position];
  if (isDrawer) {
    Object.assign(el.style, { position: "fixed", margin: "0", maxWidth: "none", maxHeight: "none", transition: "transform .2s ease", transform: offscreen });
    // `size` picks the width (left/right) or height (top/bottom); else the themable default.
    const sw = opts.size === "full" ? "100vw" : sizeLength(opts.size, DRAWER_W, "--puredashboard-dialog-drawer-w");
    const sh = opts.size === "full" ? "100vh" : sizeLength(opts.size, DRAWER_H, "--puredashboard-dialog-drawer-h");
    const W = sw ? `min(100vw, ${sw})` : "var(--puredashboard-dialog-drawer-w, min(92vw, 380px))";
    const H = sh ? `min(100vh, ${sh})` : "var(--puredashboard-dialog-drawer-h, 45vh)";
    // Reset the OPPOSITE inset to auto: the UA stylesheet gives a modal <dialog> a
    // default `inset: 0`, so pinning only `right`/`bottom` leaves the UA `left`/`top: 0`
    // in place and the panel sticks to the wrong edge. Set both axes explicitly.
    if (position === "right") Object.assign(el.style, { top: "0", right: "0", bottom: "0", left: "auto", height: "100%", width: W });
    if (position === "left") Object.assign(el.style, { top: "0", left: "0", bottom: "0", right: "auto", height: "100%", width: W });
    if (position === "top") Object.assign(el.style, { top: "0", left: "0", right: "0", bottom: "auto", width: "100%", height: H });
    if (position === "bottom") Object.assign(el.style, { bottom: "0", left: "0", right: "0", top: "auto", width: "100%", height: H });
  }

  // ---- light-dismiss fallback for browsers without closedby (Safari) ----------
  if (dismissable && !("closedBy" in HTMLDialogElement.prototype)) {
    el.addEventListener("click", (e) => {
      if (e.target !== el) return;                 // only clicks on the backdrop itself
      const r = el.getBoundingClientRect();
      const inside = r.top <= e.clientY && e.clientY <= r.top + r.height && r.left <= e.clientX && e.clientX <= r.left + r.width;
      if (!inside) ctrl.close("dismiss");
    });
  }

  // ---- lifecycle: a single idempotent teardown, driven either by our close() or
  // by the browser closing the dialog itself (Esc / native light-dismiss). --------
  let resolveClosed, torn = false;
  const closed = new Promise((res) => (resolveClosed = res));
  function teardown() {
    if (torn) return;
    torn = true;
    if (el.isConnected) el.remove();
    opts.onClose && opts.onClose(el.returnValue);
    resolveClosed(el.returnValue);
  }
  el.addEventListener("close", teardown);          // Esc / native closedby path

  const ctrl = {
    el, body, footer: foot, closed, actions: actionEls,
    show() {
      if (typeof el.showModal === "function") { try { el.showModal(); } catch { /* jsdom */ } }
      if (!el.open) el.open = true;                // fallback where modal isn't supported
      if (isDrawer) {
        if (typeof requestAnimationFrame === "function") requestAnimationFrame(() => (el.style.transform = "none"));
        else el.style.transform = "none";
      }
      return ctrl;
    },
    close(value) {
      if (value !== undefined) el.returnValue = value;
      const finish = () => {
        if (el.open) {
          if (typeof el.close === "function") { try { el.close(el.returnValue); } catch { /* jsdom */ } }
          if (el.open) { el.open = false; el.removeAttribute("open"); }   // env without close()
        }
        teardown();
      };
      if (isDrawer && el.isConnected && el.open) {  // animate out, then close + tear down
        el.style.transform = offscreen;
        let done = false;
        const fin = () => { if (done) return; done = true; el.removeEventListener("transitionend", fin); finish(); };
        el.addEventListener("transitionend", fin);
        setTimeout(fin, 300);                       // safety: fire even with no transition
      } else {
        finish();                                   // open → close+teardown; never-shown → teardown
      }
      return ctrl;
    },
  };
  return ctrl;
}

/**
 * A dialog that slides in from a side (default `"right"`). Same options and controller as
 * `dialog()`; `size` sets its width (left/right: `"sm"` 320 · `"md"` 400 · `"lg"` 560 ·
 * `"xl"` 720px · `"full"`) or height (top/bottom: 30 · 45 · 65 · 80vh · `"full"`); without
 * it the drawer is 380px wide / 45vh tall.
 *
 * @function drawer
 * @param {Object} opts - `dialog()` options; `position`: `"right"` (default) | `"left"` | `"top"` | `"bottom"`.
 * @returns {Object} The `dialog()` controller.
 */
export const drawer = (opts = {}) => dialog({ ...opts, position: opts.position || "right" });

// ---- promise-based alert / confirm / prompt --------------------------------
// Drop-in-feeling replacements for window.alert/confirm/prompt, but non-blocking
// (return a Promise), styled, and accessible (role=alertdialog + aria-describedby,
// autofocus on the primary action). Esc / backdrop / close button = cancel.
let _hseq = 0;
function mkBtn(label, cls, onClick) {
  const b = document.createElement("button");
  b.type = "button"; b.className = cls; b.textContent = label;
  b.addEventListener("click", onClick);
  return b;
}
function message(text) {
  const p = document.createElement("p");
  p.className = "puredashboard-dialog__message"; p.id = "puredashboard-dialog__message-" + (++_hseq); p.textContent = text;
  return p;
}
function actions(...buttons) {
  const row = document.createElement("div");
  row.className = "puredashboard-dialog__actions"; row.append(...buttons);
  return row;
}

/**
 * Promise-based replacements for `window.alert` / `confirm` / `prompt`: non-blocking,
 * styled, accessible (`role="alertdialog"`, focus on the primary action). They fit their
 * message (`size` default `"auto"`, at least 320px); pass `size` to widen one.
 *
 * @function alert
 * @param {string} text - The message (inserted as text).
 * @param {Object} [opts] - `{ title?, okText?, size?, dismissable?, closeButton?, className? }`. `confirm` adds `cancelText?`, `danger?`; `prompt` adds `defaultValue?`, `inputType?`.
 * @returns {Promise} `alert` → `void` once dismissed · `confirm` → `true` only if OK · `prompt` → the string, or `null` if cancelled.
 *
 * @example
 * if (await confirm("Delete 3 services?", { danger: true, okText: "Delete" })) remove();
 * const name = await prompt("New name?", { defaultValue: "web-01" });
 */
export function alert(text, opts = {}) {
  return new Promise((resolve) => {
    let settled = false;
    const msg = message(text);
    const d = dialog({
      title: opts.title, ariaLabel: opts.title ? undefined : (opts.ariaLabel || "Alert"),
      className: "puredashboard-dialog--alert" + (opts.className ? " " + opts.className : ""),
      size: opts.size === undefined ? "auto" : opts.size,   // a message fits its content unless sized
      dismissable: opts.dismissable !== false, closeButton: !!opts.closeButton,
      content: (body) => { body.append(msg); },
      footer: (foot) => {
        const ok = mkBtn(opts.okText || "OK", opts.okClass || "puredashboard-dialog__button puredashboard-dialog__button--primary", () => d.close("ok"));
        ok.setAttribute("autofocus", "");
        foot.append(actions(ok));
      },
      onClose: () => { if (!settled) { settled = true; resolve(); } },
    });
    d.el.setAttribute("role", "alertdialog");
    d.el.setAttribute("aria-describedby", msg.id);
    d.show();
  });
}

// confirm(text, opts?) → Promise<boolean> (true only if OK was chosen)
export function confirm(text, opts = {}) {
  return new Promise((resolve) => {
    let result = false, settled = false;
    const msg = message(text);
    const finish = (ok) => { result = ok; d.close(ok ? "ok" : "cancel"); };
    const d = dialog({
      title: opts.title, ariaLabel: opts.title ? undefined : (opts.ariaLabel || "Confirm"),
      className: "puredashboard-dialog--confirm" + (opts.className ? " " + opts.className : ""),
      size: opts.size === undefined ? "auto" : opts.size,   // a message fits its content unless sized
      dismissable: opts.dismissable !== false, closeButton: !!opts.closeButton,
      content: (body) => { body.append(msg); },
      footer: (foot) => {
        const cancel = mkBtn(opts.cancelText || "Cancel", opts.cancelClass || "puredashboard-dialog__button", () => finish(false));
        const ok = mkBtn(opts.okText || "OK", opts.okClass || ("puredashboard-dialog__button puredashboard-dialog__button--primary" + (opts.danger ? " puredashboard-dialog__button--danger" : "")), () => finish(true));
        ok.setAttribute("autofocus", "");
        foot.append(actions(cancel, ok));
      },
      onClose: () => { if (!settled) { settled = true; resolve(result); } },  // Esc/backdrop → false
    });
    d.el.setAttribute("role", "alertdialog");
    d.el.setAttribute("aria-describedby", msg.id);
    d.show();
  });
}

// prompt(text, opts?) → Promise<string|null> (null if cancelled/dismissed)
export function prompt(text, opts = {}) {
  return new Promise((resolve) => {
    let result = null, settled = false, input;
    const msg = message(text);
    const finish = (ok) => { result = ok ? input.value : null; d.close(ok ? "ok" : "cancel"); };
    const d = dialog({
      title: opts.title, ariaLabel: opts.title ? undefined : (opts.ariaLabel || "Prompt"),
      className: "puredashboard-dialog--prompt" + (opts.className ? " " + opts.className : ""),
      size: opts.size === undefined ? "auto" : opts.size,   // a message fits its content unless sized
      dismissable: opts.dismissable !== false, closeButton: !!opts.closeButton,
      content: (body) => {
        body.append(msg);
        input = document.createElement("input");
        input.className = "puredashboard-dialog__input"; input.value = opts.defaultValue != null ? String(opts.defaultValue) : "";
        if (opts.inputType) input.type = opts.inputType;
        input.setAttribute("autofocus", "");
        input.addEventListener("keydown", (e) => { if (e.key === "Enter") { e.preventDefault(); finish(true); } });
        body.append(input);
      },
      footer: (foot) => {
        const cancel = mkBtn(opts.cancelText || "Cancel", opts.cancelClass || "puredashboard-dialog__button", () => finish(false));
        const ok = mkBtn(opts.okText || "OK", opts.okClass || "puredashboard-dialog__button puredashboard-dialog__button--primary", () => finish(true));
        foot.append(actions(cancel, ok));
      },
      onClose: () => { if (!settled) { settled = true; resolve(result); } },  // Esc/backdrop → null
    });
    d.el.setAttribute("aria-describedby", msg.id);
    d.show();
    if (typeof requestAnimationFrame === "function") requestAnimationFrame(() => { try { input.select(); } catch { /* */ } });
  });
}
