#!/usr/bin/env python3
"""libproc 指标补丁：macOS 沙箱里 \`ps\`/\`top\` 被禁（"Operation not permitted"），
endurance.py 原来的 cpu_seconds / threads 会全变成 None。

这里用 libproc 的 proc_pidinfo(PROC_PIDTASKINFO) 直接读面板进程：
  - pti_total_user / pti_total_system：**单位不是纳秒**（libproc 头文件是错的/历史遗留），
    实测是 mach absolute time 的 tick —— 必须过 mach_timebase_info 换算。
    M4 上是 24 MHz（numer=125, denom=3，41.67ns/tick），直接把 tick 当纳秒会低报 ~42 倍。
  - pti_threadnum：线程数
  - pti_resident_size：RSS（报告仍用 footprint 的 phys_footprint，口径更真实）
"""

import ctypes
import ctypes.util
import subprocess

_libc = ctypes.CDLL(ctypes.util.find_library("c"), use_errno=True)
PROC_PIDTASKINFO = 4


class ProcTaskInfo(ctypes.Structure):
    _fields_ = [
        ("pti_virtual_size", ctypes.c_uint64),
        ("pti_resident_size", ctypes.c_uint64),
        ("pti_total_user", ctypes.c_uint64),
        ("pti_total_system", ctypes.c_uint64),
        ("pti_threads_user", ctypes.c_uint64),
        ("pti_threads_system", ctypes.c_uint64),
        ("pti_policy", ctypes.c_int32),
        ("pti_faults", ctypes.c_int32),
        ("pti_pageins", ctypes.c_int32),
        ("pti_cow_faults", ctypes.c_int32),
        ("pti_messages_sent", ctypes.c_int32),
        ("pti_messages_received", ctypes.c_int32),
        ("pti_syscalls_mach", ctypes.c_int32),
        ("pti_syscalls_unix", ctypes.c_int32),
        ("pti_csw", ctypes.c_int32),
        ("pti_threadnum", ctypes.c_int32),
        ("pti_numrunning", ctypes.c_int32),
        ("pti_priority", ctypes.c_int32),
    ]


class MachTimebaseInfo(ctypes.Structure):
    _fields_ = [("numer", ctypes.c_uint32), ("denom", ctypes.c_uint32)]


_tb = MachTimebaseInfo()
_libc.mach_timebase_info(ctypes.byref(_tb))
TICK_NS = _tb.numer / _tb.denom if _tb.denom else 1.0


def taskinfo(pid):
    info = ProcTaskInfo()
    rc = _libc.proc_pidinfo(pid, PROC_PIDTASKINFO, 0, ctypes.byref(info), ctypes.sizeof(info))
    if rc != ctypes.sizeof(info):
        return None
    return info


def cpu_seconds(pid):
    """累计 CPU（user+sys）秒。"""
    info = taskinfo(pid)
    if info is None:
        return None
    return (info.pti_total_user + info.pti_total_system) * TICK_NS / 1e9


def threads(pid):
    info = taskinfo(pid)
    return None if info is None else int(info.pti_threadnum)


def rss_task_kb(pid):
    """proc taskinfo 的 resident_size（KB）；报告主口径仍用 footprint。"""
    info = taskinfo(pid)
    return None if info is None else info.pti_resident_size / 1024.0


def fd_count(pid):
    """lsof 在沙箱里可用（ps 不行）。"""
    try:
        out = subprocess.run(["lsof", "-p", str(pid)], capture_output=True, text=True, timeout=30)
        return max(0, len(out.stdout.strip().splitlines()) - 1)
    except Exception:
        return None


def footprint_kb(pid):
    """返回 (phys_footprint KB, phys_footprint_peak KB)，与 scripts/soak.py 同口径。"""
    import re
    try:
        out = subprocess.run(["footprint", str(pid)], capture_output=True, text=True, timeout=30).stdout
    except Exception:
        return None, None
    cur = peak = None
    m = re.search(r"phys_footprint:\s*([\d.]+)\s*(KB|MB|GB)", out)
    if m:
        cur = float(m.group(1)) * {"KB": 1, "MB": 1024, "GB": 1024 * 1024}[m.group(2)]
    m = re.search(r"phys_footprint_peak:\s*([\d.]+)\s*(KB|MB|GB)", out)
    if m:
        peak = float(m.group(1)) * {"KB": 1, "MB": 1024, "GB": 1024 * 1024}[m.group(2)]
    return cur, peak
