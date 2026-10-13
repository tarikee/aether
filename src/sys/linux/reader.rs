//! Reading small kernel files without reopening them: a file is opened once and read in place
//! from offset 0 on every tick, into a buffer that is allocated once.

use std::fs::File;
use std::io::ErrorKind;
use std::os::unix::fs::FileExt;
use std::path::Path;

/// A file kept open between readings.
pub struct PFile {
    file: File,
    buf: Vec<u8>,
}

impl PFile {
    /// Opens the file, with room for `cap` bytes. `None` if it cannot be opened.
    pub fn open(path: &Path, cap: usize) -> Option<PFile> {
        let file = File::open(path).ok()?;
        Some(PFile {
            file,
            buf: vec![0; cap],
        })
    }

    /// The file's current contents, up to the buffer's size. A failed read is empty.
    pub fn read(&mut self) -> &[u8] {
        read_into(&self.file, &mut self.buf)
    }
}

/// Reads an open file in place from offset 0 into `buf` and returns what it held (at most the
/// size of `buf`). A failed read is empty. A short read ends the file: the kernel's files give
/// their whole text in one read, so no second call is made just to find the end.
pub fn read_into<'a>(file: &File, buf: &'a mut [u8]) -> &'a [u8] {
    let mut filled = 0;
    while filled < buf.len() {
        let want = buf.len() - filled;
        match file.read_at(&mut buf[filled..], filled as u64) {
            Ok(0) => break,
            Ok(n) => {
                filled += n;
                if n < want {
                    break;
                }
            }
            Err(e) if e.kind() == ErrorKind::Interrupted => continue,
            Err(_) => {
                filled = 0;
                break;
            }
        }
    }
    &buf[..filled]
}

/// The non-empty lines of a buffer.
pub fn lines(buf: &[u8]) -> impl Iterator<Item = &[u8]> {
    buf.split(|&b| b == b'\n').filter(|l| !l.is_empty())
}

/// The whitespace-separated fields of a line.
pub fn fields(line: &[u8]) -> impl Iterator<Item = &[u8]> {
    line.split(|b| b.is_ascii_whitespace())
        .filter(|f| !f.is_empty())
}

/// A plain decimal number, or `None` if the field is not one.
pub fn num(field: &[u8]) -> Option<u64> {
    if field.is_empty() {
        return None;
    }
    let mut n: u64 = 0;
    for &b in field {
        if !b.is_ascii_digit() {
            return None;
        }
        n = n.checked_mul(10)?.checked_add(u64::from(b - b'0'))?;
    }
    Some(n)
}

/// The first number after `key:` on its line, as in `MemTotal:  16384 kB` (gives 16384).
pub fn value_of(buf: &[u8], key: &str) -> Option<u64> {
    for line in lines(buf) {
        if let Some(rest) = line.strip_prefix(key.as_bytes()) {
            if let Some(after) = rest.strip_prefix(b":") {
                return fields(after).next().and_then(num);
            }
        }
    }
    None
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn numbers_and_fields() {
        assert_eq!(num(b"12345"), Some(12345));
        assert_eq!(num(b""), None);
        assert_eq!(num(b"12a"), None);
        let names: Vec<&[u8]> = fields(b"  cpu  1 2 ").collect();
        assert_eq!(names, vec![&b"cpu"[..], &b"1"[..], &b"2"[..]]);
        assert_eq!(lines(b"a\n\nb\n").count(), 2);
    }

    #[test]
    fn value_of_reads_the_key_only() {
        let buf = b"MemTotal:       16384 kB\nMemTotalX: 5\nMemAvailable:  8 kB\n";
        assert_eq!(value_of(buf, "MemTotal"), Some(16384));
        assert_eq!(value_of(buf, "MemAvailable"), Some(8));
        assert_eq!(value_of(buf, "Cached"), None);
    }

    #[test]
    fn a_kept_open_file_reads_again_from_the_start() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml");
        let mut f = PFile::open(&path, 4096).expect("Cargo.toml opens");
        let first = f.read().to_vec();
        let second = f.read().to_vec();
        assert!(!first.is_empty());
        assert_eq!(first, second);
    }
}
