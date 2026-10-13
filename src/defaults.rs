//! The file of default arguments: `aether/args` in the config folder. Its arguments come before
//! the command line's own, so the command line wins. Read for the live view and `--once` only.

use std::ffi::OsString;
use std::path::PathBuf;

/// Where the file is: `$XDG_CONFIG_HOME/aether/args`, else `$HOME/.config/aether/args`; on
/// Windows `%APPDATA%\aether\args`. `None` if the folder is not known.
pub fn path() -> Option<PathBuf> {
    path_from(
        std::env::var_os("XDG_CONFIG_HOME"),
        std::env::var_os("HOME"),
        std::env::var_os("APPDATA"),
    )
}

fn non_empty(value: Option<OsString>) -> Option<PathBuf> {
    value.filter(|v| !v.is_empty()).map(PathBuf::from)
}

fn path_from(
    xdg: Option<OsString>,
    home: Option<OsString>,
    appdata: Option<OsString>,
) -> Option<PathBuf> {
    let base = if cfg!(windows) {
        non_empty(appdata)
    } else {
        non_empty(xdg).or_else(|| non_empty(home).map(|h| h.join(".config")))
    };
    base.map(|b| b.join("aether").join("args"))
}

/// The arguments in the file, split on whitespace. A missing or unreadable file gives none.
pub fn arguments() -> Vec<String> {
    let Some(file) = path() else {
        return Vec::new();
    };
    match std::fs::read(file) {
        Ok(bytes) => split(&String::from_utf8_lossy(&bytes)),
        Err(_) => Vec::new(),
    }
}

/// The words of the file's text; a line that starts with `#` is a comment.
pub fn split(text: &str) -> Vec<String> {
    text.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .flat_map(str::split_whitespace)
        .map(str::to_string)
        .collect()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn the_folder_is_xdg_then_home_then_nothing() {
        let xdg = Some(OsString::from("/x"));
        let home = Some(OsString::from("/h"));
        if !cfg!(windows) {
            assert_eq!(
                path_from(xdg.clone(), home.clone(), None),
                Some(PathBuf::from("/x/aether/args"))
            );
            assert_eq!(
                path_from(Some(OsString::new()), home, None),
                Some(PathBuf::from("/h/.config/aether/args"))
            );
            assert_eq!(path_from(None, None, None), None);
        }
    }

    #[test]
    fn comments_and_blank_lines_are_skipped() {
        let text = "# the usual view\n\n  --by   mem  \n--once\n#--json\n";
        assert_eq!(split(text), vec!["--by", "mem", "--once"]);
    }

    #[test]
    fn an_empty_text_gives_no_arguments() {
        assert!(split("").is_empty());
        assert!(split("# only a comment").is_empty());
    }
}
