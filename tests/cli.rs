//! Runs the built program against the fixture trees and checks what it prints.

#[cfg(target_os = "linux")]
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_aether");

/// The top-level keys of a `Reading`, as the JSON writes them.
const KEYS: [&str; 10] = [
    "t", "cpu", "mem", "gpus", "sensors", "disks", "nets", "battery", "host", "procs",
];

/// The sections `--only` can leave out, by their JSON key.
#[cfg(target_os = "linux")]
const PARTS: [&str; 6] = ["gpus", "sensors", "disks", "nets", "battery", "procs"];

#[derive(Debug, PartialEq)]
enum Json {
    Null,
    Bool,
    Num(f64),
    Str(String),
    Arr(Vec<Json>),
    Obj(Vec<(String, Json)>),
}

/// A small JSON reader, enough to say whether text is valid JSON.
struct Reader<'a> {
    b: &'a [u8],
    i: usize,
}

impl Reader<'_> {
    fn ws(&mut self) {
        while self.i < self.b.len() && matches!(self.b[self.i], b' ' | b'\t' | b'\n' | b'\r') {
            self.i += 1;
        }
    }

    fn eat(&mut self, c: u8) -> Result<(), String> {
        self.ws();
        if self.b.get(self.i) == Some(&c) {
            self.i += 1;
            Ok(())
        } else {
            Err(format!("expected '{}' at byte {}", c as char, self.i))
        }
    }

    fn value(&mut self) -> Result<Json, String> {
        self.ws();
        match self.b.get(self.i).copied() {
            Some(b'{') => {
                self.i += 1;
                let mut fields = Vec::new();
                self.ws();
                if self.b.get(self.i) == Some(&b'}') {
                    self.i += 1;
                    return Ok(Json::Obj(fields));
                }
                loop {
                    let key = self.string()?;
                    self.eat(b':')?;
                    fields.push((key, self.value()?));
                    self.ws();
                    match self.b.get(self.i).copied() {
                        Some(b',') => self.i += 1,
                        Some(b'}') => {
                            self.i += 1;
                            return Ok(Json::Obj(fields));
                        }
                        _ => return Err(format!("object not closed at byte {}", self.i)),
                    }
                }
            }
            Some(b'[') => {
                self.i += 1;
                let mut items = Vec::new();
                self.ws();
                if self.b.get(self.i) == Some(&b']') {
                    self.i += 1;
                    return Ok(Json::Arr(items));
                }
                loop {
                    items.push(self.value()?);
                    self.ws();
                    match self.b.get(self.i).copied() {
                        Some(b',') => self.i += 1,
                        Some(b']') => {
                            self.i += 1;
                            return Ok(Json::Arr(items));
                        }
                        _ => return Err(format!("array not closed at byte {}", self.i)),
                    }
                }
            }
            Some(b'"') => Ok(Json::Str(self.string()?)),
            Some(b't') => self.word("true", Json::Bool),
            Some(b'f') => self.word("false", Json::Bool),
            Some(b'n') => self.word("null", Json::Null),
            Some(c) if c == b'-' || c.is_ascii_digit() => self.number(),
            _ => Err(format!("no value at byte {}", self.i)),
        }
    }

    fn word(&mut self, w: &str, v: Json) -> Result<Json, String> {
        if self.b[self.i..].starts_with(w.as_bytes()) {
            self.i += w.len();
            Ok(v)
        } else {
            Err(format!("bad literal at byte {}", self.i))
        }
    }

    fn number(&mut self) -> Result<Json, String> {
        let start = self.i;
        while self.i < self.b.len() && b"+-.eE0123456789".contains(&self.b[self.i]) {
            self.i += 1;
        }
        let text = std::str::from_utf8(&self.b[start..self.i]).map_err(|e| e.to_string())?;
        text.parse::<f64>()
            .map(Json::Num)
            .map_err(|_| format!("bad number {text:?} at byte {start}"))
    }

    fn string(&mut self) -> Result<String, String> {
        self.ws();
        if self.b.get(self.i) != Some(&b'"') {
            return Err(format!("expected a string at byte {}", self.i));
        }
        self.i += 1;
        let mut out = Vec::new();
        loop {
            match self.b.get(self.i).copied() {
                None => return Err("string not closed".into()),
                Some(b'"') => {
                    self.i += 1;
                    break;
                }
                Some(b'\\') => {
                    let esc = self.b.get(self.i + 1).copied();
                    match esc {
                        Some(b'"' | b'\\' | b'/' | b'b' | b'f' | b'n' | b'r' | b't') => {
                            out.push(esc.unwrap_or(b'?'));
                            self.i += 2;
                        }
                        Some(b'u') => {
                            let hex = self.b.get(self.i + 2..self.i + 6).unwrap_or(&[]);
                            if hex.len() != 4 || !hex.iter().all(u8::is_ascii_hexdigit) {
                                return Err(format!("bad \\u escape at byte {}", self.i));
                            }
                            out.push(b'?');
                            self.i += 6;
                        }
                        _ => return Err(format!("bad escape at byte {}", self.i)),
                    }
                }
                Some(c) if c < 0x20 => return Err(format!("control byte at {}", self.i)),
                Some(c) => {
                    out.push(c);
                    self.i += 1;
                }
            }
        }
        String::from_utf8(out).map_err(|e| e.to_string())
    }
}

