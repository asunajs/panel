// <puredashboard-popconfirm> — a confirm bubble anchored to a trigger.
// Zero-dep, no build, CSP-safe. Extends plain HTMLElement (NOT Reactive) — a
// Reactive render() would blow away the author's light-DOM trigger, and this
// component's whole job is to PRESERVE that trigger and wire a confirm panel to
// it (same wrap-children-once pattern as popover.js / button.js).
//
// It is a SPECIALISED popover: where popover.js promotes an author-supplied
// content node to the top layer, popconfirm BUILDS a fixed body — a warning
// icon, a title (the question), an optional description, and Cancel/OK buttons —
// and shows THAT in the top layer. It reuses popover.js's exact top-layer +
// fallback strategy: the native Popover API (`popover="auto"`) paints in the TOP
// LAYER and grants light-dismiss (click-outside) + Esc for free; where the API is
// missing (older engines, jsdom) we fall back to a fixed, high-z element and
// replicate outside-click + Esc + focus-return by hand (the same feature-detect
// menu.js uses). Focus moves into the panel on open and returns to the trigger on
// close.
//
// Author markup: a single trigger element as the child.
//
//   <puredashboard-popconfirm title="Delete this row?" ok-danger>
//     <button>Delete</button>
//   </puredashboard-popconfirm>
//
// The consumer performs the destructive action — popconfirm never does. Listen
// for "confirm" (OK) and "cancel" (Cancel / Esc / outside-click).

// All FIXED user-facing strings live here (English defaults). Override any subset
// via the `labels` property — e.g. pc.labels = { ok: "Đồng ý", cancel: "Huỷ" }.
// The `title`/`description` are CONTENT (properties), not fixed strings — only the
// two button labels are localised here. Function-valued keys interpolate.
const LABELS = {
  ok: "OK",
  cancel: "Cancel",
};

let uid = 0;

// The supported placements — where the panel opens relative to the trigger.
const PLACEMENTS = new Set(["top", "bottom", "left", "right"]);

// The inline warning glyph — a self-contained SVG (no shared icon module). This
// is TRUSTED constant markup (like menu.js icons / button.js spinner), never user
// data, so it may go through innerHTML. aria-hidden: the title carries meaning.
const WARN_SVG =
  '<svg class="puredashboard-popconfirm__icon-svg" viewBox="0 0 24 24" width="1em" height="1em" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" style="display:block;overflow:visible" aria-hidden="true" focusable="false">' +
  '<path d="M10.29 3.86 1.82 18a2 2 0 0 0 1.71 3h16.94a2 2 0 0 0 1.71-3L13.71 3.86a2 2 0 0 0-3.42 0z"></path>' +
  '<path d="M12 9v4"></path><path d="M12 17h.01"></path>' +
  "</svg>";

/**
 * A confirm bubble anchored to a trigger: a specialised popover with a fixed
 * confirm body. It keeps the author's trigger as an ordinary light-DOM child and
 * builds a panel — a warning icon, the `title` question, an optional
 * `description`, and Cancel/OK buttons — shown in the TOP LAYER. Clicking the
 * trigger opens it; Escape and outside-click dismiss it; focus moves into the
 * panel on open and returns to the trigger on close.
 *
 * Uses the native Popover API (`popover="auto"`) for top-layer painting and
 * light-dismiss, with a fixed/high-z fallback where the API is unavailable
 * (mirrors `popover.js` / `menu.js`). It is NOT form-associated — it is a
 * presentational confirmation container, not a value-bearing control.
 *
 * The component NEVER performs the destructive action itself — the consumer
 * listens for `confirm` and does the work. Configure via JS properties or
 * declarative attributes.
 *
 * @element puredashboard-popconfirm
 *
 * @prop {string}  title       - The confirm question (rendered as text — never HTML). Default `""`.
 * @prop {string}  description - Optional secondary line under the title (text — never HTML). Default `""`.
 * @prop {string}  placement   - Where the panel opens: `"top"` (default) | `"bottom"` | `"left"` | `"right"`.
 * @prop {boolean} okDanger    - Style the OK button as destructive (red). Default `false`.
 * @prop {boolean} disabled    - Disable the trigger (clicking it does nothing). Default `false`.
 * @prop {boolean} open        - Whether the panel is open (get/set). Reflected to the `open` attribute.
 * @prop {Object}  labels      - Override UI strings. Keys: `ok`, `cancel`. Unset keys keep the English default.
 *
 * @attr {string}  title       - Declarative form of `title`.
 * @attr {string}  description - Declarative form of `description`.
 * @attr {string}  placement   - Declarative form of `placement`.
 * @attr {boolean} ok-danger   - Declarative form of `okDanger`.
 * @attr {boolean} disabled    - Declarative form of `disabled`.
 * @attr {boolean} open        - Declarative form of `open`.
 *
 * @fires confirm - `CustomEvent` (bubbles) when OK is clicked; the panel then closes. `detail = {}`.
 * @fires cancel  - `CustomEvent` (bubbles) when Cancel is clicked, or on Esc / outside-click. `detail = {}`.
 * @fires open    - `CustomEvent` (bubbles) after the panel opens. `detail = {}`.
 * @fires close   - `CustomEvent` (bubbles) after the panel closes. `detail = {}`.
 *
 * @method show   - `show() => void` — open the panel programmatically.
 * @method hide   - `hide() => void` — close the panel and return focus to the trigger.
 * @method toggle - `toggle() => void` — open if closed, close if open.
 *
 * @cssprop [--pd-popconfirm-padding] - Panel padding (defaults to `--sp-3`).
 * @cssprop [--pd-popconfirm-gap]     - Gap between icon and text (defaults to `--sp-2`).
 *
 * @example
 * // <puredashboard-popconfirm title="Delete this row?" ok-danger>
 * //   <button>Delete</button>
 * // </puredashboard-popconfirm>
 * const pc = document.querySelector("puredashboard-popconfirm");
 * pc.addEventListener("confirm", () => actuallyDelete());
 * pc.addEventListener("cancel", () => {});
 */
