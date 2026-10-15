// <puredashboard-button> — a themed button (or link) that wraps its author label.
// Zero-dep, no build, CSP-safe. Extends plain HTMLElement (NOT Reactive) — the
// component's job is to PRESERVE the author's light-DOM children as the button
// label; a Reactive render() would blow those children away. Same wrap-children-
// once pattern as <puredashboard-form>.
//
// On connect (guarded, once) it creates ONE inner native element — a real
// <button> normally, or an <a href> when `href` is set — MOVES the author's
// children into it (order preserved, moved not serialized, so any live nodes /
// listeners / nested elements survive), and appends that element to the host.
// Everything after is kept in sync via observedAttributes: variant/size modifier
// classes, danger/block/disabled/loading state, type on the inner button, etc.
//
// Why a native inner element: a real <button> gives native focus, keyboard
// (Enter/Space) and — with type="submit" — native <form> submission for free; a
// real <a href> gives open-in-new-tab / middle-click / keyboard navigation for
// free. We add nothing custom: the native click simply bubbles.
//
// Follows the library conventions: fixed strings in a LABELS map + a `labels`
// override (only the aria-busy label needs one — the button's TEXT is author
// content), BEM classes namespaced by the tag, script hooks as SEPARATE js-…
// classes, self-contained inline SVG (the loading spinner glyph), and theming
// through the shared --pd-* token chain (--accent, --panel-2/3, --border,
// --danger, --radius, --control-height-*, --focus-ring, …) so it works with no
// theme linked. The spinner glyph markup is author-trusted (like menu.js icons).

// All FIXED user-facing strings live here (English defaults). Override any subset
// via the `labels` property — e.g. btn.labels = { loading: "Đang tải" }. The
// button's VISIBLE text is the author's child content, NOT a label; the only
// fixed string is the accessible name announced while `loading` (aria-label on
// the aria-busy host). Function-valued keys interpolate.
const LABELS = {
  loading: "Loading",
};

// The inline loading spinner glyph — a self-contained SVG ring (no shared icon
// module, no external sprite). aria-hidden decoration; the aria-busy/aria-label
// on the host carry the meaning. Trusted author markup (like menu.js icons), so
// it may go through innerHTML — it is a constant, never user data.
const SPINNER_SVG =
  '<svg class="puredashboard-button__spinner-svg" viewBox="0 0 16 16" width="1em" height="1em" fill="none" aria-hidden="true" focusable="false">' +
  '<circle class="puredashboard-button__spinner-track" cx="8" cy="8" r="6" stroke-width="2"></circle>' +
  '<path class="puredashboard-button__spinner-head" d="M8 2a6 6 0 0 1 6 6" stroke-width="2" stroke-linecap="round"></path>' +
  "</svg>";

const VARIANTS = new Set(["primary", "default", "dashed", "text", "link"]);
const SIZES = new Set(["sm", "md", "lg"]);
const STATUSES = new Set(["success", "warning", "danger"]);
const SHAPES = new Set(["default", "round", "circle"]);
const FORWARDED = ["role", "aria-checked", "aria-pressed"];   // opt-in attributes moved to the inner element