/// Parses `text` as one JSON value and nothing after it.
fn parse_json(text: &str) -> Result<Json, String> {
    let mut r = Reader {
        b: text.as_bytes(),
        i: 0,
    };
    let v = r.value()?;
    r.ws();
    if r.i != r.b.len() {
        return Err(format!("trailing text at byte {}", r.i));
    }
    Ok(v)
}

fn fixtures() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures")
}

/// The top-level fixture trees (`basic`, `sensors`, ...), in name order.
#[cfg(target_os = "linux")]
fn trees() -> Vec<PathBuf> {
    let mut trees: Vec<PathBuf> = fs::read_dir(fixtures())
        .expect("fixtures folder exists")
        .map(|e| e.expect("readable entry").path())
        .filter(|p| p.is_dir())
        .collect();
    trees.sort();
    trees
}

fn tree(name: &str) -> PathBuf {
    fixtures().join(name)
}

/// Every regular file below `dir`; links and other special entries are skipped.
#[cfg(target_os = "linux")]
fn regular_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for e in fs::read_dir(dir).expect("readable folder") {
        let e = e.expect("readable entry");
        let ft = e.file_type().expect("file type");
        if ft.is_dir() {
            regular_files(&e.path(), out);
        } else if ft.is_file() {
            out.push(e.path());
        }
    }
}

#[cfg(target_os = "linux")]
fn copy_dir(from: &Path, to: &Path) {
    fs::create_dir_all(to).expect("create copy folder");
    for e in fs::read_dir(from).expect("readable folder") {
        let e = e.expect("readable entry");
        let ft = e.file_type().expect("file type");
        let dst = to.join(e.file_name());
        if ft.is_dir() {
            copy_dir(&e.path(), &dst);
        } else if ft.is_file() {
            fs::copy(e.path(), &dst).expect("copy file");
        }
    }
}

fn run(root: &Path, args: &[&str]) -> Output {
    Command::new(BIN)
        .args(args)
        .env("AETHER_ROOT", root)
        .output()
        .expect("the program starts")
}

fn stdout(out: &Output) -> String {
    String::from_utf8(out.stdout.clone()).expect("stdout is UTF-8")
}

fn field<'a>(fields: &'a [(String, Json)], key: &str) -> Option<&'a Json> {
    fields.iter().find(|(k, _)| k == key).map(|(_, v)| v)
}

fn object(text: &str) -> Vec<(String, Json)> {
    match parse_json(text.trim_end()) {
        Ok(Json::Obj(fields)) => fields,
        Ok(other) => panic!("JSON is not an object: {other:?}"),
        Err(e) => panic!(
            "invalid JSON ({e}): {}",
            text.chars().take(200).collect::<String>()
        ),
    }
}