class PuredashboardPopconfirm extends HTMLElement {
  static get observedAttributes() {
    return ["title", "description", "placement", "ok-danger", "disabled", "open"];
  }

  constructor() {
    super();
    this._wired = false;
    this._trigger = null;   // the author's trigger (preserved)
    this._panel = null;     // the built confirm body
    this._titleEl = null;
    this._descEl = null;
    this._okBtn = null;
    this._cancelBtn = null;
    this._open = false;
    this._placement = null;
    // Feature-detect the native Popover API once (set at wire time on the built
    // panel); the rest of the module branches on this flag, exactly like
    // popover.js / menu.js.
    this._usePopover = false;
    // A template engine may set properties before upgrade, leaving a plain
    // own-property that shadows the prototype accessor. Reconcile so the setters
    // run — same pattern as popover.js / button.js.
    for (const p of ["title", "description", "placement", "okDanger", "disabled", "open", "labels"]) {
      this._upgrade(p);
    }
  }

  _upgrade(p) {
    if (Object.prototype.hasOwnProperty.call(this, p)) { const v = this[p]; delete this[p]; this[p] = v; }
  }

  attributeChangedCallback(name, _old, val) {
    if (name === "placement") {
      this._placement = PLACEMENTS.has(val) ? val : "top";
      if (this._panel) this._panel.setAttribute("data-placement", this._placement);
      this._reposition();
    } else if (name === "open" && this.isConnected) {
      if ((val !== null) !== this._open) this._setOpen(val !== null);
    } else if (this._wired) {
      // title / description / ok-danger / disabled all just re-sync the panel.
      this._sync();
    }
  }

  // _label(key, …args) → localised string: this.labels override, else the default.
  _label(key, ...a) { const v = (this.labels && this.labels[key]) ?? LABELS[key]; return typeof v === "function" ? v(...a) : v; }

  // ---- reflected properties --------------------------------------------------
  get title() { return this.getAttribute("title") || ""; }
  set title(v) { v == null ? this.removeAttribute("title") : this.setAttribute("title", v); }

  get description() { return this.getAttribute("description") || ""; }
  set description(v) { v == null ? this.removeAttribute("description") : this.setAttribute("description", v); }

  get placement() { return this._placement || "top"; }
  set placement(v) {
    this._placement = PLACEMENTS.has(v) ? v : "top";
    if (this._placement === v) this.setAttribute("placement", v);
    if (this._panel) this._panel.setAttribute("data-placement", this._placement);
    this._reposition();
  }

  get okDanger() { return this.hasAttribute("ok-danger"); }
  set okDanger(v) { this._reflectBool("ok-danger", v); }

  get disabled() { return this.hasAttribute("disabled"); }
  set disabled(v) { this._reflectBool("disabled", v); }

  get open() { return this._open; }
  set open(v) { this._setOpen(!!v); }

