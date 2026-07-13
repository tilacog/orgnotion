//! [`Reporter`] implementation that prints to stdout.

use crate::ports::Reporter;

/// Prints progress lines to stdout.
pub struct StdoutReporter;

impl Reporter for StdoutReporter {
    fn info(&mut self, message: &str) {
        println!("{message}");
    }
}