/**
 * A themed button — or link — that wraps its author-provided label content in a
 * real native `<button>` (or an `<a href>` when `href` is set), so native focus,
 * keyboard, `<form>` submission (`type="submit"`) and link navigation all work
 * with no custom event wiring. The author's children are MOVED into the inner
 * element on connect (order preserved, live nodes intact) as the label; they are
 * never serialized. Emits nothing custom — the native `click` simply bubbles.
 *
 * Configure via JS properties or declarative attributes.
 *
 * @element puredashboard-button
 *
 * @prop {string}  variant  - `"primary"` | `"default"` (secondary) | `"dashed"` | `"text"` | `"link"`. Default `"default"`.
 * @prop {string}  size     - `"sm"` | `"md"` | `"lg"`. Default `"md"`.
 * @prop {string}  status   - Intent colour that recolours the button across every variant: `"success"` (green) | `"warning"` (amber) | `"danger"` (red). Unset by default. `danger` is the legacy shorthand for `status="danger"`.
 * @prop {string}  shape    - `"default"` | `"round"` (pill) | `"circle"` (1:1, for an icon-only button). Default `"default"`.
 * @prop {boolean} danger   - Destructive (red) styling — shorthand for `status="danger"`. Default `false`.
 * @prop {boolean} disabled - Disable the control (blocks interaction; `aria-disabled` on links). Default `false`.
 * @prop {boolean} loading  - Show a leading spinner glyph, set `aria-busy`, and block interaction. Default `false`.
 * @prop {boolean} block    - Full-width (fills the host). Default `false`.
 * @prop {string}  type     - `"submit"` | `"button"` | `"reset"`, applied to the inner `<button>` so a submit button submits its `<form>` natively. Default `"button"`. Ignored when `href` is set.
 * @prop {string}  href     - When set, render an `<a href>` instead of a `<button>`.
 * @prop {string}  icon     - Optional leading inline SVG markup string (author-TRUSTED, like `menu.js` icons).
 * @prop {boolean} iconRight - Place the `icon` AFTER the label instead of before. Default `false`.
 * @prop {Object}  labels   - Override UI strings. Keys: `loading`. Unset keys keep the English default.
 *
 * @attr {string}  variant    - Declarative form of `variant`.
 * @attr {string}  size       - Declarative form of `size`.
 * @attr {string}  status     - Declarative form of `status`.
 * @attr {string}  shape      - Declarative form of `shape`.
 * @attr {boolean} danger     - Declarative form of `danger`.
 * @attr {boolean} disabled   - Declarative form of `disabled`.
 * @attr {boolean} loading    - Declarative form of `loading`.
 * @attr {boolean} block      - Declarative form of `block`.
 * @attr {string}  type       - Declarative form of `type`.
 * @attr {string}  href       - Declarative form of `href`.
 * @attr {string}  icon       - Declarative form of `icon`.
 * @attr {boolean} icon-right  - Declarative form of `iconRight`.
 * @attr {string}  aria-label - Accessible name, mirrored onto the inner `<button>`/`<a>`. REQUIRED for an icon-only button (`icon` + no children — e.g. a kebab `⋯` / hamburger `☰` menu trigger); an author name is never overwritten by the `loading` fallback.
 * @attr {string}  aria-labelledby - IDs naming the button; mirrored onto the inner element like `aria-label`.
 * @attr {string}  role - Opt-in: a role for the inner `<button>` (e.g. `"switch"`). MOVED to the inner element (removed from the host, so the host never exposes a second node with the role). Set it once; an empty string clears it.
 * @attr {string}  aria-checked - Opt-in: state for a `role="switch"`/`checkbox` button; moved to the inner element like `role`. Update it by setting the attribute on the host again.
 * @attr {string}  aria-pressed - Opt-in: state for a toggle button; moved to the inner element like `role`.
 *
 * @fires click - Native, bubbling `click` from the inner `<button>`/`<a>` (suppressed while disabled/loading).
 *
 * @method focus - `focus() => void` — focus the inner button/link.
 *
 * @cssprop [--pd-button-height] - Control height (defaults to `--control-height-md`).
 * @cssprop [--pd-button-pad-x]  - Horizontal padding (defaults to `--control-pad-x`).
 *
 * @example
 * // <puredashboard-button variant="primary">Save</puredashboard-button>
 * // <puredashboard-button href="/docs">Docs</puredashboard-button>       ← renders an <a href>
 * // <puredashboard-button type="submit">Submit</puredashboard-button>    ← submits its <form>
 * const btn = document.createElement("puredashboard-button");
 * btn.variant = "primary"; btn.append("Save");
 * btn.addEventListener("click", () => save());
 */
class PuredashboardButton extends HTMLElement {
  static get observedAttributes() {
    return ["variant", "size", "status", "shape", "danger", "disabled", "loading", "block", "type", "href", "icon", "icon-right", "aria-label", "aria-labelledby", "role", "aria-checked", "aria-pressed"];
  }

