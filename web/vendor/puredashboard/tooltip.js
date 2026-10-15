// <puredashboard-tooltip> — an accessible hover/focus tooltip. Zero-dep, no
// build, CSP-safe. Extends plain HTMLElement (NOT Reactive) — its whole job is to
// PRESERVE the author's single trigger child (a button, icon, link, …) and wrap
// it, exactly like form.js keeps its light-DOM children. A Reactive render()
// would blow that child away.
//
// Why plain HTMLElement + textContent: the tooltip label is author-supplied
// CONTENT (the `text` property). It reaches the DOM ONLY via `textContent` — never
// innerHTML/html`` — so it stays XSS-safe under a strict CSP even if `text` ever
// carried untrusted characters.
//
// Behaviour (WAI-ARIA APG "Tooltip"): on connect, keep the trigger, create a
// node with role="tooltip" + a unique id, and point the trigger's
// aria-describedby at it. Show on mouseenter AND focusin (keyboard users get it
// too) after `delay` ms; hide on mouseleave, focusout and Escape. The tooltip is
// never focusable and never traps focus. Positioning is computed in JS on show
// (read the trigger rect), placed fixed, and flips to the opposite side on
// viewport overflow — all guarded so it no-ops safely under jsdom (zero rects).
//
// Follows the library conventions: fixed strings in a LABELS map + a `labels`
// override, BEM classes namespaced by the tag, script hooks as SEPARATE js-…
// classes, theming through the shared design tokens (--panel-3, --text, --border,
// --radius-sm, --shadow-2, --font-size-sm, --duration-fast) with a --pd-* fallback
// chain so it works with no theme linked.

// All FIXED user-facing strings live here (English defaults). Override any subset
// via the `labels` property. The tooltip's own label is NOT here — it's author
// CONTENT supplied through the `text` property. These are only structural
// fallbacks (e.g. an aria-label on the wrapper when nothing else describes it).
// Function-valued keys interpolate.
const LABELS = {
  tooltip: "Tooltip",
};

let uid = 0;

/**
 * An accessible hover/focus tooltip that wraps a single trigger element (the
 * author's child — a `<button>`, icon, link, …). On connect it keeps that trigger,
 * creates a companion node with `role="tooltip"` and a unique id, and sets the
 * trigger's `aria-describedby` to that id. The tooltip shows on `mouseenter` and
 * `focusin` (so keyboard users get it too) after `delay` ms, and hides on
 * `mouseleave`, `focusout` and Escape. It is never focusable and never traps
 * focus. Configure via JS properties or declarative attributes.
 *
 * The tooltip label is author CONTENT set through the `text` property; it reaches
 * the DOM only via `textContent` (never `innerHTML`) for XSS-safety.
 *
 * @element puredashboard-tooltip
 *
 * @prop {string}  text      - The tooltip label (required content). Rendered via `textContent`. Default `""`.
 * @prop {string}  placement - `"top"` | `"bottom"` | `"left"` | `"right"`. Default `"top"`. Flips on viewport overflow.
 * @prop {number}  delay     - Milliseconds before showing on hover/focus. Default `100`.
 * @prop {boolean} disabled  - When `true`, the tooltip never shows. Default `false`.
 * @prop {Object}  labels    - Override UI strings. Keys: `tooltip`. Unset keys keep the English default.
 * @attr {string}  text      - Declarative form of `text`.
 * @attr {string}  placement - Declarative form of `placement`.
 * @attr {number}  delay     - Declarative form of `delay`.
 * @attr {boolean} disabled  - Declarative form of `disabled`.
 *
 * @method show - `show() => void` — show the tooltip immediately (unless disabled/empty).
 * @method hide - `hide() => void` — hide the tooltip immediately.
 *
 * @cssprop [--pd-tooltip-bg]     - Tooltip background (defaults to `--panel-3`).
 * @cssprop [--pd-tooltip-offset] - Gap between trigger and tooltip (defaults to `--sp-2`).
 *
 * @example
 * // <puredashboard-tooltip text="Delete" placement="bottom">
 * //   <button aria-label="Delete">🗑</button>
 * // </puredashboard-tooltip>
 * const tip = document.createElement("puredashboard-tooltip");
 * tip.text = "Save"; tip.append(button);
 */
class PuredashboardTooltip extends HTMLElement {
  static get observedAttributes() { return ["text", "placement", "delay", "disabled"]; }

