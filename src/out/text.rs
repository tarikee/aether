//! The text view for people: one reading, sorted and grouped as the view asks.

use crate::cli::View;
use aether::Reading;

/// Appends one reading as text to `out`.
#[allow(dead_code)] // until its package lands
pub fn write(_reading: &Reading, _view: &View, _out: &mut Vec<u8>) {}
