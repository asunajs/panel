//! 日志脱敏。从基线 `src/log/mod.rs:622-1179` 原样移植 —— 顺序敏感、幂等、
//! 有 32 条共享向量钉着，不重写。
//!
//! 六道规则按固定顺序跑，前一条的结果会被后一条再扫一遍；顺序的理由见 [`redact`]。

const SECRET_KEYS: &[&str] = &[
    "token",
    "password",
    "passwd",
    "pwd",
    "secret",
    "jwt",
    "ssotoken",
    "auth",
    "cookie",
    "session",
    "sid",
    "ticket",
    "signature",
    "credential",
    // 云厂商 / 网关的 API key 家族（2026-10 审计中4）。刻意写成完整词：
    // 裸 `key` 会把 `keyword` / `key=value` 这类排查用的正常参数一并误伤。
    "api_key",
    "apikey",
    "access_key",
    "private_key",
];

/// 整行脱到行尾的请求头（值里可能带 `;` 和空格，只能整行砍）。
const SECRET_HEADERS: &[&str] = &[
    "authorization",
    "proxy-authorization",
    "cookie",
    "set-cookie",
    "x-auth-token",
    "x-csrf-token",
    "x-xsrf-token",
    // 中4：`X-Api-Key` 是各家网关的凭据头；`x-panel-token` 是**本项目自己**的
    // 面板令牌 —— 插件拿到它就能冒充操作者，它出现在日志里必须整行脱。
    "x-api-key",
    "x-panel-token",
];

/// 值的终止符：ASCII 的 `& # ; " ' , ) ] }`，加上**全角**的 `）】》，。、`。
///
/// 全角那几个必须要有：面板的日志正文是中文，`请设 PANEL_COOKIE_SECURE=1）`
/// 这种写法里的 `）` 是句子的收尾，不是值的一部分 —— 不认它就会把
/// 「1）」当成值（长度 2）从而脱掉一个开关，把提示文本弄坏（实测踩过）。
fn is_value_terminator(c: char) -> bool {
    matches!(
        c,
        '&' | '#'
            | ';'
            | '"'
            | '\''
            | ','
            | ')'
            | ']'
            | '}'
            | '）'
            | '】'
            | '》'
            | '，'
            | '。'
            | '、'
    )
}

fn is_secret_key(key: &str) -> bool {
    let k = key.to_ascii_lowercase();
    SECRET_KEYS.iter().any(|s| k.contains(s))
}

/// 值看起来像「开关」而不是凭据吗？
///
/// 加这条是因为实测踩到了：面板自己的一条提示里写着
/// `请设 PANEL_COOKIE_SECURE=1`，键名含子串 `cookie` ⇒ 被脱成 `=***`，
/// 把提示文本本身弄坏了。
///
/// **只排除单字符值**：一个字符不可能是凭据，但 `token=x` 这种照样脱
/// （多脱一个字符的成本远低于漏掉一个真令牌）。布尔词（true/false）留着不排除 ——
/// `secret=true` 也可能真的是开关，但它同样不是凭据，脱了也不会误导。
fn is_flag_value(v: &[char]) -> bool {
    v.len() == 1 && (v[0].is_ascii_alphanumeric() || v[0] == '-')
}

