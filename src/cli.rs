//! Arguments and help, by hand. The grammar is written once, here.

use std::io::Write;
use std::process::ExitCode;

/// How the processes are sorted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Sort {
    Cpu,
    Mem,
    Gpu,
    Disk,
}

/// What a view shows and how.
#[derive(Clone, Debug, PartialEq)]
pub struct View {
    pub by: Sort,
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

/// The parsed command line.
#[derive(Clone, Debug, PartialEq)]
pub struct Parsed {
    /// The command's name, as the list in `main.rs` has it.
    pub command: &'static str,
    /// The PID or NAME for `proc` and `kill`.
    pub target: Option<String>,
    pub once: bool,
    pub json: bool,
    /// Seconds between live readings.
    pub interval_s: f64,
    /// Whether `-i` was given: `--once` then waits that long between its two readings.
    pub interval_given: bool,
    pub force: bool,
    pub yes: bool,
    pub view: View,
}

/// The grammar, as `aether help` prints it.
pub const HELP: &str = "\
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
  --tree                 single processes, each under its parent
  --all                  print every row, past the screen's height
  --only LIST            only these parts, e.g. cpu,mem (default: everything)
  --disk MOUNT[,MOUNT]   only these disks, by mount point (default: every disk)
  --net NAME[,NAME]      only these network interfaces (default: every one)
  --json                 one JSON object per reading; a missing reading is null
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
        interval_s: 1.0,
        interval_given: false,
        force: false,
        yes: false,
        view: View {
            by: Sort::Cpu,
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
            "--flat" => p.view.flat = true,
            "--tree" => p.view.tree = true,
            "--all" => p.view.all = true,
            "--force" => p.force = true,
            "-y" | "--yes" => p.yes = true,
            "-i" | "--interval" => {
                let v = value(&mut it, a)?;
                p.interval_given = true;
                p.interval_s = v
                    .parse::<f64>()
                    .ok()
                    .filter(|s| s.is_finite() && *s > 0.0)
                    .ok_or_else(|| format!("{a} needs a number of seconds above 0"))?;
            }
            "--by" => {
                p.view.by = match value(&mut it, a)? {
                    "cpu" => Sort::Cpu,
                    "mem" => Sort::Mem,
                    "gpu" => Sort::Gpu,
                    "disk" => Sort::Disk,
                    other => return Err(format!("--by takes cpu, mem, gpu or disk, not {other}")),
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
        ["proc" | "kill", ..] => return Err("needs one PID or NAME".to_string()),
        [w, ..] => return Err(format!("unknown command {w}")),
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
    fn kill_takes_a_target_and_flags() {
        let p = parse(&args("kill 1234 --force -y")).unwrap();
        assert_eq!(p.command, "kill");
        assert_eq!(p.target.as_deref(), Some("1234"));
        assert!(p.force && p.yes);
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
    fn bad_input_is_an_error() {
        assert!(parse(&args("frobnicate")).is_err());
        assert!(parse(&args("proc")).is_err());
        assert!(parse(&args("--by disk2")).is_err());
        assert!(parse(&args("-i 0")).is_err());
        assert!(parse(&args("--nope")).is_err());
    }
}
