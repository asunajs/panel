import ast
from pathlib import Path

targets = [
    Path('/Users/catlair/plugin-panel-perf/endurance.py'),
    Path('/Users/catlair/plugin-panel-perf-b/endurance.py'),
    Path('/Users/catlair/mobile/workspace/rust-plugin-panel/scripts/endurance.py'),
]

method = '''
    def _planned_total(self) -> float:
        """计划时长：启动参数 + 运行期延长（EXTEND_HOURS 文件）。

        为什么要这个文件：total 原来只在 run() 开头算一次，想延长就只能重启
        一段新的 —— 等于把已经跑出来的连续曲线剪断。现在只要写
        echo 168 > <段目录>/EXTEND_HOURS，下一轮循环就按新时长继续同一个进程、
        同一个数据文件，不再重启。
        """
        base = self.args.seconds or self.args.hours * 3600
        try:
            f = Path(self.workdir) / 'EXTEND_HOURS'
            if f.exists():
                want = float(f.read_text(encoding='utf-8').strip()) * 3600
                if want > base:
                    if getattr(self, '_extended_to', None) != want:
                        self._extended_to = want
                        print('[extend] 计划时长延长到 %.2f 小时（%.0fs）' % (want / 3600, want), flush=True)
                    return want
        except Exception:
            pass
        return base
'''

for p in targets:
    if not p.exists():
        print('skip', p); continue
    s = p.read_text()
    if '_planned_total' in s:
        print('already', p.name); continue
    old = '    def run(self) -> int:\n        total = self.args.seconds or self.args.hours * 3600\n'
    assert s.count(old) == 1, p
    s = s.replace(old, '    def run(self) -> int:\n        total = self._planned_total()\n')
    anchor = '    def run(self) -> int:'
    s = s.replace(anchor, method.rstrip() + '\n\n' + anchor, 1)
    old2 = '        while True:\n            elapsed = time.time() - self.started\n            if elapsed >= total or self.stopping:\n                break'
    new2 = '        while True:\n            elapsed = time.time() - self.started\n            total = self._planned_total()\n            if elapsed >= total or self.stopping:\n                break'
    assert s.count(old2) == 1, p
    s = s.replace(old2, new2)
    p.write_text(s)
    ast.parse(s)
    print('patched', p)
