//! A port of NeoForge's `NetworkComponentNegotiator`: given the payload channels each side
//! registered, decide which are used, and why a mismatch is one.

use std::collections::{BTreeMap, HashMap, HashSet};

use super::{
    ident::namespace_of,
    nbt_text::NbtText,
    payloads::{Flow, ModdedNetworkQuery, ModdedNetworkSetup, Phase, QueryComponent},
};

const KEY_PREFIX: &str = "neoforge.network.negotiation.failure";

/// The outcome for one protocol.
#[derive(Debug, PartialEq, Eq)]
pub enum Negotiation {
    /// The channels both sides use: id to version.
    Success(BTreeMap<String, String>),
    /// Per channel id, why it failed (the components the client's mismatch screen shows).
    Failure(BTreeMap<String, NbtText>),
}

/// Maps a channel namespace to the display name of the mod that owns it.
pub type ModNames<'a> = &'a dyn Fn(&str) -> Option<String>;

fn for_mod(id: &str, reason: NbtText, mod_name: ModNames<'_>) -> NbtText {
    match mod_name(namespace_of(id)).filter(|name| !name.is_empty()) {
        Some(name) => NbtText::translatable(
            format!("{KEY_PREFIX}.mod"),
            vec![NbtText::literal(name), reason],
        ),
        None => reason,
    }
}

const fn flow_name(flow: Flow) -> &'static str {
    flow.name()
}

/// `validateComponent(left, right, requestingSide)`, none means compatible.
fn validate(left: &QueryComponent, right: &QueryComponent, requesting_side: &str) -> Option<NbtText> {
    if let Some(left_flow) = left.flow {
        match right.flow {
            None => {
                return Some(NbtText::translatable(
                    format!("{KEY_PREFIX}.flow.{requesting_side}.missing"),
                    vec![NbtText::literal(flow_name(left_flow))],
                ));
            }
            Some(right_flow) if right_flow != left_flow => {
                return Some(NbtText::translatable(
                    format!("{KEY_PREFIX}.flow.{requesting_side}.mismatch"),
                    vec![
                        NbtText::literal(flow_name(left_flow)),
                        NbtText::literal(flow_name(right_flow)),
                    ],
                ));
            }
            Some(_) => {}
        }
    }
    if left.version != right.version {
        let (client_version, server_version) = if requesting_side == "client" {
            (&right.version, &left.version)
        } else {
            (&left.version, &right.version)
        };
        return Some(NbtText::translatable(
            format!("{KEY_PREFIX}.version.mismatch"),
            vec![
                NbtText::literal(client_version.clone()),
                NbtText::literal(server_version.clone()),
            ],
        ));
    }
    None
}

/// Negotiates one protocol like `NetworkComponentNegotiator.negotiate`.
///
/// An optional channel the other side does not have is dropped, a channel only one side has that
/// is not optional fails, and a channel both have must agree on the flow and the version.
#[must_use]
pub fn negotiate(
    server: &[QueryComponent],
    client: &[QueryComponent],
    mod_name: ModNames<'_>,
) -> Negotiation {
    let server_ids: HashSet<&str> = server.iter().map(|c| c.id.as_str()).collect();
    let client_ids: HashSet<&str> = client.iter().map(|c| c.id.as_str()).collect();
    let server_list: Vec<&QueryComponent> = server
        .iter()
        .filter(|c| !(c.optional && !client_ids.contains(c.id.as_str())))
        .collect();
    let client_list: Vec<&QueryComponent> = client
        .iter()
        .filter(|c| !(c.optional && !server_ids.contains(c.id.as_str())))
        .collect();

    let server_by_id: HashMap<&str, &QueryComponent> =
        server_list.iter().map(|c| (c.id.as_str(), *c)).collect();
    let client_by_id: HashMap<&str, &QueryComponent> =
        client_list.iter().map(|c| (c.id.as_str(), *c)).collect();

    // Channels only the client has (and needs), then only the server has.
    let only_client: Vec<&str> = client_list
        .iter()
        .map(|c| c.id.as_str())
        .filter(|id| !server_by_id.contains_key(id))
        .collect();
    if !only_client.is_empty() {
        return Negotiation::Failure(
            only_client
                .into_iter()
                .map(|id| {
                    let reason = NbtText::translatable(
                        format!("{KEY_PREFIX}.missing.client.server"),
                        Vec::new(),
                    );
                    (id.to_string(), for_mod(id, reason, mod_name))
                })
                .collect(),
        );
    }
    let only_server: Vec<&str> = server_list
        .iter()
        .map(|c| c.id.as_str())
        .filter(|id| !client_by_id.contains_key(id))
        .collect();
    if !only_server.is_empty() {
        return Negotiation::Failure(
            only_server
                .into_iter()
                .map(|id| {
                    let reason = NbtText::translatable(
                        format!("{KEY_PREFIX}.missing.server.client"),
                        Vec::new(),
                    );
                    (id.to_string(), for_mod(id, reason, mod_name))
                })
                .collect(),
        );
    }

    let mut channels = BTreeMap::new();
    let mut failures = BTreeMap::new();
    for component in &server_list {
        let id = component.id.as_str();
        let Some(other) = client_by_id.get(id) else {
            continue;
        };
        let reason = validate(component, other, "client").or_else(|| validate(other, component, "server"));
        match reason {
            Some(reason) => {
                failures.insert(id.to_string(), for_mod(id, reason, mod_name));
            }
            None => {
                channels.insert(id.to_string(), component.version.clone());
            }
        }
    }
    if failures.is_empty() {
        Negotiation::Success(channels)
    } else {
        Negotiation::Failure(failures)
    }
}

