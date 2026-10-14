//! `aether alert RULE...`: prints a line when a rule starts and stops holding, and can run a
//! command at each firing. `--once` checks the rules once and exits 1 if any holds.

use crate::cli::{duration, Parsed, View};
use crate::cmd::live::{catch_stops, stop_requested, wait, Sources};
use crate::out::peaks::hottest;
use crate::out::screen::net_pct;
use aether::sys::Root;
use aether::Reading;
use std::io::{self, Write};
use std::process::{Child, Command, ExitCode, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// The wait between the two readings of `--once` when `-i` is not given.
const ONCE_WAIT: Duration = Duration::from_millis(250);

/// What a rule looks at.
#[derive(Clone, Debug, PartialEq)]
pub enum Key {
    Cpu,
    Mem,
    Swap,
    Gpu,
    Vram,
    Temp,
    Disk(String),
    Net,
    Battery,
}

const KEYS: &str = "cpu, mem, swap, gpu, vram, temp, disk:MOUNT, net, battery";

/// `KEY>VALUE` or `KEY<VALUE`, with an optional hold time `/DURATION`.
#[derive(Clone, Debug, PartialEq)]
pub struct Rule {
    pub text: String,
    pub key: Key,
    pub above: bool,
    pub limit: f64,
    /// Seconds the rule must hold before it fires.
    pub hold: f64,
}

/// Parses one rule.
pub fn parse_rule(text: &str) -> Result<Rule, String> {
    let at = text
        .find(['>', '<'])
        .ok_or_else(|| format!("{text}: a rule is KEY>VALUE or KEY<VALUE, like cpu>90/30s"))?;
    let (name, rest) = text.split_at(at);
    let above = rest.starts_with('>');
    let (value, hold) = match rest[1..].split_once('/') {
        Some((v, h)) => (v, Some(h)),
        None => (&rest[1..], None),
    };
    let key = match name {
        "cpu" => Key::Cpu,
        "mem" => Key::Mem,
        "swap" => Key::Swap,
        "gpu" => Key::Gpu,
        "vram" => Key::Vram,
        "temp" => Key::Temp,
        "net" => Key::Net,
        "battery" => Key::Battery,
        _ => match name.strip_prefix("disk:") {
            Some(mount) if !mount.is_empty() => Key::Disk(mount.to_string()),
            _ => return Err(format!("{text}: unknown key {name}; the keys are {KEYS}")),
        },
    };
    let limit = value
        .parse::<f64>()
        .ok()
        .filter(|v| v.is_finite())
        .ok_or_else(|| format!("{text}: {value} is not a number"))?;
    let hold = match hold {
        Some(h) => duration(h).map_err(|m| format!("{text}: {m}"))?,
        None => 0.0,
    };
    Ok(Rule {
        text: text.to_string(),
        key,
        above,
        limit,
        hold,
    })
}

fn share(part: u64, total: u64) -> Option<f64> {
    (total > 0).then(|| part as f64 / total as f64 * 100.0)
}

impl Rule {
    /// The value this rule looks at, in a reading; `None` when the machine does not give it.
    pub fn value(&self, r: &Reading, view: &View) -> Option<f64> {
        match &self.key {
            Key::Cpu => r.cpu.pct.map(f64::from),
            Key::Mem => share(r.mem.used, r.mem.total),
            Key::Swap => share(r.mem.swap_used, r.mem.swap_total),
            Key::Gpu => r
                .gpus
                .iter()
                .filter_map(|g| g.pct.map(f64::from))
                .max_by(f64::total_cmp),
            Key::Vram => r
                .gpus
                .iter()
                .filter_map(|g| share(g.vram_used?, g.vram_total?))
                .max_by(f64::total_cmp),
            Key::Temp => hottest(r),
            Key::Disk(m) => r
                .disks
                .iter()
                .find(|d| d.mount == *m)
                .and_then(|d| share(d.used, d.total)),
            Key::Net => r
                .nets
                .iter()
                .find(|n| view.shows_net(&n.name))
                .and_then(net_pct)
                .map(f64::from),
            Key::Battery => r.battery.as_ref().map(|b| f64::from(b.pct)),
        }
    }

    /// Whether the value is past the limit.
    pub fn holds(&self, v: f64) -> bool {
        if self.above {
            v > self.limit
        } else {
            v < self.limit
        }
    }

    /// The parts of a reading this rule needs, by the names `--only` uses.
    fn parts(&self) -> &'static [&'static str] {
        match self.key {
            Key::Cpu => &["cpu"],
            Key::Mem | Key::Swap => &["mem"],
            Key::Gpu | Key::Vram => &["gpu"],
            Key::Temp => &["cpu", "gpu", "sensors"],
            Key::Disk(_) => &["disk"],
            Key::Net => &["net"],
            Key::Battery => &["battery"],
        }
    }

    /// The value as a line shows it: a percent, or degrees for `temp`.
    pub fn show(&self, v: f64) -> String {
        let n = if v.abs() < 9.95 {
            format!("{v:.1}")
        } else {
            format!("{v:.0}")
        };
        match self.key {
            Key::Temp => format!("{n}\u{b0}C"),
            _ => format!("{n}%"),
        }
    }
}

