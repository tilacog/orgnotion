//! Concrete implementations of the [`crate::ports`] traits, touching the
//! real world. These are constructed in `main` and injected into the
//! business logic; nothing else in the crate depends on them.

mod clock;
mod env;
mod fs;
mod notion_client;
mod reporter;

pub use clock::SystemClock;
pub use env::ProcessEnv;
pub use fs::RealFileSystem;
pub use notion_client::NotionrsApi;
pub use reporter::StdoutReporter;