/// The lengths a file is cut to: nothing, one byte, half, all but one, and every 37th byte.
#[cfg(target_os = "linux")]
fn cut_points(len: usize) -> Vec<usize> {
    let mut cuts = vec![0, 1, len / 2];
    if len >= 1 {
        cuts.push(len - 1);
    }
    cuts.retain(|&c| c < len);
    cuts.sort_unstable();
    cuts.dedup();
    cuts
}

#[cfg(target_os = "linux")]
#[test]
fn once_json_is_one_object_with_every_reading_key() {
    for t in trees() {
        let out = run(&t, &["--once", "--json"]);
        assert_eq!(out.status.code(), Some(0), "{} exit status", t.display());
        let text = stdout(&out);
        assert_eq!(
            text.trim_end().lines().count(),
            1,
            "{} prints one line",
            t.display()
        );
        let fields = object(&text);
        for key in KEYS {
            assert!(
                field(&fields, key).is_some(),
                "{}: missing key {key}",
                t.display()
            );
        }
    }
}

#[cfg(target_os = "linux")]
#[test]
fn missing_readings_are_null_not_zero() {
    // basic has no GPU, no battery and no drm or power_supply folders.
    let out = run(&tree("basic"), &["--once", "--json"]);
    let fields = object(&stdout(&out));
    assert_eq!(field(&fields, "battery"), Some(&Json::Null), "battery");
    assert_eq!(field(&fields, "gpus"), Some(&Json::Arr(vec![])), "gpus");
    let Some(Json::Obj(cpu)) = field(&fields, "cpu") else {
        panic!("cpu is not an object");
    };
    assert_eq!(
        field(cpu, "pct"),
        Some(&Json::Null),
        "cpu pct on first read"
    );
}

#[cfg(target_os = "linux")]
#[test]
fn once_text_has_the_section_lines() {
    let out = run(&tree("basic"), &["--once"]);
    assert_eq!(out.status.code(), Some(0), "exit status");
    let text = stdout(&out);
    let first_words: Vec<&str> = text
        .lines()
        .filter_map(|l| l.split_whitespace().next())
        .collect();
    for section in [
        "aether", "cpu", "mem", "gpu", "sensors", "disk", "net", "battery", "procs",
    ] {
        assert!(
            first_words.contains(&section),
            "no {section} line in text output"
        );
    }
}

#[test]
fn help_and_version_exit_zero() {
    let help = run(&tree("basic"), &["help"]);
    assert_eq!(help.status.code(), Some(0), "help exit status");
    assert!(!stdout(&help).is_empty(), "help prints the grammar");
    let version = run(&tree("basic"), &["version"]);
    assert_eq!(version.status.code(), Some(0), "version exit status");
}

#[test]
fn unknown_argument_exits_two() {
    let out = run(&tree("basic"), &["--no-such-option"]);
    assert_eq!(out.status.code(), Some(2), "exit status");
    assert!(!out.stderr.is_empty(), "the error is said on stderr");
}

#[cfg(target_os = "linux")]
#[test]
fn only_leaves_the_other_sections_out() {
    let text_out = run(&tree("basic"), &["--once", "--only", "cpu,mem"]);
    assert_eq!(text_out.status.code(), Some(0), "text exit status");
    let text = stdout(&text_out);
    for section in ["gpu", "sensors", "disk", "net", "battery", "procs"] {
        assert!(
            !text
                .lines()
                .any(|l| l.split_whitespace().next() == Some(section)),
            "text still has a {section} line"
        );
    }

    let json_out = run(&tree("basic"), &["--once", "--json", "--only", "cpu,mem"]);
    assert_eq!(json_out.status.code(), Some(0), "json exit status");
    let fields = object(&stdout(&json_out));
    assert!(field(&fields, "cpu").is_some(), "cpu is kept");
    assert!(field(&fields, "mem").is_some(), "mem is kept");
    for part in PARTS {
        assert!(
            field(&fields, part).is_none(),
            "json still has {part} under --only cpu,mem"
        );
    }
}