/// A rule's progress: since when it has held, and whether it has fired.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct State {
    since: Option<f64>,
    fired: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Event {
    Fired,
    Cleared,
}

impl State {
    /// One reading, at time `now` (seconds, any start). A rule fires when it has held for its
    /// hold time and clears when it stops holding; a missing value does not hold.
    pub fn step(&mut self, rule: &Rule, now: f64, value: Option<f64>) -> Option<Event> {
        if value.is_some_and(|v| rule.holds(v)) {
            let since = *self.since.get_or_insert(now);
            if !self.fired && now - since >= rule.hold {
                self.fired = true;
                return Some(Event::Fired);
            }
        } else {
            self.since = None;
            if self.fired {
                self.fired = false;
                return Some(Event::Cleared);
            }
        }
        None
    }
}

/// The UTC time of day as (hour, minute, second).
fn utc_hms() -> Option<(u32, u32, u32)> {
    let d = SystemTime::now().duration_since(UNIX_EPOCH).ok()?.as_secs();
    Some((
        (d / 3600 % 24) as u32,
        (d / 60 % 60) as u32,
        (d % 60) as u32,
    ))
}

/// The time of day, `HH:MM:SS`: local where the system gives it, else UTC.
fn clock() -> String {
    #[cfg(target_os = "linux")]
    let local = aether::sys::linux::calls::local_hms();
    #[cfg(windows)]
    let local = aether::sys::windows::calls::local_hms();
    #[cfg(not(any(target_os = "linux", windows)))]
    let local = utc_hms();
    let (h, m, s) = local.unwrap_or_else(|| {
        let d = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        (
            (d / 3600 % 24) as u32,
            (d / 60 % 60) as u32,
            (d % 60) as u32,
        )
    });
    format!("{h:02}:{m:02}:{s:02}")
}

/// Runs the command through the shell, not waiting for it.
fn spawn(cmd: &str, rule: &Rule, value: Option<f64>) -> Option<Child> {
    #[cfg(windows)]
    let mut c = {
        let mut c = Command::new("cmd");
        c.arg("/C").arg(cmd);
        c
    };
    #[cfg(not(windows))]
    let mut c = {
        let mut c = Command::new("sh");
        c.arg("-c").arg(cmd);
        c
    };
    c.env("AETHER_RULE", &rule.text)
        .env(
            "AETHER_VALUE",
            value.map_or(String::new(), |v| format!("{v:.1}")),
        )
        .env("AETHER_STATE", "fired")
        .stdin(Stdio::null())
        .spawn()
        .ok()
}