  constructor() {
    super();
    this._wrapped = false;
    this._trigger = null;
    this._tip = null;
    this._shown = false;
    this._timer = 0;
    this._id = `js-puredashboard-tooltip__tip-${++uid}`;
    // A template engine may set properties before upgrade, leaving plain own
    // properties that shadow the accessors; reconcile them (library convention).
    for (const p of ["text", "placement", "delay", "disabled", "labels"]) this._upgrade(p);
  }

  _upgrade(p) {
    if (Object.prototype.hasOwnProperty.call(this, p)) { const v = this[p]; delete this[p]; this[p] = v; }
  }

  // ---- reflected properties ------------------------------------------------
  get text() { return this._text ?? ""; }
  set text(v) { this._text = v == null ? "" : String(v); if (this._tip) this._tip.textContent = this._text; }

  get placement() { return this._placement || "top"; }
  set placement(v) { this._placement = v || "top"; }

  get delay() { const d = Number(this._delay); return Number.isFinite(d) && d >= 0 ? d : 100; }
  set delay(v) { this._delay = v; }

  get disabled() { return !!this._disabled; }
  set disabled(v) { this._disabled = !!v; if (this._disabled) this.hide(); }

  attributeChangedCallback(name, _old, val) {
    if (name === "disabled") { this.disabled = val !== null; return; }
    if (name === "delay") { this.delay = val; return; }
    this[name] = val;
  }

  // _label(key, …args) → localised string: this.labels override, else the default.
  _label(key, ...a) { const v = (this.labels && this.labels[key]) ?? LABELS[key]; return typeof v === "function" ? v(...a) : v; }

  // _wrap() BUILDS the tip and must run once. Wiring is different: disconnectedCallback
  // removes the listeners, and a RELOCATION is a disconnect plus a reconnect — re-parenting
  // a node is defined as a remove plus an insert, so a keyed repeat() reorder or a filter
  // runs both. With the wiring inside the once-only guard, a moved tooltip came back
  // permanently dead: measured, focusin showed it before a move and did nothing after.
  // Build once, wire on every connect.
  connectedCallback() {
    this._wrap();
    this._bind();
    // A tip that was SHOWING when the row moved is anchored to a trigger that has travelled
    // (position:fixed, placed once from getBoundingClientRect). This tests FOCUS only, so a
    // hover-shown tip always takes the hide branch — and that is the right outcome, not a gap.
    // Measured with a real pointer: reversing a five-row list moved the trigger from y=10 to
    // y=410, `:hover` on it went true → false, and the row that landed under the stationary
    // cursor showed ITS own tooltip. The pointer does not follow a row, so a hover-shown tip
    // has genuinely lost its reason. Focus does follow where the engine can move a row
    // atomically, which is why that case re-anchors instead.
    if (this._shown) {
      if (this.contains(document.activeElement)) this._position();
      else this.hide();
    }
  }

  _bind() {
    // addEventListener de-dupes an identical (type, handler, capture) triple, so calling
    // this on a connect that did not follow a disconnect is a no-op.
    this.addEventListener("mouseenter", this._onEnter);
    this.addEventListener("mouseleave", this._onLeave);
    this.addEventListener("focusin", this._onEnter);
    this.addEventListener("focusout", this._onLeave);
    this.addEventListener("keydown", this._onKey);
  }

  // Adopt the author's single trigger child, create the tooltip node, and wire
  // ARIA + listeners. Guarded so it runs exactly once across reconnects.
  _wrap() {
    if (this._wrapped) return;
    this._wrapped = true;

    // The trigger is the author's first element child. Keep the live node (never
    // clone) so its listeners/state survive.
    this._trigger = this.firstElementChild;

    const tip = document.createElement("span");
    tip.className = "puredashboard-tooltip__content js-puredashboard-tooltip__content";
    tip.id = this._id;
    tip.setAttribute("role", "tooltip");
    tip.setAttribute("aria-hidden", "true");
    tip.hidden = true;
    tip.textContent = this.text;           // author content — textContent ONLY
    this.appendChild(tip);
    this._tip = tip;

    if (this._trigger) {
      // Point the trigger at the tooltip for screen readers. Preserve any existing
      // aria-describedby by appending our id.
      const prev = this._trigger.getAttribute("aria-describedby");
      this._trigger.setAttribute("aria-describedby", prev ? `${prev} ${this._id}` : this._id);
    } else {
      // No trigger yet: at least label the wrapper so the role isn't orphaned.
      if (!this.hasAttribute("aria-label")) this.setAttribute("aria-label", this._label("tooltip"));
    }

    // Listeners are wired by _bind() from connectedCallback, not here — see the comment
    // there. Show on hover AND focus (keyboard parity); hide on the mirror events + Esc.
  }

