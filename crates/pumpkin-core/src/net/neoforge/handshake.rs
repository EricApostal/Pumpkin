//! The decisions of the handshake that do not depend on a connection.
//!
//! That is whether a client that
//! answered the query may join and with which channels. The connection driven part is in
//! `net::java::neoforge_session`.

use std::collections::BTreeMap;

use super::{
    channels::REGISTRY_SYNC_REQUIRED,
    declaration::Aggregate,
    negotiation::negotiate_all,
    payloads::{ModdedNetworkQuery, ModdedNetworkSetup, encode_setup_failed},
};

/// How long to wait for the client's answer to the query and its pong.
pub const PRE_BRAND_TIMEOUT_MS: u64 = 3000;
/// How long to wait for the query answer after the pong arrived.
///
/// A client handles packets in
/// order, so the answer of a `NeoForge` client is there before its pong. A vanilla client answers
/// the ping only, this is how soon it is recognised.
pub const PRE_BRAND_GRACE_MS: u64 = 1000;
/// How long the client has to apply the registries and acknowledge them.
pub const REGISTRY_SYNC_TIMEOUT_MS: u64 = 20_000;
/// How long each later step (`c:` handshake, data maps, feature flags) waits for its reply.
pub const TASK_TIMEOUT_MS: u64 = 10_000;

const NO_REGISTRY_SYNC: &str = "Your NeoForge cannot synchronise registries ({detail}). \
Install the same mod(s) as the server: {mods}.";
const NEGOTIATION_FAILED: &str = "Incompatible mods: the network channels of your mods do not \
match the server. Server mod(s): {mods}.";
pub const REGISTRY_SYNC_TIMEOUT: &str = "Your game did not accept the server registries in time. \
Install the same mod(s) as the server: {mods}.";
pub const TASK_TIMEOUT: &str = "The NeoForge handshake step \"{detail}\" timed out.";
pub const BAD_REPLY: &str = "The NeoForge handshake failed: {detail}.";
pub const UNSUPPORTED_COMMON_VERSION: &str =
    "Unsupported common network version. This installation of NeoForge only supports: 1";

/// What happens to a client that answered the query.
#[derive(Debug, PartialEq, Eq)]
pub enum Decision {
    /// Disconnect with this message.
    Reject(String),
    /// Tell the client why (`neoforge:modded_network_setup_failed`), then disconnect.
    SetupFailed { payload: Vec<u8>, message: String },
    /// The client joins with these negotiated channels.
    Accept(ModdedNetworkSetup),
}

/// Decides about a client that sent `query` and announced `announced` (its `minecraft:register`).
#[must_use]
pub fn decide(aggregate: &Aggregate, query: &ModdedNetworkQuery, announced: &[String]) -> Decision {
    if !aggregate.registries.is_empty() {
        let missing: Vec<&str> = REGISTRY_SYNC_REQUIRED
            .iter()
            .copied()
            .filter(|channel| !announced.iter().any(|a| a == channel))
            .collect();
        if !missing.is_empty() {
            return Decision::Reject(
                aggregate.text(NO_REGISTRY_SYNC, &format!("missing {}", missing.join(", "))),
            );
        }
    }
    let mod_name = |namespace: &str| aggregate.mod_name(namespace);
    match negotiate_all(&aggregate.channels, query, &mod_name) {
        Ok(setup) => Decision::Accept(setup),
        Err(reasons) => match encode_setup_failed(&reasons) {
            Ok(payload) => Decision::SetupFailed {
                payload,
                message: aggregate.text(NEGOTIATION_FAILED, ""),
            },
            Err(error) => Decision::Reject(aggregate.text(BAD_REPLY, &error.to_string())),
        },
    }
}

/// The reasons as the map the payload carries, for logs.
#[must_use]
pub fn describe_failures(failures: &BTreeMap<String, super::nbt_text::NbtText>) -> String {
    failures.keys().cloned().collect::<Vec<_>>().join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::neoforge::{
        channels::{self, own_registrations},
        declaration::{ChannelDecl, Declaration, ModInfo, RegistryDecl},
        payloads::{Flow, Phase, QueryComponent},
        registries::SyncedRegistry,
    };

    fn aggregate() -> Aggregate {
        let declaration = Declaration {
            mod_info: ModInfo {
                id: "demo".into(),
                version: "1.2".into(),
                display_name: "Demo".into(),
            },
            channels: vec![ChannelDecl {
                id: "demo:net".into(),
                version: "3".into(),
                phases: vec![Phase::Play],
                flow: Some(Flow::Clientbound),
                optional: false,
            }],
            registries: vec![RegistryDecl {
                key: "minecraft:item".into(),
                entries: vec!["demo:gem".into()],
            }],
            config_files: Vec::new(),
            datapack_entries: Vec::new(),
            data_maps: Vec::new(),
            feature_flags: Vec::new(),
            allow_non_neoforge: false,
            reject_message: None,
        };
        Aggregate::build(
            &[declaration],
            vec![SyncedRegistry {
                key: "minecraft:item".into(),
                entries: vec!["minecraft:air".into()],
            }],
        )
    }

    fn client_query(version: &str) -> ModdedNetworkQuery {
        let mut components = BTreeMap::new();
        for phase in [Phase::Configuration, Phase::Play] {
            components.insert(phase, own_registrations(phase));
        }
        components
            .get_mut(&Phase::Play)
            .unwrap()
            .push(QueryComponent {
                id: "demo:net".into(),
                version: version.into(),
                flow: Some(Flow::Clientbound),
                optional: false,
            });
        ModdedNetworkQuery { components }
    }

    fn announced() -> Vec<String> {
        channels::REGISTRY_SYNC_REQUIRED
            .iter()
            .map(ToString::to_string)
            .collect()
    }

    #[test]
    fn a_matching_client_is_accepted_with_the_negotiated_channels() {
        let Decision::Accept(setup) = decide(&aggregate(), &client_query("3"), &announced()) else {
            panic!("expected acceptance");
        };
        assert_eq!(setup.channels[&Phase::Play]["demo:net"], "3");
        assert!(
            setup
                .configuration_channels()
                .contains(&channels::FROZEN_REGISTRY.to_string())
        );
    }

    #[test]
    fn a_client_that_cannot_sync_registries_is_rejected_with_the_mod_list() {
        let Decision::Reject(message) = decide(&aggregate(), &client_query("3"), &[]) else {
            panic!("expected a rejection");
        };
        assert!(message.contains("Demo 1.2"), "{message}");
        assert!(
            message.contains("neoforge:frozen_registry_sync_start"),
            "{message}"
        );
    }

    #[test]
    fn a_version_mismatch_sends_the_setup_failed_payload() {
        let Decision::SetupFailed { payload, message } =
            decide(&aggregate(), &client_query("2"), &announced())
        else {
            panic!("expected a failed setup");
        };
        assert!(!payload.is_empty());
        assert!(message.contains("Demo 1.2"));
        // One entry keyed by the channel.
        assert_eq!(payload[0], 1);
        assert!(payload.windows(8).any(|w| w == b"demo:net"));
    }
}