/// 把消息里能识别的凭据/手机号换成 `***` / `****`。
///
/// 规则（按这个顺序跑，前一条的结果会被后一条再扫一遍）：
///
/// 1. URL userinfo：`scheme://user:pass@host` → `scheme://***:***@host`
/// 2. `Authorization:` / `Cookie:` / `Set-Cookie:` / `X-Api-Key:` … 整行值 → `***`
/// 3. JSON 字段 `"token": "…"` → `"***"`
/// 4. 裸的 `key=value`（`?`/`&` 拼在 URL 上的那种也算）→ `***`
/// 5. `Bearer xxx` → `Bearer ***`
/// 6. 11 位手机号 → 掩码（当前**保留首 3 末 4**、中间打码，仍能区分账号；
///    ❓ 是否照基线未拍板，见 docs/host-api.md §6。成文示例用 199 合成号、掩码不带尾号）
///
/// ⚠ 顺序有意义：JSON 那条必须排在裸 kv 前面 —— 否则 `{"token":"abc"}`
/// 里那个 `"` 会被当成 key 的分隔符，脱出个四不像。
///
/// URL userinfo 排在最前面：它是**结构**脱敏（`user:pass` 里两侧都可能是任意
/// 字符，包括 `=` `&`），先把它换成 `***`，后面的 kv / Bearer 才不会在
/// 密码串里做二次解释。这个顺序也保证幂等：脱过的 `***:***` 再跑一遍不变。
pub fn redact(msg: &str) -> String {
    let mut buf: Vec<char> = msg.chars().collect();
    buf = redact_url_userinfo(&buf);
    buf = redact_headers(&buf);
    buf = redact_json_fields(&buf);
    buf = redact_kv(&buf);
    buf = redact_bearer(&buf);
    buf = redact_phones(&buf);
    buf.into_iter().collect()
}

/* ======================== 单条上限 / 终端净化 ======================== */

/// 单条日志正文上限（字节）。安全审计 L15。
///
/// 32 KiB 是个刻意的折中：
/// - 正常日志（含一次 HTTP 响应体摘要、报错栈）远小于它，**不会**被截；
/// - 插件「把整个响应体 `log` 出来」这类写法会被截住 —— 一条 64 MiB 的日志行
///   会把内存环（1 MB 字节预算）直接冲干净，还会让每条都有一次磁盘写；
/// - 截断按 **UTF-8 边界**退让，绝不产生半个汉字（否则 JSONL 里就是乱码）。
pub const MAX_MSG_BYTES: usize = 32 * 1024;

/// 超长正文的尾巴。保留「原长」是为了排查时能判断「是内容就这么多，还是被我截了」。
fn truncation_note(original_len: usize) -> String {
    format!("…（已截断，原长 {original_len} 字节）")
}

/// 把 `msg` 在 `budget` 字节处截断，并按 UTF-8 边界往回退。
fn truncate_utf8(mut msg: String, budget: usize) -> String {
    let mut cut = budget.min(msg.len());
    while cut > 0 && !msg.is_char_boundary(cut) {
        cut -= 1;
    }
    msg.truncate(cut);
    msg
}

/// 写入口的正文处理：**先脱敏，再卡上限**。
///
/// 顺序：
/// 1. 超过上限的正文先粗切一刀**再脱敏** —— 否则插件打印 100 MB 正文时，
///    `redact` 要先把它摊成 `Vec<char>`（4 字节/字符）才轮到截断，那本身就是个
///    内存放大面；
/// 2. 脱敏（见 [`redact`]）；
/// 3. 最终长度 > 上限时按边界截断并补「已截断」标记 —— 脱敏可能让正文变长
///    （`a=b` 这种两字符值也会被换成 `***`），所以这一步不能省。
///
/// 截断后的总长（含标记）不超过 [`MAX_MSG_BYTES`]。
pub fn prepare_msg(raw: String) -> String {
    let original_len = raw.len();
    let mut redacted = if original_len > MAX_MSG_BYTES {
        redact(&truncate_utf8(raw, MAX_MSG_BYTES))
    } else {
        redact(&raw)
    };
    // 常态：没超上限，脱敏也没让它变长 —— 一个字节都不动。
    if original_len <= MAX_MSG_BYTES && redacted.len() <= MAX_MSG_BYTES {
        return redacted;
    }
    // 要么本来超了（上面已粗切），要么脱敏把它撑过了上限（`a=b` → `a=***`）。
    // 两种情况都要留标记，并且保证「正文 + 标记」≤ 上限。
    let note = truncation_note(original_len);
    let budget = MAX_MSG_BYTES.saturating_sub(note.len());
    redacted = truncate_utf8(redacted, budget);
    redacted.push_str(&note);
    redacted
}

