// 合成响应源，只服务 127.0.0.1。不碰任何真实账号。
import http from "node:http";

const PORT = Number(process.env.PARITY_PORT ?? 8787);

const server = http.createServer((req, res) => {
  const url = new URL(req.url, `http://127.0.0.1:${PORT}`);
  switch (url.pathname) {
    case "/cookies":
      res.writeHead(200, {
        "content-type": "text/plain",
        "set-cookie": ["a=1; Path=/", "b=2; Path=/; HttpOnly", "c=3; Path=/"],
      });
      res.end("cookies");
      break;
    case "/redirect":
      res.writeHead(302, { location: `http://127.0.0.1:${PORT}/final` });
      res.end();
      break;
    case "/final":
      res.writeHead(200, { "content-type": "text/plain" });
      res.end("final");
      break;
    case "/echo": {
      const chunks = [];
      req.on("data", (c) => chunks.push(c));
      req.on("end", () => {
        const buf = Buffer.concat(chunks);
        res.writeHead(200, { "content-type": "application/octet-stream" });
        res.end(buf);
      });
      break;
    }
    case "/latin1":
      res.writeHead(200, { "content-type": "text/plain; charset=windows-1252" });
      res.end(Buffer.from([0xe9, 0x80, 0x93, 0x61]));
      break;
    default:
      res.writeHead(404);
      res.end("no");
  }
});

server.listen(PORT, "127.0.0.1", () => {
  console.log(`parity server on http://127.0.0.1:${PORT}`);
});