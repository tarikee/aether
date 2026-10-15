//! Reads back what `json.rs` writes: one reading per line. Strict: the syntax is exactly what
//! the writer produces (no spaces, no true or false, no exponents), a value of the wrong kind
//! is an error, and the fields the summary does not use are read and dropped. A part left out
//! by `--only` is simply missing from the reading.

use aether::sys::{
    CpuSample, DiskSample, GpuSample, MemSample, NetSample, Proc, Sensor, SensorKind,
};
use aether::Reading;

/// How deep a value may nest; the writer's deepest is 4.
const DEPTH: usize = 8;

#[derive(Debug, PartialEq)]
enum V {
    Null,
    Num(f64),
    Str(String),
    Arr(Vec<V>),
    Obj(Vec<(String, V)>),
}

struct Cursor<'a> {
    b: &'a [u8],
    i: usize,
}

impl Cursor<'_> {
    fn peek(&self) -> Option<u8> {
        self.b.get(self.i).copied()
    }

    fn eat(&mut self, c: u8) -> Result<(), String> {
        if self.peek() == Some(c) {
            self.i += 1;
            Ok(())
        } else {
            Err(format!("expected '{}' at column {}", c as char, self.i + 1))
        }
    }

    fn word(&mut self, w: &str) -> Result<(), String> {
        if self.b[self.i..].starts_with(w.as_bytes()) {
            self.i += w.len();
            Ok(())
        } else {
            Err(format!("unexpected text at column {}", self.i + 1))
        }
    }

    fn value(&mut self, depth: usize) -> Result<V, String> {
        if depth > DEPTH {
            return Err("nested too deep".to_string());
        }
        match self.peek() {
            Some(b'{') => {
                self.i += 1;
                let mut fields = Vec::new();
                if self.peek() == Some(b'}') {
                    self.i += 1;
                    return Ok(V::Obj(fields));
                }
                loop {
                    let k = self.string()?;
                    self.eat(b':')?;
                    fields.push((k, self.value(depth + 1)?));
                    match self.peek() {
                        Some(b',') => self.i += 1,
                        _ => break,
                    }
                }
                self.eat(b'}')?;
                Ok(V::Obj(fields))
            }
            Some(b'[') => {
                self.i += 1;
                let mut items = Vec::new();
                if self.peek() == Some(b']') {
                    self.i += 1;
                    return Ok(V::Arr(items));
                }
                loop {
                    items.push(self.value(depth + 1)?);
                    match self.peek() {
                        Some(b',') => self.i += 1,
                        _ => break,
                    }
                }
                self.eat(b']')?;
                Ok(V::Arr(items))
            }
            Some(b'"') => Ok(V::Str(self.string()?)),
            Some(b'n') => {
                self.word("null")?;
                Ok(V::Null)
            }
            Some(b'-' | b'0'..=b'9') => {
                let start = self.i;
                while matches!(self.peek(), Some(b'-' | b'.' | b'0'..=b'9')) {
                    self.i += 1;
                }
                let text =
                    std::str::from_utf8(&self.b[start..self.i]).map_err(|e| e.to_string())?;
                text.parse::<f64>()
                    .ok()
                    .filter(|n| n.is_finite())
                    .map(V::Num)
                    .ok_or_else(|| format!("bad number at column {}", start + 1))
            }
            _ => Err(format!("unexpected text at column {}", self.i + 1)),
        }
    }

    fn string(&mut self) -> Result<String, String> {
        self.eat(b'"')?;
        let mut out: Vec<u8> = Vec::new();
        loop {
            let c = self.peek().ok_or("a string is not closed")?;
            self.i += 1;
            match c {
                b'"' => break,
                b'\\' => {
                    let e = self.peek().ok_or("a string is not closed")?;
                    self.i += 1;
                    match e {
                        b'"' => out.push(b'"'),
                        b'\\' => out.push(b'\\'),
                        b'n' => out.push(b'\n'),
                        b'r' => out.push(b'\r'),
                        b't' => out.push(b'\t'),
                        b'u' => {
                            let hex = self
                                .b
                                .get(self.i..self.i + 4)
                                .and_then(|h| std::str::from_utf8(h).ok())
                                .and_then(|h| u32::from_str_radix(h, 16).ok())
                                .and_then(char::from_u32)
                                .ok_or("a bad \\u escape")?;
                            self.i += 4;
                            let mut buf = [0u8; 4];
                            out.extend_from_slice(hex.encode_utf8(&mut buf).as_bytes());
                        }
                        _ => return Err("a bad escape in a string".to_string()),
                    }
                }
                c if c < 0x20 => return Err("a control character in a string".to_string()),
                c => out.push(c),
            }
        }
        String::from_utf8(out).map_err(|_| "a string is not UTF-8".to_string())
    }
}