fn matches_at(src: &[char], at: usize, needle: &str) -> bool {
    let n: Vec<char> = needle.chars().collect();
    if at + n.len() > src.len() {
        return false;
    }
    for (k, c) in n.iter().enumerate() {
        if !src[at + k].eq_ignore_ascii_case(c) {
            return false;
        }
    }
    true
}

fn push_masked(out: &mut Vec<char>) {
    out.push('*');
    out.push('*');
    out.push('*');
}

/// 1) URL userinfo：`scheme://user:pass@host/x` → `scheme://***:***@host/x`。
///
/// 审计中4：`https://alice:hunter2@host/x` 这种写法会把**账号和口令**原样留在
/// 盘上一周。`redact_kv` 看不见它（userinfo 里不一定有 `=`），`Bearer` 更不沾边。
///
/// 判定刻意收紧到「RFC 3986 的 authority 段」：
///
/// - `://` 前面必须真有一个 scheme（`ALPHA *( ALPHA / DIGIT / "+" / "-" / "." )`），
///   免得把正文里的 `//a@b` 误当 URL；
/// - `@` 必须出现在 authority 内（到 `/` `?` `#`、空白、引号或反斜杠为止）。
///   于是 `alice@example.com`（没 scheme）和 `https://host/a@b`（`@` 在 path 里）
///   都不动 —— 这两条是邮箱与路径，脱了反而毁排查线索。
///
/// 输出形状跟输入一致：有 `:` 才是 `***:***`，只有 user 就是 `***`。
/// 这样它对**自己的输出**幂等（`***:***` / `***` 再跑一遍不变）。
fn redact_url_userinfo(src: &[char]) -> Vec<char> {
    let mut out = Vec::with_capacity(src.len());
    let mut i = 0;
    while i < src.len() {
        if src[i] == ':' && i + 2 < src.len() && src[i + 1] == '/' && src[i + 2] == '/' {
            // 往回找 scheme 起点
            let mut s = i;
            while s > 0
                && (src[s - 1].is_ascii_alphanumeric() || matches!(src[s - 1], '+' | '-' | '.'))
            {
                s -= 1;
            }
            // scheme 必须以字母开头，且不能为空（`://x` 这种不算 URL）
            let scheme_ok = s < i && src[s].is_ascii_alphabetic();
            if scheme_ok {
                let auth_start = i + 3;
                let mut at = None;
                let mut k = auth_start;
                while k < src.len() {
                    let c = src[k];
                    if c == '@' {
                        at = Some(k);
                        break;
                    }
                    // authority 到此为止：再往后就是 path / query / fragment / 正文
                    if matches!(c, '/' | '?' | '#' | '"' | '\'' | '\\') || c.is_whitespace() {
                        break;
                    }
                    k += 1;
                }
                if let Some(at) = at {
                    out.extend(src[i..=i + 2].iter()); // `://` 原样留着
                    if src[auth_start..at].contains(&':') {
                        push_masked(&mut out);
                        out.push(':');
                        push_masked(&mut out);
                    } else {
                        push_masked(&mut out);
                    }
                    out.push('@');
                    i = at + 1;
                    continue;
                }
            }
        }
        out.push(src[i]);
        i += 1;
    }
    out
}

