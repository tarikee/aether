//! Arguments and help, by hand. The grammar is written once, here.

use std::cmp::Ordering;
use std::io::Write;
use std::process::ExitCode;

/// How the processes are sorted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Sort {
    Cpu,
    Mem,
    Gpu,
    Disk,
    Pid,
    User,
    Threads,
    Name,
    Age,
}

impl Sort {
    /// Whether the key starts from the biggest (or the newest) value; the others start from
    /// the smallest, so `pid`, `user` and `name` read A to Z.
    pub fn largest_first(self) -> bool {
        !matches!(self, Sort::Pid | Sort::User | Sort::Name)
    }

    /// Whether `--steady` smooths this key (the ones that move every tick).
    pub fn smooths(self) -> bool {
        matches!(self, Sort::Cpu | Sort::Gpu | Sort::Disk)
    }
}

/// What a row is ordered by: the fields of one process, or of one group.
pub struct Keys<'a> {
    pub pid: u32,
    pub uid: u32,
    pub threads: u32,
    pub started: u64,
    pub name: &'a str,
    /// The number for the numeric keys (missing is -1).
    pub rank: f64,
}

/// Compares two rows in the order the view asks for. Equal values fall back to the pid, so
/// rows never swap places for nothing; `--reverse` turns the order round (pid stays ascending).
pub fn compare(view: &View, a: &Keys, b: &Keys) -> Ordering {
    let ord = match view.by {
        Sort::Pid => a.pid.cmp(&b.pid),
        Sort::User => a.uid.cmp(&b.uid),
        Sort::Threads => a.threads.cmp(&b.threads),
        Sort::Name => a.name.cmp(b.name),
        Sort::Age => a.started.cmp(&b.started),
        Sort::Cpu | Sort::Mem | Sort::Gpu | Sort::Disk => a.rank.total_cmp(&b.rank),
    };
    let ord = if view.by.largest_first() {
        ord.reverse()
    } else {
        ord
    };
    let ord = if view.reverse { ord.reverse() } else { ord };
    ord.then(a.pid.cmp(&b.pid))
}

/// What a view shows and how.
#[derive(Clone, Debug, PartialEq)]
pub struct View {
    pub by: Sort,
    pub reverse: bool,
    pub steady: bool,
    /// Only processes whose name or command line contains this (ASCII case ignored).
    pub filter: Option<String>,
    pub flat: bool,
    /// Each single process under its parent (implies single processes).
    pub tree: bool,
    pub all: bool,
    /// The parts to show (`cpu`, `mem`, ...); `None` shows everything.
    pub only: Option<Vec<String>>,
    /// The mounts `--disk` names; `None` shows every disk.
    pub disks: Option<Vec<String>>,
    /// The interfaces `--net` names; `None` shows every one.
    pub nets: Option<Vec<String>>,
}

impl View {
    /// Whether `--disk` shows this mount (no `--disk` shows every disk).
    pub fn shows_disk(&self, mount: &str) -> bool {
        named(&self.disks, mount)
    }

    /// Whether `--net` shows this interface (no `--net` shows every one).
    pub fn shows_net(&self, name: &str) -> bool {
        named(&self.nets, name)
    }

    /// Whether `--only` names this part (no `--only` shows everything).
    pub fn shows(&self, part: &str) -> bool {
        match &self.only {
            None => true,
            Some(list) => list.iter().any(|p| p == part),
        }
    }
}

/// Whether a list named on the command line has this name (no list names everything).
fn named(list: &Option<Vec<String>>, name: &str) -> bool {
    match list {
        None => true,
        Some(names) => names.iter().any(|n| n == name),
    }
}

/// The parts `--only` takes.
pub const PARTS: &[&str] = &[
    "cpu", "mem", "gpu", "sensors", "disk", "net", "battery", "host", "procs",
];

/// The names of what a reading has, for a message: `a, b`, or `none`.
fn there(have: &[&str]) -> String {
    if have.is_empty() {
        "none".to_string()
    } else {
        have.join(", ")
    }
}

/// The line for a mount that the reading does not have.
pub fn missing_mount(name: &str, r: &aether::Reading) -> String {
    let have: Vec<&str> = r.disks.iter().map(|d| d.mount.as_str()).collect();
    format!("no mount \"{name}\"; there are: {}", there(&have))
}

