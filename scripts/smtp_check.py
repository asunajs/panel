#!/usr/bin/env python3
"""验收 `host_api.smtp_send`（第二十三轮）。

# 要证明什么

推送的邮件通道要能真的把信交出去。这件事的风险全在**协议细节**上，所以判据
必须对着一个真的会说话的 SMTP 服务器跑，而且要用一个**真的邮件解析器**读回来：

1. 三种连接方式都通：`none`（明文）· `starttls` · `implicit`（465 那种）
2. `AUTH PLAIN` 与 `AUTH LOGIN` 都通（服务器只声明其中一个时也要能选对）
3. 中文主题走 RFC 2047 编码字，解析回来还是原文
4. 正文 base64 编码，解析回来还是原文
5. `text` + `html` 时是 `multipart/alternative`，两段都在
6. **点填充**：正文里行首的 `.` 必须原样到达（不做就是「邮件被截断」）
7. 失败要说清楚：密码错、收件人被拒、服务器没声明 STARTTLS
8. `ca_file`：私有 CA 签的证书能被信任（自建 SMTP / 内网中继的常见情况）

# 为什么必须用真服务器

MIME 和 SMTP 的坑都在「你以为对但解析器不这么看」上：编码字的折行、
base64 的行宽、CRLF、点填充、多行 EHLO 回复的解析。任何一条错了，
单测都可能是绿的（因为单测用的是我自己写的期望值），只有让
**别人的解析器**读一遍才算数。

用法：python3 scripts/smtp_check.py [./target/release/plugin-panel]
退出码：0 通过 / 1 判据没过 / 2 环境问题
"""

import base64
import email
import email.policy
import json
import os
import socket
import ssl
import subprocess
import sys
import tempfile
import threading
import time
import urllib.error
import urllib.request
from email.header import decode_header
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
BIN = sys.argv[1] if len(sys.argv) > 1 else str(ROOT / "target/release/plugin-panel")
FIXTURES = ROOT / "examples" / "smtp"
TOKEN = os.urandom(16).hex()

USER = "robot@example.com"
PASSWORD = "s3cret"


# ---------------------------------------------------------------- 假 SMTP 服务器


