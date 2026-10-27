//! AeternaVault: backups of folders as plain, browsable folders, optionally
//! encrypted.
//!
//! Two programs are built from this library:
//! * `aeternavault` — the window (feature `gui`), see [`gui`];
//! * `aeternavault-cli` — the command line, see [`cli`].
//!
//! Both share the configuration, the backups and the activity history.

pub mod automatic;
pub mod cli;
pub mod config;
pub mod engine;
pub mod error;
#[cfg(feature = "gui")]
pub mod gui;
pub mod history;
pub mod i18n;
pub mod logging;
pub mod paths;
pub mod platform;
pub mod state;
pub mod update;
