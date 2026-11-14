/*!
 * 面板 UI 套件（第 139 轮）—— 插件页引这一个 + /app.css，就有和面板一样的控件与主题。
 *
 * 用法：
 *   <link rel="stylesheet" href="/app.css">        <!-- token + 组件样式 -->
 *   <script src="/ui.js" defer></script>            <!-- 本节定义的元素 + 主题桥 -->
 *
 *   <p-card title="账号">
 *     <p-field label="听窗口长度（秒）"><input class="input" type="number"></p-field>
 *     <p-field label="手机号"><p-sms-code id="sms-send"></p-sms-code></p-field>
 *     <p-button variant="primary">保存</p-button>
 *     <p-badge tone="ok">已登录</p-badge>
 *   </p-card>
 *
 * 设计上的两条约束：
 *   ① **不引入自己的配色**：所有颜色来自 /app.css 的 token（--bg/--fg/--line/--accent/--ok/…），
 *      面板切主题这里就跟着变 —— 这是「同质化」的来源，别在插件里另起一套。
 *   ② **元素只加行为、不搬运结构**：自定义元素内部包一层面板已有的 class（.btn/.card/…），
 *      所以外观永远和面板同源，改面板的 CSS 等于改所有插件的观感。
 */
(() => {
  'use strict';

  // ---------------------------------------------------------------- 主题桥
  // iframe 是另一个 document，拿不到父页面的 data-theme。三条来路，优先级从高到低：
  //   ?theme= → localStorage(panel-llrt-theme) → 系统偏好
  // 之后还要跟着变：父页面切主题会 postMessage({type:'panel-theme', theme})。
  const THEME_KEY = 'panel-llrt-theme';

  // ⚠ `?theme=` 只在**第一帧**说得上话：用户之后在面板里切主题时，
  // 它是过期信息（URL 不会变）。原来一直优先读它，于是「切了深色又被 storage 事件拉回浅色」
  // —— 消息确实到了，界面纹丝不动（第 140 轮实测）。收到过父窗口的消息就把它作废。
  const urlTheme = new URLSearchParams(location.search).get('theme');
  let urlWins = urlTheme === 'light' || urlTheme === 'dark';

  const readTheme = () => {
    if (urlWins && (urlTheme === 'light' || urlTheme === 'dark')) return urlTheme;
    try {
      const v = localStorage.getItem(THEME_KEY);
      if (v === 'light' || v === 'dark') return v;
    } catch { /* 隐私模式：往下走 */ }
    return window.matchMedia && window.matchMedia('(prefers-color-scheme: dark)').matches ? 'dark' : 'light';
  };

  const setTheme = (t) => {
    const name = t === 'dark' ? 'dark' : 'light';
    document.documentElement.setAttribute('data-theme', name);
    dispatchEvent(new CustomEvent('panel-theme-change', { detail: { theme: name } }));
  };

  setTheme(readTheme());

  addEventListener('message', (e) => {
    const d = e.data;
    if (!d || d.type !== 'panel-theme') return;
    if (d.theme !== 'light' && d.theme !== 'dark') return;
    urlWins = false; // 父窗口说了算，URL 里那个作废
    setTheme(d.theme);
  });

  addEventListener('storage', (e) => {
    if (e.key && e.key !== THEME_KEY) return;
    urlWins = false;
    setTheme(readTheme());
  });

  if (new URLSearchParams(location.search).get('embed') === '1') {
    document.documentElement.setAttribute('data-embed', '1');
  }

  // ---------------------------------------------------------------- 元素
  /**
   * 所有元素共用的套路：首次渲染时把孩子搬进一个带面板 class 的容器里。
   * 之后不再重排（插件改内容用普通 DOM API 就行），避免和框架抢控制权。
   */
  const build = (host, cls, tag) => {
    const box = document.createElement(tag || 'div');
    box.className = cls;
    while (host.firstChild) box.append(host.firstChild);
    host.append(box);
    return box;
  };

  /**
   * <p-select label="账号" placeholder="还没有账号" value="a" hint="…" disabled>
   *   <option value="a">账号 A</option>
   *   <option value="b">账号 B</option>
 </p-select>
   *
   * 也可以不写 <option>，用 `options='[{"value":"a","label":"账号 A"}]'`。
   * 外观照 TDesign 的 Select：一个触发器（底色/边框和 .input 一致）+ 浮层列表 +
   * 选中项打勾。原生 <select> 那张脸由 /app.css 拉齐，但列表没法自定义 —— 要完全一致就用这个。
   *
   * 读值：`el.value`；改值会冒 `change`（`detail.value` 也带着）和 `input`。
   */
  /**
   * 所有 p-select 共用的「点外面收起」。
   *
   * 为什么共享而不是每实例一个：一个页面上可能有七八个下拉，每个都在 document 上挂一个捕获
   * 监听，既浪费又互相踩（第 144 轮实测：点 body 收不起来，但手动调实例自己的处理函数就正常）。
   * 共享版只挂一次，按「现在开着哪些面板」扫，动态加进来的实例自动算数。
   */
  let outsideCloseInstalled = false;
  function installOutsideClose() {
    if (outsideCloseInstalled) return;
    outsideCloseInstalled = true;
    document.addEventListener('click', (ev) => {
      for (const panel of document.querySelectorAll('.select-panel:not([hidden])')) {
        const wrap = panel.parentElement;
        if (wrap && wrap.contains(ev.target)) continue;
        panel.hidden = true;
        const t = wrap && wrap.querySelector('.select-trigger');
        if (t) t.setAttribute('aria-expanded', 'false');
      }
    }, true);
  }

  const CARET =
    '<svg class="select-caret" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.4" ' +
    'stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M6 9l6 6 6-6"/></svg>';
  const TICK =
    '<svg class="select-tick" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.6" ' +
    'stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M20 6L9 17l-5-5"/></svg>';

  class PSelect extends HTMLElement {
    connectedCallback() {
      if (this._trigger) return;
      const self = this;
      const fromKids = [...this.querySelectorAll('option')].map((o) => ({
        value: o.value !== undefined && o.value !== null ? String(o.value) : (o.textContent || '').trim(),
        label: (o.textContent || '').trim(),
        disabled: o.disabled,
      }));
      let opts = fromKids;
      if (!opts.length) {
        try {
          opts = JSON.parse(this.getAttribute('options') || '[]').map((o) => ({
            value: String(o.value),
            label: String(o.label === undefined ? o.value : o.label),
            disabled: !!o.disabled,
          }));
        } catch {
          opts = [];
        }
      }
      this._opts = opts;
      // 原生的 <option> 只是给人写起来顺手的：读完之后清掉，免得留一堆看不见的节点
      fromKids.length && this.querySelectorAll('option').forEach((o) => o.remove());

      const wrap = document.createElement('div');
      wrap.className = 'p-select';
      const label = this.getAttribute('label');
      if (label) {
        const l = document.createElement('span');
        l.className = 'field-label';
        l.textContent = label;
        wrap.append(l);
      }

      const trigger = document.createElement('button');
      trigger.type = 'button';
      trigger.className = 'select-trigger';
      trigger.setAttribute('aria-haspopup', 'listbox');
      trigger.setAttribute('aria-expanded', 'false');
      const valueEl = document.createElement('span');
      valueEl.className = 'select-value';
      trigger.append(valueEl);
      trigger.insertAdjacentHTML('beforeend', CARET);
      if (this.hasAttribute('disabled')) trigger.disabled = true;

      const panel = document.createElement('div');
      panel.className = 'select-panel';
      panel.setAttribute('role', 'listbox');
      panel.hidden = true;

      wrap.append(trigger, panel);
      const hint = this.getAttribute('hint');
      if (hint) {
        const h = document.createElement('span');
        h.className = 'hint';
        h.textContent = hint;
        wrap.append(h);
      }
      this.append(wrap);
      this._wrap = wrap;
      this._trigger = trigger;
      this._panel = panel;
      this._valueEl = valueEl;
      this._value = this.getAttribute('value') ?? '';
      this._active = -1;

      trigger.addEventListener('click', () => this._toggle());
      // 点外面收起：交给**一个共享的**捕获监听（见 installOutsideClose）。
      // 每实例各挂一个的那版实测点 body 收不起来 —— 一个 document 上七八个监听互相干扰，
      // 出问题还难查；共享版按 DOM 现状扫一遍「开着的面板」，新加的实例自动算数。
      installOutsideClose();
      this.addEventListener('keydown', (ev) => this._key(ev));
      this._render();
    }

    _render() {
      const cur = this._opts.find((o) => o.value === this._value);
      const placeholder = this.getAttribute('placeholder') || '请选择';
      this._valueEl.textContent = cur ? cur.label : placeholder;
      this._valueEl.classList.toggle('is-placeholder', !cur);
      this._panel.textContent = '';
      if (!this._opts.length) {
        const e = document.createElement('div');
        e.className = 'select-empty';
        e.textContent = this.getAttribute('empty') || '暂无数据';
        this._panel.append(e);
      }
      this._opts.forEach((o, i) => {
        const row = document.createElement('div');
        row.className = 'select-option';
        row.setAttribute('role', 'option');
        row.dataset.value = o.value;
        row.dataset.idx = String(i);
        if (o.value === this._value) row.classList.add('is-on');
        if (o.disabled) row.classList.add('is-disabled');
        if (i === this._active) row.classList.add('is-active');
        row.innerHTML = '<span>' + o.label + '</span>' + TICK;
        row.addEventListener('click', () => {
          if (o.disabled) return;
          this._pick(o.value);
        });
        this._panel.append(row);
      });
    }

    _toggle() {
      if (this._trigger.disabled) return;
      if (this._panel.hidden) this._open();
      else this._close();
    }

    _open() {
      if (this._trigger.disabled) return;
      this._panel.hidden = false;
      this._trigger.setAttribute('aria-expanded', 'true');
      this._active = this._opts.findIndex((o) => o.value === this._value);
      this._render();
      this._panel.scrollTop = 0;
    }

    _close() {
      this._panel.hidden = true;
      this._trigger.setAttribute('aria-expanded', 'false');
    }

    _pick(v) {
      const changed = v !== this._value;
      this._value = v;
      this._close();
      this._render();
      if (changed) {
        this.dispatchEvent(new CustomEvent('change', { bubbles: true, detail: { value: v } }));
        this.dispatchEvent(new CustomEvent('input', { bubbles: true, detail: { value: v } }));
      }
    }

    _key(ev) {
      const open = !this._panel.hidden;
      const n = this._opts.length;
      if (ev.key === 'Escape') { if (open) { this._close(); ev.stopPropagation(); } return; }
      if (ev.key === 'ArrowDown' || ev.key === 'ArrowUp') {
        ev.preventDefault();
        if (!open) { this._open(); return; }
        if (!n) return;
        const dir = ev.key === 'ArrowDown' ? 1 : -1;
        let i = this._active;
        for (let k = 0; k < n; k += 1) {
          i = (i + dir + n) % n;
          if (!this._opts[i].disabled) break;
        }
        this._active = i;
        this._render();
        return;
      }
      if ((ev.key === 'Enter' || ev.key === ' ') && open) {
        ev.preventDefault();
        const o = this._opts[this._active];
        if (o && !o.disabled) this._pick(o.value);
        return;
      }
      if (ev.key === 'Enter' && !open) { ev.preventDefault(); this._open(); }
    }

    get value() { return this._value; }

    set value(v) {
      const s = v === null || v === undefined ? '' : String(v);
      if (s === this._value) return;
      this._value = s;
      if (this._trigger) this._render();
      this.dispatchEvent(new CustomEvent('change', { bubbles: true, detail: { value: s } }));
    }
  }

  /**
   * <p-number label="听窗口长度（秒）" min="10" max="3600" step="10" value="30"
   *           hint="10 – 3600" tone="ok|warn|err" disabled>
   *
   * 数字输入 + 步进（对齐 TDesign 的 InputNumber）：− ／ 数字 ／ ＋ 一个外框，
   * 原生那个上下小箭头已经由 /app.css 收掉了。
   *
   * 读值：`el.value`（number，空则 null）；监听：host 上会冒 `input` / `change`
   * （`detail.value` 也带着，方便直接取）。
   */
  class PNumber extends HTMLElement {
    connectedCallback() {
      if (this._input) return;
      const self = this;
      const num = (v) => {
        const n = Number(v);
        return Number.isFinite(n) ? n : null;
      };
      const min = num(this.getAttribute('min'));
      const max = num(this.getAttribute('max'));
      const step = num(this.getAttribute('step')) || 1;

      const wrap = document.createElement('div');
      wrap.className = 'p-number';
      const label = this.getAttribute('label');
      if (label) {
        const l = document.createElement('span');
        l.className = 'field-label';
        l.textContent = label;
        wrap.append(l);
      }
      const group = document.createElement('div');
      group.className = 'stepper';
      const tone = this.getAttribute('tone');
      if (tone === 'ok' || tone === 'warn' || tone === 'err') group.classList.add('is-' + tone);

      const btn = (text, aria, dir) => {
        const b = document.createElement('button');
        b.type = 'button';
        b.className = 'stepper-btn';
        b.textContent = text;
        b.setAttribute('aria-label', aria);
        b.addEventListener('click', () => bump(dir));
        return b;
      };
      const minus = btn('−', '减少', -1);
      const plus = btn('＋', '增加', 1);

      const input = document.createElement('input');
      input.className = 'stepper-input';
      input.type = 'number';
      input.inputMode = 'numeric';
      if (min !== null) input.min = String(min);
      if (max !== null) input.max = String(max);
      input.step = String(step);
      const initial = num(this.getAttribute('value'));
      input.value = initial === null ? '' : String(initial);
      if (this.hasAttribute('disabled')) {
        input.disabled = true;
        minus.disabled = true;
        plus.disabled = true;
      }

      group.append(minus, input, plus);
      wrap.append(group);
      const hint = this.getAttribute('hint');
      let hintEl = null;
      if (hint) {
        hintEl = document.createElement('span');
        hintEl.className = 'hint' + (tone === 'ok' || tone === 'warn' || tone === 'err' ? ' is-' + tone : '');
        hintEl.textContent = hint;
        wrap.append(hintEl);
      }
      this.append(wrap);
      this._input = input;
      this._minus = minus;
      this._plus = plus;
      this._min = min;
      this._max = max;
      this._step = step;

      const clamp = (v) => {
        if (v === null) return null;
        if (min !== null && v < min) return min;
        if (max !== null && v > max) return max;
        return v;
      };
      const fire = (kind) => {
        self.dispatchEvent(new CustomEvent(kind, { bubbles: true, detail: { value: self.value } }));
      };
      const apply = (v, kind) => {
        const c = clamp(v);
        input.value = c === null ? '' : String(c);
        syncBounds();
        fire(kind);
      };
      const syncBounds = () => {
        const cur = num(input.value);
        minus.disabled = self.hasAttribute('disabled') || (cur !== null && min !== null && cur <= min);
        plus.disabled = self.hasAttribute('disabled') || (cur !== null && max !== null && cur >= max);
      };
      function bump(dir) {
        if (self.hasAttribute('disabled')) return;
        const cur = num(input.value);
        apply((cur === null ? (min !== null ? min : 0) : cur) + dir * step, 'input');
        fire('change');
      }

      input.addEventListener('input', () => { syncBounds(); fire('input'); });
      // 手打的值可能越界（比如 9999）：失焦时收进范围，别让插件拿到脏值。
      input.addEventListener('blur', () => {
        const cur = num(input.value);
        if (cur === null) { syncBounds(); fire('change'); return; }
        if (cur !== clamp(cur)) apply(cur, 'change');
        else { syncBounds(); fire('change'); }
      });
      syncBounds();
    }

    get value() {
      const v = this._input ? Number(this._input.value) : NaN;
      return Number.isFinite(v) ? v : null;
    }

    set value(v) {
      if (!this._input) return;
      const n = Number(v);
      let out = Number.isFinite(n) ? n : null;
      if (out !== null && this._min !== null && out < this._min) out = this._min;
      if (out !== null && this._max !== null && out > this._max) out = this._max;
      this._input.value = out === null ? '' : String(out);
      this.dispatchEvent(new CustomEvent('input', { bubbles: true, detail: { value: out } }));
    }
  }

  /** <p-button variant="primary|ghost|danger" size="sm" disabled>文字</p-button> */

  /** <p-button variant="primary|ghost|danger" size="sm" disabled>文字</p-button> */
  class PButton extends HTMLElement {
    connectedCallback() {
      if (this._btn) return;
      const variant = this.getAttribute('variant');
      const size = this.getAttribute('size');
      const cls = ['btn'];
      if (variant === 'primary') cls.push('btn-primary');
      else if (variant === 'ghost') cls.push('btn-ghost');
      else if (variant === 'danger') cls.push('btn-danger');
      if (size === 'sm') cls.push('btn-sm');
      const btn = build(this, cls.join(' '), 'button');
      btn.type = this.getAttribute('type') || 'button';
      if (this.hasAttribute('disabled')) btn.disabled = true;
      this._btn = btn;
    }
    set disabled(v) { if (this._btn) this._btn.disabled = !!v; if (v) this.setAttribute('disabled', ''); else this.removeAttribute('disabled'); }
    get disabled() { return !!(this._btn && this._btn.disabled); }
  }

  /** <p-card title="账号" desc="…">内容</p-card> */
  class PCard extends HTMLElement {
    connectedCallback() {
      if (this._body) return;
      const box = build(this, 'panel');
      const title = this.getAttribute('title');
      const desc = this.getAttribute('desc');
      if (title || desc) {
        const head = document.createElement('div');
        head.className = 'panel-head';
        const h = document.createElement('h3');
        h.textContent = title || '';
        head.append(h);
        if (desc) {
          const p = document.createElement('p');
          p.className = 'muted';
          p.textContent = desc;
          head.append(p);
        }
        box.prepend(head);
      }
      this._body = box;
    }
  }

  /** <p-field label="名字" hint="说明">控件</p-field> */
  class PField extends HTMLElement {
    connectedCallback() {
      if (this._label) return;
      const wrap = build(this, 'field');
      const label = document.createElement('span');
      label.className = 'field-label';
      label.textContent = this.getAttribute('label') || '';
      wrap.prepend(label);
      const hint = this.getAttribute('hint');
      if (hint) {
        const p = document.createElement('span');
        p.className = 'hint';
        p.textContent = hint;
        wrap.append(p);
      }
      this._label = label;
    }
  }

  /** <p-badge tone="ok|warn|err|accent|idle">空闲</p-badge> */
  class PBadge extends HTMLElement {
    connectedCallback() {
      if (this._b) return;
      const tone = this.getAttribute('tone') || '';
      this._b = build(this, ('badge ' + tone).trim(), 'span');
    }
  }

  /** <p-empty text="还没有数据"></p-empty> */
  class PEmpty extends HTMLElement {
    connectedCallback() {
      if (this._e) return;
      const box = build(this, 'empty');
      const p = document.createElement('span');
      p.textContent = this.getAttribute('text') || '这里还是空的';
      box.append(p);
      this._e = box;
    }
  }

  /** <p-dialog open title="设置">内容</p-dialog>（`open` 属性控制显隐） */
  class PDialog extends HTMLElement {
    connectedCallback() {
      if (this._back) return;
      const back = document.createElement('div');
      back.className = 'dialog-back';
      const dlg = build(this, 'dialog');
      const head = document.createElement('div');
      head.className = 'dialog-head';
      const t = document.createElement('span');
      t.textContent = this.getAttribute('title') || '';
      const close = document.createElement('button');
      close.className = 'btn btn-sm btn-ghost';
      close.type = 'button';
      close.textContent = '关闭';
      close.addEventListener('click', () => this.removeAttribute('open'));
      head.append(t, close);
      dlg.prepend(head);
      back.append(dlg);
      this.append(back);
      this._back = back;
      this._sync();
    }
    static get observedAttributes() { return ['open']; }
    attributeChangedCallback() { this._sync(); }
    _sync() { if (this._back) this._back.hidden = !this.hasAttribute('open'); }
  }


  /** <p-switch checked label="启用"> —— 开关。值读 .value，改它派发 change。 */
  class PSwitch extends HTMLElement {
    static get observedAttributes() { return ['checked', 'disabled']; }
    connectedCallback() {
      if (this._i) return;
      const wrap = build(this, 'switch');
      const input = document.createElement('input');
      input.type = 'checkbox';
      input.className = 'switch-input';
      const knob = document.createElement('span');
      knob.className = 'switch-knob';
      wrap.append(input, knob);
      input.checked = this.hasAttribute('checked');
      input.disabled = this.hasAttribute('disabled');
      input.addEventListener('change', () => {
        this.toggleAttribute('checked', input.checked);
        this.dispatchEvent(new CustomEvent('change', { detail: { value: input.checked }, bubbles: true }));
      });
      this._i = input;
      const text = this.getAttribute('label');
      if (text) {
        const s = document.createElement('span');
        s.className = 'switch-text';
        s.textContent = text;
        wrap.append(s);
      }
    }
    get value() { return !!(this._i && this._i.checked); }
    set value(v) {
      if (!this._i) return;
      this._i.checked = !!v;
      this.toggleAttribute('checked', !!v);
    }
    attributeChangedCallback(name) {
      if (name === 'checked' && this._i) this._i.checked = this.hasAttribute('checked');
      if (name === 'disabled' && this._i) this._i.disabled = this.hasAttribute('disabled');
    }
  }

  /** <p-multiselect options='["a","b"]' value="a"> —— 多选（点外面的空处收起）。 */
  class PMultiselect extends HTMLElement {
    connectedCallback() {
      if (this._wrap) return;
      const wrap = build(this, 'msel');
      const trig = document.createElement('button');
      trig.type = 'button';
      trig.className = 'msel-trigger';
      const chips = document.createElement('span');
      chips.className = 'msel-chips';
      const caret = document.createElement('span');
      caret.className = 'msel-caret';
      caret.textContent = '▾';
      trig.append(chips, caret);
      const panel = document.createElement('div');
      panel.className = 'msel-panel';
      panel.hidden = true;
      wrap.append(trig, panel);

      // 选项来源：子元素 <option> 或 options 属性（字符串数组 / {value,label} 数组）
      let opts = [...this.querySelectorAll('option')].map((o) => ({
        value: o.value || o.textContent,
        label: o.textContent,
        disabled: o.disabled,
      }));
      const raw = this.getAttribute('options');
      if (raw) {
        try {
          opts = JSON.parse(raw).map((x) => (typeof x === 'string' ? { value: x, label: x } : x));
        } catch (e) { /* 写坏了就当没有 */ }
      }
      const sel = new Set(
        (this.getAttribute('value') || '')
          .split(',')
          .map((s) => s.trim())
          .filter(Boolean),
      );

      const renderChips = () => {
        chips.textContent = '';
        if (!sel.size) {
          const ph = document.createElement('span');
          ph.className = 'msel-ph';
          ph.textContent = this.getAttribute('placeholder') || '未选择';
          chips.append(ph);
          return;
        }
        for (const v of sel) {
          const o = opts.find((x) => x.value === v);
          const chip = document.createElement('span');
          chip.className = 'msel-chip';
          chip.append(document.createTextNode(o ? o.label : v));
          const x = document.createElement('span');
          x.className = 'msel-x';
          x.textContent = '×';
          x.addEventListener('click', (ev) => {
            ev.stopPropagation();
            sel.delete(v);
            sync();
          });
          chip.append(x);
          chips.append(chip);
        }
      };

      const sync = () => {
        renderChips();
        for (const row of panel.children) {
          row.classList.toggle('is-on', sel.has(row.dataset.v));
        }
        this.dispatchEvent(new CustomEvent('change', { detail: { value: [...sel] }, bubbles: true }));
      };

      for (const o of opts) {
        const row = document.createElement('button');
        row.type = 'button';
        row.className = 'msel-option' + (o.disabled ? ' is-disabled' : '');
        row.dataset.v = o.value;
        const tick = document.createElement('span');
        tick.className = 'msel-tick';
        tick.textContent = '✓';
        const label = document.createElement('span');
        label.textContent = o.label;
        row.append(tick, label);
        row.addEventListener('click', () => {
          if (o.disabled) return;
          if (sel.has(o.value)) sel.delete(o.value);
          else sel.add(o.value);
          sync();
        });
        panel.append(row);
      }
      trig.addEventListener('click', () => {
        panel.hidden = !panel.hidden;
        wrap.classList.toggle('is-open', !panel.hidden);
      });
      this._onDoc = (ev) => {
        if (!this.contains(ev.target)) {
          panel.hidden = true;
          wrap.classList.remove('is-open');
        }
      };
      document.addEventListener('click', this._onDoc);
      renderChips();
      this._wrap = wrap;
      this._panel = panel;
      this._sel = sel;
    }
    disconnectedCallback() {
      if (this._onDoc) document.removeEventListener('click', this._onDoc);
    }
    get value() { return this._sel ? [...this._sel] : []; }
    set value(list) {
      if (!this._sel) return;
      this._sel.clear();
      for (const v of list || []) this._sel.add(v);
      this.dispatchEvent(new CustomEvent('change', { detail: { value: this.value }, bubbles: true }));
    }
  }

  /** <p-tab label="名字">内容</p-tab> 放在 <p-tabs> 里。 */
  class PTab extends HTMLElement {}

  /** <p-tabs><p-tab label="甲">…</p-tab><p-tab label="乙">…</p-tab></p-tabs> */
  class PTabs extends HTMLElement {
    connectedCallback() {
      if (this._bar) return;
      const bar = document.createElement('div');
      bar.className = 'tabs-bar';
      bar.setAttribute('role', 'tablist');
      const body = document.createElement('div');
      body.className = 'tabs-body';
      const tabs = [...this.querySelectorAll('p-tab')];
      const panes = tabs.map((t, i) => {
        const pane = document.createElement('div');
        pane.className = 'tab-pane';
        pane.hidden = i !== 0;
        while (t.firstChild) pane.append(t.firstChild);
        return pane;
      });
      tabs.forEach((t, i) => {
        const b = document.createElement('button');
        b.type = 'button';
        b.className = 'tab' + (i === 0 ? ' active' : '');
        b.textContent = t.getAttribute('label') || '标签' + (i + 1);
        b.addEventListener('click', () => this.select(i));
        bar.append(b);
        t.remove();
      });
      body.append(...panes);
      this.append(bar, body);
      this._bar = bar;
      this._panes = panes;
      this._idx = 0;
    }
    select(i) {
      if (!this._panes || i < 0 || i >= this._panes.length) return;
      this._idx = i;
      [...this._bar.children].forEach((b, k) => b.classList.toggle('active', k === i));
      this._panes.forEach((p, k) => {
        p.hidden = k !== i;
      });
      this.dispatchEvent(new CustomEvent('change', { detail: { index: i }, bubbles: true }));
    }
    get index() { return this._idx || 0; }
  }

  /**
   * <p-table head="名字,状态" rows='[["a","ok"]]'></p-table>
   *
   * 为什么用 rows 属性而不是 <tr> 子元素：HTML 解析器**只允许 tr 出现在 table 里**，
   * 写在自定义元素里的 <tr> 会被直接丢掉（实测 tbody 里 0 行）。单元格可以是字符串，
   * 也可以是 {text, tone} —— tone 会变成 p-badge 的那种 badge 样式。
   */
  class PTable extends HTMLElement {
    connectedCallback() {
      if (this._t) return;
      const head = (this.getAttribute('head') || '')
        .split(',')
        .map((s) => s.trim())
        .filter(Boolean);
      const fromAttr = (() => {
        const raw = this.getAttribute('rows');
        if (!raw) return [];
        try {
          return JSON.parse(raw).map((row) =>
            (row || []).map((cell) => {
              const tr = document.createElement('tr');
              return { tr, cell };
            }),
          );
        } catch (e) {
          return [];
        }
      })();
      // 兼容用 JS 直接塞进去的 <tr>
      const rows = [...this.querySelectorAll('tr')];
      const table = build(this, 'table', 'table');
      if (head.length) {
        const thead = document.createElement('thead');
        const tr = document.createElement('tr');
        for (const h of head) {
          const th = document.createElement('th');
          th.textContent = h;
          tr.append(th);
        }
        thead.append(tr);
        table.append(thead);
      }
      const tbody = document.createElement('tbody');
      for (const r of rows) tbody.append(r);
      for (const row of fromAttr) {
        const tr = document.createElement('tr');
        for (const item of row) {
          const td = document.createElement('td');
          const c = item.cell;
          if (c && typeof c === 'object') {
            const b = document.createElement('span');
            b.className = 'badge ' + (c.tone || '');
            b.textContent = c.text == null ? '' : String(c.text);
            td.append(b);
          } else {
            td.textContent = c == null ? '' : String(c);
          }
          tr.append(td);
        }
        tbody.append(tr);
      }
      table.append(tbody);
      this._t = table;
    }
  }

  /** <p-code lang="js">const a = 1;</p-code> —— 只读代码块（有 /highlight.js 就上色） */
  class PCode extends HTMLElement {
    connectedCallback() {
      if (this._pre) return;
      const lang = this.getAttribute('lang') || 'js';
      const raw = (this.textContent || '').trim();
      // 先把原始文本节点清掉：不清的话页面上一份纯文本 + 一份高亮块（第一次就露了）
      this.textContent = '';
      const wrap = build(this, 'code-block');
      const bar = document.createElement('div');
      bar.className = 'code-block-bar';
      const tag = document.createElement('span');
      tag.className = 'code-lang';
      tag.textContent = lang;
      const copy = document.createElement('button');
      copy.type = 'button';
      copy.className = 'btn btn-sm btn-ghost';
      copy.textContent = '复制';
      copy.addEventListener('click', async () => {
        try {
          await navigator.clipboard.writeText(raw);
          copy.textContent = '已复制';
          setTimeout(() => {
            copy.textContent = '复制';
          }, 1200);
        } catch (e) {
          copy.textContent = '复制失败';
        }
      });
      bar.append(tag, copy);
      const pre = document.createElement('pre');
      pre.className = 'code-hl';
      const code = document.createElement('code');
      if (window.PanelHL) code.innerHTML = window.PanelHL.render(raw, lang);
      else code.textContent = raw;
      pre.append(code);
      wrap.append(pre);
      this.prepend(bar);
      this._pre = pre;
    }
  }

  /** <p-form submit-label="保存">…<input name="a">…</p-form> */
  class PForm extends HTMLElement {
    connectedCallback() {
      if (this._box) return;
      const box = build(this, 'pform');
      const label = this.getAttribute('submit-label');
      if (label) {
        const actions = document.createElement('div');
        actions.className = 'pform-actions';
        const b = document.createElement('button');
        b.type = 'button';
        b.className = 'btn btn-primary';
        b.textContent = label;
        b.addEventListener('click', () => {
          this.dispatchEvent(new CustomEvent('submit', { detail: { values: this.values() }, bubbles: true }));
        });
        actions.append(b);
        this.append(actions);
      }
      this._box = box;
    }
    /** 把 name 上有值的元素收集成一个对象（p-select / p-switch / p-multiselect 都认）。 */
    values() {
      const out = {};
      for (const el of this.querySelectorAll('[name]')) {
        const n = el.getAttribute('name');
        if (!n) continue;
        if (el.tagName === 'P-SWITCH') out[n] = el.value;
        else if (el.tagName === 'P-MULTISELECT' || el.tagName === 'P-SELECT') out[n] = el.value;
        else if (el.type === 'checkbox') out[n] = el.checked;
        else if (el.type === 'number') out[n] = el.value === '' ? null : Number(el.value);
        else out[n] = el.value;
      }
      return out;
    }
  }

  /** <p-sms-code length="11" seconds="60" placeholder="请输入手机号" button="获取验证码">
   *
   *  手机号 + 行内「获取验证码」合成一格（.input-group）—— 发码页（登录 / 绑定 / 领奖）
   *  共用的那格。从 mcloud 的登录页提上来：它最早照参考实现画的，别的插件要发码
   *  就得再抄一遍这一格和倒计时。
   *
   *  分工的边界：**控件只管界面，不发请求**。
   *    · 门禁：只按"还差几个字符"（满 `length` 位才放开按钮）—— 提前告知，不是替
   *      用户判断；点了才报错的写法要先失败一次才知道规则。
   *    · 点按钮派 `send`（`detail.value` 带手机号），插件去调自己的发码接口；
   *      请求期间设 `el.busy = true`（按钮禁住防连点），成功后 `el.startResend()`。
   *    · `startResend(seconds)` 是**界面节奏，不是权威** —— 能不能发、还剩几次，
   *      最终是服务端说了算。它的用处只是别让用户连点：连点只会喂限流。
   *      倒计时接管后按钮写「重新获取（N 秒）」并禁用，`stopResend()` / 倒计时走完
   *      回到「获取验证码」，是否可点重新按位数算。
   *  读值：`el.value`（只剩数字）；手机号变化冒 `input` / `change`。
   *  输入框上回车 = 点行内按钮（手机上少一次瞄准），不可点时 disabled 自然挡住。
   */
  class PSmsCode extends HTMLElement {
    static get observedAttributes() { return ['disabled']; }
    connectedCallback() {
      if (this._in) return;
      const self = this;
      this._len = Math.max(1, Math.round(Number(this.getAttribute('length')) || 11));
      this._timer = null;
      this._busy = false;
      const group = build(this, 'input-group');
      const input = document.createElement('input');
      input.type = 'tel';
      input.inputMode = 'numeric';
      input.autocomplete = 'tel';
      input.maxLength = this._len;
      input.placeholder = this.getAttribute('placeholder') || '请输入手机号';
      const btn = document.createElement('button');
      btn.type = 'button';
      btn.className = 'btn';
      btn.textContent = this.getAttribute('button') || '获取验证码';
      group.append(input, btn);
      this._in = input;
      this._btn = btn;

      input.addEventListener('input', () => {
        // 只留数字；`maxlength` 管不住粘贴，这里再截一遍
        const digits = input.value.replace(/\D/g, '').slice(0, self._len);
        if (digits !== input.value) input.value = digits;
        self._gate();
        self.dispatchEvent(new CustomEvent('input', { detail: { value: digits }, bubbles: true }));
      });
      input.addEventListener('change', () => {
        self.dispatchEvent(new CustomEvent('change', { detail: { value: self.value }, bubbles: true }));
      });
      input.addEventListener('keydown', (ev) => {
        if (ev.key === 'Enter' && !btn.disabled) {
          ev.preventDefault();
          btn.click();
        }
      });
      btn.addEventListener('click', () => {
        if (btn.disabled) return;
        self.dispatchEvent(new CustomEvent('send', { detail: { value: self.value }, bubbles: true }));
      });
      this._gate();
    }
    disconnectedCallback() {
      // 元素被摘掉（弹窗销毁）还留着定时器的话，它会在后台一直跑
      if (this._timer) {
        clearInterval(this._timer);
        this._timer = null;
      }
    }
    attributeChangedCallback(name) {
      if (name === 'disabled' && this._in) {
        this._in.disabled = this.hasAttribute('disabled');
        this._gate();
      }
    }
    /** 按钮可不可点：整体禁用 / 发码请求中 / 倒计时里 / 位数不满，任一都不行。
     *  ⚠ 倒计时自己每秒都会写一次 disabled，这里不排它的话「重新获取（N 秒）」
     *    会被重画重新点亮（mcloud 第 145 轮踩过：每次 load() 重画都走到这）。 */
    _gate() {
      if (!this._btn) return;
      const ok =
        !this.hasAttribute('disabled') &&
        !this._busy &&
        this._timer === null &&
        this.value.length === this._len;
      this._btn.disabled = !ok;
    }
    get value() { return this._in ? this._in.value : ''; }
    set value(v) {
      if (!this._in) return;
      this._in.value = String(v === undefined || v === null ? '' : v).replace(/\D/g, '').slice(0, this._len);
      this._gate();
    }
    get disabled() { return this.hasAttribute('disabled'); }
    set disabled(v) { this.toggleAttribute('disabled', !!v); }
    /** 发码请求进行中（按钮禁住防连点）。控件不改按钮文字 —— 那一两秒很短，
     *  成败的话由插件的 toast 说，别在这里闪一下「…」。 */
    get busy() { return !!this._busy; }
    set busy(v) {
      this._busy = !!v;
      this._gate();
    }
    startResend(seconds) {
      if (!this._btn) return;
      let left = Math.max(1, Math.round(Number(seconds) || 60));
      if (this._timer) clearInterval(this._timer);
      const tick = () => {
        if (left <= 0) {
          this.stopResend();
          return;
        }
        this._btn.textContent = `重新获取（${left} 秒）`;
        this._btn.disabled = true;
        left -= 1;
      };
      tick();
      this._timer = setInterval(tick, 1000);
    }
    /** 停掉倒计时，按钮回到「获取验证码」，可不可点重新按位数算。 */
    stopResend() {
      if (this._timer) {
        clearInterval(this._timer);
        this._timer = null;
      }
      if (!this._btn) return;
      this._btn.textContent = this.getAttribute('button') || '获取验证码';
      this._gate();
    }
  }

  const define = (name, cls) => {
    if (!customElements.get(name)) customElements.define(name, cls);
  };
  define('p-select', PSelect);
  define('p-number', PNumber);
  define('p-button', PButton);
  define('p-card', PCard);
  define('p-field', PField);
  define('p-badge', PBadge);
  define('p-empty', PEmpty);
  define('p-dialog', PDialog);
  define('p-switch', PSwitch);
  define('p-multiselect', PMultiselect);
  define('p-tab', PTab);
  define('p-tabs', PTabs);
  define('p-table', PTable);
  define('p-code', PCode);
  define('p-form', PForm);
  define('p-sms-code', PSmsCode);

  // 给不需要元素、只想拿主题/版本的地方
  window.PanelUI = {
    theme: () => document.documentElement.getAttribute('data-theme') || 'light',
    setTheme,
    version: '0.4.0',
  };
})();