fn redact_headers(src: &[char]) -> Vec<char> {
    let mut out = Vec::with_capacity(src.len());
    let mut i = 0;
    while i < src.len() {
        // 只在「行首 / 空白之后 / 括号逗号之后」认头名，避免把正文里的词当请求头。
        // 括号和逗号也放进来是因为插件常把整个 header 表打在一行里
        // （`headers=(Cookie: …)` / `{Authorization: …}`），只认空白的话这些全漏。
        //
        // 全角括号（`（Cookie: …）`）同样要认：这是个中文项目，日志里写全角括号
        // 比半角常见得多，只认半角就是「整条漏脱」。配套的右括号已经在
        // `is_value_terminator` 里了 —— 两边必须成对，否则会把后面的正文一起吃掉。
        let boundary = i == 0
            || src[i - 1] == '\n'
            || src[i - 1].is_whitespace()
            || matches!(src[i - 1], '(' | '[' | '{' | ',' | ';' | '（' | '【' | '《' | '"' | '\'');
        if boundary {
            let mut hit = None;
            for h in SECRET_HEADERS {
                if matches_at(src, i, h) {
                    let after = i + h.chars().count();
                    // 头名后面允许空白，然后必须是 `:`
                    let mut k = after;
                    // 中间可能夹一个**收尾引号**：JSON 里写的是 "authorization": "..."，
                    // 只跳空白的话这条整条漏脱（安全审计 M9）。
                    while k < src.len()
                        && (src[k] == ' ' || src[k] == '\t' || src[k] == '"' || src[k] == '\'')
                    {
                        k += 1;
                    }
                    if k < src.len() && src[k] == ':' {
                        hit = Some((after, k));
                        break;
                    }
                }
            }
            if let Some((_name_end, colon)) = hit {
                // 值要脱到哪为止？
                //
                // - 头在**行首/空白之后**（真正的 HTTP 头）→ 脱到行尾。
                //   Cookie 值里可能有 `;`、空格、甚至 `)`，早停就会把后半截漏在盘上。
                // - 头在**括号之后**（插件把 header 表打在一行里，`(Cookie: …)`）
                //   → 脱到配对的右括号为止，不然会把 `)` 一起吃掉，行就断了。
                let inline = i > 0 && matches!(src[i - 1], '(' | '[' | '{' | '（' | '【' | '《');
                out.extend(src[i..=colon].iter()); // 头名 + ':'
                                                   // 冒号后面那点空白留着：`Cookie: ***` 比 `Cookie:***` 好读，
                                                   // 也让「值被脱了」和「值本来就不存在」看起来不一样。
                let mut k = colon + 1;
                while k < src.len() && (src[k] == ' ' || src[k] == '\t') {
                    out.push(src[k]);
                    k += 1;
                }
                push_masked(&mut out);
                while k < src.len() {
                    if src[k] == '\n' {
                        break;
                    }
                    if inline && matches!(src[k], ')' | ']' | '}' | '）' | '】' | '》') {
                        break;
                    }
                    k += 1;
                }
                i = k;
                continue;
            }
        }
        out.push(src[i]);
        i += 1;
    }
    out
}

fn redact_json_fields(src: &[char]) -> Vec<char> {
    let mut out = Vec::with_capacity(src.len());
    let mut i = 0;
    while i < src.len() {
        if src[i] == '"' {
            let ks = i + 1;
            let mut j = ks;
            while j < src.len() && src[j] != '"' && src[j] != '\n' {
                j += 1;
            }
            if j < src.len() && src[j] == '"' && j > ks {
                let key: String = src[ks..j].iter().collect();
                let mut k = j + 1;
                while k < src.len() && src[k].is_whitespace() {
                    k += 1;
                }
                if k < src.len() && src[k] == ':' {
                    let mut m = k + 1;
                    while m < src.len() && src[m].is_whitespace() {
                        m += 1;
                    }
                    if m < src.len() && src[m] == '"' && is_secret_key(&key) {
                        out.extend(src[i..=k].iter()); // "key":
                        out.push('"');
                        push_masked(&mut out);
                        out.push('"');
                        let mut n = m + 1;
                        while n < src.len() && src[n] != '"' && src[n] != '\n' {
                            n += 1;
                        }
                        i = if n < src.len() { n + 1 } else { n };
                        continue;
                    }
                }
            }
        }
        out.push(src[i]);
        i += 1;
    }
    out
}

