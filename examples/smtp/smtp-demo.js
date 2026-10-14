// SMTP 夹具（第二十三轮）—— 把 `smtp_send` 暴露成一个路由，方便脚本从外面驱动。
//
// 真实插件不会这么写（它会把服务器配置放在设置页里），这里只是为了验收时
// 能自由指定参数：验收脚本要分别试 starttls / implicit / none、
// 以及各种失败路径（认证错、收件人被拒、服务器没声明 STARTTLS）。

import { register_route, smtp_send, log } from 'host-api';

function json(status, obj) {
    return {
        status: status,
        headers: { 'content-type': 'application/json; charset=utf-8' },
        body: JSON.stringify(obj),
    };
}

register_route('/plugins/smtp-demo/send', 'POST', function (ctx) {
    const opts = ctx.body ? JSON.parse(ctx.body) : {};
    try {
        const r = smtp_send(opts);
        log('smtp: ' + JSON.stringify(r));
        return json(200, { ok: true, result: r });
    } catch (e) {
        // 失败也返回 200 + ok:false —— 脚本要断言的是**错误文本**，
        // 用 4xx/5xx 会让「哪种失败」和「路由本身坏了」混在一起
        return json(200, { ok: false, error: String(e && e.message ? e.message : e) });
    }
});