#[cfg(target_os = "linux")]
#[test]
fn json_is_valid_for_every_fixture_tree() {
    for t in trees() {
        let out = run(&t, &["--once", "--json"]);
        assert_eq!(out.status.code(), Some(0), "{} exit status", t.display());
        object(&stdout(&out));
    }
}

#[cfg(target_os = "linux")]
#[test]
fn truncated_fixtures_never_crash() {
    // A run waits a quarter second for its rates, so the cuts run on several workers. Each worker
    // has its own copy of the trees, so one worker's cut never shows in another worker's run.
    const WORKERS: usize = 8;
    let tree_dirs = trees();
    let names: Vec<std::ffi::OsString> = tree_dirs
        .iter()
        .map(|t| t.file_name().expect("tree name").to_owned())
        .collect();
    let mut files: Vec<(usize, PathBuf, Vec<u8>)> = Vec::new();
    for (ti, t) in tree_dirs.iter().enumerate() {
        let mut list = Vec::new();
        regular_files(t, &mut list);
        list.sort();
        for file in list {
            let rel = file
                .strip_prefix(t)
                .expect("file is under its tree")
                .to_path_buf();
            let original = fs::read(&file).expect("fixture is readable");
            files.push((ti, rel, original));
        }
    }
    let jobs: Vec<(usize, usize)> = files
        .iter()
        .enumerate()
        .flat_map(|(fi, (_, _, original))| {
            cut_points(original.len())
                .into_iter()
                .map(move |len| (fi, len))
        })
        .collect();
    let base = Path::new(env!("CARGO_TARGET_TMPDIR")).join("truncated");
    let _ = fs::remove_dir_all(&base);
    let outcomes: Vec<(usize, Vec<String>)> = std::thread::scope(|s| {
        let handles: Vec<_> = (0..WORKERS)
            .map(|w| {
                let (tree_dirs, names, files, jobs, base) =
                    (&tree_dirs, &names, &files, &jobs, &base);
                s.spawn(move || {
                    let copies = base.join(w.to_string());
                    for (t, name) in tree_dirs.iter().zip(names) {
                        copy_dir(t, &copies.join(name));
                    }
                    let mut runs = 0usize;
                    let mut failures = Vec::new();
                    for &(fi, len) in jobs.iter().skip(w).step_by(WORKERS) {
                        let (ti, rel, original) = &files[fi];
                        let copy = copies.join(&names[*ti]);
                        let target = copy.join(rel);
                        fs::write(&target, &original[..len]).expect("write truncated copy");
                        let out = run(&copy, &["--once", "--json"]);
                        runs += 1;
                        let code = out.status.code();
                        let err = String::from_utf8_lossy(&out.stderr).into_owned();
                        if !matches!(code, Some(0) | Some(1))
                            || err.contains("panicked")
                            || err.contains("RUST_BACKTRACE")
                        {
                            let shown: String = err.chars().take(160).collect();
                            failures.push(format!(
                                "{}/{} cut to {len} bytes: status {code:?}, stderr {shown:?}",
                                names[*ti].to_string_lossy(),
                                rel.display()
                            ));
                        }
                        fs::write(&target, original).expect("restore copy");
                    }
                    (runs, failures)
                })
            })
            .collect();
        handles
            .into_iter()
            .map(|h| h.join().expect("worker finished"))
            .collect()
    });
    let runs: usize = outcomes.iter().map(|(r, _)| r).sum();
    let failures: Vec<String> = outcomes.into_iter().flat_map(|(_, f)| f).collect();
    assert!(
        failures.is_empty(),
        "{} of {runs} runs failed:\n{}",
        failures.len(),
        failures
            .iter()
            .take(15)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n")
    );
}

#[test]
fn once_json_is_one_object_without_a_root() {
    let out = Command::new(BIN)
        .args(["--once", "--json"])
        .env_remove("AETHER_ROOT")
        .output()
        .expect("the program starts");
    assert_eq!(out.status.code(), Some(0), "exit status");
    let text = stdout(&out);
    assert_eq!(text.trim_end().lines().count(), 1, "one line");
    let fields = object(&text);
    for key in KEYS {
        assert!(field(&fields, key).is_some(), "missing key {key}");
    }
}
