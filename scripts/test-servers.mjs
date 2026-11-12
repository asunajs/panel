// 给 examples/demo-plugin.js 用的本地测试服务器：
//   - WebSocket echo（默认 9911，绑 0.0.0.0）
//   - 假 SMTP（默认 9925，只说协议必需的那几句）
//
// 为什么要绑 0.0.0.0 而不是 127.0.0.1：面板的 SSRF 门**禁止插件访问回环**
// （那是面板自己），但允许局域网。所以自检要用这台机器的局域网地址去连，
// 例如 ws://192.168.3.196:9911/ 。
//
// 用法：node scripts/test-servers.mjs [wsPort] [smtpPort]
import net from 'node:net';
import crypto from 'node:crypto';

const wsPort = Number(process.argv[2] || 9911);
const smtpPort = Number(process.argv[3] || 9925);

// ---------------------------------------------------------------- WebSocket echo
const ws = net.createServer((sock) => {
  let handshaken = false;
  let buf = Buffer.alloc(0);
  sock.on('data', (chunk) => {
    buf = Buffer.concat([buf, chunk]);
    if (!handshaken) {
      const end = buf.indexOf('\r\n\r\n');
      if (end < 0) return;
      const head = buf.slice(0, end).toString('latin1');
      buf = buf.slice(end + 4);
      const key = /sec-websocket-key:\s*(\S+)/i.exec(head);
      if (!key) { sock.destroy(); return; }
      const accept = crypto
        .createHash('sha1')
        .update(key[1] + '258EAFA5-E914-47DA-95CA-C5AB0DC85B11')
        .digest('base64');
      sock.write(
        'HTTP/1.1 101 Switching Protocols\r\n' +
          'Upgrade: websocket\r\n' +
          'Connection: Upgrade\r\n' +
          'Sec-WebSocket-Accept: ' + accept + '\r\n\r\n',
      );
      handshaken = true;
      console.log('[ws] 握手完成');
    }
    // 逐帧处理（只处理文本帧 + 掩码，够用了）
    while (buf.length >= 2) {
      const opcode = buf[0] & 0x0f;
      const masked = (buf[1] & 0x80) !== 0;
      let len = buf[1] & 0x7f;
      let off = 2;
      if (len === 126) {
        if (buf.length < 4) return;
        len = buf.readUInt16BE(2);
        off = 4;
      } else if (len === 127) {
        return; // 自检用不到这么大的帧
      }
      const need = off + (masked ? 4 : 0) + len;
      if (buf.length < need) return;
      let payload = buf.slice(off + (masked ? 4 : 0), need);
      if (masked) {
        const mask = buf.slice(off, off + 4);
        payload = Buffer.from(payload.map((b, i) => b ^ mask[i % 4]));
      }
      buf = buf.slice(need);
      if (opcode === 0x8) { sock.end(); return; }
      if (opcode === 0x1) {
        console.log('[ws] 收到: ' + payload.toString('utf8'));
        const out = Buffer.from(payload);
        // 服务端发出的帧不加掩码
        const header = out.length < 126 ? Buffer.from([0x81, out.length]) : null;
        if (header) sock.write(Buffer.concat([header, out]));
        else {
          const h = Buffer.alloc(4);
          h[0] = 0x81;
          h[1] = 126;
          h.writeUInt16BE(out.length, 2);
          sock.write(Buffer.concat([h, out]));
        }
      }
    }
  });
  sock.on('error', () => {});
});
ws.listen(wsPort, '0.0.0.0', () => console.log('[ws] echo on 0.0.0.0:' + wsPort));

// ---------------------------------------------------------------- 假 SMTP
const smtp = net.createServer((sock) => {
  let data = false;
  let buf = '';
  sock.write('220 test-smtp ready\r\n');
  sock.on('data', (chunk) => {
    buf += chunk.toString('utf8');
    let idx;
    while ((idx = buf.indexOf('\r\n')) >= 0) {
      const line = buf.slice(0, idx);
      buf = buf.slice(idx + 2);
      if (data) {
        if (line === '.') {
          data = false;
          sock.write('250 queued\r\n');
          console.log('[smtp] 收到一封信');
        } else {
          console.log('[smtp] | ' + line.slice(0, 90));
        }
        continue;
      }
      const cmd = line.toUpperCase();
      if (cmd.startsWith('EHLO')) sock.write('250-test-smtp\r\n250 8BITMIME\r\n');
      else if (cmd.startsWith('HELO')) sock.write('250 test-smtp\r\n');
      else if (cmd.startsWith('MAIL FROM') || cmd.startsWith('RCPT TO')) sock.write('250 ok\r\n');
      else if (cmd.startsWith('DATA')) { data = true; sock.write('354 go ahead\r\n'); }
      else if (cmd.startsWith('QUIT')) { sock.write('221 bye\r\n'); sock.end(); }
      else sock.write('250 ok\r\n');
    }
  });
  sock.on('error', () => {});
});
smtp.listen(smtpPort, '0.0.0.0', () => console.log('[smtp] fake on 0.0.0.0:' + smtpPort));