/// Parses one line into a value; nothing may follow it.
fn parse(line: &str) -> Result<V, String> {
    let mut c = Cursor {
        b: line.as_bytes(),
        i: 0,
    };
    let v = c.value(0)?;
    if c.i != line.len() {
        return Err(format!("unexpected text at column {}", c.i + 1));
    }
    Ok(v)
}

type Fields<'a> = &'a [(String, V)];

fn obj(v: &V) -> Result<Fields<'_>, String> {
    match v {
        V::Obj(f) => Ok(f),
        _ => Err("expected an object".to_string()),
    }
}

fn list<'a>(o: Fields<'a>, k: &str) -> Result<&'a [V], String> {
    match field(o, k)? {
        V::Arr(a) => Ok(a),
        _ => Err(format!("{k} is not a list")),
    }
}

fn field<'a>(o: Fields<'a>, k: &str) -> Result<&'a V, String> {
    o.iter()
        .find(|(n, _)| n == k)
        .map(|(_, v)| v)
        .ok_or_else(|| format!("{k} is missing"))
}

/// A number that may be null.
fn num(o: Fields<'_>, k: &str) -> Result<Option<f64>, String> {
    match field(o, k)? {
        V::Null => Ok(None),
        V::Num(n) => Ok(Some(*n)),
        _ => Err(format!("{k} is not a number")),
    }
}

fn need(o: Fields<'_>, k: &str) -> Result<f64, String> {
    num(o, k)?.ok_or_else(|| format!("{k} is null"))
}

fn whole(o: Fields<'_>, k: &str) -> Result<Option<u64>, String> {
    match num(o, k)? {
        Some(n) if n >= 0.0 && n.fract() == 0.0 => Ok(Some(n as u64)),
        Some(_) => Err(format!("{k} is not a whole number")),
        None => Ok(None),
    }
}

fn need_whole(o: Fields<'_>, k: &str) -> Result<u64, String> {
    whole(o, k)?.ok_or_else(|| format!("{k} is null"))
}

/// A whole number a slim recording leaves out: 0 when the key is missing.
fn whole_or_0(o: Fields<'_>, k: &str) -> Result<u64, String> {
    if o.iter().any(|(n, _)| n == k) {
        need_whole(o, k)
    } else {
        Ok(0)
    }
}

fn text(o: Fields<'_>, k: &str) -> Result<String, String> {
    match field(o, k)? {
        V::Str(s) => Ok(s.clone()),
        _ => Err(format!("{k} is not text")),
    }
}

/// Like `whole` and `f32_of`, for keys a slim recording leaves out.
fn opt_whole(o: Fields<'_>, k: &str) -> Result<Option<u64>, String> {
    if o.iter().any(|(n, _)| n == k) {
        whole(o, k)
    } else {
        Ok(None)
    }
}

fn opt_f32(o: Fields<'_>, k: &str) -> Result<Option<f32>, String> {
    if o.iter().any(|(n, _)| n == k) {
        f32_of(o, k)
    } else {
        Ok(None)
    }
}

fn f32_of(o: Fields<'_>, k: &str) -> Result<Option<f32>, String> {
    Ok(num(o, k)?.map(|n| n as f32))
}

