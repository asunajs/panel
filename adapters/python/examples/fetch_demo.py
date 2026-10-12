# examples/fetch_demo.py —— http_fetch 自测插件
#
# 起一个本地回环 HTTP server 当被测端，然后每 1s 用 http_fetch 打一次。
# 只依赖标准库的 http.server + threading（PocketPy 里没有这两个模块，
# 所以这个 example 是 CPython 专用的；核心 API 的示例看 demo.py）。
#
# 跑：python3 -m host_api examples/fetch_demo.py --once
#     python3 -m host_api examples/fetch_demo.py --run-for 3

import json
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

from host_api import every, http_fetch, log


class _Handler(BaseHTTPRequestHandler):
    def do_GET(self):
        if self.path == "/missing":
            body = b"not found"
            self.send_response(404)
        else:
            body = json.dumps({"ok": True, "path": self.path, "x-demo": self.headers.get("X-Demo", "")}).encode("utf-8")
            self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def do_POST(self):
        length = int(self.headers.get("Content-Length", "0") or 0)
        raw = self.rfile.read(length) if length else b""
        body = json.dumps({"method": "POST", "echo": raw.decode("utf-8")}).encode("utf-8")
        self.send_response(201)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, fmt, *args):
        pass  # 别把 server 自己的访问日志混进插件日志


def start_server():
    server = ThreadingHTTPServer(("127.0.0.1", 0), _Handler)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    return server


SERVER = start_server()
BASE = "http://127.0.0.1:{}".format(SERVER.server_address[1])
log("本地回环 server 已启动: {}".format(BASE))


def on_ping(payload):
    opts = {"method": "GET", "headers": {"X-Demo": "host_api"}, "timeout_ms": 3000}
    resp = http_fetch(BASE + "/api/ping", opts)
    log("GET /api/ping -> status={} content-type={} body={}".format(
        resp["status"], resp["headers"].get("Content-Type"), resp["body"]))

    resp2 = http_fetch(BASE + "/echo", {"method": "POST", "body": '{"n": 1}', "timeout_ms": 3000})
    log("POST /echo -> status={} body={}".format(resp2["status"], resp2["body"]))

    resp3 = http_fetch(BASE + "/missing", {"timeout_ms": 3000})
    log("GET /missing -> status={} body={}".format(resp3["status"], resp3["body"]))


every("ping", "1s", on_ping)
log("已注册 ping 任务")