  _reflectBool(attr, v) { if (v) this.setAttribute(attr, ""); else this.removeAttribute(attr); }

  connectedCallback() {
    this._wire();
    // Honour a declarative open="" once wired.
    if (this.hasAttribute("open")) this._setOpen(true);
    // Re-anchor an already-open panel after a RELOCATION — see the same block in popover.js
    // for the full reasoning. Short version: re-parenting is a remove plus an insert, so a
    // keyed list moving this row re-runs connectedCallback; the line above short-circuits in
    // _setOpen because the state already says open; and closing instead would run
    // _returnFocus() (here also _focusPanel() on the way back in), taking focus off whatever
    // the user was using and emitting a "close" nobody asked for.
    if (this._open) { this._showPanel(); this._reposition(); }
  }

  disconnectedCallback() {
    this._removeGlobals();
    if (this._trigger) this._trigger.removeEventListener("click", this._onTriggerClick);
  }

  // Identify the trigger, keep it in place, BUILD the confirm panel, promote it to
  // the top layer (or mark it as the fallback), and wire ARIA + the trigger click.
  // Guarded so it runs exactly once even across disconnect/reconnect.
  _wire() {
    if (this._wired) return;
    this._wired = true;

    if (!this._placement) this._placement = PLACEMENTS.has(this.getAttribute("placement")) ? this.getAttribute("placement") : "top";

    // Trigger: the first element child (the author's button/link).
    this._trigger = this.firstElementChild;
    if (!this._trigger) return;   // nothing to wire

    this._buildPanel();

    // Promote the panel to the top layer where the API exists; otherwise mark it
    // as the fallback (a plain fixed element toggled via [data-open]).
    this._usePopover = typeof this._panel.showPopover === "function";
    if (this._usePopover) {
      this._panel.setAttribute("popover", "auto");
      // The browser fires "toggle" on popover state changes (incl. light-dismiss
      // / Esc); keep our state + ARIA in sync when the platform closes it.
      this._panel.addEventListener("toggle", this._onToggle);
    } else {
      this._panel.classList.add("puredashboard-popconfirm__panel--fallback");
    }

    // Wire the trigger: it controls a dialog-like panel.
    this._trigger.setAttribute("aria-haspopup", "dialog");
    this._trigger.setAttribute("aria-expanded", "false");
    this._trigger.setAttribute("aria-controls", this._panel.id);
    this._trigger.addEventListener("click", this._onTriggerClick);

    this._sync();
  }

  // Build the confirm body ONCE: warning icon + title + optional description +
  // Cancel/OK buttons. Title/description are set via textContent (never innerHTML)
  // so untrusted content can't inject markup; the warning icon is trusted constant
  // SVG. Appended to the host so it lives in the light DOM until the top layer
  // takes over on open.
  _buildPanel() {
    const panel = document.createElement("div");
    panel.className = "puredashboard-popconfirm__panel js-puredashboard-popconfirm__panel";
    panel.id = `js-puredashboard-popconfirm__panel-${++uid}`;
    panel.setAttribute("data-placement", this._placement);
    // alertdialog for destructive confirmations, dialog otherwise (resolved in _sync).
    panel.setAttribute("role", "dialog");

    const body = document.createElement("div");
    body.className = "puredashboard-popconfirm__body";

    const icon = document.createElement("span");
    icon.className = "puredashboard-popconfirm__icon";
    icon.setAttribute("aria-hidden", "true");
    icon.innerHTML = WARN_SVG;   // trusted constant SVG (never user data)

    const text = document.createElement("div");
    text.className = "puredashboard-popconfirm__text";

    const titleEl = document.createElement("div");
    titleEl.className = "puredashboard-popconfirm__title";
    titleEl.id = `${panel.id}__title`;

    const descEl = document.createElement("div");
    descEl.className = "puredashboard-popconfirm__desc";
    descEl.id = `${panel.id}__desc`;

    text.appendChild(titleEl);
    text.appendChild(descEl);
    body.appendChild(icon);
    body.appendChild(text);

    const actions = document.createElement("div");
    actions.className = "puredashboard-popconfirm__actions";

    const cancelBtn = document.createElement("button");
    cancelBtn.type = "button";
    cancelBtn.className = "puredashboard-popconfirm__btn puredashboard-popconfirm__btn--cancel js-puredashboard-popconfirm__cancel";
    cancelBtn.addEventListener("click", this._onCancel);

    const okBtn = document.createElement("button");
    okBtn.type = "button";
    okBtn.className = "puredashboard-popconfirm__btn puredashboard-popconfirm__btn--ok js-puredashboard-popconfirm__ok";
    okBtn.addEventListener("click", this._onConfirm);

    actions.appendChild(cancelBtn);
    actions.appendChild(okBtn);

    panel.appendChild(body);
    panel.appendChild(actions);

    this._panel = panel;
    this._titleEl = titleEl;
    this._descEl = descEl;
    this._cancelBtn = cancelBtn;
    this._okBtn = okBtn;

    this.appendChild(panel);
  }

