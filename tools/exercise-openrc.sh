#!/bin/bash
# exercise-openrc.sh - run real OpenRC commands on a pty; diff the output with C vs openrc-rs swapped in
# 20261106 - Created by Claude (claude.ai, Anthropic) - scottfk
# 20261106 - Add mountinfo/fstabinfo/seedrng/netmount/value-applet commands - Claude - scottfk
# 20261106 - Add deptree commands: regenerate and print the cache, rc-depend queries - Claude - scottfk
# 20261107 - Add rc-depend option cases and mark_service_* round trips - Claude - scottfk
# 20261107 - Add rc-service options and service_* state queries - Claude - scottfk
# 20261107 - Add rc-update show/add/del/stack round trips - Claude - scottfk
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
run /usr/libexec/rc/bin/mountinfo -p '^/' -F '^(cgroup2?|tracefs|debugfs)$'
run /usr/libexec/rc/bin/mountinfo -s /
run /usr/libexec/rc/bin/mountinfo -e
run /usr/libexec/rc/bin/fstabinfo
run /usr/libexec/rc/bin/fstabinfo -t ext4,vfat,swap -m
run /usr/libexec/rc/bin/fstabinfo -p '>0'
run rc-service netmount restart
run rc-service seedrng restart
run 'RC_SVCNAME=sshd /usr/libexec/rc/bin/service_get_value nosuch'
run 'RC_SVCNAME=exercise /usr/libexec/rc/bin/service_set_value k v && RC_SVCNAME=exercise /usr/libexec/rc/bin/service_get_value k'
run '/usr/libexec/rc/bin/is_newer_than /etc/fstab /etc/conf.d; echo newer=$?'
run '/usr/libexec/rc/bin/shell_var net.eth0 foo-bar'
run '/usr/libexec/rc/bin/rc-depend -u && cat /run/openrc/deptree /run/openrc/depconfig'
run 'touch /etc/conf.d/local && rc-status default'
run /usr/libexec/rc/bin/rc-depend sshd
run /usr/libexec/rc/bin/rc-depend -t ineed,iuse,iwant,iafter sshd netmount local
run /usr/libexec/rc/bin/rc-depend -t needsme,usesme localmount
run /usr/libexec/rc/bin/rc-depend -a -s sshd
run /usr/libexec/rc/bin/rc-depend -o net
run /usr/libexec/rc/bin/rc-depend -t ibefore,iprovide,providedby net logger localmount
run /usr/libexec/rc/bin/rc-depend -s -a -T sshd
run /usr/libexec/rc/bin/rc-depend -t ineed,iuse,iwant,iafter,ibefore -o localmount
run /usr/libexec/rc/bin/rc-depend --help
run /usr/libexec/rc/bin/rc-depend nosuch
run '/usr/libexec/rc/sbin/mark_service_inactive local; rc-status default; /usr/libexec/rc/sbin/mark_service_started local; rc-status default'
run 'RC_SVCNAME=local /usr/libexec/rc/sbin/mark_service_failed; rc-status --crashed; rc-service local zap; rc-service local start'
run /usr/libexec/rc/sbin/mark_service_started
run rc-service -l
run 'rc-service -e sshd; echo e=$?; rc-service -e nosuch; echo e=$?'
run 'rc-service -r sshd; rc-service -r net.lo; rc-service -r nosuch; echo r=$?'
run 'rc-service -s sshd status; rc-service -S sshd status; echo s=$?; rc-service -N local status; rc-service -c sshd status; echo c=$?'
run 'rc-service -i nosuch start; echo i=$?'
run 'rc-service -Z local restart'
run 'FOO=bar LC_ALL=C rc-service -d local status'
run 'for s in started stopped starting inactive crashed; do /usr/libexec/rc/bin/service_$s sshd; echo "$s=$?"; done'
run 'RC_SVCNAME=sshd /usr/libexec/rc/bin/service_started; echo $?; /usr/libexec/rc/bin/service_started_daemon sshd /usr/sbin/sshd; echo d=$?; /usr/libexec/rc/bin/service_started_daemon sshd /usr/sbin/sshd 1; echo d1=$?'
run rc-update
run 'rc-update -v show'
run 'rc-update show default boot; rc-update show nosuch; echo s=$?'
run 'rc-update add sshd default; echo a=$?; rc-update add nosuch; echo a=$?; rc-update add local; echo a=$?'
run 'rc-update add sshd boot nonetwork; echo a=$?; rc-update show nonetwork; rc-update del sshd boot nonetwork; echo d=$?; rc-update del sshd nonetwork; echo d=$?'
run 'rc-update -a del nosuch; echo d=$?; rc-update -a add sshd; echo a=$?'
run 'rc-update -s add nonetwork default; readlink /etc/runlevels/default/nonetwork; rc-update -s del nonetwork default; echo s=$?; rc-update -s add boot; echo s=$?'
run 'rc-update -u; echo u=$?'