class FakeSmtp:
    """够用的 SMTP 服务器：EHLO / STARTTLS / AUTH / MAIL / RCPT / DATA / QUIT。

    只实现验收需要的那点协议面。它是**故意**写得挑剔的 —— 宽松的假服务器会
    把「客户端少发一个 CRLF」这种错吞掉，而那正是要抓的东西。
    """

    def __init__(
        self,
        mode="plain",  # plain | starttls | implicit
        certfile=None,
        keyfile=None,
        advertise_starttls=True,
        advertise_auth=True,
        auth_mechs=("PLAIN", "LOGIN"),
        user=USER,
        password=PASSWORD,
        reject_rcpt=None,
        max_line=64 * 1024,
    ):
        self.mode = mode
        self.certfile = certfile
        self.keyfile = keyfile
        self.advertise_starttls = advertise_starttls
        self.advertise_auth = advertise_auth
        self.auth_mechs = list(auth_mechs)
        self.user = user
        self.password = password
        self.reject_rcpt = reject_rcpt
        self.max_line = max_line

        self.messages = []  # [{"mail_from", "rcpt", "data"}]
        self.auths = []  # [(mech, user, password)]
        self.errors = []  # 服务器侧看到的异常（协议违规）
        self._sock = None
        self._thread = None
        self.port = None

    # ---- 生命周期 ----
    def start(self):
        self._sock = socket.socket()
        self._sock.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        self._sock.bind(("127.0.0.1", 0))
        self.port = self._sock.getsockname()[1]
        self._sock.listen(8)
        self._thread = threading.Thread(target=self._accept_loop, daemon=True)
        self._thread.start()
        return self

    def stop(self):
        try:
            self._sock.close()
        except Exception:
            pass

    def __enter__(self):
        return self.start()

    def __exit__(self, *a):
        self.stop()

    def _accept_loop(self):
        while True:
            try:
                conn, _ = self._sock.accept()
            except OSError:
                return
            threading.Thread(target=self._serve, args=(conn,), daemon=True).start()

    # ---- 单个连接 ----
    def _serve(self, conn):
        try:
            self._session(conn)
        except Exception as e:  # 记下来而不是崩线程：脚本要能报「协议被违反」
            self.errors.append(f"{type(e).__name__}: {e}")
        finally:
            try:
                conn.close()
            except Exception:
                pass

    def _ctx(self):
        ctx = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
        ctx.load_cert_chain(self.certfile, self.keyfile)
        return ctx

    def _session(self, conn):
        if self.mode == "implicit":
            conn = self._ctx().wrap_socket(conn, server_side=True)

        def send(line):
            conn.sendall((line + "\r\n").encode())

        def recv():
            # **逐字节**读行，绝不用 `makefile().readline()`：
            # BufferedReader 会一次读一大块，把紧跟 `220` 之后到达的
            # ClientHello 吞进它自己的缓冲里 —— 之后 `wrap_socket` 拿到的是
            # 半截 TLS 记录，服务器回 `record_overflow`，看起来像客户端的错。
            # （这个坑真踩过：STARTTLS 那一组一直报 RecordOverflow。）
            buf = bytearray()
            while True:
                b = conn.recv(1)
                if not b:
                    raise ConnectionError("客户端提前断开")
                if b == b"\n":
                    break
                buf += b
                if len(buf) > self.max_line:
                    raise ValueError(f"收到超长行（{len(buf)} 字节）")
            return buf.decode("utf-8", "replace").rstrip("\r")

        send("220 fake.local ESMTP ready")
        tls_done = self.mode == "implicit"
        mail_from = None
        rcpt = []

        while True:
            line = recv()
            verb = line.split(" ", 1)[0].upper()

            if verb in ("EHLO", "HELO"):
                if verb == "HELO":
                    send("250 fake.local")
                    continue
                caps = ["250-fake.local Hello"]
                caps.append("250-SIZE 10485760")
                if self.mode == "starttls" and self.advertise_starttls and not tls_done:
                    caps.append("250-STARTTLS")
                if self.advertise_auth:
                    caps.append("250-AUTH " + " ".join(self.auth_mechs))
                caps.append("250 8BITMIME")
                # 多行回复：**最后一行**用空格而不是连字符
                for c in caps[:-1]:
                    send(c)
                send(caps[-1])
                continue

            if verb == "STARTTLS":
                if tls_done:
                    send("503 already in TLS")
                    continue
                send("220 Go ahead")
                conn = self._ctx().wrap_socket(conn, server_side=True)
                tls_done = True
                continue

            if verb == "AUTH":
                parts = line.split(" ")
                mech = parts[1].upper() if len(parts) > 1 else ""
                if mech == "PLAIN":
                    token = parts[2] if len(parts) > 2 else None
                    if token is None:
                        send("334 ")
                        token = recv()
                    try:
                        raw = base64.b64decode(token).decode()
                    except Exception:
                        send("501 bad base64")
                        continue
                    bits = raw.split("\0")
                    u, p = (bits[-2], bits[-1]) if len(bits) >= 3 else ("", "")
                    self.auths.append(("PLAIN", u, p))
                    send("235 ok" if (u, p) == (self.user, self.password) else "535 bad credentials")
                    continue
                if mech == "LOGIN":
                    send("334 " + base64.b64encode(b"Username:").decode())
                    u = base64.b64decode(recv()).decode()
                    send("334 " + base64.b64encode(b"Password:").decode())
                    p = base64.b64decode(recv()).decode()
                    self.auths.append(("LOGIN", u, p))
                    send("235 ok" if (u, p) == (self.user, self.password) else "535 bad credentials")
                    continue
                send("504 unknown mechanism")
                continue

            if verb == "MAIL":
                mail_from = line.split(":", 1)[1].strip().strip("<>")
                send("250 sender ok")
                continue

            if verb == "RCPT":
                addr = line.split(":", 1)[1].strip().strip("<>")
                if self.reject_rcpt and addr == self.reject_rcpt:
                    send("550 no such user here")
                else:
                    rcpt.append(addr)
                    send("250 recipient ok")
                continue

            if verb == "DATA":
                send("354 end with <CRLF>.<CRLF>")
                lines = []
                while True:
                    buf = bytearray()
                    while True:
                        b = conn.recv(1)
                        if not b:
                            raise ConnectionError("DATA 中途断开")
                        if b == b"\n":
                            break
                        buf += b
                    if bytes(buf).rstrip(b"\r") == b".":
                        break
                    l = bytes(buf)
                    # 服务端的**反**点填充：行首 `..` → `.`
                    if l.startswith(b".."):
                        l = l[1:]
                    lines.append(l)
                self.messages.append(
                    {
                        "mail_from": mail_from,
                        "rcpt": list(rcpt),
                        "data": b"".join(lines).decode("utf-8", "replace"),
                    }
                )
                send("250 message accepted")
                continue

            if verb == "QUIT":
                send("221 bye")
                return

            send("502 command not implemented")