  // Keep the panel's mutable aspects in sync with the host state: title/description
  // text, button labels, danger styling, role, and the trigger's aria-controls. All
  // text via textContent (never innerHTML) — untrusted title/description stays inert.
  _sync() {
    if (!this._panel) return;

    this._titleEl.textContent = this.title || "";
    const desc = this.description || "";
    this._descEl.textContent = desc;
    this._descEl.hidden = !desc;

    this._cancelBtn.textContent = this._label("cancel");
    this._okBtn.textContent = this._label("ok");

    // Destructive confirmations get the danger button + the stronger alertdialog
    // role so assistive tech announces the interruption.
    this._okBtn.classList.toggle("puredashboard-popconfirm__btn--danger", this.okDanger);
    this._panel.setAttribute("role", this.okDanger ? "alertdialog" : "dialog");

    // Accessible name: point at the title node (labelledby), plus the description.
    this._panel.setAttribute("aria-labelledby", this._titleEl.id);
    if (desc) this._panel.setAttribute("aria-describedby", this._descEl.id);
    else this._panel.removeAttribute("aria-describedby");

    this._panel.setAttribute("data-placement", this._placement || "top");
  }

  _onTriggerClick = (e) => {
    e.preventDefault();
    if (this.disabled) return;
    this.toggle();
  };

  // Clicking OK: emit "confirm", then close. The consumer performs the action —
  // popconfirm never does. Closing after the emit keeps focus-return sane.
  _onConfirm = (e) => {
    e.preventDefault();
    this.emit("confirm");
    this._setOpen(false);
  };

  // Clicking Cancel: emit "cancel", then close. Esc / outside-click route here too.
  _onCancel = (e) => {
    if (e) e.preventDefault();
    this.emit("cancel");
    this._setOpen(false);
  };

  // The platform "toggle" event tells us the popover's real state — reconcile so
  // native light-dismiss / Esc flip our state + ARIA. When the platform closes it
  // WITHOUT going through our Cancel button (outside-click / Esc), emit "cancel".
  _onToggle = (e) => {
    const nowOpen = e.newState === "open";
    if (nowOpen === this._open) return;
    this._open = nowOpen;
    this._reflectOpen();
    if (nowOpen) { this._reposition(); this._focusPanel(); this.emit("open"); }
    else {
      // If we didn't already emit an outcome for this close (button path sets the
      // flag), the platform dismissed it → treat as cancel.
      if (!this._settled) this.emit("cancel");
      this._settled = false;
      this.emit("close");
      this._returnFocus();
    }
  };

  // ---- open/close ------------------------------------------------------------
  show() { this._setOpen(true); }
  hide() { this._setOpen(false); }
  toggle() { this._setOpen(!this._open); }

  _setOpen(next) {
    if (!this._wired) this._wire();
    if (!this._trigger || !this._panel) return;
    if (next === this._open) return;

    // Mark that this close carries an explicit outcome (confirm/cancel already
    // emitted by the button handler) so _onToggle doesn't double-emit "cancel".
    if (!next) this._settled = true;

    this._open = next;
    this._reflectOpen();

    if (next) {
      this._settled = false;
      // Re-sync labels/title/description just before showing: a template engine or
      // consumer may have set `labels` (a plain property, not an observed attribute)
      // after wiring, so the built panel could hold stale defaults.
      this._sync();
      this._showPanel();
      this._reposition();
      this._focusPanel();
      this._addGlobals();
      this.emit("open");
    } else {
      this._hidePanel();
      this._removeGlobals();
      this.emit("close");
      this._returnFocus();
      this._settled = false;
    }
  }

  // Reflect state onto the host attribute + the trigger's aria-expanded.
  _reflectOpen() {
    if (this._open) { if (!this.hasAttribute("open")) this.setAttribute("open", ""); }
    else if (this.hasAttribute("open")) this.removeAttribute("open");
    if (this._trigger) this._trigger.setAttribute("aria-expanded", this._open ? "true" : "false");
  }

