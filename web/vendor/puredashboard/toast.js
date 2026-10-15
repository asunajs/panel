// toast.js — transient notifications. Zero-dep, no build, CSP-safe (layout via CSSOM
// inline styles; theme the .puredashboard-toast__* classes via toast.css / --toast-* tokens).
//
// Uses a single popover="manual" stack container so toasts live in the TOP LAYER —
// above normal page content and other popovers (dropdowns, tooltips), regardless of
// z-index. NOTE: a modal <dialog> (showModal) still paints above popovers — that's a
// platform top-layer ordering rule, not something we can override — so toasts are
// meant for the non-modal flow. Browsers without the Popover API fall back to a
// fixed, high-z-index container.
//
//   import { toast } from "./vendor/puredashboard/toast.js";
//   toast("Saved");                                  // info, auto-dismiss 4s
//   toast.success("Node added");
//   toast.error("Save failed", { duration: 0 });     // 0 = sticky (no auto-dismiss)
//   const t = toast.warn("Reconnecting…"); t.close(); // manual close
//
// opts: { type: "info"|"success"|"warn"|"error", duration=4000 (ms, 0=sticky),
//         dismissable=true (× button), className, onClose }

let stack = null;
let stackShown = false;
const TYPES = new Set(["info", "success", "warn", "error"]);

function ensureStack() {
  if (stack && stack.isConnected) return stack;
  stack = document.createElement("div");
  stack.className = "puredashboard-toast-stack";
  // layout inline so it works with no CSS; pointer-events:none lets clicks fall
  // through the gaps, while each toast re-enables them.
  Object.assign(stack.style, {
    position: "fixed", inset: "auto 1rem 1rem auto", margin: "0", padding: "0", border: "0",
    background: "transparent", display: "flex", flexDirection: "column", gap: "10px",
    maxWidth: "min(92vw, 360px)", pointerEvents: "none",
  });
  if (typeof stack.showPopover === "function") stack.setAttribute("popover", "manual");  // top-layer
  else stack.style.zIndex = "9999";                 // fallback: high stacking context
  document.body.appendChild(stack);
  stackShown = false;
  return stack;
}

// raise shows the popover stack if it isn't already — re-shown after it emptied and
// hid itself, so the 2nd-and-later toasts still land in the top layer.
function raise() {
  if (stack.hasAttribute("popover") && !stackShown) { try { stack.showPopover(); stackShown = true; } catch { /* */ } }
}

/**
 * Show a transient notification in the top layer (bottom-right stack). Imperative —
 * call it; it returns a handle. Shortcuts: `toast.info` / `toast.success` / `toast.warn`
 * / `toast.error`. Note: a modal `<dialog>` still paints above toasts (a platform rule).
 *
 * @function toast
 * @param {string} text - The message (inserted as text).
 * @param {Object} [opts] - `{ type?: "info"|"success"|"warn"|"error", duration?: number (ms, default 4000, 0 = sticky), dismissable?: boolean (× button, default true), className?, onClose? }`.
 * @returns {Object} `{ el, close() }`.
 *
 * @example
 * import { toast } from "LIB/toast.js";
 * toast.success("Saved");
 * const t = toast.error("Save failed", { duration: 0 });   // sticky until t.close()
 */
export function toast(text, opts = {}) {
  const s = ensureStack();
  const type = TYPES.has(opts.type) ? opts.type : "info";
  const duration = opts.duration != null ? opts.duration : 4000;

  const el = document.createElement("div");
  el.className = "puredashboard-toast puredashboard-toast--" + type + (opts.className ? " " + opts.className : "");
  el.setAttribute("role", type === "error" || type === "warn" ? "alert" : "status");
  Object.assign(el.style, { pointerEvents: "auto", transition: "opacity .2s ease, transform .2s ease", opacity: "0", transform: "translateY(8px)" });

  const msg = document.createElement("span");
  msg.className = "puredashboard-toast__message"; msg.textContent = text;
  el.appendChild(msg);

  let timer = null, closed = false;
  function close() {
    if (closed) return;
    closed = true;
    if (timer) clearTimeout(timer);
    Object.assign(el.style, { opacity: "0", transform: "translateY(8px)" });   // animate out
    let done = false;
    const fin = () => {
      if (done) return;
      done = true;
      el.removeEventListener("transitionend", fin);
      el.remove();
      if (s.children.length === 0 && s.hasAttribute("popover")) { try { s.hidePopover(); stackShown = false; } catch { /* */ } }
      opts.onClose && opts.onClose();
    };
    el.addEventListener("transitionend", fin);
    setTimeout(fin, 250);                            // safety: fire even with no transition
  }

  if (opts.dismissable !== false) {
    const b = document.createElement("button");
    b.type = "button"; b.className = "puredashboard-toast__close"; b.setAttribute("aria-label", "Dismiss"); b.textContent = "×";
    b.addEventListener("click", close);
    el.appendChild(b);
  }

  s.appendChild(el);
  raise();
  if (typeof requestAnimationFrame === "function") requestAnimationFrame(() => Object.assign(el.style, { opacity: "1", transform: "none" }));
  else Object.assign(el.style, { opacity: "1", transform: "none" });

  if (duration > 0) timer = setTimeout(close, duration);
  return { el, close };
}

toast.info = (t, o) => toast(t, { ...o, type: "info" });
toast.success = (t, o) => toast(t, { ...o, type: "success" });
toast.warn = (t, o) => toast(t, { ...o, type: "warn" });
toast.error = (t, o) => toast(t, { ...o, type: "error" });
