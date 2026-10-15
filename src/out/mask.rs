//! Hides the values of secrets in a command line, so `proc` never shows a password or a token.

/// Words that make a name a secret's when they appear in it (ASCII case ignored).
const WORDS: &[&str] = &[
    "PASSWORD", "PASSWD", "TOKEN", "SECRET", "AUTH", "APIKEY", "API_KEY",
];

/// Names that are a secret as a flag on their own: `--password VALUE` hides the next argument.
const FLAGS: &[&str] = &[
    "PASSWORD", "PASSWD", "TOKEN", "SECRET", "AUTH", "APIKEY", "API_KEY",
];

/// Whether an `NAME=VALUE` name (leading dashes ignored) is a secret's: it holds one of
/// `WORDS`, or it is `KEY`, or it ends with `_KEY` or `-KEY`. So `--monkey` is not one.
fn secret_name(name: &str) -> bool {
    let up = name.trim_start_matches('-').to_ascii_uppercase();
    WORDS.iter().any(|w| up.contains(w))
        || up == "KEY"
        || up.ends_with("_KEY")
        || up.ends_with("-KEY")
}

/// Whether an argument is a flag whose next argument is a secret: `--password`, `-token`,
/// `--api-key` (dashes and case ignored).
fn secret_flag(arg: &str) -> bool {
    if !arg.starts_with('-') {
        return false;
    }
    let name = arg
        .trim_start_matches('-')
        .to_ascii_uppercase()
        .replace('-', "_");
    FLAGS.contains(&name.as_str())
}

/// The command line with the values of secrets replaced by `***`. The arguments are the
/// parts between single spaces, so the spacing is kept; a name stays and only the value goes.
pub fn mask_secrets(cmdline: &str) -> String {
    let mut out: Vec<String> = Vec::new();
    let mut hide_next = false;
    for arg in cmdline.split(' ') {
        if hide_next {
            hide_next = false;
            out.push("***".to_string());
        } else if secret_flag(arg) {
            hide_next = true;
            out.push(arg.to_string());
        } else {
            match arg.split_once('=') {
                Some((name, _)) if secret_name(name) => out.push(format!("{name}=***")),
                _ => out.push(arg.to_string()),
            }
        }
    }
    out.join(" ")
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn secret_values_are_hidden_and_names_kept() {
        assert_eq!(mask_secrets("-AUTH_PASSWORD=abc123"), "-AUTH_PASSWORD=***");
        assert_eq!(mask_secrets("GITHUB_TOKEN=ghp_x"), "GITHUB_TOKEN=***");
        assert_eq!(mask_secrets("--token=xyz"), "--token=***");
        assert_eq!(mask_secrets("--private-key=x"), "--private-key=***");
        assert_eq!(mask_secrets("API_KEY=1 KEY=2"), "API_KEY=*** KEY=***");
    }

    #[test]
    fn the_argument_after_a_secret_flag_is_hidden() {
        assert_eq!(mask_secrets("tool --token xyz"), "tool --token ***");
        assert_eq!(
            mask_secrets("tool -Password pw --x 1"),
            "tool -Password *** --x 1"
        );
        assert_eq!(mask_secrets("tool --api-key abc"), "tool --api-key ***");
        assert_eq!(mask_secrets("tool --token"), "tool --token");
    }

    #[test]
    fn other_arguments_are_unchanged() {
        assert_eq!(mask_secrets("--monkey=1"), "--monkey=1");
        assert_eq!(
            mask_secrets("python  app.py --port=80"),
            "python  app.py --port=80"
        );
        assert_eq!(mask_secrets("/usr/bin/sleep 30"), "/usr/bin/sleep 30");
        assert_eq!(mask_secrets(""), "");
    }
}