  constructor() {
    super();
    this._wrapped = false;
    this._scheduled = false;
    this._el = null;        // the inner <button> or <a>
    this._labelHost = null; // wrapper holding the moved author children
    this._fwd = {};         // role / aria-checked / aria-pressed moved from the host to the inner element
    this._moving = false;   // true while we remove a moved attribute from the host (its change callback must not clear it)
    // A template engine may set properties before upgrade, leaving plain
    // own-properties that shadow the accessors. Reconcile them (same pattern as
    // the rest of the library).
    for (const p of ["variant", "size", "status", "shape", "danger", "disabled", "loading", "block", "type", "href", "icon", "iconRight", "labels"]) {
      this._upgrade(p);
    }
  }

  _upgrade(p) {
    if (Object.prototype.hasOwnProperty.call(this, p)) { const v = this[p]; delete this[p]; this[p] = v; }
  }

  // ---- reflected properties ---------------------------------------------------
  // Backed by attributes so JS and declarative HTML stay in one source of truth.
  get variant() { return this.getAttribute("variant") || "default"; }
  set variant(v) { v == null ? this.removeAttribute("variant") : this.setAttribute("variant", v); }

  get size() { return this.getAttribute("size") || "md"; }
  set size(v) { v == null ? this.removeAttribute("size") : this.setAttribute("size", v); }

  get type() { return this.getAttribute("type") || "button"; }
  set type(v) { v == null ? this.removeAttribute("type") : this.setAttribute("type", v); }

  get href() { return this.getAttribute("href"); }
  set href(v) { v == null ? this.removeAttribute("href") : this.setAttribute("href", v); }

  get icon() { return this.getAttribute("icon"); }
  set icon(v) { v == null ? this.removeAttribute("icon") : this.setAttribute("icon", v); }

  get status() { return this.getAttribute("status"); }
  set status(v) { v == null ? this.removeAttribute("status") : this.setAttribute("status", v); }

  get shape() { return this.getAttribute("shape") || "default"; }
  set shape(v) { v == null ? this.removeAttribute("shape") : this.setAttribute("shape", v); }

  get danger() { return this.hasAttribute("danger"); }
  set danger(v) { this._reflectBool("danger", v); }

  get disabled() { return this.hasAttribute("disabled"); }
  set disabled(v) { this._reflectBool("disabled", v); }

  get loading() { return this.hasAttribute("loading"); }
  set loading(v) { this._reflectBool("loading", v); }

  get block() { return this.hasAttribute("block"); }
  set block(v) { this._reflectBool("block", v); }

  get iconRight() { return this.hasAttribute("icon-right"); }
  set iconRight(v) { this._reflectBool("icon-right", v); }

  _reflectBool(attr, v) { if (v) this.setAttribute(attr, ""); else this.removeAttribute(attr); }

  attributeChangedCallback() {
    if (this._moving) return;
    // Any observed attribute changing re-syncs the inner element (cheap; the
    // element is built once and only its classes/attrs are toggled). `href`
    // changing between set/unset can change WHICH element we need — rebuild then.
    if (!this._wrapped) return;
    const needAnchor = this.href != null;
    const isAnchor = this._el && this._el.tagName === "A";
    if (needAnchor !== isAnchor) this._rebuild();
    else this._sync();
  }

  // _label(key, …args) → localised string: this.labels override, else the default.
  _label(key, ...a) { const v = (this.labels && this.labels[key]) ?? LABELS[key]; return typeof v === "function" ? v(...a) : v; }

  connectedCallback() {
    // Defer the wrap to a microtask so children appended right AFTER a
    // programmatic `document.body.append(btn); btn.append("Save")` are captured
    // too — not only the parse-time declarative children present at connect.
    // Guarded (_wrapped) so it still runs exactly once across reconnects.
    if (this._wrapped) return;
    if (this._scheduled) return;
    this._scheduled = true;
    queueMicrotask(() => { this._scheduled = false; this._wrap(); });
  }

  // Create the inner element ONCE and MOVE the author's children into it as the
  // label. Guarded so it runs exactly once across disconnect/reconnect — moving
  // children again would be a no-op at best, re-wrapping the wrapper at worst.
  _wrap() {
    if (this._wrapped) return;
    this._wrapped = true;

    // A span holds the moved author children so we can freely re-order the icon /
    // spinner around it without disturbing the label content itself.
    const labelHost = document.createElement("span");
    labelHost.className = "puredashboard-button__label js-puredashboard-button__label";
    while (this.firstChild) labelHost.appendChild(this.firstChild);
    this._labelHost = labelHost;

    this._buildInner();
  }