/// Negotiates the configuration and play protocols (the ones NeoForge has registrations for) and
/// builds the `neoforge:network` setup, or the failure reasons of all channels.
pub fn negotiate_all(
    server: &BTreeMap<Phase, Vec<QueryComponent>>,
    client: &ModdedNetworkQuery,
    mod_name: ModNames<'_>,
) -> Result<ModdedNetworkSetup, BTreeMap<String, NbtText>> {
    let mut channels = BTreeMap::new();
    let mut failures = BTreeMap::new();
    for phase in [Phase::Configuration, Phase::Play] {
        let empty = Vec::new();
        let outcome = negotiate(
            server.get(&phase).unwrap_or(&empty),
            client.components.get(&phase).unwrap_or(&empty),
            mod_name,
        );
        match outcome {
            Negotiation::Success(map) => {
                channels.insert(phase, map);
            }
            Negotiation::Failure(reasons) => failures.extend(reasons),
        }
    }
    if failures.is_empty() {
        Ok(ModdedNetworkSetup { channels })
    } else {
        Err(failures)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::neoforge::channels::own_registrations;

    fn component(id: &str, version: &str, flow: Option<Flow>, optional: bool) -> QueryComponent {
        QueryComponent {
            id: id.into(),
            version: version.into(),
            flow,
            optional,
        }
    }

    fn no_names(_: &str) -> Option<String> {
        None
    }

    #[test]
    fn optional_channels_the_other_side_lacks_are_dropped() {
        let server = [component("a:x", "1", None, true), component("a:y", "1", None, false)];
        let client = [component("a:y", "1", None, false)];
        assert_eq!(
            negotiate(&server, &client, &no_names),
            Negotiation::Success(BTreeMap::from([("a:y".into(), "1".into())]))
        );
    }

    #[test]
    fn a_required_channel_the_client_lacks_fails_with_the_mod_name() {
        let server = [component("a:x", "1", None, false)];
        let names = |namespace: &str| (namespace == "a").then(|| "Mod A".to_string());
        let Negotiation::Failure(reasons) = negotiate(&server, &[], &names) else {
            panic!("expected a failure");
        };
        assert_eq!(
            reasons["a:x"],
            NbtText::translatable(
                "neoforge.network.negotiation.failure.mod",
                vec![
                    NbtText::literal("Mod A"),
                    NbtText::translatable(
                        "neoforge.network.negotiation.failure.missing.server.client",
                        vec![]
                    )
                ]
            )
        );
        let client = [component("a:z", "1", None, false)];
        let Negotiation::Failure(reasons) = negotiate(&[], &client, &no_names) else {
            panic!("expected a failure");
        };
        assert_eq!(
            reasons["a:z"],
            NbtText::translatable(
                "neoforge.network.negotiation.failure.missing.client.server",
                vec![]
            )
        );
    }

    #[test]
    fn version_and_flow_mismatches_fail() {
        let server = [component("a:x", "2", Some(Flow::Clientbound), false)];
        let client = [component("a:x", "1", Some(Flow::Clientbound), false)];
        let Negotiation::Failure(reasons) = negotiate(&server, &client, &no_names) else {
            panic!("expected a failure");
        };
        assert_eq!(
            reasons["a:x"],
            NbtText::translatable(
                "neoforge.network.negotiation.failure.version.mismatch",
                vec![NbtText::literal("1"), NbtText::literal("2")]
            )
        );

        let client = [component("a:x", "2", Some(Flow::Serverbound), false)];
        let Negotiation::Failure(reasons) = negotiate(&server, &client, &no_names) else {
            panic!("expected a failure");
        };
        assert_eq!(
            reasons["a:x"],
            NbtText::translatable(
                "neoforge.network.negotiation.failure.flow.client.mismatch",
                vec![NbtText::literal("CLIENTBOUND"), NbtText::literal("SERVERBOUND")]
            )
        );
    }

    #[test]
    fn a_plain_neoforge_client_negotiates_with_a_server_that_has_only_its_own_channels() {
        let server = BTreeMap::from([
            (Phase::Configuration, own_registrations(Phase::Configuration)),
            (Phase::Play, own_registrations(Phase::Play)),
        ]);
        let client = ModdedNetworkQuery {
            components: server.clone(),
        };
        let setup = negotiate_all(&server, &client, &no_names).unwrap();
        assert_eq!(setup.channels[&Phase::Configuration].len(), 11);
        assert_eq!(setup.channels[&Phase::Play].len(), 9);
    }

    #[test]
    fn a_modded_client_channel_the_server_lacks_fails_negotiate_all() {
        let server = BTreeMap::from([(Phase::Configuration, Vec::new())]);
        let client = ModdedNetworkQuery {
            components: BTreeMap::from([(
                Phase::Play,
                vec![component("m:c", "1", None, false)],
            )]),
        };
        let failures = negotiate_all(&server, &client, &no_names).unwrap_err();
        assert!(failures.contains_key("m:c"));
    }
}
