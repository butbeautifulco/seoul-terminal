//! Library surface for `seoul-daemon`.
//!
//! Most of the daemon's logic lives in modules consumed only by the binary
//! (`main.rs`). This `lib.rs` exists so integration tests (and, in time,
//! out-of-process tools) can reach into pure helpers like the singleton
//! lock without dragging the whole binary into the test build graph.
//!
//! Keep the public surface here intentionally narrow. If a module would
//! pull in tokio/runtime state (e.g., `host`, `server`, `session`), prefer
//! testing it via the binary integration tests rather than re-exporting.

pub mod lock;