  // Build (or rebuild) the inner <button>/<a>, keeping the SAME label host node
  // (author children never move again). Called on first wrap and whenever `href`
  // toggles the element kind.
  _buildInner() {
    const nav = this.href != null;
    const el = document.createElement(nav ? "a" : "button");
    el.className = "puredashboard-button__el js-puredashboard-button__el";

    // Icon (leading by default) + label. Order is fixed up in _sync via iconRight.
    // The icon markup is author-TRUSTED (documented, like menu.js) — a constant or
    // author-supplied SVG string, never untrusted user data.
    el.appendChild(this._labelHost);

    // Suppress activation while disabled/loading. On a <button> the native
    // `disabled` already blocks clicks; on an <a> (which has no `disabled`) we set
    // aria-disabled and preventDefault navigation here. Capturing so we win before
    // the click bubbles out of the host.
    el.addEventListener("click", this._onClick, true);

    if (this._el && this._el.parentNode === this) this.replaceChild(el, this._el);
    else this.appendChild(el);
    this._el = el;
    this._sync();
  }

  _rebuild() { this._buildInner(); }

  _onClick = (e) => {
    if (this.disabled || this.loading) { e.preventDefault(); e.stopImmediatePropagation(); }
  };

  // Keep every mutable aspect of the inner element in sync with the host's state.
  // Runs after build and on any observed-attribute change.
  _sync() {
    const el = this._el;
    if (!el) return;
    const nav = el.tagName === "A";

    // Variant + size modifier classes (BEM). `default`/`md` are the base — no class.
    const variant = VARIANTS.has(this.variant) ? this.variant : "default";
    const size = SIZES.has(this.size) ? this.size : "md";
    const cls = ["puredashboard-button__el", "js-puredashboard-button__el"];
    if (variant !== "default") cls.push(`puredashboard-button__el--${variant}`);
    if (size !== "md") cls.push(`puredashboard-button__el--${size}`);
    // status intent color (success/warning/danger). `danger` boolean is the
    // legacy shorthand for status="danger"; keep emitting `--danger` for back-compat.
    const status = STATUSES.has(this.status) ? this.status : (this.danger ? "danger" : null);
    if (status) {
      cls.push("puredashboard-button__el--status");
      cls.push(status === "danger" ? "puredashboard-button__el--danger" : `puredashboard-button__el--status-${status}`);
    }
    const shape = SHAPES.has(this.shape) ? this.shape : "default";
    if (shape !== "default") cls.push(`puredashboard-button__el--${shape}`);
    if (this.block) cls.push("puredashboard-button__el--block");
    if (this.loading) cls.push("puredashboard-button__el--loading");
    // Icon-only (an icon and no label content, whitespace aside): the empty label slot would still be a flex item, and the
    // gap before it pushed the icon off-centre by half a gap — visible in a circle button. The modifier hides the slot.
    const label = this._labelHost;
    if (this.icon && label && !label.children.length && !label.textContent.trim()) cls.push("puredashboard-button__el--icon-only");
    el.className = cls.join(" ");

    // Host-level block modifier (so the host itself can fill its container).
    this.classList.toggle("puredashboard-button--block", this.block);

    // disabled / loading → block interaction + a11y state.
    const blocked = this.disabled || this.loading;
    if (nav) {
      // Links have no native `disabled`; use aria-disabled (+ the click guard).
      el.href = this.href ?? "";
      if (blocked) { el.setAttribute("aria-disabled", "true"); el.removeAttribute("href"); }
      else el.removeAttribute("aria-disabled");
      el.removeAttribute("type");
      el.removeAttribute("disabled");
    } else {
      el.disabled = blocked;
      // type=submit lives on the inner <button> so it submits its <form> natively.
      el.setAttribute("type", this.type || "button");
      el.removeAttribute("aria-disabled");
      el.removeAttribute("href");
    }

    // Accessible name. An `aria-label` / `aria-labelledby` set on the HOST is mirrored
    // onto the inner element — the host carries no role, so a name there is ignored by
    // assistive tech while the focusable <button>/<a> would stay nameless. This is what
    // makes an ICON-ONLY button (kebab ⋯ / hamburger ☰ menu triggers) announce itself.
    const raw = this.getAttribute("aria-label");
    const name = raw === this._ariaOwn ? null : raw;      // ignore the fallback WE wrote
    const namedBy = this.getAttribute("aria-labelledby");
    if (name) el.setAttribute("aria-label", name);
    else if (this.loading) el.setAttribute("aria-label", this._label("loading"));
    else el.removeAttribute("aria-label");
    if (namedBy) el.setAttribute("aria-labelledby", namedBy); else el.removeAttribute("aria-labelledby");

    // role / aria-checked / aria-pressed: MOVED from the host to the inner element, so a switch/toggle button is one node in
    // the accessibility tree (a role on the host would nest a second one around the native button). The value lives in
    // this._fwd; setting the attribute on the host again updates it, an empty string clears it.
    for (const a of FORWARDED) {
      const v = this.getAttribute(a);
      if (v !== null) {
        if (v === "") delete this._fwd[a]; else this._fwd[a] = v;
        this._moving = true;
        try { this.removeAttribute(a); } finally { this._moving = false; }
      }
      if (this._fwd[a] != null) el.setAttribute(a, this._fwd[a]); else el.removeAttribute(a);
    }

    // aria-busy while loading (announced on the host). The loading fallback name is
    // only OURS to manage — an author-supplied aria-label is never clobbered.
    if (this.loading) {
      this.setAttribute("aria-busy", "true");
      // Writing an OBSERVED attribute re-enters attributeChangedCallback even when the
      // value is unchanged — so only write when it actually differs, or this recurses.
      if (!name) {
        this._ariaOwn = this._label("loading");
        if (raw !== this._ariaOwn) this.setAttribute("aria-label", this._ariaOwn);
      }
    } else {
      this.removeAttribute("aria-busy");
      // Only OUR fallback is removed — an author name set meanwhile stays put.
      if (raw != null && raw === this._ariaOwn) { this.removeAttribute("aria-label"); this._ariaOwn = null; }
    }

    this._syncGlyphs();
  }

