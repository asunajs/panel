// menu.js — anchored dropdown / action menu. Zero-dep, no build, CSP-safe. Same
// imperative family as dialog.js / toast.js. Theme via the .puredashboard-menu-* classes.
//
// Uses popover="auto" so the menu lives in the TOP LAYER (above page content,
// escapes overflow:hidden ancestors) and gets light-dismiss (click-outside) + Esc
// for free. Falls back to a fixed, high-z-index element where the Popover API is
// unavailable. Navigation items are real <a href> (open-in-new-tab etc. keep
// working); action items are <button>s.
//
//   menu(anchorEl, [
//     { label: "Open",   href: "#/nodes/web" },              // a real link
//     { label: "Rename", icon: ICON_PENCIL, value: "rename", shortcut: "F2" },
//     { separator: true },
//     { group: "View", items: [                              // a labelled group
//       { label: "Show sidebar", checked: true, onSelect: (it) => {} },   // checkbox item
//     ] },
//     { group: "Sort by", radio: "name", onSelect: (v) => {}, items: [    // radio group
//       { label: "Name", value: "name" }, { label: "Date", value: "date" },
//     ] },
//     { label: "Share", items: [ { label: "Copy link", value: "copy" } ] }, // submenu
//     { label: "Delete", value: "delete", danger: true },
//   ]).then((picked) => { /* picked = "rename" | "delete" | … | null (dismissed) */ });
//
// item:      { label, value?, href?, target?, icon?, shortcut?, danger?, disabled?,
//              checked?, closeOnSelect?, onSelect?, items?, separator? }
//   icon:    optional inline SVG markup string or a DOM node — kept dependency-free so
//            this module stands alone. TRUSTED author config (inserted as markup).
//   checked: a boolean turns the item into a CHECKBOX item (role=menuitemcheckbox).
//   items:   a nested array turns the item into a SUBMENU trigger.
// group:     { group: "Label", items: [...] }                  — a labelled group
//            { group: "Label", radio: <value>, onSelect, items } — a radio group
//                (items become role=menuitemradio; the one whose `value` === `radio` is checked)
// opts:      { placement?, className?, labels?, hoverDelay?, finalFocus? }

/**
 * Open an anchored dropdown / action menu in the top layer; resolve with the chosen
 * value. Imperative (same family as dialog.js / toast.js) — not a custom element.
 *
 * Supports plain action items, real `<a href>` link items, icons + keyboard-shortcut
 * hints, separators, labelled groups, checkbox items, radio groups, and nested
 * submenus. Full APG menu keyboard map: Arrow keys, Home/End, Enter/Space, typeahead,
 * ArrowRight/ArrowLeft to enter/leave a submenu, Esc to close.
 *
 * @function menu
 * @param {HTMLElement} anchor - Element to position under (usually the trigger button). Gets `aria-haspopup`/`aria-expanded` while open, and focus back on close.
 * @param {Array<Object>} items - Items and groups. Item: `{ label, value?, href?, target?, icon?, shortcut?, danger?, disabled?, checked?, closeOnSelect?, onSelect?, items?, separator? }`. Group: `{ group: label, items, radio?, onSelect? }`.
 * @param {Object} [opts] - `{ placement?: "bottom-start"|"bottom-end"|"top-start"|"top-end"|"right-start"|"left-start", className?: string, labels?: Object, hoverDelay?: number, finalFocus?: boolean, onEdgeNav?: (dir: -1|1) => boolean }`. `onEdgeNav` is called when Arrow{Left,Right} at the root level has no submenu to open/close — `<puredashboard-menubar>` uses it to walk to the adjacent menu.
 * @returns {Promise<*>} Resolves with the picked item's `value`, or `null` if dismissed (Esc / click-outside / a press on the anchor). The promise also carries `.close(value?)` and `.el` (the root popup) so a caller can drive it (e.g. a menubar). Clicking the anchor of the OPEN menu closes it and does not re-open it: `menu()` called for that anchor from the same click's handler returns an already-resolved `null` (`.el` is `null`), so the usual `btn.onclick = () => menu(btn, …)` wiring toggles.
 *
 * @example
 * const picked = await menu(btn, [
 *   { label: "Rename", value: "rename", icon: ICON_PENCIL, shortcut: "F2" },
 *   { label: "Share", items: [{ label: "Copy link", value: "copy" }] },   // submenu
 *   { group: "Sort by", radio: "name", onSelect: (v) => sort(v), items: [
 *     { label: "Name", value: "name" }, { label: "Date", value: "date" } ] },
 *   { separator: true },
 *   { label: "Delete", value: "delete", danger: true },
 * ]);   // → "rename" | "copy" | "delete" | null
 */
