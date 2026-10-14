//! Account names by uid, from the directory service.

use super::calls;
use std::collections::HashMap;

/// The account names by uid, read once.
pub fn names() -> HashMap<u32, String> {
    calls::user_names()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn root_has_a_name() {
        assert_eq!(names().get(&0).map(String::as_str), Some("root"));
    }
}
