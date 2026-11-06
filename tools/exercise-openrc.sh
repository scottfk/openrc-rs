#!/bin/bash
# exercise-openrc.sh - run real OpenRC commands on a pty; diff the output with C vs openrc-rs swapped in
# 20261106 - Created by Claude (claude.ai, Anthropic) - scottfk
export TERM=xterm COLUMNS=100
run() { echo "### $*"; script -qec "$*" /dev/null 2>&1; echo "### exit $?"; }
run rc-status
run rc-status -a
run rc-status -s
run rc-status --crashed
run rc-update show
run rc-update show -v default
run rc-service sshd status
run rc-service sshd restart
run rc-service local restart
run rc-service local stop
run rc-service local stop
run rc-service local start
run rc-service nosuch start
run /etc/init.d/sshd describe
run /etc/init.d/sshd status
run /etc/init.d/local zap
run /etc/init.d/local start
run openrc default
run 'EINFO_QUIET=yes rc-service local restart'
run 'EINFO_VERBOSE=yes rc-service local restart'
run 'TERM=dumb rc-service local restart'
run 'rc-service -C local restart'
run 'EINFO_COLOR=no rc-status'
run 'COLUMNS=40 rc-service local restart'
