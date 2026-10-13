# aether

The whole machine as numbers: CPU, memory, GPUs, sensors, disks, network, battery, and every
process with what it uses. It prints numbers and leaves the drawing to whoever reads them.

It is a Rust library (the core) with a small command-line program on top. Licence GPL-3.0-only.

## What it shows

- CPU: total use, every thread, GHz, temperature, load average.
- Memory: used, available, cached, swap, pressure.
- GPUs: use, VRAM, temperature, fan speed, power.
- Every sensor: temperatures, fans and power, with names.
- Disks: space used, and read and write rates, per mount.
- Network: receive and send rates per interface.
- Battery, on a laptop, and uptime.
- Every process: CPU %, memory, GPU % and VRAM, disk read and write rates, threads, user and age.
  Processes are grouped by app by default.

## Install

Needs a stable Rust toolchain.

    cargo build --release

The program is `target/release/aether`.

## Commands

```
aether: the whole machine as numbers.

Usage:
  aether [-i SECS] [--by cpu|mem|gpu|disk] [--flat] [--all] [--only LIST] [--json]
      live: every vital and every process, rewritten in place
  aether --once [same options]
      one reading, then exit
  aether proc PID|NAME
      one process in full: command line, program, folder, parents, CPU time, open files
  aether kill PID|NAME [--force] [-y]
      asks the process to stop and waits 3 s; says if it is still there.
      --force kills it. By NAME it lists the matches and asks first; -y skips the question.
  aether check
      what this machine and OS offer, and what is missing
  aether version
      the version (also --version and -V)
  aether help

Options:
  -i, --interval SECS    seconds between live readings (default 1)
  --by cpu|mem|gpu|disk  sort the processes (default cpu)
  --flat                 single processes, not grouped by app
  --all                  print every row, past the screen's height
  --only LIST            only these parts, e.g. cpu,mem (default: everything)
  --json                 one JSON object per reading; a missing reading is null
```

## JSON

`aether --json` prints one object per reading, on one line. A reading the machine does not have
is `null`, never 0. Rates and percentages that need two readings are `null` until the second one.

| Key | Holds |
|---|---|
| `t` | time of the reading, in seconds |
| `cpu` | `pct` (whole machine, %), `threads` (one % per thread), `mhz`, `temp_c`, `load` (1, 5 and 15 minute averages) |
| `mem` | `total`, `used`, `available`, `cached`, `swap_total`, `swap_used` (bytes); `pressure` (%) |
| `gpus` | list: `name`, `vendor`, `pct`, `temp_c`, `power_w`, `vram_used`, `vram_total` (bytes), `fan_rpm` |
| `sensors` | list: `chip`, `label`, `kind` (temp, fan or power), `value` |
| `disks` | list: `mount`, `dev`, `total`, `used` (bytes), `read_bps`, `write_bps` |
| `nets` | list: `name`, `rx_bps`, `tx_bps` |
| `battery` | `pct`, `state`, `power_w`, `minutes_left`; `null` when there is no battery |
| `host` | `uptime_s`, `threads` |
| `procs` | list: `pid`, `ppid`, `uid`, `threads`, `name`, `state`, `cpu_pct`, `gpu_pct`, `mem`, `rss`, `started_s`, `vram`, `read_bps`, `write_bps` |

Rates are bytes per second. CPU per process is a percent of the whole machine. Memory per process
is resident minus shared.

## How light

Measured on Linux, on a 12-thread desktop with about 530 processes running. Each program ran by itself for 60 seconds, one after the other.

| Program | CPU time in 60 s | Peak resident memory | Program file |
|---|---|---|---|
| `aether --json` | 0.43 s (about 0.7% of one core) | 3,468 kB | 615,520 bytes |
| btop 1.4.7, `--update 1000`, in a pseudo-terminal | 0.82 s (about 1.4% of one core) | 25,236 kB | 1,653,176 bytes |

Method: CPU time is `utime + stime` from `/proc/PID/stat`, read at the end of the minute; peak
memory is `VmHWM` from `/proc/PID/status`. btop ran under a small pseudo-terminal of 50 by 160
characters, so no window opened.

How it reads: files are opened once and re-read in place, and each process keeps its files open
while it lives. A process that used the CPU is read on every tick; a sleeping one is read on a
third of the ticks, so an idle process's numbers can be up to three ticks old. A process that
starts and ends between two ticks counts in the CPU total but not in the table.

## Status

| System | Status |
|---|---|
| Linux | Built and run. AMD GPUs are read. NVIDIA and Intel GPUs are not yet. |
| Windows | Compiles (checked, not run). Every source says "not built for this OS yet". |
| macOS | Compiles (checked, not run). Every source says "not built for this OS yet". |

## Licence

GPL-3.0-only. See `LICENSE`.