  _showPanel() {
    if (this._usePopover) {
      // Guard the API call: even where showPopover exists it can throw (already
      // open, disconnected). Fall through to the visual fallback marker.
      try { if (!this._panel.matches(":popover-open")) this._panel.showPopover(); }
      catch { this._panel.setAttribute("data-open", ""); }
    } else {
      this._panel.setAttribute("data-open", "");
    }
  }

  _hidePanel() {
    if (this._usePopover) {
      try { if (this._panel.matches(":popover-open")) this._panel.hidePopover(); } catch { /* */ }
    }
    this._panel.removeAttribute("data-open");
  }

  // Move focus into the panel on open — default to Cancel (the safe choice for a
  // destructive confirm), falling back to OK. Guarded for jsdom where focus is a
  // no-op but still callable.
  _focusPanel() {
    const target = this._cancelBtn || this._okBtn;
    if (target && typeof target.focus === "function") { try { target.focus(); } catch { /* */ } }
  }

  // Return focus to the trigger when the panel closes, so keyboard users aren't
  // stranded on a now-hidden element.
  _returnFocus() {
    if (this._trigger && typeof this._trigger.focus === "function") { try { this._trigger.focus(); } catch { /* */ } }
  }

  // ---- fallback dismiss (Esc + outside-click) --------------------------------
  // The native popover=auto handles these for free; we only add manual handlers in
  // the fallback path (jsdom / older engines).
  _addGlobals() {
    if (this._usePopover) return;
    document.addEventListener("keydown", this._onKey, true);
    document.addEventListener("pointerdown", this._onOutside, true);
  }
  _removeGlobals() {
    document.removeEventListener("keydown", this._onKey, true);
    document.removeEventListener("pointerdown", this._onOutside, true);
  }
  _onKey = (e) => { if (e.key === "Escape" && this._open) { e.preventDefault(); this._onCancel(); } };
  _onOutside = (e) => {
    if (!this._open) return;
    const t = e.target;
    if (this._panel && this._panel.contains(t)) return;
    if (this._trigger && this._trigger.contains(t)) return;
    this._onCancel();
  };

  // ---- positioning -----------------------------------------------------------
  // Anchor the panel to the trigger per `placement`, computed from
  // getBoundingClientRect on open, flipping within the viewport best-effort.
  // Guarded for jsdom, where layout metrics are 0 and window sizes are absent.
  _reposition() {
    if (!this._open || !this._trigger || !this._panel) return;
    let r;
    try { r = this._trigger.getBoundingClientRect(); } catch { return; }
    if (!r || (r.width === 0 && r.height === 0 && r.top === 0 && r.left === 0)) {
      // jsdom / not laid out — nothing sensible to compute; leave it to CSS.
      return;
    }
    const gap = 8;
    const vw = window.innerWidth || 0;
    const vh = window.innerHeight || 0;
    const pw = this._panel.offsetWidth || 240;
    const ph = this._panel.offsetHeight || 120;
    const side = this.placement;

    let top, left;
    if (side === "top" || side === "bottom") {
      top = side === "bottom" ? r.bottom + gap : r.top - gap - ph;
      if (vh) {
        if (side === "bottom" && top + ph > vh && r.top - gap - ph > 0) top = r.top - gap - ph;
        else if (side === "top" && top < 0 && r.bottom + gap + ph < vh) top = r.bottom + gap;
      }
      left = r.left + (r.width - pw) / 2;
    } else {   // left / right
      left = side === "right" ? r.right + gap : r.left - gap - pw;
      if (vw) {
        if (side === "right" && left + pw > vw && r.left - gap - pw > 0) left = r.left - gap - pw;
        else if (side === "left" && left < 0 && r.right + gap + pw < vw) left = r.right + gap;
      }
      top = r.top + (r.height - ph) / 2;
    }

    if (vw) left = Math.max(8, Math.min(left, vw - pw - 8));
    if (vh) top = Math.max(8, Math.min(top, vh - ph - 8));

    this._panel.style.top = top + "px";
    this._panel.style.left = left + "px";
  }

  // ---- events ----------------------------------------------------------------
  emit(name, detail = {}) {
    this.dispatchEvent(new CustomEvent(name, { bubbles: true, detail }));
  }
}

customElements.define("puredashboard-popconfirm", PuredashboardPopconfirm);

export { PuredashboardPopconfirm };
