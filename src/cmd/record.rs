//! `aether record`: reads like the live view without drawing, until Ctrl-C or the duration, then
//! prints the peaks. `aether peaks FILE` prints the same summary from a recorded file.

use crate::cli::Parsed;
use crate::cmd::live::{catch_stops, finish, stop_requested, wait, Sources};
use crate::out::peaks::{cut, Peaks};
use crate::out::{json, jsonin};
use aether::sys::Root;
use aether::Reading;
use std::fs::File;
use std::io::{self, BufRead, BufReader, BufWriter, Write};
use std::process::ExitCode;
use std::time::{Duration, Instant};

/// The wait between the first reading and the second, so the rates are numbers.
const FIRST_WAIT: Duration = Duration::from_millis(250);

/// Prints the summary, as text or as one JSON object.
fn print(peaks: &Peaks, json: bool) -> io::Result<()> {
    let s = if json { peaks.json() } else { peaks.text() };
    let mut out = io::stdout().lock();
    out.write_all(s.as_bytes())?;
    out.flush()
}

/// `aether record`.
pub fn run(parsed: &Parsed) -> ExitCode {
    let mut file = match &parsed.to {
        Some(path) => match File::create(path) {
            Ok(f) => Some(BufWriter::new(f)),
            Err(e) => {
                eprintln!("aether: cannot write {path}: {e}");
                return ExitCode::FAILURE;
            }
        },
        None => None,
    };
    catch_stops();
    let root = Root::system();
    let started = Instant::now();
    let mut sources = Sources::open(&root);
    let mut r = Reading::default();
    let mut kept = Reading::default();
    let mut line: Vec<u8> = Vec::new();
    let mut peaks = Peaks::default();
    sources.fill(&mut r, started, &parsed.view);
    wait(FIRST_WAIT);
    let every = Duration::from_secs_f64(parsed.interval_s);
    let limit = parsed.duration_s.map(Duration::from_secs_f64);
    let begun = Instant::now();
    let mut failed = false;
    while !stop_requested() {
        let tick = Instant::now();
        sources.fill(&mut r, started, &parsed.view);
        peaks.add(&r);
        if let Some(f) = &mut file {
            kept.clone_from(&r);
            if !parsed.all_procs {
                cut(&mut kept.procs);
            }
            line.clear();
            json::write_record(&kept, &parsed.view, &mut line, parsed.all_procs);
            if f.write_all(&line).and_then(|()| f.flush()).is_err() {
                eprintln!("aether: could not write the file");
                failed = true;
                break;
            }
        }
        let mut pause = every.saturating_sub(tick.elapsed());
        if let Some(l) = limit {
            let left = l.saturating_sub(begun.elapsed());
            if left.is_zero() {
                break;
            }
            pause = pause.min(left);
        }
        wait(pause);
    }
    let shown = finish(print(&peaks, parsed.json));
    if failed {
        ExitCode::FAILURE
    } else {
        shown
    }
}

/// `aether peaks FILE`.
pub fn peaks(parsed: &Parsed) -> ExitCode {
    let Some(path) = parsed.target.as_deref() else {
        return ExitCode::from(2);
    };
    match summarise(path) {
        Ok(p) => finish(print(&p, parsed.json)),
        Err(m) => {
            eprintln!("aether: {m}");
            ExitCode::FAILURE
        }
    }
}

