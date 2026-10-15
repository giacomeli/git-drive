//! P2P notification channel between the devices of a drive (phase 1).
//!
//! Devices tell each other "I pushed commit X"; the receiver wakes its pull
//! watcher, which syncs through git exactly as without the channel. Git stays
//! the source of truth: a lost, duplicated or late notice only delays a
//! change until the next poll. With `p2p_enabled` off (or a build without the
//! `p2p` feature) none of this runs.

#[cfg(feature = "p2p")]
pub mod channel;
pub mod identity;
#[cfg(feature = "p2p")]
pub mod node;
pub mod protocol;
pub mod registry;
