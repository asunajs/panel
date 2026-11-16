#!/usr/bin/env python3
"""perf-run 段启动器：自包含（本目录），不依赖 target/，重启后仍可用。

依赖：
  - 本目录 endurance.py（从仓库 scripts/endurance.py 冻结复制）
  - 本目录 libproc_metrics.py（沙箱里 ps 被禁，用 libproc 读 CPU/线程/footprint）
  - 本目录 usage-plugins/*.js（真实公开接口测试插件）
  - 本目录 plugin-panel（冻结 release 二进制）
环境变量：
  PERF_OUT_DIR        本段产物目录（必填；supervisor 传 runs/run-<时间戳>）
  PERF_USAGE_PLUGINS  插件源码目录（默认 ./usage-plugins）
"""
import importlib.util
import os
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
REPO = Path(os.environ.get("PERF_REPO", "/Users/catlair/mobile/workspace/rust-plugin-panel"))
sys.path.insert(0, str(HERE))

import libproc_metrics  # noqa: E402

spec = importlib.util.spec_from_file_location("endurance", HERE / "endurance.py")
endurance = importlib.util.module_from_spec(spec)
spec.loader.exec_module(endurance)

# 冻结副本里的 endurance.py 的 REPO 会算成仓库的父目录；显式改回仓库根。
endurance.REPO = REPO

# ps/top 在沙箱里不可用 → 用 libproc 顶替，否则 CPU / 线程全是 None。
endurance.cpu_seconds = libproc_metrics.cpu_seconds
endurance.threads = libproc_metrics.threads
endurance.fd_count = libproc_metrics.fd_count

OUT_DIR = Path(os.environ.get("PERF_OUT_DIR", str(HERE / "runs" / "adhoc"))).resolve()
OUT_DIR.mkdir(parents=True, exist_ok=True)
endurance.OUT_DIR = OUT_DIR

USAGE_PLUGINS = Path(os.environ.get("PERF_USAGE_PLUGINS", str(HERE / "usage-plugins")))
_orig_write_plugins = endurance.write_plugins


def write_plugins(pdir, rev):
    _orig_write_plugins(pdir, rev)
    if USAGE_PLUGINS.is_dir():
        for f in sorted(USAGE_PLUGINS.glob("*.js")):
            (Path(pdir) / f.name).write_text(f.read_text(encoding="utf-8"), encoding="utf-8")
    fast_iv = os.environ.get("PERF_FAST_IV")
    if fast_iv:
        _f = Path(pdir) / "perf-js.js"
        _f.write_text(
            _f.read_text(encoding="utf-8").replace("'50ms'", "'" + fast_iv + "'"),
            encoding="utf-8",
        )



endurance.write_plugins = write_plugins

# 追加峰值内存（footprint 的 phys_footprint_peak）到原始 jsonl。
_orig_sample = endurance.Runner.sample


def sample(self):
    row = _orig_sample(self)
    if self.panel_alive():
        pid = self.proc.pid
        cur, peak = libproc_metrics.footprint_kb(pid)
        row["rss_footprint_kb"] = cur
        row["rss_peak_kb"] = peak
        row["task_rss_kb"] = libproc_metrics.rss_task_kb(pid)
    return row


endurance.Runner.sample = sample

if __name__ == "__main__":
    sys.exit(endurance.main())