const LABELS = {
  submenu: (label) => `${label} submenu`,
};

// Inlined icons (rule: each component carries its own SVG — no shared icon module).
// overflow:visible so strokes near the viewBox edge aren't clipped by the UA default.
const ICON_CHECK =
  '<svg viewBox="0 0 16 16" width="14" height="14" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" style="overflow:visible"><path d="M3 8.4 6.3 11.7 13 4.6"/></svg>';
const ICON_DOT =
  '<svg viewBox="0 0 16 16" width="14" height="14" fill="currentColor" style="overflow:visible"><circle cx="8" cy="8" r="3.2"/></svg>';
const ICON_CHEVRON =
  '<svg viewBox="0 0 16 16" width="14" height="14" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" style="overflow:visible"><path d="M6 3.5 10.5 8 6 12.5"/></svg>';

const B = "puredashboard-menu";        // BEM block = the component's class name

// Neutralize a script-executing scheme in an item's href so a URL sourced from data
// can't become click-to-XSS (relative/hash/mailto/tel/http(s) pass through). Leading
// control chars/whitespace are stripped since the browser ignores them (java\tscript:).
function safeUrl(u) {
  const scheme = String(u ?? "").replace(/[\x00-\x20]+/g, "").match(/^([a-z][a-z0-9+.-]*):/i);
  if (!scheme) return u;
  const s = scheme[1].toLowerCase();
  return (s === "javascript" || s === "vbscript" || s === "data") ? "#" : u;
}

// Text content (labels, shortcuts) — a string goes in via textContent (never parsed as
// markup, so a label sourced from data can't inject); a DOM node is appended as-is.
function spanText(cls, content) {
  const s = document.createElement("span");
  s.className = cls;
  if (content == null) return s;
  if (content.nodeType) s.appendChild(content);
  else s.textContent = String(content);
  return s;
}
// Icons are TRUSTED author config (inline SVG markup string or a DOM node) — same
// trust boundary as `raw()` elsewhere in the library. Never feed them untrusted data.
function spanIcon(cls, icon) {
  const s = document.createElement("span");
  s.className = cls;
  if (icon == null) return s;                      // empty = the reserved gutter slot
  if (typeof icon === "string") s.innerHTML = icon;
  else if (icon.nodeType) s.appendChild(icon);
  return s;
}

// Flatten a level's entries (groups included) so we can tell whether this level needs
// an icon gutter / an indicator gutter — every item then reserves the same leading
// slots and all labels line up, with or without an icon of their own.
function leafItems(entries) {
  const out = [];
  for (const e of entries || []) {
    if (!e || e.separator) continue;
    if (e.group !== undefined) out.push(...leafItems(e.items));
    else out.push(e);
  }
  return out;
}
const isCheckable = (it, radioGroup) => radioGroup != null || typeof it.checked === "boolean";

// Anchors whose open menu was just dismissed by a press ON the anchor. The usual trigger
// wiring is `button.onclick = () => menu(button, …)`: the pointerdown closes the open
// menu (light-dismiss — the anchor is outside the popup), then the SAME click's handler
// would open a fresh one, so the menu never closes from its own button. While an anchor
// is in this set, `menu()` for it resolves null at once instead of re-opening; the entry
// clears once the gesture's click has run (or the gesture is cancelled).
const reclick = new WeakSet();
function suppressReopen(anchor) {
  reclick.add(anchor);
  let timer = 0;
  const clear = () => {
    clearTimeout(timer);
    window.removeEventListener("click", clear);
    window.removeEventListener("pointercancel", clear);
    reclick.delete(anchor);
  };
  window.addEventListener("click", clear);
  window.addEventListener("pointercancel", clear);
  timer = setTimeout(clear, 1000);    // a press with no click (released off-screen)
}

