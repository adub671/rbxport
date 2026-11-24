//! What makes a fork build a fork build.
//!
//! A build compiled with `RBXPORT_FORK` set installs beside stock rbxport
//! under its own name and identifier (`tauri.fork.conf.json`) and never
//! updates itself: the release feed describes upstream's builds, and
//! installing one would replace the fork with stock. Upstream changes arrive
//! by merging and rebuilding.

/// Whether this binary was compiled as a fork build.
pub const BUILD: bool = option_env!("RBXPORT_FORK").is_some();
