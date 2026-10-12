// examples/recovery-plugins/flaky.js
//
// 验证熔断的「半开成功 → 完全恢复」路径：
// 前 3 次故意抛异常（攒够阈值触发熔断），之后一切正常。
//
// 预期：
//   t=1s/2s/3s  失败 3 次 → 熔断，进入冷却
//   冷却结束    半开放行一次 → 这次成功 → 完全恢复，退避计数清零
//   之后        回到正常节奏
host.every('flaky', '1s', function (p) {
    if (p.runs <= 3) {
        throw new Error('前 3 次故意失败（第 ' + p.runs + ' 次）');
    }
    if (p.runs % 5 === 0) {
        host.log(`flaky 已恢复正常，第 ${p.runs} 次触发`);
    }
});