  disconnectedCallback() {
    this._clearTimer();
    this.removeEventListener("mouseenter", this._onEnter);
    this.removeEventListener("mouseleave", this._onLeave);
    this.removeEventListener("focusin", this._onEnter);
    this.removeEventListener("focusout", this._onLeave);
    this.removeEventListener("keydown", this._onKey);
  }

  _clearTimer() { if (this._timer) { clearTimeout(this._timer); this._timer = 0; } }

  _onEnter = () => {
    if (this.disabled || !this.text) return;
    this._clearTimer();
    const d = this.delay;
    if (d <= 0) { this.show(); return; }
    this._timer = setTimeout(() => { this._timer = 0; this.show(); }, d);
  };

  _onLeave = () => { this.hide(); };

  _onKey = (e) => { if (e.key === "Escape" && this._shown) { e.stopPropagation(); this.hide(); } };

  // ---- public API ----------------------------------------------------------
  /** Show the tooltip now (unless disabled or empty). */
  show() {
    this._clearTimer();
    if (this.disabled || !this.text || !this._tip || this._shown) return;
    this._shown = true;
    this._tip.hidden = false;
    this._tip.setAttribute("aria-hidden", "false");
    this._tip.classList.add("puredashboard-tooltip__content--visible");
    this._position();
  }

  /** Hide the tooltip now. */
  hide() {
    this._clearTimer();
    if (!this._tip || !this._shown) { this._shown = false; return; }
    this._shown = false;
    this._tip.hidden = true;
    this._tip.setAttribute("aria-hidden", "true");
    this._tip.classList.remove("puredashboard-tooltip__content--visible");
  }

  // Position the tooltip relative to the trigger, fixed, per placement, with a
  // small offset. Flips to the opposite side on viewport overflow (best-effort).
  // Guarded for jsdom, where getBoundingClientRect returns all-zero rects — in
  // that case we simply leave the CSS default placement (no crash, still shows).
  _position() {
    const trigger = this._trigger, tip = this._tip;
    if (!trigger || !tip || typeof trigger.getBoundingClientRect !== "function") return;
    const r = trigger.getBoundingClientRect();
    // jsdom / detached nodes: zero-sized rect → skip JS positioning entirely.
    if (!r.width && !r.height && !r.top && !r.left) return;

    const tw = tip.offsetWidth || 0, th = tip.offsetHeight || 0;
    const vw = window.innerWidth || 0, vh = window.innerHeight || 0;
    // Offset in px — read the resolved --pd-tooltip-offset, default 8.
    let gap = 8;
    try {
      const raw = getComputedStyle(tip).getPropertyValue("--pd-tooltip-offset").trim();
      const n = parseFloat(raw);
      if (Number.isFinite(n)) gap = n;
    } catch { /* getComputedStyle unavailable */ }

    let place = this.placement;
    // Flip if the chosen side would overflow and the opposite side fits.
    if (vw && vh) {
      if (place === "top" && r.top - gap - th < 0 && r.bottom + gap + th <= vh) place = "bottom";
      else if (place === "bottom" && r.bottom + gap + th > vh && r.top - gap - th >= 0) place = "top";
      else if (place === "left" && r.left - gap - tw < 0 && r.right + gap + tw <= vw) place = "right";
      else if (place === "right" && r.right + gap + tw > vw && r.left - gap - tw >= 0) place = "left";
    }

    let top = 0, left = 0;
    switch (place) {
      case "bottom": top = r.bottom + gap; left = r.left + (r.width - tw) / 2; break;
      case "left":   top = r.top + (r.height - th) / 2; left = r.left - gap - tw; break;
      case "right":  top = r.top + (r.height - th) / 2; left = r.right + gap; break;
      case "top":
      default:       top = r.top - gap - th; left = r.left + (r.width - tw) / 2; break;
    }
    // Clamp into the viewport so it never renders off-screen.
    if (vw) left = Math.max(4, Math.min(left, vw - tw - 4));
    if (vh) top = Math.max(4, Math.min(top, vh - th - 4));

    tip.setAttribute("data-placement", place);
    tip.style.position = "fixed";
    tip.style.top = top + "px";
    tip.style.left = left + "px";
  }
}

customElements.define("puredashboard-tooltip", PuredashboardTooltip);

export { PuredashboardTooltip };
