/**
 * 任务状态 → 徽章样式 + **给人看的字**，以及时刻的显示格式。
 *
 * # 为什么单独一个模块
 *
 * 这份映射原来长在 `views/task-list.js` 里（私有常量），而仪表盘的「任务概览」
 * 也有一列状态 —— 它当时是**直接把后端原值吐出来**的，于是同一个界面里
 * 列表页写「运行中」、仪表盘写 `running`。一处写中文、一处露英文，看起来
 * 就像"没设计过"；两处各存一份映射迟早还会漂，所以抽出来共用。
 *
 * # 取值
 *
 * 后端保证的就是下面这几个（见 `src/web.rs` 的 `job_rows`）。
 * 认不出来的状态**原样显示**，不猜也不吞 —— 新增一个状态时，
 * 宁可看到一个陌生的英文词，也不要看到「未知」而不知道到底是哪个。
 */

export const STATUS_MAP = {
  running: { cls: 'running', text: '运行中' },
  paused: { cls: 'idle', text: '已暂停' },
  tripped: { cls: 'failed', text: '已熔断' },
  error: { cls: 'failed', text: '执行出错' },
  idle: { cls: 'idle', text: '待触发' },
  // 面板删掉的（墓碑）：插件代码仍然声明它，所以它还在库里但不调度
  deleted: { cls: 'idle', text: '已删除' },
};

/** 状态 → `{cls, text}`。认不出来的给中性样式 + 原值。 */
export function statusBadge(status) {
  return STATUS_MAP[status] || { cls: 'idle', text: String(status ?? '—') };
}

/**
 * 时刻只显示到**分钟**。
 *
 * 后端给的是 `2026-10-17 08:00:00.000`，而调度最小粒度就是分钟 ——
 * 秒和毫秒挂在表格里只是噪声（`08:00:00.000` 比 `08:00` 难扫得多）。
 * ⚠ 只截**显示**：API 那个字段原样不动（`scripts/smoke.py` 拿它做断言）。
 */
export function minuteOf(ts) {
  if (!ts) return '—';
  return String(ts).replace('T', ' ').slice(0, 16);
}
