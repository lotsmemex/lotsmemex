//! Product-owned local capture coordinator.
//!
//! Dependency policy: `reqwest` is rustls/blocking-only and all implicit proxy,
//! redirect and decompression features are disabled; `rusqlite` is bundled for a
//! predictable SQLite with WAL; `flate2` and `brotli` are used only behind the
//! fetcher's decoded-size and expansion-ratio limits.

pub mod bibliographic;
pub mod capture_progress;
pub mod config;
pub mod coordinator;
mod dedup;
pub mod fetch;
mod groups;
pub mod insights;
pub mod ipc;
pub mod item_chat;
pub mod mailbox;
mod markdown_blocks;
mod model_test;
pub mod models;
mod provider_generation;
pub mod queue;
pub mod render;
pub mod seams;
pub mod source;
pub mod translation_request;
pub mod url_input;

pub use config::{
    daemon_install, daemon_status, daemon_uninstall, render_launchd_plist,
    render_surface_launchd_plist, surface_install, surface_status, surface_uninstall,
    validate_private_lan, AgentConfig,
};
pub use groups::GroupEditWireOp;
pub use ipc::{run_daemon, run_daemon_with_sources, send_command, Command, Producer, Response};
pub use provider_generation::{repair_saved_translation_tables, PurposeRoutes, TranslationProviderCapability};
pub use url_input::{extract_url, UrlSummary};