/// Reads the file line by line into the summary.
fn summarise(path: &str) -> Result<Peaks, String> {
    let file = File::open(path).map_err(|e| format!("cannot read {path}: {e}"))?;
    let mut peaks = Peaks::default();
    let mut reader = BufReader::new(file);
    let mut text = String::new();
    let mut n = 0usize;
    loop {
        text.clear();
        let got = reader
            .read_line(&mut text)
            .map_err(|e| format!("{path}: {e}"))?;
        if got == 0 {
            break;
        }
        n += 1;
        // A line without its newline is the last one, and may have been cut off mid-write.
        let whole = text.ends_with('\n');
        let line = text.trim_end_matches(['\n', '\r']);
        if line.trim().is_empty() {
            continue;
        }
        match jsonin::reading(line) {
            Ok(r) => peaks.add(&r),
            Err(_) if !whole => {
                eprintln!("aether: {path}: last line incomplete, skipped");
            }
            Err(m) => return Err(format!("{path}: line {n}: {m}")),
        }
    }
    if peaks.readings == 0 {
        return Err(format!("{path}: no readings"));
    }
    Ok(peaks)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn peaks_of_a_small_file() {
        let dir = std::env::temp_dir().join(format!("aether-peaks-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("rec.jsonl");
        std::fs::write(
            &f,
            "{\"t\":1,\"cpu\":{\"pct\":null,\"temp_c\":40}}\n\
             {\"t\":2,\"cpu\":{\"pct\":50,\"temp_c\":45}}\n\n\
             {\"t\":3,\"cpu\":{\"pct\":10,\"temp_c\":41}}\n",
        )
        .unwrap();
        let p = summarise(f.to_str().unwrap()).unwrap();
        assert_eq!(p.readings, 3);
        assert_eq!((p.cpu.max, p.cpu.at, p.cpu.avg()), (50.0, 2.0, 30.0));
        assert_eq!(p.hottest.max, 45.0);
        assert!(p.text().contains("CPU"));
        std::fs::write(&f, "{\"t\":1}\nnot json\n").unwrap();
        let e = summarise(f.to_str().unwrap()).unwrap_err();
        assert!(e.contains("line 2"), "{e}");
        std::fs::write(&f, "").unwrap();
        assert!(summarise(f.to_str().unwrap()).is_err());
        assert!(summarise(dir.join("none").to_str().unwrap()).is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_cut_last_line_is_skipped_and_other_bad_lines_are_not() {
        let dir = std::env::temp_dir().join(format!("aether-peaks-cut-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("rec.jsonl");
        let path = f.to_str().unwrap();
        std::fs::write(&f, "{\"t\":1}\n{\"t\":2,\"cpu\":{\"pct\":5").unwrap();
        assert_eq!(summarise(path).unwrap().readings, 1);
        std::fs::write(&f, "{\"t\":1}\n{\"t\":2,\"cpu\":{\"pct\":5}\n").unwrap();
        assert!(summarise(path).unwrap_err().contains("line 2"));
        std::fs::write(&f, "{\"t\":1}\nnot json\n{\"t\":3}\n").unwrap();
        assert!(summarise(path).unwrap_err().contains("line 2"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn old_lines_with_every_process_and_new_slim_lines_both_count() {
        let old = "{\"t\":1.0,\"cpu\":{\"pct\":5.123456,\"temp_c\":null},\"procs\":[{\"pid\":4,\"ppid\":1,\"uid\":0,\
                   \"threads\":2,\"name\":\"web\",\"state\":\"S\",\"cpu_pct\":12.5,\"gpu_pct\":null,\
                   \"mem\":900,\"rss\":800,\"started_s\":0,\"vram\":null,\"read_bps\":null,\
                   \"write_bps\":null}]}";
        let new =
            "{\"t\":2,\"cpu\":{\"pct\":7.5,\"temp_c\":null},\"procs\":[{\"pid\":5,\"name\":\"db\",\
                   \"cpu_pct\":30.25,\"mem\":1000}]}";
        let dir = std::env::temp_dir().join(format!("aether-peaks-old-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("rec.jsonl");
        std::fs::write(&f, format!("{old}\n{new}\n")).unwrap();
        let p = summarise(f.to_str().unwrap()).unwrap();
        assert_eq!(p.readings, 2);
        assert_eq!(p.top_cpu.len(), 2);
        assert!(p.top_cpu.iter().any(|e| e.name == "web" && e.value == 12.5));
        assert!(p
            .top_mem
            .iter()
            .any(|e| e.name == "db" && e.value == 1000.0));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
