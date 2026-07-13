//! [`Env`] implementation backed by the process environment.

use crate::ports::Env;

/// Reads real process environment variables.
pub struct ProcessEnv;

impl Env for ProcessEnv {
    fn var(&self, key: &str) -> Option<String> {
        std::env::var(key).ok()
    }
}