/// Reads one line of `aether --json` into a reading. Parts the summary does not use stay empty.
pub fn reading(line: &str) -> Result<Reading, String> {
    let v = parse(line)?;
    let top = obj(&v)?;
    let mut r = Reading {
        t: need(top, "t")?,
        ..Reading::default()
    };
    if let Ok(c) = field(top, "cpu") {
        let c = obj(c)?;
        r.cpu = CpuSample {
            pct: f32_of(c, "pct")?,
            temp_c: f32_of(c, "temp_c")?,
            ..CpuSample::default()
        };
    }
    if let Ok(m) = field(top, "mem") {
        let m = obj(m)?;
        r.mem = MemSample {
            total: need_whole(m, "total")?,
            used: need_whole(m, "used")?,
            swap_total: need_whole(m, "swap_total")?,
            swap_used: need_whole(m, "swap_used")?,
            ..MemSample::default()
        };
    }
    if top.iter().any(|(k, _)| k == "gpus") {
        for g in list(top, "gpus")? {
            let g = obj(g)?;
            r.gpus.push(GpuSample {
                name: text(g, "name")?,
                vendor: "",
                pct: f32_of(g, "pct")?,
                temp_c: f32_of(g, "temp_c")?,
                power_w: None,
                vram_used: whole(g, "vram_used")?,
                vram_total: whole(g, "vram_total")?,
                fan_rpm: None,
            });
        }
    }
    if top.iter().any(|(k, _)| k == "sensors") {
        for s in list(top, "sensors")? {
            let s = obj(s)?;
            let kind = match text(s, "kind")?.as_str() {
                "temp" => SensorKind::Temp,
                "fan" => SensorKind::Fan,
                "power" => SensorKind::Power,
                other => return Err(format!("unknown sensor kind {other}")),
            };
            r.sensors.push(Sensor {
                chip: text(s, "chip")?,
                label: text(s, "label")?,
                kind,
                value: need(s, "value")? as f32,
            });
        }
    }
    if top.iter().any(|(k, _)| k == "disks") {
        for d in list(top, "disks")? {
            let d = obj(d)?;
            r.disks.push(DiskSample {
                mount: text(d, "mount")?,
                dev: text(d, "dev")?,
                total: need_whole(d, "total")?,
                used: need_whole(d, "used")?,
                read_bps: whole(d, "read_bps")?,
                write_bps: whole(d, "write_bps")?,
                busy_pct: f32_of(d, "busy_pct")?,
            });
        }
    }
    if top.iter().any(|(k, _)| k == "nets") {
        for n in list(top, "nets")? {
            let n = obj(n)?;
            r.nets.push(NetSample {
                name: text(n, "name")?,
                rx_bps: whole(n, "rx_bps")?,
                tx_bps: whole(n, "tx_bps")?,
                link_bps: whole(n, "link_bps")?,
                addrs: Vec::new(),
            });
        }
    }
    if top.iter().any(|(k, _)| k == "procs") {
        for p in list(top, "procs")? {
            let p = obj(p)?;
            r.procs.push(Proc {
                pid: need_whole(p, "pid")? as u32,
                ppid: whole_or_0(p, "ppid")? as u32,
                uid: whole_or_0(p, "uid")? as u32,
                threads: whole_or_0(p, "threads")? as u32,
                name: text(p, "name")?,
                state: match text(p, "state") {
                    Ok(t) => t.bytes().next().unwrap_or(b'?'),
                    Err(_) if !p.iter().any(|(n, _)| n == "state") => b'?',
                    Err(m) => return Err(m),
                },
                cpu_pct: f32_of(p, "cpu_pct")?,
                gpu_pct: opt_f32(p, "gpu_pct")?,
                mem: need_whole(p, "mem")?,
                rss: whole_or_0(p, "rss")?,
                started_s: whole_or_0(p, "started_s")?,
                vram: opt_whole(p, "vram")?,
                read_bps: opt_whole(p, "read_bps")?,
                write_bps: opt_whole(p, "write_bps")?,
            });
        }
    }
    Ok(r)
}

/// The path of every value in one line of JSON, for the test that keeps `fields` in step: an
/// object's value is `a.b`, a list's items are `a[]`; a list of objects gives the paths of
/// their values (`a[].b`), and a list or object with nothing in it gives none.
#[cfg(test)]
pub(crate) fn key_paths(line: &str) -> Result<std::collections::BTreeSet<String>, String> {
    fn walk(v: &V, path: &str, out: &mut std::collections::BTreeSet<String>) {
        match v {
            V::Obj(fields) => {
                for (k, v) in fields {
                    let next = if path.is_empty() {
                        k.clone()
                    } else {
                        format!("{path}.{k}")
                    };
                    walk(v, &next, out);
                }
            }
            V::Arr(items) => {
                for v in items {
                    walk(v, &format!("{path}[]"), out);
                }
            }
            _ => {
                out.insert(path.to_string());
            }
        }
    }
    let mut out = std::collections::BTreeSet::new();
    walk(&parse(line)?, "", &mut out);
    Ok(out)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn strings_numbers_and_nesting_are_read() {
        let v = parse(r#"{"a":[1,-2.5,null],"b":"x\"\\\n\u0001é"}"#).unwrap();
        let o = obj(&v).unwrap();
        assert_eq!(
            field(o, "a").unwrap(),
            &V::Arr(vec![V::Num(1.0), V::Num(-2.5), V::Null])
        );
        assert_eq!(text(o, "b").unwrap(), "x\"\\\n\u{1}é");
    }

    #[test]
    fn anything_else_is_refused() {
        for bad in [
            "",
            "{",
            r#"{"a":1,}"#,
            r#"{"a": 1}"#,
            r#"{"a":true}"#,
            r#"{"a":1e3}"#,
            r#"{"a":"\x"}"#,
            r#"{"a":1} x"#,
            "[[[[[[[[[[1]]]]]]]]]]",
        ] {
            assert!(parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn a_reading_needs_its_time_and_right_kinds() {
        assert!(reading("{}").is_err());
        assert!(reading(r#"{"t":"1"}"#).is_err());
        let r = reading(r#"{"t":2.5,"cpu":{"pct":null,"temp_c":50.5}}"#).unwrap();
        assert_eq!(r.t, 2.5);
        assert_eq!(r.cpu.pct, None);
        assert_eq!(r.cpu.temp_c, Some(50.5));
        assert!(reading(r#"{"t":1,"mem":{"total":1}}"#).is_err());
    }
}
