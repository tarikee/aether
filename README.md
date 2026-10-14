# aether

The whole machine as numbers: CPU, memory, GPUs, sensors, disks, network, battery, and every
process with what it uses. In a terminal it draws a full-screen view; piped, it prints numbers.

It is a Rust library (the core) with a small command-line program on top. Licence GPL-3.0-only.

<img src="assets/view.svg" alt="The aether view in a 160 by 45 terminal: a first line with CPU, GPU, RAM and network at a glance, the CPU with its graph and a bar per thread, memory and a GPU with their graphs, disks, network and sensors side by side, and the process table sorted by CPU, each in its own box." width="880">

The values in the picture are made up.

## Install

Needs a stable Rust toolchain.

    cargo install --git https://github.com/tarikee/aether

From a clone:

    cargo install --path .

Or build without installing: `cargo build --release`, then `target/release/aether`.

## What it shows

- CPU: total use, every thread, GHz, temperature, load average.
- Memory: used, available, cached, swap, pressure.
- GPUs: use, VRAM, temperature, fan speed, power.
- Every sensor: temperatures, fans and power, with names.
- Disks: space used, busy time, and read and write rates, per mount.
- Network: receive and send rates per interface, with a graph of each, and the link's speed.
- Battery, on a laptop, and uptime.
- Every process: CPU %, memory, GPU % and VRAM, disk read and write rates, threads, user and age.
  Processes are grouped by app by default.