/// The line for the first `--disk` mount or `--net` interface the reading does not have.
pub fn missing_name(view: &View, r: &aether::Reading) -> Option<String> {
    if let Some(list) = &view.disks {
        if let Some(name) = list
            .iter()
            .find(|n| !r.disks.iter().any(|d| &d.mount == *n))
        {
            return Some(missing_mount(name, r));
        }
    }
    if let Some(list) = &view.nets {
        if let Some(name) = list.iter().find(|n| !r.nets.iter().any(|x| &x.name == *n)) {
            let have: Vec<&str> = r.nets.iter().map(|x| x.name.as_str()).collect();
            return Some(format!(
                "no interface \"{name}\"; there are: {}",
                there(&have)
            ));
        }
    }
    None
}

/// The parsed command line.
#[derive(Clone, Debug, PartialEq)]
pub struct Parsed {
    /// The command's name, as the list in `main.rs` has it.
    pub command: &'static str,
    /// The PID, NAME or :PORT for `proc` and `kill`; the PORT for `port`.
    pub target: Option<String>,
    pub once: bool,
    pub json: bool,
    /// `--plain`: the plain text view even on a terminal that can show the full screen.
    pub plain: bool,
    /// `--no-color`: the full-screen view without colour codes.
    pub no_color: bool,
    /// `--no-keys`: the full-screen view takes no keys.
    pub no_keys: bool,
    /// `--no-mouse`: the full-screen view does not take the mouse.
    pub no_mouse: bool,
    /// Seconds between live readings.
    pub interval_s: f64,
    /// Whether `-i` was given: `--once` then waits that long between its two readings.
    pub interval_given: bool,
    pub force: bool,
    pub yes: bool,
    /// The signal for `kill --signal`, as written (a name or a number).
    pub signal: Option<String>,
    /// `--no-defaults`: the file of default arguments is not read.
    pub no_defaults: bool,
    /// `record --to FILE`: where the readings are written.
    pub to: Option<String>,
    /// `record --all-procs`: every process on each recorded line, whole.
    pub all_procs: bool,
    /// `record --for DURATION`, in seconds.
    pub duration_s: Option<f64>,
    /// `alert --run CMD`: the command run on each firing.
    pub run: Option<String>,
    /// The rules of `alert`, as written.
    pub rules: Vec<String>,
    pub view: View,
}

/// A length of time as `90s`, `30m` or `8h` (a whole or decimal number and one unit), in seconds.
/// The shortest and longest `-i` interval, in seconds.
const MIN_INTERVAL_S: f64 = 0.1;
const MAX_INTERVAL_S: f64 = 3600.0;
/// The longest `record --for`, in seconds: 30 days.
const MAX_FOR_S: f64 = 30.0 * 86400.0;

pub fn duration(text: &str) -> Result<f64, String> {
    let bad = || format!("{text} is not a time: write 90s, 30m or 8h");
    let (num, unit) = text.split_at(text.len().saturating_sub(1));
    let scale = match unit {
        "s" => 1.0,
        "m" => 60.0,
        "h" => 3600.0,
        _ => return Err(bad()),
    };
    num.parse::<f64>()
        .ok()
        .filter(|n| n.is_finite() && *n > 0.0)
        .map(|n| n * scale)
        .ok_or_else(bad)
}

