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
    pub all: bool,
    /// The parts to show (`cpu`, `mem`, ...); `None` shows everything.
    pub only: Option<Vec<String>>,
}

impl View {
    /// Whether `--only` names this part (no `--only` shows everything).
    pub fn shows(&self, part: &str) -> bool {
        match &self.only {
            None => true,
            Some(list) => list.iter().any(|p| p == part),
        }
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
  aether [-i SECS] [--by KEY] [--reverse] [--steady] [--filter TEXT] [--flat] [--all] [--only LIST] [--json]
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
  --by KEY               sort the processes: cpu (default), mem, gpu, disk, pid, user,
                         threads, name, age (newest first; user is the uid)
  --reverse              turn the order round
  --steady               with --by cpu, gpu or disk: order by a smoothed value, so rows
                         do not jump every tick (the numbers shown stay raw)
  --filter TEXT          only processes whose name or command line contains TEXT
  --flat                 single processes, not grouped by app
  --all                  print every row, past the screen's height
  --only LIST            only these parts, e.g. cpu,mem (default: everything)
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
            reverse: false,
            steady: false,
            filter: None,
            flat: false,
            all: false,
            only: None,
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
            "--reverse" => p.view.reverse = true,
            "--steady" => p.view.steady = true,
            "--filter" => {
                let text = value(&mut it, a)?;
                if text.is_empty() {
                    return Err("--filter needs some text".to_string());
                }
                p.view.filter = Some(text.to_string());
            }
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
