//! The NeoForge client handshake, owned by the host.
//!
//! Plugins only declare what they add (see `neoforge.wit`): their mod, payload channels, extra
//! synced registries, config files and the policy for clients without NeoForge. The host merges
//! the declarations of all plugins into one [`Aggregate`](declaration::Aggregate) and runs the
//! handshake for every connection, in the order a NeoForge server uses
//! (`docs/neoforge-protocol.md` of the plugin API):
//!
//! 1. before the server brand: `minecraft:unregister`, `minecraft:register`, the empty
//!    `neoforge:register` query and a ping. A client that answers is negotiated
//!    (`neoforge:network`) and marked as NeoForge, the others get the declared policy,
//! 2. after the brand: the registry synchronisation (`neoforge:frozen_registry*`) and its
//!    acknowledgement,
//! 3. after the registry data and tags: `c:version` / `c:register`, the config files, the data map
//!    and feature flag checks.
//!
//! This module has the encoders and the decisions, which do not touch a connection. The part that
//! is driven by the packets of a connection is `net::java::neoforge_session`.

pub mod buf;
pub mod channels;
pub mod declaration;
pub mod extra_registries;
pub mod handshake;
pub mod ident;
pub mod nbt_text;
pub mod negotiation;
pub mod payloads;
pub mod play_sync;
pub mod recipe_sync;
pub mod registries;