The full-screen view is shown when the output is a terminal, and not with `--once`, `--json` or
`--plain`. It needs a UTF-8 locale and at least 60 columns and 15 rows. Its first line holds the
whole machine at a glance: CPU, GPU, RAM and network in percent (network as the busier direction's
share of the link's speed, or the rate when the speed is not known). Below that it fills the terminal: each
part in its own box, bars that stretch to their line, graphs of the last readings (CPU, memory,
the first GPU, the first interface), a bar per CPU thread (a cell each when there are too many),
and the process table down to the last row. From 140 columns the CPU sits beside memory and the
GPUs, and disks, network and sensors share a row; from 100, memory sits beside the GPU and disks
beside network; below that, one box per row. A short terminal gives up graph rows, then the
sensors, then the thread bars, so the process table keeps 8 rows (5 under 30 rows).

## The view's keys

| Key | What it does |
|---|---|
| `↑ ↓  PgUp PgDn` | select a process |
| `Home End` | the first or the last one |
| `← →` | the sort key |
| `r` | reverse the order |
| `t` | tree (f: flat; grouped is the default) |
| `/` | filter by name or command line; Enter keeps it, Esc clears it |
| `space` | pause the list |
| `Enter` | the selected process in full |
| `k  K` | end it, or force it (then y or n) |
| `+  -` | the interval |
| `s` | every sensor, in place of the table |
| `?` | this help |
| `q` | quit |

## Commands

```
aether: the whole machine as numbers.

Usage:
  aether [-i SECS] [--by KEY] [--reverse] [--steady] [--filter TEXT] [--flat] [--all] [--only LIST] [--json]
      live: every vital and every process, rewritten in place
  aether --once [same options]
      one reading, then exit
  aether port [PORT] [--all] [--json]
      the listening sockets: protocol, address, port, pid, process, user, sorted by port.
      PORT shows who holds that port, listening or connected; --all adds connections.
      A process that cannot be seen shows a dash (root shows it).
  aether proc PID|NAME|:PORT
      one process in full: command line, program, folder, parents, CPU time, open files
  aether kill PID|NAME|:PORT [--force] [-y] [-s NAME|NUMBER]
      asks the process to stop and waits 3 s; says if it is still there.
      --force kills it. By NAME it lists the matches and asks first; -y skips the question.
      -s sends that one signal and does not wait. :PORT is the process listening on it.
  aether check
      what this machine and OS offer, and what is missing
  aether version
      the version (also --version and -V)
  aether help

Options:
  -i, --interval SECS    seconds between live readings (default 1)
  --by KEY               sort the processes: cpu (default), mem, gpu, disk, pid, user,
                         threads, name, age (newest first; user is the uid)
  --reverse              turn the order round
  --steady               with --by cpu, gpu or disk: order by a smoothed value, so rows
                         do not jump every tick (the numbers shown stay raw)
  --filter TEXT          only processes whose name or command line contains TEXT
  --flat                 single processes, not grouped by app
  --tree                 single processes, each under its parent
  --all                  print every row, past the screen's height (port: every connection)
  --only LIST            only these parts, e.g. cpu,mem (default: everything)
  --disk MOUNT[,MOUNT]   only these disks, by mount point (default: every disk)
  --net NAME[,NAME]      only these network interfaces (default: every one)
  --json                 one JSON object per reading; a missing reading is null
  --plain                the plain text view, not the full screen on a terminal
  --no-color             the full screen without colour (NO_COLOR does the same)
  --no-keys              the full screen takes no keys
  -s, --signal NAME|NUMBER  kill: the signal to send (TERM, SIGSTOP, 19; 1 to 64)
  --no-defaults          do not read the file of default arguments (aether/args)
```

## JSON

`aether --json` prints one object per reading, on one line. A reading the machine does not have
is `null`, never 0. Rates and percentages that need two readings are `null` until the second one.

| Key | Holds |
|---|---|
| `t` | time of the reading, in seconds |
| `cpu` | `pct` (whole machine, %), `threads` and `threads_mhz` (one value per thread), `mhz`, `temp_c`, `power_w`, `load` (1, 5 and 15 minute averages) |
| `mem` | `total`, `used`, `available`, `cached`, `swap_total`, `swap_used` (bytes); `pressure` (%) |
| `gpus` | list: `name`, `vendor`, `pct`, `temp_c`, `power_w`, `vram_used`, `vram_total` (bytes), `fan_rpm` |
| `sensors` | list: `chip`, `label`, `kind` (temp, fan or power), `value` |
| `disks` | list: `mount`, `dev`, `total`, `used` (bytes), `read_bps`, `write_bps`, `busy_pct` |
| `nets` | list: `name`, `rx_bps`, `tx_bps`, `link_bps`, `addrs` |
| `battery` | `pct`, `state`, `power_w`, `minutes_left`; `null` when there is no battery |
| `host` | `uptime_s`, `threads` |
| `procs` | list: `pid`, `ppid`, `uid`, `threads`, `name`, `state`, `cpu_pct`, `gpu_pct`, `mem`, `rss`, `started_s`, `vram`, `read_bps`, `write_bps` |

Rates are bytes per second. CPU per process is a percent of the whole machine. Memory per process
is resident minus shared.

## How light

Measured on Linux, on a machine with 12 threads and 558 processes running. Each monitor ran alone, after the one before
it had ended, for 45 seconds of reading, with a reading every second. CPU is the ticks of the
process and its live children from `/proc`; memory is its peak resident size; the program size
is the file that runs.

Each monitor runs in a pseudo-terminal of 50 rows by 160 columns, except the JSON reading, which
has no terminal. It is left 2 seconds to settle, then read from `/proc`.

| Monitor | Version | CPU seconds | % of one core | Peak memory (MB) | Read calls per reading | Program (kB) |
|---|---|---:|---:|---:|---:|---:|
| aether | aether 0.1.0 | 0.34 | 0.76 | 3.9 | 340.6 | 763 |
| aether --json | aether 0.1.0 | 0.29 | 0.64 | 3.6 | 332.5 | 763 |
| top | top from procps-ng 4.0.6 | 0.77 | 1.71 | 21.6 | 1141.0 | 142 |
| htop | htop 3.4.1 | 5.90 | 13.10 | 20.0 | 11631.4 | 373 |
| btop | btop version: 1.4.7 | 0.69 | 1.53 | 24.6 | 639.5 | 1614 |
| bottom | bottom 0.14.9 | 3.30 | 7.33 | 8.3 | 3091.0 | 4988 |
| glances | Glances version:	4.5.8 | 6.54 | 14.52 | 59.5 | 4680.6 |  |
| atop | Version: 2.13.0 - 2026/07/18 19:38:52    | 8.11 | 18.01 | 37.2 | 23934.9 | 391 |
| gotop | gotop v4.2.0 (20220929T182223) | 1.62 | 3.60 | 23.0 | 2632.9 | 7552 |
| zenith | zenith 0.15.1 | 7.43 | 16.50 | 48.0 | 23956.9 | 2267 |

Run `python3 bench/weigh.py` to measure on your own machine. It needs only the standard library,
and it reads `/proc`, so it does nothing on other systems.

A figure is as fresh as its source: a busy process is read on every reading and a sleeping one on
every third, so an idle process's numbers can be up to three readings old; a busy process's shared
memory is up to three looks old and its disk rates average over the last two looks; a kernel
thread shows no disk rate.

## Next to btop

| | aether | btop |
|---|---|---|
| CPU, memory and file size | Measured in the table above, lighter than btop | Measured in the table above |
| GPU use, VRAM and disk rate per process, in the list | yes | not in the list |
| JSON output | yes | — |
| One file of default arguments | yes | — |
| Mouse | not yet | yes |
| Themes and layout presets | not yet | yes |
| Graphs per core and per process | not yet | yes |
| NVIDIA and Intel GPUs | not yet | yes |
| Windows | yes (see Status) | no (only the separate port btop4win) |
| macOS and the BSDs | not yet (see Status) | yes |

Both have: a tree, a filter, sort keys, signals, disks, network, sensors and battery.

## Status

| System | Status |
|---|---|
| Linux | Built and run. AMD GPUs are read. NVIDIA and Intel GPUs are not yet. |
| Windows | Built; its tests run on GitHub's Windows machine. Everything is read except sensors, load average and GPU use per process. GPU and battery are not yet seen on a real machine, and the view's keys are not built on Windows yet. |
| macOS | Compiles (checked with `cargo check`, not run). Every source says "not built for this OS yet", and the view falls back to plain. |

## Licence

GPL-3.0-only. See `LICENSE`.