/// The grammar, as `aether help` prints it.
pub const HELP: &str = "\
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
  aether proc PID|NAME|:PORT [--json]
      one process in full: command line (secrets masked), program, folder, parents, CPU time,
      threads, open files. --json prints it as one object (an array when a name matches several).
  aether kill PID|NAME|:PORT [--force] [-y] [-s NAME|NUMBER] [--json]
      asks the process to stop and waits 3 s; says if it is still there.
      --force kills it. By NAME it lists the matches and asks first; -y skips the question.
      -s sends that one signal and does not wait. :PORT is the process listening on it.
      --json needs -y; it never asks and prints one object with a result per process.
  aether record [--to FILE] [--all-procs] [--for DURATION] [-i SECS] [--only LIST]
      reads without drawing until Ctrl-C or DURATION (90s, 30m, 8h), then prints the peaks:
      the highest value of each reading, when, and the average; the five processes with the
      most CPU and the five with the most memory. --json prints that as one object.
      --to FILE also writes one JSON line per reading, with the 10 processes using the most
      CPU and the 10 using the most memory (pid, name, CPU, memory); --all-procs keeps all.
  aether peaks FILE
      the same summary from a file `record --to` wrote (or from any `aether --json` output)
  aether alert RULE... [--run CMD] [-i SECS] [--once] [--json]
      prints a line when a rule starts and stops holding. RULE is KEY>VALUE or KEY<VALUE, with
      an optional hold time: cpu>90/30s. KEY: cpu, mem, swap, gpu, vram, temp (hottest, C),
      disk:MOUNT (used %), net (the first line's NET %), battery.
      --run CMD runs CMD (sh -c; cmd /C on Windows) at each firing, with AETHER_RULE,
      AETHER_VALUE and AETHER_STATE set. --once: two readings, the rules that hold are
      printed; exit 1 if any holds, 0 if none. --json prints each line as one object
      (time in Unix seconds, state, value).
  aether check
      what this machine and OS offer, and what is missing
  aether version
      the version (also --version and -V)
  aether help

Options:
  -i, --interval SECS    seconds between live readings, 0.1 to 3600 (default 1)
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
  --no-keys              the full screen takes no keys and no mouse
  --no-mouse             the full screen takes keys but not the mouse
  --to FILE              record: also write every reading to FILE, one JSON line each
  --all-procs            record: write every process, whole, on each line of --to FILE
  --for DURATION         record: stop after this long (90s, 30m, 8h; at most 720h)
  --run CMD              alert: the command run when a rule fires
  -s, --signal NAME|NUMBER  kill: the signal to send (TERM, SIGSTOP, 19; 1 to 64)
  --no-defaults          do not read the file of default arguments (aether/args)
";

/// `aether version`: prints the program's version.
pub fn version(_parsed: &Parsed) -> ExitCode {
    let _ = writeln!(std::io::stdout(), "aether {}", env!("CARGO_PKG_VERSION"));
    ExitCode::SUCCESS
}

/// `aether help`: prints the grammar.
pub fn help(_parsed: &Parsed) -> ExitCode {
    let _ = std::io::stdout().write_all(HELP.as_bytes());
    ExitCode::SUCCESS
}

/// The value after a flag that takes one.
fn value<'a>(it: &mut std::slice::Iter<'a, String>, flag: &str) -> Result<&'a str, String> {
    it.next()
        .map(String::as_str)
        .ok_or_else(|| format!("{flag} needs a value"))
}

