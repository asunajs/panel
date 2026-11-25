/*!
 * 面板 UI 套件 —— 插件页引这一个 + /app.css，就有和面板一样的控件与主题。
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
  // —— 消息确实到了，界面纹丝不动（实测如此）。收到过父窗口的消息就把它作废。
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
   * 选中项打勾。原生 <select> 那张脸由 /app.css 对齐，但列表没法自定义 —— 要完全一致就用这个。
   *
   * 读值：`el.value`；改值会冒 `change`（`detail.value` 也带着）和 `input`。
   */
  /**
   * 所有 p-select 共用的「点外面收起」。
   *
   * 为什么共享而不是每实例一个：一个页面上可能有七八个下拉，每个都在 document 上挂一个捕获
   * 监听，既浪费又互相踩（实测：点 body 收不起来，但手动调实例自己的处理函数就正常）。
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
    /** 改按钮文字（「登录」→「登录中…」）。
     *  ⚠ 为什么需要它：`<p-button>` 里面**还包着一颗** `<button>`，直接写
     *    `pBtn.textContent = '…'` 会把里面那颗按钮整个换成一个文本节点 —— 按钮消失，
     *    只剩一行字（插件侧最早的写法就是去摸私有的 `btn._btn`，见 mgmusic 的
     *    `setBtnText`）。这里给一个公开属性，插件不用再碰内部字段。 */
    set label(v) { if (this._btn) this._btn.textContent = String(v === undefined || v === null ? '' : v); }
    get label() { return this._btn ? this._btn.textContent : ''; }
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
      // 内容区：`.dialog` 有 max-height，但**没有滚动区**时，多出来的一截连
      // 按钮行会被画到卡片外面 —— cmcc 的「添加账号」就是这么坏的：按钮行落到遮罩上、
      // 半透明地压在页面背景上。
      // `.dialog-body { overflow:auto; min-height:0 }` 早就在 app.css 里，只是没人替插件包上；
      // 而文档里的标准写法（p-field 直接摆在 <p-dialog> 里）恰好是**没包**的那种。
      // 所以：插件自己包了 .dialog-body 就原样用；没包就替它包一层，
      // 按钮行（.dialog-actions）留在外面 —— 滚动时不动，永远在卡片底部。
      if (!dlg.querySelector(':scope > .dialog-body')) {
        const body = document.createElement('div');
        body.className = 'dialog-body';
        const kids = [...dlg.children].filter((c) => c !== head);
        const firstActions = kids.find((c) => c.classList.contains('dialog-actions'));
        for (const el of kids) {
          if (el.classList.contains('dialog-actions')) continue;
          body.append(el);
        }
        dlg.insertBefore(body, firstActions || null);
      }
      back.append(dlg);
      this.append(back);
      this._back = back;
      this._sync();
    }
    static get observedAttributes() { return ['open']; }
    attributeChangedCallback() { this._sync(); }
    _sync() { if (this._back) this._back.hidden = !this.hasAttribute('open'); }
  }


  /** <p-switch checked label="启用"> —— 开关。值读 .value，改它派发 change。
   *
   *  ⚠ 外层必须是 **<label>**，不能是 <div>（2026-11-23 改）：
   *  `.switch-input` 是一条 `opacity:0; width:0; height:0` 的隐藏 checkbox，真正能看见的
   *  是旁边的 knob。用 <div> 包着，**点 knob / 点文字都没有任何反应** —— 只有 Tab 聚焦到
   *  那个 0×0 的 input 再按空格才改得动，等于鼠标用户用不了这个控件。改成 <label> 之后
   *  点框内任何位置都会激活里面的 checkbox（原生行为，不用自己加 click），键盘、读屏也照旧。
   *  门禁：scripts/ui-kit-check.mjs 现在点的是 **knob**（真实鼠标路径），点隐藏 input 会
   *  把这类 bug 全放过。 */
  class PSwitch extends HTMLElement {
    static get observedAttributes() { return ['checked', 'disabled']; }
    connectedCallback() {
      if (this._i) return;
      const wrap = build(this, 'switch', 'label');
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
     *    会被重画重新点亮（mcloud：每次 load() 重画都会走到这里）。 */
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

  // ---------------------------------------------------------------- 运行日志台
  //
  // `<p-log plugin="yangshipin" height="320">` —— 实时看任务在跑什么。
  //
  // 数据源是面板自己的 `GET /api/logs/stream`（SSE）。插件页和面板**同源**，会话 cookie
  // 自动带上，所以不用额外传令牌；`since=<最后一行的 id>` 能把断线那段补回来。
  //
  // ⚠ 面板的流**不按插件过滤**（`?plugin=` 它不认），所有插件的行都会推过来 ⇒ 这里自己筛。
  // ⚠ 一行一律走 `textContent`（日志里有用户可控的字符串）。
  // ⚠ 只在页面上时才连：`stop()` 在 `disconnectedCallback` 里调，别让定时器/连接漏出去。
  const TERM_MAX_LINES = 400;
  const TERM_RANK = { debug: 0, info: 0, start: 0, success: 0, warn: 1, fail: 2, error: 2, fatal: 3 };

  /** 时间戳：**月-日 时:分:秒.毫秒**。
   *
   *  ⚠ 别只给时分秒：日志跨天之后就分不清是哪天 —— 面板自己的日志页在 2026-11-15
   *  就被用户指着说过一次（`docs/review-20261115.md` 的 C3，用户原话「隔天翻日志不知道哪天」），
   *  当时的修法是 `fmtLogTime` = 月-日 + 时分秒、时间列 118px。做这个控件时
   *  又只写了时分秒，用户第二次指出来（「时间又是只有小时」）。这里跟面板的日志页对齐。
   *
   *  毫秒留着：一轮任务里同一秒会打好几行，`09:19:09.880` / `.881` 的顺序是有意义的。
   *  面板给的是 epoch 毫秒；旧面板/历史给的是 `2026-10-17 10:50:42.498` 这种串。 */
  function termStamp(raw) {
    const p = (n, w = 2) => String(n).padStart(w, '0');
    const fmt = (d) =>
      p(d.getMonth() + 1) + '-' + p(d.getDate()) + ' ' +
      p(d.getHours()) + ':' + p(d.getMinutes()) + ':' + p(d.getSeconds()) + '.' +
      p(d.getMilliseconds(), 3);
    if (typeof raw === 'number' && Number.isFinite(raw)) return fmt(new Date(raw));
    const t = String(raw === undefined || raw === null ? '' : raw).trim();
    if (t === '') return '—';
    // `YYYY-MM-DDTHH:MM:SS…`（ISO）或 `YYYY-MM-DD HH:MM:SS…`：解析后重排
    if (/^\d{4}-\d{2}-\d{2}[T ]/.test(t)) {
      const d = new Date(t.replace(' ', 'T'));
      if (!Number.isNaN(d.getTime())) return fmt(d);
    }
    // 已经是 `MM-DD HH:MM:SS(.mmm)` 就原样用；只有时分秒的补不了日期，原样显示
    return t;
  }

  class PLog extends HTMLElement {
    connectedCallback() {
      if (this._body) return;
      this._plugin = this.getAttribute('plugin') || '';
      this._level = this.getAttribute('level') || 'info';
      this._max = Math.max(20, Math.round(Number(this.getAttribute('lines')) || TERM_MAX_LINES));
      this._height = Math.max(80, Math.round(Number(this.getAttribute('height')) || 320));
      this._lines = 0;
      this._shown = 0;
      this._lastId = 0;
      this._es = null;
      this._retry = 1000;
      // 去重与保序用（见 _ingest）：id 见过的丢掉；没有 id 的实时帧按内容认亲。
      this._seenIds = new Set();
      this._twins = new Map();

      const box = build(this, 'term');
      const bar = document.createElement('div');
      bar.className = 'term-bar';
      const title = document.createElement('span');
      title.className = 'term-title';
      title.textContent = this.getAttribute('title') || '运行日志';
      const meta = document.createElement('span');
      meta.className = 'term-meta';
      const acts = document.createElement('div');
      acts.className = 'term-actions';
      const followWrap = document.createElement('label');
      followWrap.className = 'term-follow';
      const follow = document.createElement('input');
      follow.type = 'checkbox';
      follow.checked = !this.hasAttribute('nofollow');
      followWrap.append(follow, document.createTextNode('跟随'));
      const level = document.createElement('select');
      level.className = 'term-level';
      for (const [v, label] of [['info', '全部'], ['warn', '提醒以上'], ['error', '只看错误']]) {
        const op = document.createElement('option');
        op.value = v;
        op.textContent = label;
        level.append(op);
      }
      level.value = this._level;
      const clearBtn = document.createElement('button');
      clearBtn.type = 'button';
      clearBtn.className = 'btn btn-sm term-clear';
      clearBtn.textContent = '清空';
      acts.append(followWrap, level, clearBtn);
      bar.append(title, meta, acts);

      const body = document.createElement('div');
      body.className = 'term-body';
      body.style.maxHeight = this._height + 'px';
      const empty = document.createElement('span');
      empty.className = 'term-empty';
      empty.textContent = '（还没有日志）';
      body.append(empty);

      box.append(bar, body);
      this._body = body;
      this._meta = meta;
      this._follow = follow;
      this._levelSel = level;

      // 级别下拉**每次进页面都回「全部」**：浏览器会"恢复"上次选的值，
      // 用户可能某次刷新后停在「只看错误」而一行都看不到（mcloud 踩过）。
      level.addEventListener('change', () => {
        this._level = level.value;
        this.rerender();
      });
      clearBtn.addEventListener('click', () => this.clear());
      this.start();
    }

    disconnectedCallback() { this.stop(); }

    /** 当前门槛过不过。`start/success/debug` 归最低档；`fail` 跟 warn 一起出现。 */
    _passes(lv) {
      return (TERM_RANK[lv] ?? 0) >= (TERM_RANK[this._level] ?? 0);
    }

    _setMeta(status) {
      if (!this._meta) return;
      const only = this._level === 'info' ? '' : this._level === 'warn' ? '提醒以上' : '只看错误';
      const parts = [this._plugin || '全部插件', '显示 ' + this._shown + ' 行'];
      if (status) parts.push(status);
      if (only) parts.push('只显示「' + only + '」');
      const text = parts.join(' · ');
      if (this._meta.textContent !== text) this._meta.textContent = text;
    }

    /** 画一行：`addLine(level, ts, text)`。插件也能直接调（不想进面板日志的行）。
     *
     *  ⚠ 名字**不能叫 `append`**：那是 `Element.prototype.append`，覆盖掉之后
     *  `build()` 里的 `host.append(box)` 会被当成"画一行日志"，整个控件就渲染成空壳
     *  （`children=0` 但 `_body` 又是有的 —— 判空要同时看两者）。 */
    addLine(lv, ts, text, id, before) {
      const body = this._body;
      if (!body) return null;
      const emptyEl = body.querySelector('.term-empty');
      if (emptyEl) emptyEl.remove();
      const line = document.createElement('span');
      line.className = 'term-line term-line-' + lv;
      line.dataset.lv = lv;
      line.dataset.ts = String(ts);
      if (id !== undefined && id !== null) line.dataset.id = String(id);
      const t = document.createElement('span');
      t.className = 'term-ts';
      t.textContent = termStamp(ts) + ' ';
      const l = document.createElement('span');
      l.className = 'term-lv term-lv-' + lv;
      l.textContent = String(lv).padEnd(7);
      const msg = document.createElement('span');
      msg.className = 'term-msg';
      msg.textContent = String(text === undefined || text === null ? '' : text);
      line.append(t, l, msg);
      // `before` 非空 ⇒ 插在它后面（补发批次晚到时按 ts 归位，而不是硬追加到末尾）
      if (before && before.parentElement === body) body.insertBefore(line, before.nextSibling);
      else body.appendChild(line);
      this._lines += 1;
      if (this._passes(lv)) this._shown += 1;
      while (this._lines > this._max && body.firstElementChild) {
        const cut = body.firstElementChild;
        if (cut.dataset && cut.dataset.lv && this._passes(cut.dataset.lv)) this._shown -= 1;
        cut.remove();
        this._lines -= 1;
      }
      if (!this._passes(lv)) line.style.display = 'none';
      this._setMeta('');
      if (this._follow && this._follow.checked) body.scrollTop = body.scrollHeight;
      return line;
    }

    /** 改级别后重画：屏上已有的行按新门槛显示/隐藏（不清屏，也不重连）。 */
    rerender() {
      const body = this._body;
      if (!body) return;
      let shown = 0;
      for (const line of body.querySelectorAll('.term-line')) {
        const ok = this._passes(line.dataset.lv);
        line.style.display = ok ? '' : 'none';
        if (ok) shown += 1;
      }
      this._shown = shown;
      this._setMeta('');
    }

    clear() {
      const body = this._body;
      if (!body) return;
      body.textContent = '';
      const empty = document.createElement('span');
      empty.className = 'term-empty';
      empty.textContent = '（还没有日志）';
      body.append(empty);
      this._lines = 0;
      this._shown = 0;
      this._setMeta('');
    }

    /**
     * 收一行：**去重 + 保序**，然后才画。
     *
     * 为什么需要它（2026-11-21 现场：插件页日志「时间倒流」、两个号看起来在并发）：
     *   · 实时广播的帧**没有 id**（`LogLine` 结构里就没有这个字段），补发/历史批次才有；
     *   · 服务端 `since` 补发与实时广播**会重叠**（`api_logs_stream` 的注释原话：
     *     「重复由前端按 id 去重」），而补发批次可能**晚于**实时帧到达；
     *   · 直接 append ⇒ 旧行落在新行后面：屏上时间倒流，两个账号的行交错，
     *     看着像「同一个号时间乱 / 两个号并发」。
     *
     * 所以：id 见过的丢掉；没有 id 的帧按 (ts|level|text) 认亲；
     * ts 比已画的最后一行还早 ⇒ 插到它该在的位置，不硬追加。
     */
    _ingest(row) {
      const body = this._body;
      if (!body || row === null || typeof row !== 'object') return false;
      const id = typeof row.id === 'number' ? row.id : null;
      const ts = Number(row.ts) || Date.now();
      let lv = typeof row.level === 'string' && row.level ? row.level : 'info';
      let text = String(row.message === undefined || row.message === null ? '' : row.message);
      // 插件自己的语义级别以 `[debug] `/`[fail] ` 前缀带出来（面板只有 info/warn/error 三档）
      const m = /^\[(debug|start|ok|fail|fatal|success)\]\s*/.exec(text);
      if (m) {
        lv = m[1] === 'ok' ? 'success' : m[1] === 'fatal' ? 'error' : m[1];
        text = text.slice(m[0].length);
      }
      const key = ts + '|' + lv + '|' + text;
      if (id !== null) {
        if (this._seenIds.has(id)) return false; // 补发批次里已经画过
        this._seenIds.add(id);
        if (this._seenIds.size > 800) this._seenIds.delete(this._seenIds.values().next().value);
        if (id > this._lastId) this._lastId = id;
        const twin = this._twins.get(key);
        if (twin) {
          // 同一条：实时帧先画了（没有 id），这一帧只是把 id 补上，别再画一遍
          twin.dataset.id = String(id);
          this._twins.delete(key);
          return false;
        }
      } else if (this._twins.has(key)) {
        return false; // 内容一模一样，别再画一遍
      }

      // 归位：从尾部往前找第一个 ts 不大于它的行，插在它后面。
      let anchor = null;
      for (let el = body.lastElementChild; el; el = el.previousElementSibling) {
        if (!el.classList || !el.classList.contains('term-line')) continue;
        if (Number(el.dataset.ts) <= ts) { anchor = el; break; }
      }
      const line = this.addLine(lv, ts, text, id, anchor);
      if (!line) return false;
      if (id === null) {
        this._twins.set(key, line);
        if (this._twins.size > 200) this._twins.delete(this._twins.keys().next().value);
      }
      return true;
    }

    /** 先铺历史（`GET /api/logs?plugin=…&limit=…`），再连实时流。
     *
     *  只铺一次：打开页面时看到「刚才那段」是基本预期 —— 不然用户点开插件页，
     *  任务已经在跑第 3 分钟，屏幕上却是空的，看起来像"没在跑"。
     *  ⚠ 历史是**倒序**给的（`ORDER BY id DESC`），要翻过来再画。 */
    async _seed() {
      if (this._seeded) return;
      this._seeded = true;
      const q = new URLSearchParams({ limit: String(Math.min(this._max, 200)) });
      if (this._plugin) q.set('plugin', this._plugin);
      let rows = [];
      try {
        const r = await fetch('/api/logs?' + q.toString(), { headers: { accept: 'application/json' } });
        if (r.ok) {
          const d = await r.json();
          rows = Array.isArray(d && d.logs) ? d.logs : [];
        }
      } catch { /* 拿不到历史就只看实时，不影响功能 */ }
      if (rows.length === 0) return;
      // 实时流可能抢在前面画了行（历史这一枪要绕一圈网络）——那就别再铺，
      // 否则旧行会盖在新行后面，屏上时间倒流。
      if (this._lines > 0) return;
      // 走 _ingest：历史行同样要过一遍去重/保序（它和实时帧会重叠）
      for (const row of rows.slice().reverse()) this._ingest(row);
    }

    /** 连面板的日志流。断线自己重连（**带 `since`**，把断线那段补回来）。 */
    start() {
      if (this._es !== null || typeof EventSource === 'undefined') return;
      this._seed();
      const q = new URLSearchParams();
      if (this._lastId > 0) q.set('since', String(this._lastId));
      this._es = new EventSource('/api/logs/stream' + (q.toString() ? '?' + q.toString() : ''));
      this._setMeta('连接中…');
      this._es.onopen = () => { this._retry = 1000; this._setMeta(''); };
      this._es.onmessage = (ev) => {
        let d;
        try { d = JSON.parse(ev.data); } catch { return; }
        if (d === null || typeof d !== 'object') return;
        if (this._plugin && d.plugin && d.plugin !== this._plugin) return;
        this._ingest(d);
      };
      // 面板在广播追不上时会推一帧具名事件 `lagged`（`{skipped}`）：这一段日志**真的没了**，
      // 明说一句比让用户以为"任务没输出"好。无名帧收不到它，得单独挂。
      this._es.addEventListener('lagged', (ev) => {
        let d = {};
        try { d = JSON.parse(ev.data); } catch { /* 就当地丢了 */ }
        this.addLine('warn', Date.now(), '（日志太多，中间漏了一段：' + String(d.skipped || '已丢弃') + '）');
      });
      this._es.onerror = () => {
        // EventSource 的自动重连**不带** `since` ⇒ 断线那段会丢。自己重开。
        this.stop();
        this._setMeta('重连中…');
        const wait = this._retry;
        this._retry = Math.min(this._retry * 2, 15000);
        setTimeout(() => { if (this.isConnected) this.start(); }, wait);
      };
    }

    stop() {
      if (this._es !== null) {
        this._es.onerror = null;
        this._es.close();
        this._es = null;
      }
    }
  }
  // ⚠ 注册要**紧跟着类定义**：末尾那串 define 在类之前执行，写在那儿会撞上 TDZ
  //   （class 不像 function 那样提升），报 `Cannot access 'PLog' before initialization`。
  define('p-log', PLog);

  // ---------------------------------------------------------------- 确认 / 输入弹窗
  //
  // 为什么要有：插件里到处是 `confirm('确定删除？')` / `prompt('这个账号叫什么？')`。
  // 那是**浏览器的原生对话框** —— 它是另一个窗口（macOS 上跑到屏幕中间），
  // 和面板长得完全不一样；在 iframe 里还可能被浏览器拦掉；移动端更是丑得没法看。
  // 面板早就不再用原生框了（自绘 askConfirm），但插件一直没得用。
  //
  // 这两个函数用面板自己的 `.dialog*` 画，返回 Promise：
  //   await PanelUI.confirm('删掉它？')                    → true / false
  //   await PanelUI.confirm({ text, title, okLabel, danger })
  //   await PanelUI.prompt({ title, label, value })       → string / null
  //
  // ⚠ 别在这两个函数里写业务：它们只负责「问一句、拿答案」，请求归插件。
  function mountDialog(parts) {
    const back = document.createElement('div');
    back.className = 'dialog-back';
    // 打个标记：页面上可能已经有插件自己的 `<p-dialog>`（也是 .dialog-back），
    // 测试和插件想「只找确认框」时靠它，别用 document.querySelector('.dialog-back') ——
    // 那会拿到先出现的那个（门禁自己踩了一次）。
    back.dataset.panelDialog = parts.kind || 'confirm';
    const dlg = document.createElement('div');
    dlg.className = 'dialog';
    dlg.setAttribute('role', 'dialog');
    dlg.setAttribute('aria-modal', 'true');
    const head = document.createElement('div');
    head.className = 'dialog-head';
    const title = document.createElement('span');
    title.textContent = parts.title || '确认操作';
    head.append(title);
    dlg.append(head);
    if (parts.text) {
      const p = document.createElement('p');
      p.className = 'hint';
      // 多行文本（插件里常写 '\n'）要保留换行，否则一整段挤成一行
      p.style.whiteSpace = 'pre-line';
      p.textContent = parts.text;
      dlg.append(p);
    }
    if (parts.field) dlg.append(parts.field);
    const acts = document.createElement('div');
    acts.className = 'dialog-actions';
    const cancel = document.createElement('button');
    cancel.type = 'button';
    cancel.className = 'btn';
    cancel.textContent = parts.cancelLabel || '取消';
    const ok = document.createElement('button');
    ok.type = 'button';
    ok.className = 'btn ' + (parts.danger ? 'btn-danger' : 'btn-primary');
    ok.textContent = parts.okLabel || '确定';
    acts.append(cancel, ok);
    dlg.append(acts);
    back.append(dlg);
    document.body.append(back);
    return { back, dlg, cancel, ok };
  }

  /** 关掉一层弹窗并清理监听。**焦点还给打开它的那个元素** —— 否则键盘用户会被扔回页面顶部。 */
  function unmount(back, onKey, resolve, value, restore) {
    back.remove();
    document.removeEventListener('keydown', onKey, true);
    if (restore && restore.focus) { try { restore.focus(); } catch (e) { /* 元素可能已经没了 */ } }
    resolve(value);
  }

  function dialogCommon(opts, build) {
    const o = typeof opts === 'string' ? { text: opts } : (opts || {});
    const restore = document.activeElement;
    return new Promise((resolve) => {
      const parts = build(o);
      const { back, cancel, ok } = mountDialog(parts);
      const finish = (v) => unmount(back, onKey, resolve, v, restore);
      const onKey = (ev) => {
        if (ev.key === 'Escape') { ev.preventDefault(); ev.stopPropagation(); finish(parts.cancelValue === undefined ? false : parts.cancelValue); }
        else if (ev.key === 'Enter' && !ev.isComposing) {
          // 输入框里的回车 = 确定；按钮上的回车交给按钮自己（避免双触发）
          if (ev.target && ev.target.tagName === 'BUTTON') return;
          ev.preventDefault(); finish(parts.okValue());
        }
      };
      cancel.addEventListener('click', () => finish(parts.cancelValue === undefined ? false : parts.cancelValue));
      ok.addEventListener('click', () => finish(parts.okValue()));
      back.addEventListener('click', (ev) => { if (ev.target === back) finish(parts.cancelValue === undefined ? false : parts.cancelValue); });
      document.addEventListener('keydown', onKey, true);
      (parts.focusEl || ok).focus();
    });
  }

  function pConfirm(opts) {
    return dialogCommon(opts, (o) => ({
      kind: 'confirm',
      title: o.title,
      text: o.text,
      okLabel: o.okLabel,
      cancelLabel: o.cancelLabel,
      danger: o.danger,
      cancelValue: false,
      okValue: () => true,
    }));
  }

  /** 输入框弹窗。返回输入的内容（**空串也算有效答案**），取消返回 null。 */
  function pPrompt(opts) {
    return dialogCommon(opts, (o) => {
      const field = document.createElement('label');
      field.className = 'field';
      if (o.label) {
        const lab = document.createElement('span');
        lab.className = 'field-label';
        lab.textContent = o.label;
        field.append(lab);
      }
      const input = document.createElement('input');
      input.className = 'input';
      input.type = 'text';
      input.value = o.value === undefined || o.value === null ? '' : String(o.value);
      if (o.placeholder) input.placeholder = o.placeholder;
      input.autocomplete = 'off';
      field.append(input);
      return {
        kind: 'prompt',
        title: o.title,
        text: o.text,
        okLabel: o.okLabel,
        cancelLabel: o.cancelLabel,
        field,
        focusEl: input,
        cancelValue: null,
        okValue: () => input.value,
      };
    });
  }

  // ── 给插件页的纯函数（不碰网络、不碰布局）────────────────────────────
  //  这四个东西四个插件的 webui 各实现过一遍：掩码、时间格式、提示行、忙碌按钮。
  //  收在面板一处，插件调用即可；口径要改只改这里。
  const pMask = (v, head = 3, tail = 4) => {
    const s = v === undefined || v === null ? '' : String(v);
    if (s === '' || s.length <= head + tail) return s;
    return s.slice(0, head) + '***' + s.slice(-tail);
  };

  const pPad = (n) => String(n).padStart(2, '0');

  /** 只有时分秒（页头、表格里够用）。 */
  const pFmtTime = (ms) => {
    if (!ms) return '—';
    const d = new Date(ms);
    return pPad(d.getHours()) + ':' + pPad(d.getMinutes()) + ':' + pPad(d.getSeconds());
  };

  /** 带日期（日志 / 运行记录）：只显示时分秒的话，隔天翻记录看不出是哪天。 */
  const pFmtLogTime = (ms) => {
    if (!ms) return '—';
    const d = new Date(ms);
    return pPad(d.getMonth() + 1) + '-' + pPad(d.getDate()) + ' ' + pFmtTime(ms);
  };

  /** 提示行：空串清空；kind = 'error' | 'warn' 切颜色（类在 /app.css：.hint-err / .hint-warn）。 */
  const pHint = (el, text, kind) => {
    const node = typeof el === 'string' ? document.getElementById(el) : el;
    if (!node) return;
    node.textContent = text === undefined || text === null ? '' : String(text);
    node.classList.remove('hint-err', 'hint-warn');
    if (kind === 'error' || kind === 'err') node.classList.add('hint-err');
    else if (kind === 'warn' || kind === 'warning') node.classList.add('hint-warn');
  };

  /** HTML 转义：拼 innerHTML 时把用户数据过一遍（四个插件里只有 mcloud 自己写了一份）。 */
  const pEsc = (s) => String(s === undefined || s === null ? '' : s)
    .replace(/&/g, '&amp;')
    .replace(/</g, '&lt;')
    .replace(/>/g, '&gt;')
    .replace(/"/g, '&quot;');

  /** 忙碌态：禁按钮 + 换文案，恢复时还原原文案。`<p-button>` 走它的 label/disabled。 */
  const pBusy = (btn, on, label) => {
    if (!btn) return;
    const isP = btn.tagName === 'P-BUTTON';
    const read = () => (isP ? btn.label : btn.textContent) || '';
    const write = (t) => { if (isP) btn.label = t; else btn.textContent = t; };
    if (on) {
      if (btn.dataset.busyText === undefined) btn.dataset.busyText = read();
      btn.disabled = true;
      write(label || '处理中…');
    } else {
      btn.disabled = false;
      if (btn.dataset.busyText !== undefined) {
        write(btn.dataset.busyText);
        delete btn.dataset.busyText;
      }
    }
  };

  /**
   * 插件页调自己后端的统一入口（原先是四个插件各写一份 call/api/post）。
   *
   * 语义（**定死**，别再各写一套）：
   *   · 2xx 且是 JSON → resolve 解析后的对象
   *   · 非 2xx → reject `PanelApiError`：`message` 取后端 `{error}`（带 `ref` 时附编号），
   *     并把 `status` / `data` 挂在错误上，调用方要不要区分自己看
   *   · 字符串 body 一律拒绝（后端 Json 提取器会回 415，见 pitfalls §7.14）
   */
  class PanelApiError extends Error {
    constructor(message, status, data) {
      super(message);
      this.name = 'PanelApiError';
      this.status = status;
      this.data = data;
    }
  }

  async function pApi(path, opts = {}) {
    const method = String(opts.method || (opts.body === undefined ? 'GET' : 'POST')).toUpperCase();
    /** @type {Record<string, string>} */
    const headers = Object.assign({}, opts.headers || {});
    if (opts.write) headers['x-panel'] = '1';

    let body;
    if (opts.body !== undefined) {
      if (typeof opts.body === 'string') {
        throw new Error('PanelUI.api：body 传对象，别传字符串（字符串不带 content-type，后端回 415）');
      }
      headers['content-type'] = 'application/json';
      body = JSON.stringify(opts.body);
    }

    const ac = typeof AbortController === 'function' ? new AbortController() : null;
    const timeoutMs = opts.timeoutMs === undefined ? 30000 : Number(opts.timeoutMs);
    const timer = ac && timeoutMs > 0 ? setTimeout(() => ac.abort(), timeoutMs) : null;

    let res;
    try {
      res = await fetch(path, {
        method,
        headers,
        body,
        credentials: 'same-origin',
        signal: ac ? ac.signal : undefined,
      });
    } catch (e) {
      const aborted = e && e.name === 'AbortError';
      throw new PanelApiError(
        aborted ? '等了 ' + Math.round(timeoutMs / 1000) + ' 秒还没回来' : '请求没发出去：' + ((e && e.message) || e),
        0,
        null,
      );
    } finally {
      if (timer) clearTimeout(timer);
    }

    const text = await res.text().catch(() => '');
    let data = null;
    try {
      data = text === '' ? null : JSON.parse(text);
    } catch {
      // 502「正忙」、403 少头这类回的是纯文本 —— 原样带上，别假装是 JSON
      data = null;
    }

    if (!res.ok) {
      let msg = (data && data.error) || text.trim().slice(0, 300) || 'HTTP ' + res.status;
      if (data && data.error && data.ref) msg += '（编号 ' + data.ref + '）';
      throw new PanelApiError(msg, res.status, data);
    }
    return data;
  }

  // ── 插件版本 ───────────────────────────────────────────────
  //  插件页要显示「当前装的是哪一版」，但版本的真源是面板读的清单（`<name>.panel.json`）——
  //  插件自己也有一份（package.json / 源码常量），多一份就多一次漂移的机会。
  //  这里统一从面板的 `/api/plugins/files` 取（同源会话 cookie 就够，插件页不需要令牌）。
  /** 从 `location.pathname` 里认出自己在哪个插件下：`/plugins/<name>/…`。 */
  const pluginNameFromPath = () => {
    const m = /^\/plugins\/([^/]+)/.exec(location.pathname || '');
    return m ? decodeURIComponent(m[1]) : '';
  };

  /** 面板里那份清单的版本（拿不到就空串，别编）。 */
  let verCache = null;
  async function pluginVersion(name) {
    const who = name || pluginNameFromPath();
    if (!who) return '';
    try {
      if (!verCache) {
        const r = await fetch('/api/plugins/files', { headers: { accept: 'application/json' } });
        verCache = r.ok ? await r.json() : null;
      }
      const list = (verCache && (verCache.files || verCache.plugins)) || [];
      const hit = list.find((f) => f && f.name === who);
      return (hit && hit.version) || '';
    } catch {
      return '';
    }
  }

  /** `<p-plugin-ver>`：右上角那枚「v0.7.0」。拿不到版本就整个隐藏，不显示空的 `v`。 */
  class PPluginVer extends HTMLElement {
    constructor() {
      super();
      this._done = false;
    }
    connectedCallback() {
      if (this._done) return;
      this._done = true;
      if (!this.classList.contains('plugin-ver')) this.classList.add('plugin-ver');
      this.setAttribute('title', '当前安装的版本（来自插件清单）');
      pluginVersion(this.getAttribute('name') || '')
        .then((v) => {
          if (!v) {
            this.hidden = true;
            return;
          }
          this.textContent = 'v' + v;
        })
        .catch(() => {
          this.hidden = true;
        });
    }
  }

  if (!customElements.get('p-plugin-ver')) customElements.define('p-plugin-ver', PPluginVer);

  // 给不需要元素、只想拿主题/版本的地方
  window.PanelUI = {
    theme: () => document.documentElement.getAttribute('data-theme') || 'light',
    setTheme,
    // 别再用版本号做特性判断（这里以前写死 '0.4.0'，早过期了）；要判断就用「有没有这个成员」。
    version: (document.querySelector('meta[name="panel-version"]') || {}).content || 'unknown',
    confirm: pConfirm,
    prompt: pPrompt,
    // 纯函数：四个插件各自重复实现过的那几个
    mask: pMask,
    esc: pEsc,
    fmtTime: pFmtTime,
    fmtLogTime: pFmtLogTime,
    hint: pHint,
    busy: pBusy,
    pluginVersion,
    api: pApi,
    ApiError: PanelApiError,
  };
})();
