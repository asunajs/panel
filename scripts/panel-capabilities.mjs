/// 面板给插件的能力 ↔ **首次出现的面板版本**。
///
/// 为什么要有这张表：插件的 `min_panel` 一旦写错，两个方向都难受 ——
///   · **写低了**：用户的面板版本够不着那些能力，装上去跑一半报「不是函数」，甚至页面打不开；
///   · **写高了**：插件里就会长出一堆「其实不需要」的兼容分支，谁也不敢删。
/// 所以「用了什么 → 至少要哪版」必须是**一处真源**，而不是凭印象写。
///
/// 版本值怎么来的：对每条能力在面板仓里找到首次引入它的提交，再用
/// `git describe --tags --contains <commit>` 反查第一个包含它的 release tag。
/// 重新核对：`node scripts/check-plugin-compat.mjs --explain`
///
/// 注意：
///   · `since` 写的是**面板版本**（不是插件版本）；
///   · 面板自己还没发版的能力标 `dev`，含义是「下一个 release」——
///     发版前跑 `--strict` 会让它显形，提醒把 min_panel 提上去。

export const CAPABILITIES = [
  // 清单字段
  { id: 'manifest.version', since: '0.6.0', where: 'manifest', pattern: /^\s*"version"\s*:/m, note: '清单 version（商店显示已装哪版）' },
  { id: 'manifest.on_panel_open', since: '0.6.0', where: 'manifest', pattern: /"on_panel_open"\s*:/, note: '打开面板时回调一次' },

  // 面板控件（web/ui.js 注册的自定义元素）
  { id: 'ui.p-button', since: '0.4.0', where: 'webui', pattern: /<p-button\b/ },
  { id: 'ui.p-field', since: '0.4.0', where: 'webui', pattern: /<p-field\b/ },
  { id: 'ui.p-dialog', since: '0.4.0', where: 'webui', pattern: /<p-dialog\b/ },
  { id: 'ui.p-badge', since: '0.4.0', where: 'webui', pattern: /<p-badge\b/ },
  { id: 'ui.p-card', since: '0.4.0', where: 'webui', pattern: /<p-card\b/ },
  { id: 'ui.p-empty', since: '0.4.0', where: 'webui', pattern: /<p-empty\b/ },
  { id: 'ui.p-select', since: '0.4.0', where: 'webui', pattern: /<p-select\b/ },
  { id: 'ui.p-number', since: '0.4.0', where: 'webui', pattern: /<p-number\b/ },
  { id: 'ui.p-switch', since: '0.4.0', where: 'webui', pattern: /<p-switch\b/ },
  { id: 'ui.p-code', since: '0.4.0', where: 'webui', pattern: /<p-code\b/ },
  { id: 'ui.p-tab', since: '0.4.0', where: 'webui', pattern: /<p-tab\b/ },
  { id: 'ui.p-multiselect', since: '0.4.0', where: 'webui', pattern: /<p-multiselect\b/ },
  { id: 'ui.p-sms-code', since: '0.4.0', where: 'webui', pattern: /<p-sms-code\b/, note: '发码格（R11 要求用它）' },
  { id: 'ui.p-log', since: '0.7.0', where: 'webui', pattern: /<p-log\b/, note: '实时日志（R14 要求任务型插件用）' },
  { id: 'ui.p-plugin-ver', since: '0.8.3', where: 'webui', pattern: /<p-plugin-ver\b/, note: '右上角版本药丸（版本来自清单，第 257 轮）' },

  // 面板 JS 面（window.PanelUI）
  { id: 'PanelUI.confirm', since: '0.7.0', where: 'webui', pattern: /PanelUI\.confirm\s*\(/ },
  { id: 'PanelUI.prompt', since: '0.7.0', where: 'webui', pattern: /PanelUI\.prompt\s*\(/ },
  { id: 'PanelUI.mask', since: '0.8.0', where: 'webui', pattern: /PanelUI\.mask\s*\(/ },
  { id: 'PanelUI.fmtTime', since: '0.8.0', where: 'webui', pattern: /PanelUI\.fmtTime\s*\(/ },
  { id: 'PanelUI.fmtLogTime', since: '0.8.0', where: 'webui', pattern: /PanelUI\.fmtLogTime\s*\(/ },
  { id: 'PanelUI.hint', since: '0.8.0', where: 'webui', pattern: /PanelUI\.hint\s*\(/ },
  { id: 'PanelUI.pluginVersion', since: '0.8.3', where: 'webui', pattern: /PanelUI\.pluginVersion\s*\(/, note: '读插件的清单版本（配合 <p-plugin-ver>，第 257 轮）' },
  { id: 'PanelUI.busy', since: '0.8.0', where: 'webui', pattern: /PanelUI\.busy\s*\(/ },
  { id: 'PanelUI.esc', since: '0.8.0', where: 'webui', pattern: /PanelUI\.esc\s*\(/ },
  { id: 'PanelUI.api', since: '0.8.0', where: 'webui', pattern: /PanelUI\.api\s*\(/ },

  // 宿主 API（panel:host）
  { id: 'host.run_once', since: '0.7.0', where: 'server', pattern: /host\.run_once\s*\(/ },
  { id: 'host.set_job_paused', since: '0.4.0', where: 'server', pattern: /set_job_paused\s*\(/ },
  { id: 'host.sqlite_exec_sync', since: '0.4.0', where: 'server', pattern: /sqlite_exec_sync\s*\(/ },
  { id: 'host.ws_open', since: '0.6.0', where: 'server', pattern: /ws_open\s*\(/ },
  { id: 'host.smtp_send', since: '0.6.0', where: 'server', pattern: /smtp_send\s*\(/ },
  { id: 'host.panel_at_least', since: '0.4.0', where: 'server', pattern: /panel_at_least\s*\(/ },

  // LLRT 运行时（由面板内置）
  { id: 'runtime.fs.existsSync', since: '0.8.0', where: 'server', pattern: /\.existsSync\s*\(/ },
  { id: 'runtime.CompressionStream', since: '0.8.0', where: 'server', pattern: /\bCompressionStream\b/ },
  { id: 'runtime.DecompressionStream', since: '0.8.0', where: 'server', pattern: /\bDecompressionStream\b/ },

  // 插件页响应头（插件自己声明的 CSP）
  { id: 'webui.x-panel-csp', since: '0.8.2', where: 'server', pattern: /['"]x-panel-csp['"]\s*:/, note: '插件页声明自己需要的 CSP（滑块页要用）' },
  { id: 'host.log.debug', since: '0.8.2', where: 'server', pattern: /['"]debug['"]\s*\)|log\(\s*['"]debug['"]/, note: '第四级日志（空闲卸载不再挤占 info）' },
];

/** 版本比较：只认 x.y.z；`dev` 视为比任何已发布版本都大。 */
export function cmpVersion(a, b) {
  const parse = (v) => (v === 'dev' ? [999, 0, 0] : String(v).split('.').map((n) => Number(n) || 0));
  const pa = parse(a);
  const pb = parse(b);
  for (let i = 0; i < Math.max(pa.length, pb.length); i += 1) {
    const d = (pa[i] || 0) - (pb[i] || 0);
    if (d !== 0) return d > 0 ? 1 : -1;
  }
  return 0;
}

export function maxVersion(list) {
  return list.reduce((acc, v) => (cmpVersion(v, acc) > 0 ? v : acc), '0.0.0');
}
