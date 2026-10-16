//! Beacon engine (ARCHITECTURE 5).
//!
//! The modules marked *contract* are shared by every phase-1 workstream and change only by
//! agreement; the others are owned by one workstream each.
//!
//! Pipeline (5.1), phase-1 subset:
//!
//! ```text
//! config::load -> vfs::index -> route::route -> (jshost: generators) -> merge::resolve
//!   -> lock + manifest -> emit::emit (dir via ledger) | archive::write (.mcpack/.mcaddon)
//! ```

// contract
pub mod config;
pub mod diag;
pub mod event;
pub mod host;
pub mod model;
pub mod path;
pub mod suffix;

// owned by workstreams
pub mod archive;
pub mod canonical;
pub mod emit;
pub mod lock;
pub mod manifest;
pub mod merge;
pub mod route;
pub mod sarif;
pub mod trust;
pub mod vfs;