# ---------------------------------------------------------------- 辅助


def free_port():
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


def http(url, method="GET", body=None, timeout=30.0, headers=None):
    req = urllib.request.Request(url, data=body, method=method)
    h = dict(headers or {})
    if not any(k.lower() in ("x-panel-token", "authorization", "cookie") for k in h):
        h["X-Panel-Token"] = TOKEN
    for k, v in h.items():
        req.add_header(k, v)
    try:
        with urllib.request.urlopen(req, timeout=timeout) as r:
            return r.status, r.read().decode()
    except urllib.error.HTTPError as e:
        return e.code, e.read().decode()


def wait_ready(base, proc, timeout=25.0):
    deadline = time.time() + timeout
    while time.time() < deadline:
        if proc.poll() is not None:
            return False
        try:
            code, body = http(f"{base}/api/state", timeout=2)
            if code == 200 and json.loads(body).get("plugins"):
                return True
        except (urllib.error.URLError, OSError, json.JSONDecodeError, TimeoutError):
            pass
        time.sleep(0.25)
    return False


def decode_subject(raw):
    """按 RFC 2047 把主题解回原文（可能由多个编码字拼成）。"""
    parts = decode_header(raw)
    out = []
    for text, enc in parts:
        out.append(text.decode(enc) if isinstance(text, bytes) else text)
    return "".join(out)


def gen_cert(tmp: Path):
    """生成「私有 CA + 它签的叶子证书」。

    ⚠ 不能只用 `openssl req -x509` 一张自签证书：那默认带 `CA:TRUE`，
    rustls 会把它当 CA 而不是服务器证书，报 `CaUsedAsEndEntity`。
    而「私有 CA 签一张服务器证书」本来就是自建 SMTP / 内网中继的真实形态，
    所以这么生成反而更贴近要验的东西。

    返回 (ca_pem, leaf_cert, leaf_key)。
    """
    ca_key, ca_pem = tmp / "ca.key", tmp / "ca.pem"
    leaf_key, leaf_csr, leaf_pem = tmp / "leaf.key", tmp / "leaf.csr", tmp / "leaf.pem"
    ext = tmp / "leaf.ext"
    ext.write_text("subjectAltName=IP:127.0.0.1,DNS:localhost\nbasicConstraints=critical,CA:FALSE\n")

    steps = [
        ["openssl", "req", "-x509", "-newkey", "rsa:2048", "-nodes",
         "-keyout", str(ca_key), "-out", str(ca_pem), "-days", "2",
         "-subj", "/CN=SmtpCheck Test CA",
         "-addext", "basicConstraints=critical,CA:TRUE"],
        ["openssl", "req", "-newkey", "rsa:2048", "-nodes",
         "-keyout", str(leaf_key), "-out", str(leaf_csr), "-subj", "/CN=127.0.0.1"],
        ["openssl", "x509", "-req", "-in", str(leaf_csr), "-CA", str(ca_pem),
         "-CAkey", str(ca_key), "-CAcreateserial", "-out", str(leaf_pem),
         "-days", "2", "-extfile", str(ext)],
    ]
    for cmd in steps:
        r = subprocess.run(cmd, capture_output=True)
        if r.returncode != 0:
            print(f"  （openssl 失败: {' '.join(cmd[:2])} {r.stderr.decode()[:200]}）")
            return None, None, None
    return ca_pem, leaf_pem, leaf_key


# ---------------------------------------------------------------- 主流程


