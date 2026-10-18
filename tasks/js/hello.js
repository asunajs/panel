// @name hello-js
// @cron 0 3 * * *
// @params {"from":"tasks/js/hello.js"}
//
// 每天 03:00 跑一次。JS 脚本可以 import 'host-api'（调度类函数除外）。
import { log } from 'host-api';

log('hello from a JS script; params =', process.env.PANEL_PARAMS || '{}');