export function menu(anchor, items, opts = {}) {
  // The click that dismissed this anchor's open menu must not open it again.
  if (anchor && reclick.has(anchor)) {
    const settled = Promise.resolve(null);
    settled.close = () => {};
    settled.el = null;
    return settled;
  }
  const labels = { ...LABELS, ...(opts.labels || {}) };
  const hoverDelay = opts.hoverDelay ?? 120;
  const levels = [];                 // open levels; [0] = root popup
  let closed = false, settle;
  const done = new Promise((res) => (settle = res));

  // ---------------------------------------------------------------- lifecycle
  function closeAll(value) {
    if (closed) return;
    closed = true;
    while (levels.length) dropLevel();
    document.removeEventListener("keydown", onKey, true);
    document.removeEventListener("pointerdown", onOutside, true);
    if (anchor) {
      anchor.removeEventListener("pointerdown", onAnchorDown, true);
      anchor.removeAttribute("aria-expanded");
      if (anchor.dataset && anchor.dataset.pdMenuHaspopup === "1") { anchor.removeAttribute("aria-haspopup"); delete anchor.dataset.pdMenuHaspopup; }
      if (opts.finalFocus !== false && anchor.isConnected && typeof anchor.focus === "function") anchor.focus();
    }
    settle(value ?? null);
  }
  function dropLevel() {
    const lv = levels.pop();
    if (!lv) return;
    if (lv.trigger) { lv.trigger.setAttribute("aria-expanded", "false"); lv.trigger.classList.remove(`${B}__item--open`); }
    try { if (lv.usePopover && lv.el.matches(":popover-open")) lv.el.hidePopover(); } catch { /* not supported */ }
    lv.el.remove();
  }
  // close every level deeper than `depth` (0 = keep the root only)
  function closeDeeperThan(depth) { while (levels.length > depth + 1) dropLevel(); }

  // ------------------------------------------------------------------ a level
  function openLevel(entries, { anchorEl, placement, trigger, label, focusFirst }) {
    const depth = levels.length;
    const m = document.createElement("div");
    m.className = B + (depth ? ` ${B}--submenu` : "") + (opts.className && !depth ? " " + opts.className : "");
    m.setAttribute("role", "menu");
    if (label) m.setAttribute("aria-label", label);
    Object.assign(m.style, { position: "fixed", margin: "0", inset: "auto" });
    const usePopover = typeof m.showPopover === "function";
    if (usePopover) m.setAttribute("popover", "auto");
    else m.style.zIndex = String(9998 + depth);

    const leaves = leafItems(entries);
    if (leaves.some((it) => it.icon)) m.classList.add(`${B}--icons`);
    if (entries.some((e) => e.group !== undefined && e.radio !== undefined) || leaves.some((it) => typeof it.checked === "boolean"))
      m.classList.add(`${B}--indicators`);

    const lv = { el: m, depth, trigger, usePopover, hoverTimer: 0 };
    entries.forEach((e) => m.appendChild(renderEntry(e, lv)));

    // A submenu is a DOM CHILD of its parent popup so the Popover API treats the two
    // as NESTED — light-dismiss then closes the child without closing the parent.
    (depth ? levels[depth - 1].el : document.body).appendChild(m);
    levels.push(lv);
    position(m, anchorEl, placement);
    try { if (usePopover) m.showPopover(); } catch { /* not supported */ }
    if (usePopover) position(m, anchorEl, placement);      // re-measure once laid out
    if (trigger) { trigger.setAttribute("aria-expanded", "true"); trigger.classList.add(`${B}__item--open`); }
    if (focusFirst) focusItem(lv, enabledItems(lv)[0]);
    return lv;
  }

  // ------------------------------------------------------------------- render
  function renderEntry(e, lv, radioGroup) {
    if (e && e.separator) {
      const hr = document.createElement("div");
      hr.className = `${B}__separator`;
      hr.setAttribute("role", "separator");
      return hr;
    }
    if (e && e.group !== undefined) return renderGroup(e, lv);
    return renderItem(e, lv, radioGroup);
  }

  function renderGroup(g, lv) {
    const box = document.createElement("div");
    box.className = `${B}__group`;
    box.setAttribute("role", "group");
    if (g.group) {
      const lbl = spanText(`${B}__group-label`, g.group);
      lbl.id = `${B}-g-${++renderGroup.n}`;
      box.setAttribute("aria-labelledby", lbl.id);
      box.appendChild(lbl);
    }
    (g.items || []).forEach((it) => box.appendChild(renderEntry(it, lv, g.radio !== undefined ? g : null)));
    return box;
  }
  renderGroup.n = 0;

  function renderItem(it, lv, radioGroup) {
    const sub = Array.isArray(it.items) && it.items.length > 0;
    const nav = !!it.href && !sub;
    const radio = !!radioGroup;
    const checkbox = !radio && typeof it.checked === "boolean";
    const el = document.createElement(nav ? "a" : "button");
    el.className = `${B}__item js-${B}__item` + (it.danger ? ` ${B}__item--danger` : "");
    el.setAttribute("role", radio ? "menuitemradio" : checkbox ? "menuitemcheckbox" : "menuitem");
    el.tabIndex = -1;
    if (nav) { el.href = safeUrl(it.href); if (it.target) el.target = it.target; }
    else el.type = "button";
    if (it.disabled) el.setAttribute("aria-disabled", "true");
    if (radio) el.setAttribute("aria-checked", String(radioGroup.radio === it.value));
    if (checkbox) el.setAttribute("aria-checked", String(!!it.checked));
    if (sub) { el.setAttribute("aria-haspopup", "menu"); el.setAttribute("aria-expanded", "false"); }

    // Leading gutters — rendered as PLACEHOLDERS when this item has none, so every
    // label in the level lines up whether or not it carries an icon / a checkmark.
    if (lv.el.classList.contains(`${B}--indicators`))
      el.appendChild(spanIcon(`${B}__indicator`, radio ? ICON_DOT : checkbox ? ICON_CHECK : null));
    if (lv.el.classList.contains(`${B}--icons`)) el.appendChild(spanIcon(`${B}__icon`, it.icon));
    el.appendChild(spanText(`${B}__label`, it.label));
    if (it.shortcut) el.appendChild(spanText(`${B}__shortcut`, it.shortcut));
    if (sub) el.appendChild(spanIcon(`${B}__chevron`, ICON_CHEVRON));
    el.__pdItem = it;                              // ArrowRight needs the item to open its submenu

    if (sub) {
      el.addEventListener("click", (ev) => {
        ev.preventDefault();
        if (it.disabled) return;
        if (el.getAttribute("aria-expanded") === "true") { closeDeeperThan(lv.depth); return; }
        openSubmenu(it, el, lv, false);
      });
      el.addEventListener("pointerenter", () => {
        clearTimeout(lv.hoverTimer);
        if (it.disabled) return;
        lv.hoverTimer = setTimeout(() => {
          if (el.getAttribute("aria-expanded") === "true") return;
          closeDeeperThan(lv.depth);
          openSubmenu(it, el, lv, false);
        }, hoverDelay);
      });
    } else {
      el.addEventListener("click", (ev) => {
        if (it.disabled) { ev.preventDefault(); return; }
        select(it, el, lv, radioGroup, ev);
      });
      // hovering a plain item dismisses any submenu opened from this level
      el.addEventListener("pointerenter", () => { clearTimeout(lv.hoverTimer); closeDeeperThan(lv.depth); });
    }
    el.addEventListener("pointerleave", () => clearTimeout(lv.hoverTimer));
    return el;
  }

  // -------------------------------------------------------------- interaction
  function openSubmenu(it, triggerEl, lv, focusFirst) {
    closeDeeperThan(lv.depth);
    return openLevel(it.items, {
      anchorEl: triggerEl,
      placement: "right-start",
      trigger: triggerEl,
      label: typeof it.label === "string" ? labels.submenu(it.label) : undefined,
      focusFirst,
    });
  }

  function select(it, el, lv, radioGroup, ev) {
    const radio = !!radioGroup, checkbox = !radio && typeof it.checked === "boolean";
    if (radio) {
      radioGroup.radio = it.value;
      const box = el.closest(`.${B}__group`) || lv.el;          // only this group's items
      box.querySelectorAll('[role="menuitemradio"]').forEach((n) => n.setAttribute("aria-checked", "false"));
      el.setAttribute("aria-checked", "true");
      if (radioGroup.onSelect) radioGroup.onSelect(it.value, it);
    } else if (checkbox) {
      it.checked = !it.checked;
      el.setAttribute("aria-checked", String(it.checked));
    }
    if (it.onSelect) it.onSelect(it, radio ? it.value : checkbox ? it.checked : undefined);
    // checkbox / radio items keep the menu open by default (you usually toggle several);
    // plain actions and links close it. `closeOnSelect` overrides per item.
    const close = it.closeOnSelect ?? !(radio || checkbox);
    if (!close) return;
    if (it.href) { closeAll(it.value ?? null); return; }   // let the link navigate
    ev.preventDefault();
    closeAll(it.value ?? null);
  }

  const enabledItems = (lv) => [...lv.el.querySelectorAll(`.js-${B}__item:not([aria-disabled="true"])`)]
    .filter((n) => n.closest(`.${B}`) === lv.el);        // skip items owned by a nested level
  const focusItem = (lv, el) => { if (el) el.focus(); };
  const levelOf = (node) => levels.find((lv) => lv.el === (node && node.closest && node.closest(`.${B}`))) || levels[levels.length - 1];

  let buffer = "", bufferTimer = 0;
  function typeahead(lv, key) {
    clearTimeout(bufferTimer);
    buffer += key.toLowerCase();
    bufferTimer = setTimeout(() => (buffer = ""), 600);
    const its = enabledItems(lv);
    const from = its.indexOf(document.activeElement);
    const text = (n) => (n.querySelector(`.${B}__label`) || n).textContent.trim().toLowerCase();
    for (let i = 1; i <= its.length; i++) {
      const n = its[(from + i + its.length) % its.length];
      if (text(n).startsWith(buffer)) { focusItem(lv, n); return; }
    }
  }

  function onKey(e) {
    if (!levels.length) return;
    const lv = levelOf(document.activeElement) || levels[levels.length - 1];
    const its = enabledItems(lv);
    const cur = its.indexOf(document.activeElement);
    const k = e.key;
    if (k === "Escape") {
      e.preventDefault();
      const deep = levels[levels.length - 1];      // Esc always peels the innermost level
      if (deep.depth > 0) { const t = deep.trigger; closeDeeperThan(deep.depth - 1); focusItem(levels[deep.depth - 1], t); }
      else closeAll(null);
      return;
    }
    if (k === "Tab") { e.preventDefault(); closeAll(null); return; }
    if (k === "ArrowDown" || k === "ArrowUp") {
      e.preventDefault();
      if (!its.length) return;
      focusItem(lv, its[k === "ArrowDown" ? (cur + 1) % its.length : (cur - 1 + its.length) % its.length]);
      return;
    }
    if (k === "Home" || k === "End") {
      e.preventDefault();
      if (its.length) focusItem(lv, its[k === "Home" ? 0 : its.length - 1]);
      return;
    }
    if (k === "ArrowRight") {
      const el = its[cur];
      if (el && el.getAttribute("aria-haspopup") === "menu") {
        e.preventDefault();
        const open = levels[lv.depth + 1];
        if (open && open.trigger === el) focusItem(open, enabledItems(open)[0]);
        else openSubmenu(el.__pdItem, el, lv, true);
        return;
      }
      if (lv.depth === 0 && edgeNav(1)) e.preventDefault();
      return;
    }
    if (k === "ArrowLeft") {
      if (lv.depth > 0) { e.preventDefault(); const t = lv.trigger; closeDeeperThan(lv.depth - 1); focusItem(levels[lv.depth - 1], t); }
      else if (edgeNav(-1)) e.preventDefault();
      return;
    }
    if (k === " " || k === "Enter") {
      const el = its[cur];
      if (!el) return;
      if (el.getAttribute("aria-haspopup") === "menu") {       // open + step into the submenu
        e.preventDefault();
        closeDeeperThan(lv.depth);
        openSubmenu(el.__pdItem, el, lv, true);
        return;
      }
      if (el.tagName === "BUTTON") { e.preventDefault(); el.click(); }
      return;                                   // links: let the browser activate them
    }
    if (k.length === 1 && !e.ctrlKey && !e.metaKey && !e.altKey && /\S/.test(k)) { e.preventDefault(); typeahead(lv, k); }
  }

  // Escape hatch for a horizontal parent (the menubar): Arrow{Left,Right} at the ROOT
  // level with no submenu to open/close hands off, so the neighbouring menu can take
  // over. Returns true when the host consumed it.
  function edgeNav(dir) { return !!(opts.onEdgeNav && opts.onEdgeNav(dir) !== false); }

  function onOutside(e) {
    if (levels.some((lv) => lv.el.contains(e.target)) || (anchor && anchor.contains(e.target))) return;
    closeAll(null);
  }
  // A press on the anchor of THIS open menu closes it (and keeps the click from re-opening it).
  function onAnchorDown() { suppressReopen(anchor); closeAll(null); }

  // ------------------------------------------------------------------- launch
  if (anchor) {
    if (!anchor.hasAttribute("aria-haspopup")) { anchor.setAttribute("aria-haspopup", "menu"); if (anchor.dataset) anchor.dataset.pdMenuHaspopup = "1"; }
    anchor.setAttribute("aria-expanded", "true");
    anchor.addEventListener("pointerdown", onAnchorDown, true);
  }
  const root = openLevel(items, { anchorEl: anchor, placement: opts.placement || "bottom-start", focusFirst: true });

  if (!root.usePopover) document.addEventListener("pointerdown", onOutside, true);
  else root.el.addEventListener("toggle", (e) => { if (e.newState === "closed") closeAll(null); });
  document.addEventListener("keydown", onKey, true);

  // The promise doubles as a tiny controller so a caller (e.g. a menubar) can close
  // or re-anchor the menu it opened, while `await menu(...)` keeps working.
  done.close = (v) => closeAll(v);
  done.el = root.el;
  return done;
}

