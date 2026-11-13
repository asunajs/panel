/*!
 * 面板 UI 套件（第 139 轮）—— 插件页引这一个 + /app.css，就有和面板一样的控件与主题。
 *
 * 用法：
 *   <link rel="stylesheet" href="/app.css">        <!-- token + 组件样式 -->
 *   <script src="/ui.js" defer></script>            <!-- 本节定义的元素 + 主题桥 -->
 *
 *   <p-card title="账号">
 *     <p-field label="听窗口长度（秒）"><input class="input" type="number"></p-field>
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

  // 给不需要元素、只想拿主题/版本的地方
  window.PanelUI = {
    theme: () => document.documentElement.getAttribute('data-theme') || 'light',
    setTheme,
    version: '0.4.0',
  };
})();
