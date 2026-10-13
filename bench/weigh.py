#!/usr/bin/env python3
"""Weigh aether against other terminal monitors on this machine (Linux only).

Usage: python3 bench/weigh.py [--seconds 60] [--aether PATH] [--path EXTRA_DIR ...]

Each monitor found runs alone, after the previous one has ended, in a pseudo-terminal
of 50 rows by 160 columns (the JSON reading has no terminal). It is left 2 seconds to
settle, then read for --seconds from /proc: CPU ticks and read calls of the process and
its live descendants, and its peak resident memory. Prints a Markdown table.
Standard library only. Reads /proc, so it does nothing elsewhere.
"""

import argparse
import fcntl
import os
import pty
import re
import shutil
import signal
import struct
import subprocess
import sys
import termios
import threading
import time

# (table name, command, seconds between readings, runs in a pseudo-terminal).
# Each interval is the monitor's own option. A new monitor is one line here.
MONITORS = [
    ("aether", ["aether", "-i", "1"], 1, True),
    ("aether --json", ["aether", "--json", "-i", "1"], 1, False),
    ("top", ["top", "-d", "1"], 1, True),
    ("htop", ["htop", "-d", "10"], 1, True),  # htop's -d counts tenths of a second
    ("btop", ["btop", "--update", "1000"], 1, True),
    ("bottom", ["btm", "-r", "1000"], 1, True),
    ("glances", ["glances", "-t", "1"], 1, True),
    ("atop", ["atop", "1"], 1, True),
    ("gotop", ["gotop", "-r", "1s"], 1, True),
    ("zenith", ["zenith", "-r", "1000"], 1, True),
]

ROWS, COLS = 50, 160
SETTLE = 2
CLK_TCK = os.sysconf("SC_CLK_TCK")
ENV = dict(os.environ, TERM="xterm-256color")
ANSI = re.compile(r"\x1b\[[0-9;?]*[A-Za-z]")
running = []  # processes still alive, ended on the way out


def locate(prog, extra):
    for folder in os.environ.get("PATH", "").split(os.pathsep) + extra:
        path = os.path.join(folder, prog)
        if os.path.isfile(path) and os.access(path, os.X_OK):
            return path
    return None


def version(path):
    # Most monitors take --version; atop only knows -V and prints its usage for --version.
    for flag in ("--version", "-V"):
        try:
            done = subprocess.run([path, flag], stdin=subprocess.DEVNULL, capture_output=True,
                                  timeout=5, env=ENV)
        except (OSError, subprocess.SubprocessError):
            return "?"
        text = ANSI.sub("", (done.stdout or done.stderr).decode("utf-8", "replace")).strip()
        first = (text.splitlines() or ["?"])[0][:40]
        if not first.startswith("Usage"):
            return first
    return "?"


def is_script(path):
    with open(path, "rb") as f:
        return f.read(2) == b"#!"


def snapshot():
    """Children and CPU ticks (user + system) of every process, from /proc."""
    kids, ticks = {}, {}
    for name in os.listdir("/proc"):
        if not name.isdigit():
            continue
        try:
            with open(f"/proc/{name}/stat") as f:
                rest = f.read().rsplit(")", 1)[1].split()
        except OSError:
            continue
        kids.setdefault(int(rest[1]), []).append(int(name))
        ticks[int(name)] = int(rest[11]) + int(rest[12])
    return kids, ticks


def tree(root, kids):
    group, todo = [], [root]
    while todo:
        pid = todo.pop()
        group.append(pid)
        todo.extend(kids.get(pid, []))
    return group


def sample(root):
    """CPU ticks and read calls of root and its descendants; VmHWM of root in kB."""
    kids, ticks = snapshot()
    group = tree(root, kids)
    cpu = sum(ticks.get(pid, 0) for pid in group)
    reads, hwm = 0, None
    for pid in group:
        try:
            with open(f"/proc/{pid}/io") as f:
                for line in f:
                    if line.startswith("syscr:"):
                        reads += int(line.split()[1])
        except OSError:
            pass
    try:
        with open(f"/proc/{root}/status") as f:
            for line in f:
                if line.startswith("VmHWM:"):
                    hwm = int(line.split()[1])
    except OSError:
        pass
    return cpu, reads, hwm


