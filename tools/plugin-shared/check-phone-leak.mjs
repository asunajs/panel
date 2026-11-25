// 示例手机号白名单校验（2026-11-22）。
//
// # 规则
//
// 仓库里的手机号**一律用白名单里的假号**（ALLOW，教科书假号与全 0 形态）；
// 掩码形态的尾四位也必须在白名单尾号里。不在白名单就是**错误**（退出码 1）。
//
// 理由：手机号是个人可识别信息，示例、夹具、文档、发布说明都算公开内容。
// 白名单制而不是"看起来像假的就行" —— 判据要能被机器执行，也要能被一眼审。
//
// 已知需要额外拦下的尾号以哈希形式放在 DENY_TAIL_HASHES 里（不存原文：
// 这个文件本身也是公开的，写明文等于换个地方再放一遍）。
//
// 用法：
//
//     node scripts/check-phone-leak.mjs            # 扫 git 跟踪的文本文件
//     node scripts/check-phone-leak.mjs --all      # 连未跟踪的一起扫
import { execFileSync } from 'node:child_process';
// 手机号泄露门禁（2026-11-22）。
//
// # 为什么要有这条
//
// 几个仓里曾把真实号码「中间四位清零、**留尾号**」当成脱敏（`1XX0000` + 真尾号），
// 于是文档示例、测试夹具、更新日志里到处是这种号，谁都能认出是哪个账号；
// 面板的 Release 说明里还直接带过掩码后的形态。用户的原话：「真实账号的尾号也不行」。
//
// # 规则（强硬、白名单制）
//
// 任何 11 位手机号形状的字符串，**必须**是 ALLOW 里那几个"教科书假号"之一；
// 掩码形状的尾四位也必须在 ALLOW 的尾号集合里。不在白名单就是**错误**（退出码 1），
// 不是警告 —— 这类东西一旦进 git，就只能靠改历史收回。
//
// 已知真实尾号**单独禁**：连散文里的「尾号 XXXX」也算错。它们的原文**不写在这里**
// （写了等于换个地方再泄露一遍），只存 `sha256(salt + 尾号)` 的前 16 位。
//
// 用法（各仓 `npm run check` / 面板 `gate.sh` 里都会调）：
//
//     node scripts/check-phone-leak.mjs            # 扫 git 跟踪的文本文件
//     node scripts/check-phone-leak.mjs --all      # 连未跟踪的一起扫（提交前自查用）
import { createHash } from 'node:crypto';
import { existsSync, readFileSync } from 'node:fs';
import { dirname, extname, join, relative, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

/** 仓库根：从脚本所在目录往上找 `.git`。
 *  ⚠ 不能写死 `..`：这份脚本在面板仓是 `tools/plugin-shared/`（根在上两级），
 *  在插件仓是 `scripts/`（根在上一级）—— 写死一个就有一边扫错目录（静默"全过"）。 */
function findRoot(start) {
  let d = start;
  for (let i = 0; i < 6; i += 1) {
    if (existsSync(join(d, '.git'))) return d;
    const up = dirname(d);
    if (up === d) break;
    d = up;
  }
  return start;
}

const ROOT = findRoot(resolve(dirname(fileURLToPath(import.meta.url)), '..'));

/** 唯一允许出现的手机号（教科书假号，尾号互不相同、与任何真实账号无关）。 */
const ALLOW = [
  // 教科书假号
  '13800138000',
  '13700137001',
  '13600136002',
  '13900139003',
  '13500135004',
  // 各仓已用、形态一眼假的合成号（全 0 中段 / 顺号 / 重复号）——
  // 逐个列出来是**故意的**：白名单要能一眼审，别搞模式匹配，模式一松就漏。
  '13800000000', '13800000001', '13800000002', '13800000003',
  '13800000004', '13800000005', '13800000011', '13800000012',
  '13800000021', '13800000022', '13800001234',
  '13900000000', '13911112222', '13900139000',
  '19900000000', '19900000001', '19900000002', '19900199001',
  '19100000000', '15900000000',
  '13500135000', '13500135001', '13600136001',
  '13900000001', '13900000002', '13900000003', '13900000009',
  '19100000001', '19100000002', '15900000001', '15900000002',
  '13700137004', '13800138001', '15912345678',
];
const ALLOW_TAILS = new Set(ALLOW.map((n) => n.slice(-4)));

/**
 * 已知的真实尾号（**存哈希，不存原文**）：出现在任何地方都算泄露 ——
 * 连散文里的「尾号 XXXX」也不行，尾号本身就是可识别信息。
 *
 * ⚠ 这里放的是 `sha256('panel-phone-gate' + 尾号)` 的前 16 位：门禁要认得它，
 * 但这个文件本身不能成为第二份泄露源（以前直接把尾号写在这里，等于换个地方又贴一遍）。
 * 加新尾号：`node -e "console.log(require('crypto').createHash('sha256').update('panel-phone-gate'+'1234').digest('hex').slice(0,16))"`
 */
const DENY_TAIL_HASHES = new Set([
  'dfb17934ecc59c80',
  'aa8a8caf512c77ca',
  '5e50610d85f16373',
]);

const TEXT_EXT = new Set([
  '.md', '.mjs', '.js', '.ts', '.tsx', '.json', '.txt', '.yml', '.yaml', '.sh', '.css', '.html',
]);
/** 这些目录是构建产物/依赖，不扫（它们要么来自源码，要么不是本项目文本）。 */
const SKIP_DIR = /(^|\/)(node_modules|dist|release|target|\.git|data|\.tmp)\//;

function listFiles() {
  const all = process.argv.includes('--all');
  if (all) {
    const out = execFileSync('git', ['ls-files', '--cached', '--others', '--exclude-standard'], {
      cwd: ROOT, encoding: 'utf8',
    });
    return out.split('\n').filter(Boolean);
  }
  const out = execFileSync('git', ['ls-files'], { cwd: ROOT, encoding: 'utf8' });
  return out.split('\n').filter(Boolean);
}

const files = listFiles().filter((f) => TEXT_EXT.has(extname(f)) && !SKIP_DIR.test(`${f}/`));
const bad = [];

for (const f of files) {
  let text;
  try {
    text = readFileSync(join(ROOT, f), 'utf8');
  } catch {
    continue;
  }
  const lines = text.split('\n');
  for (let i = 0; i < lines.length; i += 1) {
    const line = lines[i];
    // 跳过 hex dump / base64 常量：那些长串里随便三个数字就能拼出一个"手机号"，
    // 抓包样本和密钥表全是这种行（实测抓包样本的 hex 里就误报过一串"手机号"）。
    const t = line.trim();
    if (t.length >= 32 && /^[0-9a-fA-F'"+,\s]+$/.test(t)) continue;
    if (t.length >= 32 && /^[A-Za-z0-9+/=]{32,}$/.test(t)) continue;
    // ① 完整 11 位号码：必须在白名单
    for (const m of line.matchAll(/(?<![0-9])1[3-9][0-9]{9}(?![0-9])/g)) {
      const n = m[0];
      if (!ALLOW.includes(n)) bad.push([f, i + 1, `完整号码 ${n}`, '不在白名单里']);
    }
    // ② 掩码形状：尾四位必须在白名单尾号里
    for (const m of line.matchAll(/(?<![0-9])(1[0-9]{2})\*{2,8}([0-9]{4})(?![0-9])/g)) {
      const tail = m[2];
      if (!ALLOW_TAILS.has(tail)) bad.push([f, i + 1, `掩码 ${m[0]}`, `尾号 ${tail} 不在白名单里`]);
    }
    // ③ 已知真实尾号：任何位置都算
    // ③ 已知真实尾号（按哈希比，文件里不出现原文）
    for (const m of line.matchAll(/(?<![0-9])([0-9]{4})(?![0-9])/g)) {
      const h = createHash('sha256').update(`panel-phone-gate${m[1]}`).digest('hex').slice(0, 16);
      if (DENY_TAIL_HASHES.has(h)) {
        bad.push([f, i + 1, `尾号（哈希 ${h.slice(0, 6)}…）`, '这个尾号不允许出现在仓里']);
      }
    }
  }
}

if (bad.length === 0) {
  console.log(`ok: 没有手机号泄露（扫了 ${files.length} 个文本文件；白名单 ${ALLOW.length} 个假号）`);
  process.exit(0);
}

console.error(`✗ 发现 ${bad.length} 处手机号泄露：\n`);
for (const [f, ln, what, why] of bad.slice(0, 40)) {
  console.error(`  ${relative(ROOT, join(ROOT, f))}:${ln}  ${what} —— ${why}`);
}
if (bad.length > 40) console.error(`  …还有 ${bad.length - 40} 处`);
console.error(`
改法：换成 ALLOW 里的假号（${ALLOW.slice(0, 3).join(' / ')} …）。
真实数据只放本机 data/（已 gitignore），仓里不留。
`);
process.exit(1);