// position pins the popup next to the anchor per `placement` (side-align), flipping to
// the opposite side when it would overflow, then clamping to the viewport.
function position(m, anchor, placement = "bottom-start") {
  if (!anchor || !anchor.getBoundingClientRect) return;
  const r = anchor.getBoundingClientRect();
  const mw = m.offsetWidth || 180, mh = m.offsetHeight || 200;
  const gap = 4, vw = window.innerWidth, vh = window.innerHeight;
  const [side, align = "start"] = String(placement).split("-");
  let top, left;
  if (side === "left" || side === "right") {
    left = side === "right" ? r.right + gap : r.left - gap - mw;
    if (side === "right" && left + mw > vw - 8 && r.left - gap - mw > 8) left = r.left - gap - mw;
    if (side === "left" && left < 8 && r.right + gap + mw < vw - 8) left = r.right + gap;
    top = align === "end" ? r.bottom - mh : r.top - gap;
  } else {
    top = side === "top" ? r.top - gap - mh : r.bottom + gap;
    if (side !== "top" && top + mh > vh && r.top - gap - mh > 0) top = r.top - gap - mh;   // flip up
    if (side === "top" && top < 0 && r.bottom + gap + mh < vh) top = r.bottom + gap;
    left = align === "end" ? r.right - mw : r.left;
    m.style.minWidth = Math.max(r.width, 160) + "px";
  }
  m.style.left = Math.max(8, Math.min(left, vw - mw - 8)) + "px";
  m.style.top = Math.max(8, Math.min(top, vh - mh - 8)) + "px";
}