  // (Re)place the leading/trailing glyphs — the loading spinner and/or the author
  // icon — around the label host, honouring iconRight. Rebuilt each sync so the
  // markup source stays a single constant (idempotent, no stale nodes).
  _syncGlyphs() {
    const el = this._el;
    // Drop any previously inserted glyphs (keep only the label host).
    for (const g of [...el.querySelectorAll(".js-puredashboard-button__spinner, .js-puredashboard-button__icon")]) g.remove();

    // The visible glyph while loading is the spinner (it REPLACES the leading
    // icon slot); otherwise, the author icon if provided.
    const makeSpinner = () => {
      const s = document.createElement("span");
      s.className = "puredashboard-button__spinner js-puredashboard-button__spinner";
      s.setAttribute("aria-hidden", "true");
      s.innerHTML = SPINNER_SVG;            // trusted constant SVG (never user data)
      return s;
    };
    const makeIcon = () => {
      if (!this.icon) return null;
      const s = document.createElement("span");
      s.className = "puredashboard-button__icon js-puredashboard-button__icon";
      s.setAttribute("aria-hidden", "true");
      s.innerHTML = this.icon;              // author-trusted SVG markup (like menu.js)
      return s;
    };

    const label = this._labelHost;
    const leading = [];
    const trailing = [];
    if (this.loading) leading.push(makeSpinner());
    const iconNode = makeIcon();
    if (iconNode) (this.iconRight ? trailing : leading).push(iconNode);

    for (const g of leading) el.insertBefore(g, label);
    for (const g of trailing) el.appendChild(g);
  }

  focus() { this._el?.focus(); }

  // Expose the inner element for callers that need the real button/link.
  get inner() { return this._el; }
}

customElements.define("puredashboard-button", PuredashboardButton);

export { PuredashboardButton };
