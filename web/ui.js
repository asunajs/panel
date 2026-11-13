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
