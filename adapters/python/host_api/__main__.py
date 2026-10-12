"""host_api 运行器：``python3 -m host_api <plugin.py> [--once] [--db <path>]``。

行为（对齐 Rust 侧引擎）：

1. 用 importlib 从**路径**加载插件文件（插件名 = 文件名去扩展名）
2. 执行插件顶层代码 → 插件里的 ``register_cron`` / ``every`` 完成注册
3. 逐秒 tick（实际是 deadline 驱动：睡到最近一个到期时刻，上限 1 秒），
   触发所有到期任务；handler 抛异常只记 ``last_error``，进程继续
4. ``--once`` 只跑一轮就退出（强制每个任务各触发一次，用于冒烟）

退出码：0 = 全部成功；1 = 有任务失败（便于 CI 判定）；2 = 用法/加载错误。
"""

import importlib.util
import os
import re
import sys
import time
from datetime import datetime

try:
    from . import HostApiError, bind, fmt_ts, get_host
except ImportError:  # 直接 python3 host_api/__main__.py 时
    sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
    from host_api import HostApiError, bind, fmt_ts, get_host

USAGE = ("用法: python3 -m host_api <plugin.py> [--once] [--db <path>] [--run-for <seconds>]\n"
         "                       [--job <name>=<expr>]...\n"
         "  --job <n>=<e>  把 register_handler 登记过的处理函数 n 按表达式 e 挂上调度，可重复\n"
         "                 （等价于嵌入式宿主里「在面板上新建任务」这一步）")


def _usage_error(msg):
    sys.stderr.write("[runner] {}\n{}\n".format(msg, USAGE))
    return 2


def _parse_args(argv):
    plugin_path = None
    once = False
    db_path = None
    run_for = None
    jobs = []

    i = 0
    while i < len(argv):
        arg = argv[i]
        if arg == "--once":
            once = True
        elif arg == "--db":
            i += 1
            if i >= len(argv):
                raise HostApiError("--db 需要一个路径")
            db_path = argv[i]
        elif arg.startswith("--db="):
            db_path = arg[len("--db="):]
        elif arg == "--job":
            i += 1
            if i >= len(argv):
                raise HostApiError("--job 需要一个 <name>=<expr>")
            jobs.append(argv[i])
        elif arg.startswith("--job="):
            jobs.append(arg[len("--job="):])
        elif arg == "--run-for":
            i += 1
            if i >= len(argv):
                raise HostApiError("--run-for 需要一个秒数")
            try:
                run_for = float(argv[i])
            except ValueError:
                raise HostApiError("--run-for 的值不是数字: {}".format(argv[i]))
        elif arg in ("-h", "--help"):
            print(USAGE)
            raise SystemExit(0)
        elif arg.startswith("-"):
            raise HostApiError("未知参数: {}".format(arg))
        elif plugin_path is None:
            plugin_path = arg
        else:
            raise HostApiError("多余的参数: {}".format(arg))
        i += 1

    if plugin_path is None:
        raise HostApiError("缺少插件文件路径")
    return plugin_path, once, db_path, run_for, jobs


def load_plugin(path):
    """用 importlib 从路径加载插件，返回 (插件名, module)。"""
    path = os.path.abspath(path)
    if not os.path.isfile(path):
        raise HostApiError("插件文件不存在: {}".format(path))

    plugin_name = os.path.splitext(os.path.basename(path))[0]
    module_name = "plugin_" + re.sub(r"\W", "_", plugin_name)
    spec = importlib.util.spec_from_file_location(module_name, path)
    if spec is None or spec.loader is None:
        raise HostApiError("无法为 {} 创建 import spec".format(path))

    module = importlib.util.module_from_spec(spec)
    # 先登记进 sys.modules：插件内部 self-import / 后续 reload 都要靠它
    sys.modules[module_name] = module
    spec.loader.exec_module(module)
    return plugin_name, module


def _print_summary(host):
    sys.stdout.write("[runner] 插件 `{}` 注册了 {} 个任务：\n".format(host.plugin, len(host.tasks)))
    for task in host.tasks:
        sys.stdout.write("[runner]   - {}\n".format(task.describe()))
    sys.stdout.flush()


def _report(host, fired, label):
    ok_count = 0
    fail_count = 0
    for task, ok in fired:
        if ok:
            ok_count += 1
        else:
            fail_count += 1
    sys.stdout.write(
        "[runner] {}：成功 {} / 失败 {}（共 {} 次触发）\n".format(
            label, ok_count, fail_count, len(fired)
        )
    )
    for task in host.tasks:
        if task.last_error:
            sys.stdout.write("[runner]   ! {} last_error = {}\n".format(task.name, task.last_error))
    sys.stdout.flush()
    return fail_count


def main(argv=None):
    argv = list(sys.argv[1:] if argv is None else argv)
    try:
        plugin_path, once, db_path, run_for, jobs = _parse_args(argv)
    except HostApiError as exc:
        return _usage_error(str(exc))

    # 插件名 = 文件名去扩展名，payload.plugin 用的就是它
    plugin_name = os.path.splitext(os.path.basename(plugin_path))[0]
    # 必须在 exec 插件之前绑定：插件顶层就会调 register_cron
    host = bind(plugin=plugin_name, db_path=db_path)

    try:
        plugin_name, _module = load_plugin(plugin_path)
    except SystemExit as exc:
        sys.stderr.write("[runner] 插件在加载阶段调用了 sys.exit({})\n".format(exc.code))
        return 2
    except BaseException:
        import traceback

        sys.stderr.write("[runner] 插件加载失败:\n")
        traceback.print_exc(file=sys.stderr)
        return 2

    # --job 必须在插件顶层代码跑完之后才挂：那时 register_handler 才登记完
    for spec in jobs:
        if "=" not in spec:
            return _usage_error("--job 需要 <name>=<expr> 形式: {}".format(spec))
        jname, jexpr = spec.split("=", 1)
        try:
            host.schedule_handler(jname.strip(), jexpr.strip())
        except (ValueError, HostApiError) as exc:
            return _usage_error("--job {} 失败: {}".format(spec, exc))
        sys.stdout.write("[runner] 已挂上任务 `{}`（{}）\n".format(jname.strip(), jexpr.strip()))

    if host.handlers:
        sys.stdout.write("[runner] register_handler 登记了 {} 个处理函数: {}\n"
                         .format(len(host.handlers), ", ".join(sorted(host.handlers))))
    sys.stdout.flush()

    _print_summary(host)

    if not host.tasks:
        sys.stdout.write("[runner] 插件没有注册任何任务\n")

    failures = 0

    if once:
        sys.stdout.write("[runner] --once：强制触发 {} 个任务各一次（scheduledAt = firedAt = now）\n"
                         .format(len(host.tasks)))
        sys.stdout.flush()
        failures += _report(host, host.fire_all(datetime.now()), "--once 结果")
        host.close()
        return 1 if failures else 0

    sys.stdout.write("[runner] 进入调度循环（Ctrl-C 退出）\n")
    sys.stdout.flush()
    deadline = time.monotonic() + run_for if run_for is not None else None
    total = []
    try:
        while True:
            now = datetime.now()
            total.extend(host.tick(now))
            if deadline is not None and time.monotonic() >= deadline:
                break
            # deadline 驱动：睡到最近一个到期时刻，上限 1 秒（逐秒 tick）
            time.sleep(host.sleep_hint(now, max_tick=1.0))
    except KeyboardInterrupt:
        sys.stdout.write("[runner] 收到 Ctrl-C，退出调度循环\n")

    failures += _report(host, total, "调度循环结束")
    host.close()
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
