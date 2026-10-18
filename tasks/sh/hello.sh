#!/bin/sh
# @name hello-sh
# @cron 0 3 * * *
# @params {"from":"tasks/sh/hello.sh"}
#
# Shell 脚本是原生 /bin/sh 进程：没有 host-api。
echo "hello from a shell script; params = ${PANEL_PARAMS}"