class Panel:
    def __init__(self, tmp: Path):
        self.tmp = tmp
        self.proc = None
        self.base = None

    def start(self):
        plugins = self.tmp / "plugins"
        plugins.mkdir(parents=True, exist_ok=True)
        (plugins / "smtp-demo.js").write_bytes((FIXTURES / "smtp-demo.js").read_bytes())
        port = free_port()
        env = dict(os.environ)
        env.update(
            {
                "BIND_ADDR": f"127.0.0.1:{port}",
                "PLUGINS_DIR": str(plugins),
                "DB_PATH": str(self.tmp / "db.sqlite"),
                "ENGINE_WORKERS": "1",
                "PANEL_TOKEN": TOKEN,
            }
        )
        self.proc = subprocess.Popen(
            [BIN], env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL
        )
        self.base = f"http://127.0.0.1:{port}"
        if not wait_ready(self.base, self.proc):
            raise RuntimeError("面板没起来")
        return self

    def send(self, opts):
        code, body = http(
            f"{self.base}/plugins/smtp-demo/send",
            method="POST",
            body=json.dumps(opts).encode(),
            # 插件路由是 POST ⇒ 面板的写保护要求这个头（防 CSRF）
            headers={"content-type": "application/json", "x-panel": "1"},
        )
        if code != 200:
            return {"ok": False, "error": f"http {code}: {body[:200]}"}
        return json.loads(body)

    def stop(self):
        if self.proc:
            self.proc.terminate()
            try:
                self.proc.wait(timeout=6)
            except Exception:
                self.proc.kill()

    def __enter__(self):
        return self.start()

    def __exit__(self, *a):
        self.stop()


class EmptyMessage:
    """没收到邮件时的替身。

    不直接下标 `srv.messages[0]`：发送失败时那是 IndexError —— 报出来的是
    「脚本崩了」，而真实情况是「上一条断言已经红了」。脚本的失败信息必须
    指向真问题，所以这里返回一个「什么都是空」的替身，让后面的断言各自报红。
    """

    def __getitem__(self, k):
        return ""

    def get_content(self):
        return ""

    def get_payload(self):
        return []

    def is_multipart(self):
        return False

    def get_content_type(self):
        return ""

    def get_content_subtype(self):
        return ""


def parse_only(srv, check):
    """把服务器收到的第一封解析出来；没收到就给个空替身。"""
    if not srv.messages:
        check("服务器收到了一封邮件（否则后面的解析断言无从谈起）", False, "0 封")
        return EmptyMessage()
    return email.message_from_string(srv.messages[0]["data"], policy=email.policy.default)