/// 裸的 `key=value`。
///
/// 为什么不只认 `?key=` / `&key=`：插件排查时最爱打的是 `token=xxx` 这种**片段**
/// （没有 URL 上下文），只认 query 形式的话这些全漏 —— 而它们恰恰是最常出现的泄漏面。
///
/// key 必须在**词边界**上（前面不是字母数字或下划线），免得把
/// `mytoken=x` 里的 `token` 误当成 key 之外的东西。
///
/// 值的终止符刻意取得比较保守（含 `,` `)` `]` `}`）：**多脱一点比少脱一点安全**，
/// 而少脱一个字节就意味着一个真令牌留在盘上一周。
fn redact_kv(src: &[char]) -> Vec<char> {
    let mut out = Vec::with_capacity(src.len());
    let mut i = 0;
    while i < src.len() {
        // ⚠ 必须用 **ASCII** 判定，不能用 `char::is_alphanumeric()`：后者是 Unicode
        // 语义（汉字算字母），于是 `用token=abc123` 里的 `用` 会让「词边界」不成立，
        // 整条**漏脱** —— 而中文日志里紧贴写法（`用token=` / `账号token=`）恰恰很常见。
        // JS / Python 两侧本来就是 ASCII，这里是三边里唯一分叉过的一处（2026-10-15 修）。
        let boundary = i == 0 || !(src[i - 1].is_ascii_alphanumeric() || src[i - 1] == '_');
        if boundary && (src[i].is_ascii_alphabetic() || src[i] == '_') {
            let ks = i;
            let mut j = i;
            while j < src.len()
                && (src[j].is_ascii_alphanumeric() || matches!(src[j], '_' | '-' | '.'))
            {
                j += 1;
            }
            if j < src.len() && src[j] == '=' && j > ks {
                let key: String = src[ks..j].iter().collect();
                // 值的范围先量出来：既要判断它是不是开关，也要在脱的时候跳过它
                let mut ve = j + 1;
                while ve < src.len() && !is_value_terminator(src[ve]) && !src[ve].is_whitespace() {
                    ve += 1;
                }
                if is_secret_key(&key) && !is_flag_value(&src[j + 1..ve]) {
                    out.extend(src[ks..=j].iter()); // key + '='
                    push_masked(&mut out);
                    i = ve;
                    continue;
                }
                // 不是敏感 key：把 key 整体吐出去，从 '=' 继续
                out.extend(src[i..j].iter());
                i = j;
                continue;
            }
        }
        out.push(src[i]);
        i += 1;
    }
    out
}

fn redact_bearer(src: &[char]) -> Vec<char> {
    let mut out = Vec::with_capacity(src.len());
    let mut i = 0;
    while i < src.len() {
        // 同上：ASCII 判定（`用Bearer xxx` 要脱）
        let boundary = i == 0 || !src[i - 1].is_ascii_alphanumeric();
        if boundary && matches_at(src, i, "bearer") {
            let after = i + 6;
            if after < src.len() && src[after].is_whitespace() {
                // 连那个空白一起吐出去（`Bearer ***`，不是 `Bearer***`）
                out.extend(src[i..=after].iter());
                push_masked(&mut out);
                let mut k = after + 1; // after 那个空白已经吐出去了，别重复
                while k < src.len() && !src[k].is_whitespace() && src[k] != '"' && src[k] != '\'' {
                    k += 1;
                }
                i = k;
                continue;
            }
        }
        out.push(src[i]);
        i += 1;
    }
    out
}

fn redact_phones(src: &[char]) -> Vec<char> {
    let mut out = Vec::with_capacity(src.len());
    let mut i = 0;
    while i < src.len() {
        if src[i] == '1' && (i == 0 || !src[i - 1].is_ascii_digit()) {
            // 往后数连续数字，正好 11 位、且第 2 位是 3-9 才当手机号
            let mut j = i;
            while j < src.len() && src[j].is_ascii_digit() {
                j += 1;
            }
            let len = j - i;
            let next_is_digit = j < src.len() && src[j].is_ascii_digit();
            if len == 11 && ('3'..='9').contains(&src[i + 1]) && !next_is_digit {
                out.extend(src[i..i + 3].iter()); // 前 3 位
                out.extend(['*'; 4]);
                out.extend(src[i + 7..j].iter()); // 后 4 位
                i = j;
                continue;
            }
        }
        out.push(src[i]);
        i += 1;
    }
    out
}

#[cfg(test)]
#[path = "redact_tests.rs"]
mod tests;
