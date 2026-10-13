//! One JSON object per reading, every process included. A missing value is `null`.

use aether::Reading;

/// Appends one reading as one JSON object and a newline to `out`.
#[allow(dead_code)] // until its package lands
pub fn write(_reading: &Reading, _out: &mut Vec<u8>) {}