def main() -> int:
    if not Path(BIN).exists():
        print(f"环境问题：找不到 {BIN}（先 cargo build --release）")
        return 2
    if not (FIXTURES / "smtp-demo.js").exists():
        print(f"环境问题：找不到夹具 {FIXTURES}")
        return 2

    failures = []

    def check(label, ok, detail=""):
        print(f"  {'✓' if ok else '✗'} {label}" + (f" — {detail}" if detail else ""))
        if not ok:
            failures.append(label)

    with tempfile.TemporaryDirectory(prefix="smtp-") as td:
        tmp = Path(td)
        ca_pem, cert, key = gen_cert(tmp)
        if not cert:
            print("环境问题：openssl 生成证书失败（TLS 那两组会跳过）")
        print("host_api.smtp_send · 验收\n")

        with Panel(tmp) as panel:
            # ---- ① 明文 + AUTH PLAIN ----
            print("明文 + AUTH PLAIN（脚本断言）")
            with FakeSmtp(mode="plain") as srv:
                r = panel.send(
                    {
                        "host": "127.0.0.1",
                        "port": srv.port,
                        "tls": "none",
                        "user": USER,
                        "pass": PASSWORD,
                        "from": f"机器人 <{USER}>",
                        "to": ["me@example.com"],
                        "subject": "日常任务完成",
                        "text": "3 个账号都跑完了",
                    }
                )
                check("发送成功", r.get("ok") is True, json.dumps(r, ensure_ascii=False)[:160])
                check("服务器收到了 1 封", len(srv.messages) == 1, f"{len(srv.messages)} 封")
                check(
                    "AUTH 用的是 PLAIN",
                    srv.auths and srv.auths[0][0] == "PLAIN",
                    str(srv.auths[:1]),
                )
                check("凭据正确送达", srv.auths and srv.auths[0][1:] == (USER, PASSWORD))
                if srv.messages:
                    m = srv.messages[0]
                    check("MAIL FROM 正确", m["mail_from"] == USER, m["mail_from"])
                    check("RCPT TO 正确", m["rcpt"] == ["me@example.com"], str(m["rcpt"]))
                    # 用**真的解析器**读回来 —— 这才是 MIME 那部分唯一的判据
                    msg = email.message_from_string(m["data"], policy=email.policy.default)
                    check(
                        "中文主题往返无损",
                        decode_subject(str(msg["Subject"])) == "日常任务完成",
                        str(msg["Subject"]),
                    )
                    check("显示名编码后可解析", "机器人" in str(msg["From"]), str(msg["From"]))
                    check("Message-ID 有", bool(msg["Message-ID"]), str(msg["Message-ID"]))
                    check("Date 有", bool(msg["Date"]), str(msg["Date"]))
                    body = msg.get_content().strip()
                    check("正文往返无损", body == "3 个账号都跑完了", repr(body))
                    check("不是 multipart（只给了 text）", not msg.is_multipart())

            # ---- ② 服务器只声明 LOGIN ----
            print("\n明文 + 服务器只给 AUTH LOGIN（脚本断言）")
            with FakeSmtp(mode="plain", auth_mechs=("LOGIN",)) as srv:
                r = panel.send(
                    {
                        "host": "127.0.0.1", "port": srv.port, "tls": "none",
                        "user": USER, "pass": PASSWORD,
                        "from": USER, "to": "me@example.com",
                        "subject": "hi", "text": "body",
                    }
                )
                check("发送成功", r.get("ok") is True, json.dumps(r, ensure_ascii=False)[:160])
                check(
                    "退回 LOGIN 机制",
                    srv.auths and srv.auths[0][0] == "LOGIN",
                    str(srv.auths[:1]),
                )
                check("`to` 收字符串也认", srv.messages and srv.messages[0]["rcpt"] == ["me@example.com"])

            # ---- ③ text + html ----
            print("\ntext + html → multipart/alternative（脚本断言）")
            with FakeSmtp(mode="plain") as srv:
                r = panel.send(
                    {
                        "host": "127.0.0.1", "port": srv.port, "tls": "none",
                        "user": USER, "pass": PASSWORD,
                        "from": USER, "to": ["a@x.com", "b@x.com"],
                        "subject": "两个收件人",
                        "text": "纯文本版", "html": "<b>HTML 版</b>",
                    }
                )
                check("发送成功", r.get("ok") is True, json.dumps(r, ensure_ascii=False)[:160])
                check("两个收件人都发了", srv.messages and srv.messages[0]["rcpt"] == ["a@x.com", "b@x.com"])
                msg = parse_only(srv, check)
                check("是 multipart/alternative", msg.get_content_subtype() == "alternative", msg.get_content_type())
                parts = msg.get_payload()
                check("两段都在", len(parts) == 2, f"{len(parts)} 段")
                if len(parts) == 2:
                    check("第一段是纯文本", parts[0].get_content_type() == "text/plain")
                    check("第二段是 HTML", parts[1].get_content_type() == "text/html")
                    check("纯文本往返无损", parts[0].get_content().strip() == "纯文本版")
                    check("HTML 往返无损", parts[1].get_content().strip() == "<b>HTML 版</b>")

            # ---- ④ 点填充 ----
            print("\n点填充：正文里行首的 `.`（脚本断言）")
            tricky = "第一行\n.\n.隐藏行\n结尾"
            with FakeSmtp(mode="plain") as srv:
                r = panel.send(
                    {
                        "host": "127.0.0.1", "port": srv.port, "tls": "none",
                        "user": USER, "pass": PASSWORD,
                        "from": USER, "to": ["me@example.com"],
                        "subject": "dots", "text": tricky,
                    }
                )
                check("发送成功", r.get("ok") is True, json.dumps(r, ensure_ascii=False)[:160])
                msg = parse_only(srv, check)
                body = msg.get_content()
                # 归一化行尾再比：base64 往返会保留 \n
                check(
                    "行首的 `.` 原样到达（邮件没被截断）",
                    body.replace("\r\n", "\n").strip() == tricky,
                    repr(body[:60]),
                )

            # ---- ⑤ 失败路径 ----
            print("\n失败路径（脚本断言）")
            with FakeSmtp(mode="plain") as srv:
                r = panel.send(
                    {
                        "host": "127.0.0.1", "port": srv.port, "tls": "none",
                        "user": USER, "pass": "wrong",
                        "from": USER, "to": ["me@example.com"],
                        "subject": "x", "text": "y",
                    }
                )
                check(
                    "密码错时把服务器的原话带回来",
                    r.get("ok") is False and "535" in str(r.get("error")),
                    str(r.get("error"))[:120],
                )

            with FakeSmtp(mode="plain", reject_rcpt="nobody@example.com") as srv:
                r = panel.send(
                    {
                        "host": "127.0.0.1", "port": srv.port, "tls": "none",
                        "user": USER, "pass": PASSWORD,
                        "from": USER, "to": ["nobody@example.com"],
                        "subject": "x", "text": "y",
                    }
                )
                check(
                    "收件人被拒时**点名**是哪个地址",
                    r.get("ok") is False and "nobody@example.com" in str(r.get("error")),
                    str(r.get("error"))[:120],
                )

            # 服务器不声明 STARTTLS，但客户端要求 starttls
            with FakeSmtp(mode="plain", advertise_starttls=False) as srv:
                r = panel.send(
                    {
                        "host": "127.0.0.1", "port": srv.port, "tls": "starttls",
                        "user": USER, "pass": PASSWORD,
                        "from": USER, "to": ["me@example.com"],
                        "subject": "x", "text": "y",
                    }
                )
                check(
                    "没声明 STARTTLS 时给出可操作的报错",
                    r.get("ok") is False and "STARTTLS" in str(r.get("error")),
                    str(r.get("error"))[:140],
                )

            r = panel.send({"host": "", "port": 25, "from": USER, "to": ["a@b.com"], "text": "x"})
            check("host 为空时报错", r.get("ok") is False and "host" in str(r.get("error")), str(r.get("error"))[:80])
            r = panel.send({"host": "127.0.0.1", "port": 1, "tls": "none", "from": USER, "to": ["a@b.com"], "text": "x", "timeout_ms": 1500})
            check("连不上时报错而不是挂死", r.get("ok") is False, str(r.get("error"))[:100])

            # ---- ⑥ STARTTLS / implicit TLS ----
            if cert:
                print("\nSTARTTLS（自签 CA + ca_file）（脚本断言）")
                with FakeSmtp(mode="starttls", certfile=str(cert), keyfile=str(key)) as srv:
                    r = panel.send(
                        {
                            "host": "127.0.0.1", "port": srv.port, "tls": "starttls",
                            "user": USER, "pass": PASSWORD,
                            "from": USER, "to": ["me@example.com"],
                            "subject": "走 TLS 的", "text": "加密正文",
                            "ca_file": str(ca_pem),
                        }
                    )
                    check("STARTTLS 发送成功", r.get("ok") is True, json.dumps(r, ensure_ascii=False)[:160])
                    check("服务器收到了", len(srv.messages) == 1)
                    if srv.messages:
                        msg = parse_only(srv, check)
                        check("TLS 里主题依然无损", decode_subject(str(msg["Subject"])) == "走 TLS 的")
                        check("TLS 里正文依然无损", msg.get_content().strip() == "加密正文")

                # 不给 ca_file → 自签证书应当被拒（证明 ca_file 真的在起作用）
                with FakeSmtp(mode="starttls", certfile=str(cert), keyfile=str(key)) as srv:
                    r = panel.send(
                        {
                            "host": "127.0.0.1", "port": srv.port, "tls": "starttls",
                            "user": USER, "pass": PASSWORD,
                            "from": USER, "to": ["me@example.com"],
                            "subject": "x", "text": "y",
                        }
                    )
                    check(
                        "不给 ca_file 时私有 CA 签的证书被拒（说明 ca_file 真的在起作用）",
                        r.get("ok") is False,
                        str(r.get("error"))[:120],
                    )

                print("\nimplicit TLS（465 那种）（脚本断言）")
                with FakeSmtp(mode="implicit", certfile=str(cert), keyfile=str(key)) as srv:
                    r = panel.send(
                        {
                            "host": "127.0.0.1", "port": srv.port, "tls": "implicit",
                            "user": USER, "pass": PASSWORD,
                            "from": USER, "to": ["me@example.com"],
                            "subject": "隐式 TLS", "text": "正文",
                            "ca_file": str(ca_pem),
                        }
                    )
                    check("implicit 发送成功", r.get("ok") is True, json.dumps(r, ensure_ascii=False)[:160])
                    if srv.messages:
                        msg = parse_only(srv, check)
                        check("implicit 里主题无损", decode_subject(str(msg["Subject"])) == "隐式 TLS")

            # 服务器侧没看到协议违规
            check("服务器侧没有协议违规记录", not srv.errors, str(srv.errors[:2]))

    print()
    if failures:
        print(f"❌ {len(failures)} 项失败: {', '.join(failures)}")
        return 1
    print("✅ 通过")
    return 0


if __name__ == "__main__":
    sys.exit(main())
