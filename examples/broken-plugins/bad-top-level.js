// 顶层就抛异常：加载阶段失败。
host.log('这一行会先执行');
throw new Error('顶层故意抛出的异常');
