#!/bin/bash
# exercise-openrc.sh - run real OpenRC commands on a pty; diff the output with C vs openrc-rs swapped in
# 20261106 - Created by Claude (claude.ai, Anthropic) - scottfk
# 20261106 - Add mountinfo/fstabinfo/seedrng/netmount/value-applet commands - Claude - scottfk
# 20261106 - Add deptree commands: regenerate and print the cache, rc-depend queries - Claude - scottfk
# 20261107 - Add rc-depend option cases and mark_service_* round trips - Claude - scottfk
# 20261107 - Add rc-service options and service_* state queries - Claude - scottfk
# 20261107 - Add rc-update show/add/del/stack round trips - Claude - scottfk
# 20261107 - Add rc-status modes, formats and state filters - Claude - scottfk
# 20261107 - Add openrc-run simple commands (ours when swapped in, though not installed yet) - Claude - scottfk
# 20261107 - Add openrc-run start: dry runs, already started, and C-stop/our-start round trips - Claude - scottfk
# 20261108 - Add openrc-run stop/restart/condrestart/pause; openrc-run is now installed, so call it directly - Claude - scottfk
# 20261108 - Add openrc (ours through the multi-call binary when swapped in, as it isn't installed yet) - Claude - scottfk
# 20261108 - openrc is installed now: call it directly; add an rc_logger run - Claude - scottfk
# 20261109 - Add start-stop-daemon: test mode, user/group change, umask, nice, capabilities, stop by pidfile - Claude - scottfk
# 20261109 - Add supervise-daemon: the sv-test service (if installed), signals through the FIFO, a direct start as nobody - Claude - scottfk
# 20261109 - Mask supervised services' uptimes; the ssd test-mode stop matches /bin/true, not sv-test's sleep - Claude - scottfk
# 20261109 - Add openrc-shutdown's harmless commands (never -D: on sysvinit it really reboots) - Claude - scottfk
# 20261109 - Add openrc-init when not PID 1 (it only returns 1) - Claude - scottfk
# 20261109 - Add rc-environ (printing only) and rc-abort without RC_PID - Claude - scottfk
export TERM=xterm COLUMNS=100
# (A supervised service's uptime, "started 00:01:02 (0)", is masked.)
run() {
	echo "### $*"
	script -qec "$*" /dev/null 2>&1 | sed -E 's/started [0-9]+:[0-9]{2}:[0-9]{2} \(/started HH:MM:SS (/'
	echo "### exit ${PIPESTATUS[0]}"
}
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
run 'rc-status -f ini; rc-status -f ini -a'
run 'rc-status -l; rc-status -r; rc-status -u; rc-status -m; rc-status -S'
run 'rc-status -c; echo c=$?; rc-status nosuch; echo n=$?; rc-status nosuch boot; echo nb=$?'
run 'rc-status -i started; rc-status -i stopped -i failed; rc-status -i bogus; echo i=$?'
run 'rc-status -f json; echo f=$?; rc-status boot default nonetwork'
OR=/sbin/openrc-run
export OR
run '$OR /etc/init.d/sshd describe; $OR /etc/init.d/local describe'
run '$OR /etc/init.d/sshd status; echo s=$?; $OR /etc/init.d/netmount status; $OR /etc/init.d/savecache status; echo s=$?'
run '$OR /etc/init.d/sshd ineed iuse iwant iafter ibefore iprovide; $OR /etc/init.d/localmount needsme usesme wantsme'
run '$OR /etc/init.d/sshd -s status; echo s=$?; $OR /etc/init.d/sshd -S status; echo S=$?'
run '$OR /etc/init.d/sshd checkconfig; echo c=$?; $OR /etc/init.d/sshd nosuchcmd; echo n=$?'
run '$OR /etc/init.d/sshd; echo u=$?; $OR; echo d=$?'
run '$OR /etc/init.d/local -v describe; $OR /etc/init.d/local -d status 2>&1 | grep -c .'
run '$OR /etc/init.d/sshd start; echo s=$?; $OR /etc/init.d/local -Z start; $OR /etc/init.d/savecache --dry-run start'
run 'IN_BACKGROUND=yes $OR /etc/init.d/local start; echo b=$?; $OR /etc/init.d/netmount ineed'
run '/etc/init.d/local stop >/dev/null; $OR /etc/init.d/local start; echo l=$?; rc-service local status'
run '/etc/init.d/netmount stop >/dev/null; $OR /etc/init.d/netmount start; echo n=$?; rc-service netmount status'
run '/etc/init.d/local stop >/dev/null; $OR /etc/init.d/local --nodeps start start; echo d=$?; rc-service local status'
run '$OR /etc/init.d/local -Z stop; $OR /etc/init.d/localmount -Z stop; $OR /etc/init.d/net.lo -Z restart; $OR /etc/init.d/sshd -Z condrestart'
run '$OR /etc/init.d/local stop; echo s=$?; $OR /etc/init.d/local stop; echo s=$?; rc-service local status; $OR /etc/init.d/local start'
run '$OR /etc/init.d/netmount restart; echo r=$?; rc-service netmount status; $OR /etc/init.d/local --nodeps restart; echo r=$?'
run '$OR /etc/init.d/local condrestart; echo c=$?; $OR /etc/init.d/savecache condrestart; echo c=$?; $OR /etc/init.d/local pause; echo p=$?'
run 'IN_BACKGROUND=yes $OR /etc/init.d/local stop; echo b=$?; rc-service local status; IN_BACKGROUND=yes $OR /etc/init.d/local stop; echo b=$?; $OR /etc/init.d/local zap start'
# (No real runlevel switches: those would take the network down under us.)
RC=/sbin/openrc
export RC
run '$RC -S; echo s=$?; $RC --help | head -3; $RC nosuch; echo n=$?; $RC -s local status; $RC -o nosuch; echo o=$?'
run '$RC; echo r=$?; $RC default; echo d=$?; rc-status -r'
run '/etc/init.d/local stop >/dev/null; $RC -n default; echo n=$?; rc-service local status; /etc/init.d/local stop >/dev/null; $RC; rc-service local status'
run 'RC_BOOTLEVEL=boot $RC -o default; echo o=$?; cat /run/openrc/krunlevel; echo; $RC; echo k=$?; ls /run/openrc/krunlevel 2>&1; rm -f /run/openrc/krunlevel'
run 'cp /etc/rc.conf /tmp/rc.conf.ex; echo rc_logger=YES >> /etc/rc.conf; /etc/init.d/local stop >/dev/null; $RC; echo l=$?; cp /tmp/rc.conf.ex /etc/rc.conf; tail -n +2 /var/log/rc.log | tail -6 | sed "s/ at .*//"'
SSD=/sbin/start-stop-daemon
export SSD
run '$SSD -t -S -x /bin/sleep -- 30; $SSD -t -K -x /bin/true; echo t=$?; $SSD -K -x /bin/nosuch; echo n=$?'
run '$SSD -S -b -m -p /run/ex-sleep.pid -u nobody:nobody -k 027 -N 3 -x /bin/sleep -- 30; echo s=$?; sleep 0.3; ps -o user=,group=,ni=,args= -p $(cat /run/ex-sleep.pid); $SSD -S -p /run/ex-sleep.pid -x /bin/sleep; echo again=$?; $SSD -K -p /run/ex-sleep.pid; echo k=$?; ls /run/ex-sleep.pid 2>&1'
run '$SSD -S -b -m -p /run/ex-cap.pid -u nobody --capabilities ^cap_net_bind_service,!cap_sys_admin -x /bin/sleep -- 30; sleep 0.3; grep -E "^(Cap(Inh|Amb|Bnd)|Uid)" /proc/$(cat /run/ex-cap.pid)/status; $SSD -K -R TERM/2 -p /run/ex-cap.pid; echo k=$?'
# supervise-daemon: the sv-test service (see README) and a direct run.
if [[ -e /etc/init.d/sv-test ]]; then
	run 'rc-service sv-test status; rc-service sv-test restart; echo r=$?; sleep 0.5; rc-service sv-test status; cat /run/openrc/options/sv-test/start_count; echo'