/// Parses the arguments (without the program name).
pub fn parse(args: &[String]) -> Result<Parsed, String> {
    let mut p = Parsed {
        command: "live",
        target: None,
        once: false,
        json: false,
        plain: false,
        no_color: false,
        no_keys: false,
        no_mouse: false,
        interval_s: 1.0,
        interval_given: false,
        force: false,
        yes: false,
        signal: None,
        no_defaults: false,
        all_procs: false,
        to: None,
        duration_s: None,
        run: None,
        rules: Vec::new(),
        view: View {
            by: Sort::Cpu,
            reverse: false,
            steady: false,
            filter: None,
            flat: false,
            tree: false,
            all: false,
            only: None,
            disks: None,
            nets: None,
        },
    };
    let mut words: Vec<&str> = Vec::new();
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        let a = arg.as_str();
        match a {
            "-h" | "--help" => p.command = "help",
            "-V" | "--version" => p.command = "version",
            "--once" => p.once = true,
            "--json" => p.json = true,
            "--plain" => p.plain = true,
            "--no-color" => p.no_color = true,
            "--no-keys" => p.no_keys = true,
            "--no-mouse" => p.no_mouse = true,
            "--flat" => p.view.flat = true,
            "--reverse" => p.view.reverse = true,
            "--steady" => p.view.steady = true,
            "--filter" => {
                let text = value(&mut it, a)?;
                if text.is_empty() {
                    return Err("--filter needs some text".to_string());
                }
                p.view.filter = Some(text.to_string());
            }
            "--tree" => p.view.tree = true,
            "--all" => p.view.all = true,
            "--force" => p.force = true,
            "--all-procs" => p.all_procs = true,
            "-y" | "--yes" => p.yes = true,
            "--no-defaults" => p.no_defaults = true,
            "--to" => p.to = Some(value(&mut it, a)?.to_string()),
            "--for" => {
                let secs = duration(value(&mut it, a)?)?;
                if secs > MAX_FOR_S {
                    return Err("--for takes at most 720h (30 days)".to_string());
                }
                p.duration_s = Some(secs);
            }
            "--run" => p.run = Some(value(&mut it, a)?.to_string()),
            "-s" | "--signal" => p.signal = Some(value(&mut it, a)?.to_string()),
            "-i" | "--interval" => {
                let v = value(&mut it, a)?;
                p.interval_given = true;
                p.interval_s = v
                    .parse::<f64>()
                    .ok()
                    .filter(|s| (MIN_INTERVAL_S..=MAX_INTERVAL_S).contains(s))
                    .ok_or_else(|| {
                        format!("{a} needs a number of seconds from 0.1 to 3600, not {v}")
                    })?;
            }
            "--by" => {
                p.view.by = match value(&mut it, a)? {
                    "cpu" => Sort::Cpu,
                    "mem" => Sort::Mem,
                    "gpu" => Sort::Gpu,
                    "disk" => Sort::Disk,
                    "pid" => Sort::Pid,
                    "user" => Sort::User,
                    "threads" => Sort::Threads,
                    "name" => Sort::Name,
                    "age" => Sort::Age,
                    other => {
                        return Err(format!(
                            "--by takes cpu, mem, gpu, disk, pid, user, threads, name or age, not {other}"
                        ))
                    }
                };
            }
            "--disk" => {
                let list = value(&mut it, a)?;
                p.view.disks = Some(list.split(',').map(str::to_string).collect());
            }
            "--net" => {
                let list = value(&mut it, a)?;
                p.view.nets = Some(list.split(',').map(str::to_string).collect());
            }
            "--only" => {
                let list = value(&mut it, a)?;
                if let Some(bad) = list.split(',').find(|s| !PARTS.contains(s)) {
                    return Err(format!(
                        "no part \"{bad}\"; there are: {}",
                        PARTS.join(", ")
                    ));
                }
                p.view.only = Some(list.split(',').map(str::to_string).collect());
            }
            _ if a.starts_with('-') && a.len() > 1 => return Err(format!("unknown option {a}")),
            _ => words.push(a),
        }
    }
    if p.command == "help" || p.command == "version" {
        return Ok(p);
    }
    match words.as_slice() {
        [] => {}
        ["help"] => p.command = "help",
        ["version"] => p.command = "version",
        ["check"] => p.command = "check",
        ["proc", t] => {
            p.command = "proc";
            p.target = Some((*t).to_string());
        }
        ["kill", t] => {
            p.command = "kill";
            p.target = Some((*t).to_string());
        }
        ["record"] => p.command = "record",
        ["peaks", f] => {
            p.command = "peaks";
            p.target = Some((*f).to_string());
        }
        ["alert", rules @ ..] if !rules.is_empty() => {
            p.command = "alert";
            p.rules = rules.iter().map(|r| (*r).to_string()).collect();
        }
        ["peaks"] => return Err("peaks needs a FILE".to_string()),
        ["alert"] => return Err("alert needs at least one rule, like cpu>90/30s".to_string()),
        ["record", ..] => return Err("record takes no argument; use --to FILE".to_string()),
        ["peaks", ..] => return Err("peaks needs one FILE".to_string()),
        ["port"] => p.command = "port",
        ["port", t] => {
            if crate::cmd::port::parse_port(t).is_none() {
                return Err(format!("{t} is not a port number (0 to 65535)"));
            }
            p.command = "port";
            p.target = Some((*t).to_string());
        }
        ["port", ..] => return Err("port takes at most one PORT".to_string()),
        ["proc" | "kill", ..] => return Err("needs one PID or NAME".to_string()),
        [w, ..] => return Err(format!("unknown command {w}")),
    }
    if p.signal.is_some() && p.command != "kill" {
        return Err("--signal goes with kill".to_string());
    }
    if (p.to.is_some() || p.duration_s.is_some()) && p.command != "record" {
        return Err("--to and --for go with record".to_string());
    }
    if p.all_procs && p.command != "record" {
        return Err("--all-procs goes with record".to_string());
    }
    if p.run.is_some() && p.command != "alert" {
        return Err("--run goes with alert".to_string());
    }
    if p.signal.is_some() && p.force {
        return Err("--signal and --force do not go together".to_string());
    }
    Ok(p)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    fn args(line: &str) -> Vec<String> {
        line.split_whitespace().map(str::to_string).collect()
    }

    #[test]
    fn no_arguments_is_live() {
        let p = parse(&args("")).unwrap();
        assert_eq!(p.command, "live");
        assert!(!p.once);
        assert_eq!(p.view.by, Sort::Cpu);
    }

    #[test]
    fn port_takes_an_optional_port() {
        assert_eq!(parse(&args("port")).unwrap().command, "port");
        let p = parse(&args("port :8080 --all --json")).unwrap();
        assert_eq!(p.target.as_deref(), Some(":8080"));
        assert!(p.view.all && p.json);
        assert!(parse(&args("port web")).is_err());
        assert!(parse(&args("port 1 2")).is_err());
        assert_eq!(
            parse(&args("kill :80")).unwrap().target.as_deref(),
            Some(":80")
        );
    }

    #[test]
    fn kill_takes_a_target_and_flags() {
        let p = parse(&args("kill 1234 --force -y")).unwrap();
        assert_eq!(p.command, "kill");
        assert_eq!(p.target.as_deref(), Some("1234"));
        assert!(p.force && p.yes);
    }

    #[test]
    fn signal_is_for_kill_only_and_not_with_force() {
        let p = parse(&args("kill 1234 -s STOP")).unwrap();
        assert_eq!(p.signal.as_deref(), Some("STOP"));
        assert_eq!(
            parse(&args("kill 1234 --signal 19"))
                .unwrap()
                .signal
                .as_deref(),
            Some("19")
        );
        assert!(parse(&args("-s TERM")).is_err());
        assert!(parse(&args("kill 1234 -s TERM --force")).is_err());
        assert!(parse(&args("kill 1234 -s")).is_err());
    }

    #[test]
    fn no_defaults_is_read() {
        assert!(parse(&args("--once --no-defaults")).unwrap().no_defaults);
        assert!(!parse(&args("--once")).unwrap().no_defaults);
    }

    #[test]
    fn disk_and_net_lists_are_read() {
        let p = parse(&args("--once --disk /,/data --net enp3s0")).unwrap();
        assert_eq!(
            p.view.disks,
            Some(vec!["/".to_string(), "/data".to_string()])
        );
        assert_eq!(p.view.nets, Some(vec!["enp3s0".to_string()]));
        assert!(p.view.shows_disk("/data") && !p.view.shows_disk("/home"));
        assert!(p.view.shows_net("enp3s0") && !p.view.shows_net("wlan0"));
    }

    #[test]
    fn sort_and_only_are_read() {
        let p = parse(&args("--once --by gpu --only cpu,mem --json")).unwrap();
        assert!(p.once && p.json);
        assert_eq!(p.view.by, Sort::Gpu);
        assert_eq!(
            p.view.only,
            Some(vec!["cpu".to_string(), "mem".to_string()])
        );
    }

    #[test]
    fn record_peaks_and_alert_are_read() {
        let p = parse(&args("record --to f.jsonl --for 90s -i 2")).unwrap();
        assert_eq!(p.command, "record");
        assert_eq!(
            (p.to.as_deref(), p.duration_s, p.interval_s),
            (Some("f.jsonl"), Some(90.0), 2.0)
        );
        assert_eq!(
            parse(&args("record --for 8h")).unwrap().duration_s,
            Some(28800.0)
        );
        assert!(parse(&args("record --all-procs")).unwrap().all_procs);
        assert!(!parse(&args("record")).unwrap().all_procs);
        assert!(parse(&args("--all-procs")).is_err());
        assert!(parse(&args("record --for 90")).is_err());
        assert!(parse(&args("record --for 0s")).is_err());
        let p = parse(&args("peaks f.jsonl --json")).unwrap();
        assert_eq!((p.command, p.target.as_deref()), ("peaks", Some("f.jsonl")));
        assert!(parse(&args("peaks")).is_err());
        let p = parse(&args("alert cpu>90/30s mem>80 --run beep --once")).unwrap();
        assert_eq!(p.command, "alert");
        assert_eq!(p.rules, vec!["cpu>90/30s", "mem>80"]);
        assert_eq!(p.run.as_deref(), Some("beep"));
        assert!(parse(&args("alert")).is_err());
        assert!(parse(&args("--to f")).is_err());
        assert!(parse(&args("--run x")).is_err());
    }

    #[test]
    fn bad_input_is_an_error() {
        assert!(parse(&args("frobnicate")).is_err());
        assert!(parse(&args("proc")).is_err());
        assert!(parse(&args("--by disk2")).is_err());
        assert!(parse(&args("-i 0")).is_err());
        assert!(parse(&args("--nope")).is_err());
    }

    #[test]
    fn unknown_parts_and_mounts_are_named() {
        let err = parse(&args("--only cpu,nope")).unwrap_err();
        assert!(
            err.contains("no part \"nope\"; there are: cpu, mem"),
            "{err}"
        );
        assert!(parse(&args("--only cpu,mem,net,procs")).is_ok());
        let view = parse(&args("--disk /x")).unwrap().view;
        let empty = aether::Reading::default();
        assert_eq!(
            missing_name(&view, &empty).as_deref(),
            Some("no mount \"/x\"; there are: none")
        );
        let net = parse(&args("--net nope")).unwrap().view;
        assert_eq!(
            missing_name(&net, &empty).as_deref(),
            Some("no interface \"nope\"; there are: none")
        );
        assert!(missing_name(&parse(&args("--once")).unwrap().view, &empty).is_none());
    }

    #[test]
    fn interval_and_for_have_ranges() {
        assert!(parse(&args("-i 0.1")).is_ok());
        assert!(parse(&args("-i 3600")).is_ok());
        assert!(parse(&args("-i 0.09")).is_err());
        assert!(parse(&args("-i 3600.5")).is_err());
        assert!(parse(&args("-i NaN")).is_err());
        assert!(parse(&args("record -i 0.05")).is_err());
        assert!(parse(&args("record --for 720h")).is_ok());
        assert!(parse(&args("record --for 721h")).is_err());
        assert!(parse(&args("record --for 0m")).is_err());
    }

    #[test]
    fn filter_reverse_steady_and_the_new_keys_are_read() {
        let p = parse(&args("--once --by name --reverse --steady --filter Aether")).unwrap();
        assert_eq!(p.view.by, Sort::Name);
        assert!(p.view.reverse && p.view.steady);
        assert_eq!(p.view.filter.as_deref(), Some("Aether"));
        assert!(parse(&["--filter".to_string(), String::new()]).is_err());
        assert!(parse(&args("--filter")).is_err());
        assert_eq!(parse(&args("--by age")).unwrap().view.by, Sort::Age);
    }

    fn keys(pid: u32, name: &str, rank: f64) -> Keys<'_> {
        Keys {
            pid,
            uid: 0,
            threads: 1,
            started: u64::from(pid),
            name,
            rank,
        }
    }

    fn view(by: Sort, reverse: bool) -> View {
        View {
            by,
            reverse,
            steady: false,
            filter: None,
            flat: false,
            all: false,
            only: None,
            tree: false,
            disks: None,
            nets: None,
        }
    }

    #[test]
    fn json_is_read_for_proc_kill_and_alert() {
        assert!(parse(&args("proc 1 --json")).unwrap().json);
        let k = parse(&args("kill 1 --json -y")).unwrap();
        assert!(k.json && k.yes);
        assert!(parse(&args("alert cpu>90 --json")).unwrap().json);
    }

    #[test]
    fn unknown_options_fail_for_every_command() {
        for line in [
            "proc 1 --bogus",
            "kill 1 --bogus",
            "alert cpu>1 --bogus",
            "port --bogus",
            "record --bogus",
            "peaks f.jsonl --bogus",
            "check --bogus",
            "version --bogus",
            "help --bogus",
            "--bogus",
            "--bogus proc 1",
        ] {
            assert_eq!(
                parse(&args(line)).unwrap_err(),
                "unknown option --bogus",
                "{line}"
            );
        }
    }

    #[test]
    fn equal_values_fall_back_to_pid_and_reverse_turns_the_order() {
        let (a, b) = (keys(5, "b", 1.0), keys(3, "a", 1.0));
        assert_eq!(compare(&view(Sort::Cpu, false), &a, &b), Ordering::Greater);
        assert_eq!(compare(&view(Sort::Name, false), &a, &b), Ordering::Greater);
        assert_eq!(compare(&view(Sort::Name, true), &a, &b), Ordering::Less);
        assert_eq!(compare(&view(Sort::Age, false), &a, &b), Ordering::Less);
        let c = keys(9, "c", 4.0);
        assert_eq!(compare(&view(Sort::Cpu, false), &c, &a), Ordering::Less);
        assert_eq!(compare(&view(Sort::Pid, false), &a, &b), Ordering::Greater);
    }
}
