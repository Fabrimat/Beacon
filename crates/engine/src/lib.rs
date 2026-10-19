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
//!
//! Phase-4 contract: `symbols` (symbol table + rename), `api_index` (versioned API index),
//! `debug` (minecraft-debugger interop), `plugin` (git plugin pins, `plugin update`).
//!
//! Phase-5 contract: `setup` (wizard report, Minecraft detection), `gametest` (`beacon test`).
//!
//! Phase-2 seams: `statics::items` after routing, `registry::derive` before merge, and one
//! writer per artifact kind in `artifact/`.

// contract
pub mod api_index;
pub(crate) mod apicheck;
pub mod config;
pub mod debug;
pub mod diag;
pub mod event;
pub mod gametest;
pub mod host;
pub mod migrate;
pub mod model;
pub mod path;
pub mod plugin;
pub mod settings;
pub mod setup;
pub mod suffix;
pub mod symbols;

// owned by workstreams
pub mod agents;
pub mod archive;
mod artifact;
pub mod build;
mod cache;
pub mod canonical;
pub mod emit;
pub mod init;
pub mod legacy_filter;
pub mod lock;
pub mod manifest;
pub mod merge;
pub mod nbt;
mod packlist;
pub mod registry;
pub mod release;
pub mod route;
pub mod sarif;
pub mod scaffold;
pub mod statics;
pub mod template;
pub mod trust;
pub mod vfs;
pub mod world;
