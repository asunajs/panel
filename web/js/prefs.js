/*
  web/js/prefs.js —— 只影响「这块界面怎么显示」的浏览器本地偏好。

  和面板状态（`/api/state`）、插件配置、任务调度分开：这里的东西换台机器、
  换个浏览器就各是各的，不改变任何实际行为。主题也是同一类（见 app.js）。
  所以存 localStorage，不进 `data/panel.db`。

  ⚠ 读不到 localStorage（隐私模式、禁用存储）时不能让页面挂掉：
  回落到模块内存里的一份值，至少本次打开期间开关仍然有效。
*/

/// 插件卡片上的「执行引擎 / 自定义解释器路径」整块设置。
///
/// 默认**隐藏**：绝大多数插件用内置 QuickJS 就够了，下拉框和路径输入框
/// 常驻在每张卡片上只会把真正要看的信息（状态、任务数）挤下去。
/// 需要按插件切 Node / Bun 的人，在「设置 → 插件」里打开它。
const ENGINE_UI_KEY = 'panel-plugin-engine-ui-v1';

/// 存不下时的兜底值（只在本次页面生命周期内有效）。
let fallback = false;

export function showPluginEngineUi() {
  try {
    const v = localStorage.getItem(ENGINE_UI_KEY);
    // 没存过 = 默认隐藏；存过就按存的来（'0' 也算明确关闭）
    return v === null ? fallback : v === '1';
  } catch {
    return fallback;
  }
}

export function setPluginEngineUi(on) {
  fallback = on === true;
  try {
    localStorage.setItem(ENGINE_UI_KEY, fallback ? '1' : '0');
  } catch {
    /* 隐私模式下写不进去：本次打开期间用 fallback，下次回到默认隐藏 */
  }
}