/// `aether alert`.
pub fn run(parsed: &Parsed) -> ExitCode {
    let mut rules = Vec::new();
    for text in &parsed.rules {
        match parse_rule(text) {
            Ok(r) => rules.push(r),
            Err(m) => {
                eprintln!("aether: {m}");
                return ExitCode::from(2);
            }
        }
    }
    let mut parts: Vec<String> = Vec::new();
    for p in rules.iter().flat_map(Rule::parts) {
        if !parts.iter().any(|x| x == p) {
            parts.push((*p).to_string());
        }
    }
    let mut view = parsed.view.clone();
    view.only = Some(parts);
    let root = Root::system();
    let started = Instant::now();
    let mut sources = Sources::open(&root);
    let mut r = Reading::default();
    sources.fill(&mut r, started, &view);
    if parsed.once {
        wait(if parsed.interval_given {
            Duration::from_secs_f64(parsed.interval_s)
        } else {
            ONCE_WAIT
        });
        sources.fill(&mut r, started, &view);
        let mut any = false;
        let mut out = io::stdout().lock();
        for rule in &rules {
            if let Some(v) = rule.value(&r, &view).filter(|v| rule.holds(*v)) {
                any = true;
                if writeln!(out, "{}  holds  ({})", rule.text, rule.show(v)).is_err() {
                    break;
                }
            }
        }
        let _ = out.flush();
        return ExitCode::from(u8::from(any));
    }
    catch_stops();
    wait(ONCE_WAIT);
    let every = Duration::from_secs_f64(parsed.interval_s);
    let mut states = vec![State::default(); rules.len()];
    let mut children: Vec<Child> = Vec::new();
    while !stop_requested() {
        let tick = Instant::now();
        sources.fill(&mut r, started, &view);
        children.retain_mut(|c| matches!(c.try_wait(), Ok(None)));
        for (rule, state) in rules.iter().zip(states.iter_mut()) {
            let value = rule.value(&r, &view);
            let Some(event) = state.step(rule, r.t, value) else {
                continue;
            };
            let word = match event {
                Event::Fired => "fired",
                Event::Cleared => "cleared",
            };
            let shown = value.map_or("no reading".to_string(), |v| rule.show(v));
            let mut out = io::stdout().lock();
            if writeln!(out, "{}  {}  {word}  ({shown})", clock(), rule.text)
                .and_then(|()| out.flush())
                .is_err()
            {
                return ExitCode::SUCCESS;
            }
            if event == Event::Fired {
                if let Some(cmd) = &parsed.run {
                    children.extend(spawn(cmd, rule, value));
                }
            }
        }
        wait(every.saturating_sub(tick.elapsed()));
    }
    ExitCode::SUCCESS
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn rules_are_parsed() {
        let r = parse_rule("cpu>90/30s").unwrap();
        assert_eq!(
            (r.key, r.above, r.limit, r.hold),
            (Key::Cpu, true, 90.0, 30.0)
        );
        let r = parse_rule("battery<20").unwrap();
        assert_eq!(
            (r.key, r.above, r.limit, r.hold),
            (Key::Battery, false, 20.0, 0.0)
        );
        let r = parse_rule("disk:/home>85.5/2m").unwrap();
        assert_eq!(r.key, Key::Disk("/home".to_string()));
        assert_eq!((r.limit, r.hold), (85.5, 120.0));
        assert_eq!(parse_rule("temp>-5").unwrap().limit, -5.0);
    }

    #[test]
    fn bad_rules_say_why() {
        for (bad, word) in [
            ("cpu", "KEY>VALUE"),
            ("cpux>5", "unknown key"),
            ("disk:>5", "unknown key"),
            ("cpu>", "not a number"),
            ("cpu>abc", "not a number"),
            ("cpu>5/", "not a time"),
            ("cpu>5/10", "not a time"),
            ("cpu>5/0s", "not a time"),
            ("cpu>inf", "not a number"),
        ] {
            let e = parse_rule(bad).unwrap_err();
            assert!(e.contains(word), "{bad}: {e}");
        }
    }

    fn feed(rule: &str, steps: &[(f64, Option<f64>)]) -> Vec<Option<Event>> {
        let rule = parse_rule(rule).unwrap();
        let mut s = State::default();
        steps.iter().map(|(t, v)| s.step(&rule, *t, *v)).collect()
    }

    #[test]
    fn a_rule_fires_after_its_hold_and_clears_when_it_stops() {
        let got = feed(
            "cpu>90/3s",
            &[
                (0.0, Some(95.0)),
                (1.0, Some(95.0)),
                (2.0, Some(50.0)),
                (3.0, Some(95.0)),
                (5.0, Some(95.0)),
                (6.0, Some(99.0)),
                (7.0, Some(95.0)),
                (8.0, None),
                (9.0, Some(95.0)),
            ],
        );
        use Event::{Cleared, Fired};
        assert_eq!(
            got,
            vec![
                None,
                None,
                None,
                None,
                None,
                Some(Fired),
                None,
                Some(Cleared),
                None
            ]
        );
    }

    #[test]
    fn no_hold_fires_at_once_and_below_works() {
        use Event::{Cleared, Fired};
        let got = feed(
            "battery<20",
            &[
                (0.0, Some(30.0)),
                (1.0, Some(10.0)),
                (2.0, Some(10.0)),
                (3.0, Some(20.0)),
            ],
        );
        assert_eq!(got, vec![None, Some(Fired), None, Some(Cleared)]);
        let got = feed("cpu>5", &[(0.0, Some(5.0))]);
        assert_eq!(got, vec![None]);
    }

    #[test]
    fn values_come_from_the_reading() {
        let view = crate::cli::parse(&[]).unwrap().view;
        let mut r = Reading::default();
        r.mem.total = 200;
        r.mem.used = 50;
        r.cpu.temp_c = Some(61.0);
        assert_eq!(parse_rule("mem>1").unwrap().value(&r, &view), Some(25.0));
        assert_eq!(parse_rule("swap>1").unwrap().value(&r, &view), None);
        assert_eq!(parse_rule("cpu>1").unwrap().value(&r, &view), None);
        assert_eq!(parse_rule("temp>1").unwrap().value(&r, &view), Some(61.0));
        assert_eq!(parse_rule("temp>1").unwrap().show(61.0), "61\u{b0}C");
        assert_eq!(parse_rule("cpu>1").unwrap().show(3.17), "3.2%");
    }
}
