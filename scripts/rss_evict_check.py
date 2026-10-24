#!/usr/bin/env python3
"""验证「插件被卸出内存之后，进程占用会不会真的降下来」。

为什么单独写一个：这是**平台相关**的问题，而且差别很大。
  - Linux：free 之后页通常还给内核，RSS 会掉。
  - macOS：实测 free 之后物理占用纹丝不动（分配 128MB、写满、free，占用仍是 132MB，
    malloc_zone_pressure_relief() 返回 0）。所以 macOS 上这个脚本大概率报"没降"，
    那是操作系统行为，不是面板的问题。

用法：

    python3 scripts/rss_evict_check.py --bin /path/to/plugin-panel

它会：起一个只跑临时插件实例 → 等插件被惰性卸出 → 对比卸出前后的占用 → 给结论。
全程 ~90 秒，不碰你的数据目录。
"""
import argparse
import os
import re
import shutil
import subprocess
import sys
import tempfile
import time
from pathlib import Path

EVICT_MARK = "卸下内存"


def rss_kb(pid):
    """进程占用（KB）。Linux 用 VmRSS，macOS 用 ps 的 RSS。"""
    if sys.platform.startswith("linux"):
        try:
            txt = Path("/proc/%d/status" % pid).read_text()
            m = re.search(r"VmRSS:\s+(\d+) kB", txt)
            return int(m.group(1)) if m else None
        except OSError:
            return None
    try:
        out = subprocess.check_output(["ps", "-o", "rss=", "-p", str(pid)], text=True)
        return int(out.strip()) if out.strip() else None
    except (subprocess.CalledProcessError, ValueError):
        return None


PLUGIN_SRC = """
import { register_cron } from 'host-api';
// 只注册一个远期任务：这样它既不忙，也不会被"即将触发"挡着不卸
register_cron('idle', '0 3 * * *', function () { return 'ok'; });
"""


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--bin", required=True, help="面板二进制路径（绝对路径）")
    ap.add_argument("--evict-ms", type=int, default=30000, help="闲置多久卸出（默认 30 秒）")
    ap.add_argument("--timeout", type=float, default=180, help="最多等多少秒")
    args = ap.parse_args()

    binary = Path(args.bin).resolve()
    if not binary.exists():
        sys.exit("找不到二进制：%s" % binary)

    work = Path(tempfile.mkdtemp(prefix="rss-evict-"))
    plugins = work / "plugins"
    tasks = work / "tasks"
    plugins.mkdir()
    tasks.mkdir()
    (plugins / "idle.js").write_text(PLUGIN_SRC)

    env = dict(os.environ)
    env.update({
        "BIND_ADDR": "127.0.0.1:0",
        "PLUGINS_DIR": str(plugins),
        "TASKS_DIR": str(tasks),
        "DB_PATH": str(work / "panel.db"),
        "PANEL_TOKEN": "t",
        "PANEL_ALLOW_PLAIN_TOKEN": "1",
        "PLUGIN_LAZY": "1",
        "PLUGIN_IDLE_EVICT_MS": str(args.evict_ms),
        # 提前装回的时长必须小于闲置时长，否则引擎会警告并自己改成 10 秒
        "PLUGIN_PRELOAD_MS": str(max(1000, min(10000, args.evict_ms // 2))),
        "PANEL_LOG_LEVEL": "info",
    })

    log_path = work / "panel.log"
    log_fh = open(log_path, "w")
    proc = subprocess.Popen([str(binary)], stdout=log_fh, stderr=subprocess.STDOUT, env=env)

    print("平台：%s" % sys.platform)
    print("二进制：%s" % binary)
    print("等插件加载并闲置 %d 秒后被卸出…\n" % (args.evict_ms // 1000))

    before = None
    after = None
    evicted_at = None
    t0 = time.time()
    try:
        while time.time() - t0 < args.timeout:
            time.sleep(2)
            cur = rss_kb(proc.pid)
            txt = log_path.read_text(errors="ignore")
            loaded = "插件已加载" in txt
            if cur and loaded and before is None:
                before = cur
                print("  加载后      %7.2f MB" % (cur / 1024))
            if EVICT_MARK in txt and evicted_at is None:
                evicted_at = time.time()
                print("  已卸出      %7.2f MB  ← 日志出现「卸下内存」" % (cur / 1024 if cur else 0))
            if evicted_at and time.time() - evicted_at > 20 and cur:
                after = cur
                print("  卸出 20 秒后 %7.2f MB" % (cur / 1024))
                break
    finally:
        proc.terminate()
        try:
            proc.wait(timeout=10)
        except subprocess.TimeoutExpired:
            proc.kill()
        log_fh.close()

    print()
    if before is None:
        print("✗ 面板没起来或插件没加载，看日志：%s" % log_path)
        return 2
    if evicted_at is None:
        print("✗ 等不到卸载。可能这个插件有活定时器/被标了长期加载，日志：%s" % log_path)
        return 2
    if after is None:
        print("✗ 没采到卸载后的数据，日志：%s" % log_path)
        return 2

    drop = (before - after) / 1024.0
    print("=" * 46)
    print("卸载前 %.2f MB → 卸载后 %.2f MB" % (before / 1024, after / 1024))
    print("差值 %+.2f MB" % (-drop))
    if drop >= 0.5:
        print("✓ 结论：卸出后内存**真的降下来了**（-%.2f MB）" % drop)
        print("  这说明这个平台上「闲时卸载」是有效的省内存手段。")
        return 0
    print("✗ 结论：卸出后内存**没有下降**（差 %.2f MB）" % drop)
    print("  这个平台在进程内释放内存后不会立刻还给系统。")
    print("  面板侧再怎么做（卸插件、定时还页）都改变不了这个数字，只有重启进程才有效。")
    return 1


if __name__ == "__main__":
    sys.exit(main())