fi
SV=/sbin/supervise-daemon
export SV
run 'export RC_SVCNAME=exsv; $SV exsv -S -p /run/exsv.pid -u nobody -D 200ms /bin/sleep -- 30; echo s=$?; sleep 0.5; ps -o user=,args= -p $(cat /run/openrc/options/exsv/child_pid); $SV exsv -s TERM; sleep 1; cat /run/openrc/options/exsv/start_count; echo; $SV exsv -K -p /run/exsv.pid; echo k=$?; ls /run/exsv.pid /run/openrc/supervise-exsv.ctl 2>&1'
# openrc-shutdown: only what can't shut the system down.  Not --dry-run:
# with sysvinit's /run/initctl it still changes the runlevel (QUIRKS.md).
OSD=/sbin/openrc-shutdown
export OSD
run '$OSD -c; echo c=$?; $OSD; echo u=$?; $OSD -r -p now; echo two=$?; $OSD -r 25:00; echo t=$?; $OSD -h | head -3'
# openrc-init: not PID 1, so it returns 1 at once.
run '/sbin/openrc-init; echo i=$?; /sbin/openrc-init reboot; echo i=$?'
# rc-environ: print only (saving needs a starting service); rc-abort with
# no RC_PID does nothing.
run 'rc-environ; echo e=$?; rc-environ -e -s sv-test; echo e=$?; /usr/libexec/rc/sbin/rc-abort; echo a=$?'
