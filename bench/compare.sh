#!/bin/sh
# Weight of two commands over the same time, read from each process's own /proc entry.
#
# Usage: sh bench/compare.sh SECONDS 'COMMAND A' 'COMMAND B'
#
# Each command is started alone, left to run for SECONDS, then read and ended.
# Printed per command: CPU seconds used and the percent of one core that is,
# peak resident memory (VmHWM), the number of processes at the read, and the
# size of the program file. Set PTY=1 to run a command in a pseudo-terminal
# (needs script(1)); the command's own process is then found by its name.
# Linux only (reads /proc).

set -u

if [ $# -ne 3 ]; then
    echo "usage: sh $0 SECONDS 'COMMAND A' 'COMMAND B'" >&2
    exit 2
fi
secs=$1
case $secs in
    ''|*[!0-9]*) echo "SECONDS must be a whole number" >&2; exit 2 ;;
esac
clk=$(getconf CLK_TCK)

measure() {
    cmd=$1
    prog=${cmd%% *}
    if [ -n "${PTY:-}" ]; then
        if ! command -v script > /dev/null 2>&1; then
            echo "$cmd: not measured, script(1) is not installed"
            return 1
        fi
        t0=$(cut -d' ' -f1 /proc/uptime)
        script -qfc "stty rows 50 cols 160; exec $cmd" /dev/null > /dev/null 2>&1 &
        sleep 2
        pid=$(ps -eo pid=,args= | awk -v c="$cmd" '{ p = $1; sub(/^ *[0-9]+ +/, ""); if (index($0, c) == 1) last = p } END { print last }')
    else
        t0=$(cut -d' ' -f1 /proc/uptime)
        sh -c "exec $cmd" > /dev/null 2>&1 &
        pid=$!
    fi
    if [ -z "${pid:-}" ] || [ ! -r "/proc/$pid/stat" ]; then
        echo "$cmd: not measured, the process did not start"
        return 1
    fi
    sleep "$secs"
    t1=$(cut -d' ' -f1 /proc/uptime)
    ticks=$(sed 's/.*) //' "/proc/$pid/stat" | awk '{ print $12 + $13 }')
    hwm=$(awk '/^VmHWM:/ { print $2 }' "/proc/$pid/status")
    procs=$(ls -d /proc/[0-9]* | wc -l)
    size=$(wc -c < "$(command -v "$prog")")
    kill "$pid" 2>/dev/null
    awk -v c="$cmd" -v ticks="$ticks" -v clk="$clk" -v t0="$t0" -v t1="$t1" \
        -v hwm="$hwm" -v procs="$procs" -v size="$size" '
        BEGIN {
            el = t1 - t0; cpu = ticks / clk
            printf "%s\n  cpu %.2f s over %.1f s = %.2f%% of one core\n", c, cpu, el, cpu / el * 100
            printf "  peak resident memory %d kB, processes at the read %d, program %d bytes\n", hwm, procs, size
        }'
    return 0
}

measure "$2"
measure "$3"