def take_tty():
    """Runs in the child: the pseudo-terminal becomes its controlling terminal (/dev/tty)."""
    try:
        fcntl.ioctl(0, termios.TIOCSCTTY, 0)
    except OSError:
        pass


def drain(fd):
    try:
        while os.read(fd, 65536):
            pass
    except OSError:
        pass


def end(proc):
    """TERM to the process and its descendants, KILL after 2 s, then reap."""
    def signal_all(sig):
        kids, _ = snapshot()
        for pid in tree(proc.pid, kids):
            try:
                os.kill(pid, sig)
            except OSError:
                pass
        try:
            os.killpg(proc.pid, sig)
        except OSError:
            pass

    signal_all(signal.SIGTERM)
    try:
        proc.wait(timeout=2)
    except subprocess.TimeoutExpired:
        signal_all(signal.SIGKILL)
        proc.wait()
    running.remove(proc)


def measure(path, cmd, interval, use_pty, seconds):
    """Returns (cpu seconds, percent of one core, peak kB, read calls per reading) or None."""
    argv = [path] + cmd[1:]
    master = None
    if use_pty:
        master, slave = pty.openpty()
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", ROWS, COLS, 0, 0))
        proc = subprocess.Popen(argv, stdin=slave, stdout=slave, stderr=slave, env=ENV,
                                start_new_session=True, close_fds=True, preexec_fn=take_tty)
        os.close(slave)
        threading.Thread(target=drain, args=(master,), daemon=True).start()
    else:
        proc = subprocess.Popen(argv, stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL,
                                stderr=subprocess.DEVNULL, env=ENV, start_new_session=True)
    running.append(proc)
    try:
        time.sleep(SETTLE)
        if proc.poll() is not None:
            return None
        cpu0, reads0, _ = sample(proc.pid)
        t0 = time.monotonic()
        time.sleep(seconds)
        if proc.poll() is not None:
            return None
        cpu1, reads1, hwm = sample(proc.pid)
        elapsed = time.monotonic() - t0
        cpu_s = (cpu1 - cpu0) / CLK_TCK
        per_reading = (reads1 - reads0) / (seconds / interval)
        return cpu_s, cpu_s / elapsed * 100, hwm, per_reading
    finally:
        end(proc)
        if master is not None:
            os.close(master)


def main():
    if not sys.platform.startswith("linux"):
        print("weigh.py reads /proc, so it measures on Linux only. Nothing was measured here.")
        return 0
    parser = argparse.ArgumentParser(description="Weigh aether against other terminal monitors.")
    parser.add_argument("--seconds", type=int, default=60, help="reading time per monitor")
    parser.add_argument("--aether", metavar="PATH", help="the aether program to weigh")
    parser.add_argument("--path", metavar="EXTRA_DIR", nargs="+", action="extend", default=[],
                        help="folders searched after PATH for the monitors")
    args = parser.parse_args()
    if args.seconds < 1:
        parser.error("--seconds must be at least 1")

    rows, missing = [], []
    try:
        for name, cmd, interval, use_pty in MONITORS:
            if cmd[0] == "aether" and args.aether:
                path = args.aether
            else:
                path = locate(cmd[0], args.path)
            if path is None:
                missing.append(name)
                continue
            size = "" if is_script(path) else f"{os.path.getsize(path) / 1024:.0f}"
            result = measure(path, cmd, interval, use_pty, args.seconds)
            ver = version(path)
            if result is None:
                rows.append(f"| {name} | {ver} | — | — | — | — | {size} |")
            else:
                cpu_s, pct, hwm, per_reading = result
                mem = "—" if hwm is None else f"{hwm / 1024:.1f}"
                rows.append(f"| {name} | {ver} | {cpu_s:.2f} | {pct:.2f} | {mem} | "
                            f"{per_reading:.1f} | {size} |")
            time.sleep(3)
    finally:
        for proc in list(running):
            end(proc)

    print("| Monitor | Version | CPU seconds | % of one core | Peak memory (MB) | "
          "Read calls per reading | Program (kB) |")
    print("|---|---|---:|---:|---:|---:|---:|")
    for row in rows:
        print(row)
    if missing:
        print(f"\nNot found: {', '.join(missing)}.")
    procs = sum(1 for name in os.listdir("/proc") if name.isdigit())
    print(f"\nThreads: {os.cpu_count()}. Processes: {procs}. Kernel: {os.uname().release}.")
    return 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except KeyboardInterrupt:
        sys.exit(130)
