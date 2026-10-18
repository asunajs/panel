# @name hello-py
# @cron 0 3 * * *
# @params {"from":"tasks/py/hello.py"}
#
# Python 脚本是原生 CPython 进程：没有 host-api，只有环境变量和 stdout/stderr。
import json
import os

params = json.loads(os.environ.get("PANEL_PARAMS") or "{}")
print("hello from a Python script; params =", params)
