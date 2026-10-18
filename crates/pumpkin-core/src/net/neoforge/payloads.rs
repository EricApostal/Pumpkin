//! Encoders and decoders of the `NeoForge` 26.3 negotiation payloads.
//!
//! The layouts are documented in `docs/neoforge-protocol.md` of the plugin API (with the `NeoForge`
//! source each comes from). Trailing bytes in a payload the client knows are a decode error on
//! the client, so every encoder here is exact.

use std::collections::BTreeMap;

use super::{
    buf::{CodecError, MAX_ELEMENTS, Reader, Writer},
    channels::COMMON_NETWORK_VERSION,
    ident::normalize,
    nbt_text::NbtText,
};

/// Minecraft's `ConnectionProtocol`, as far as payloads use it. The ordinal is the wire value.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Phase {
    Handshake,
    Play,
    Status,
    Login,
    Configuration,
}

impl Phase {
    #[must_use]
    pub const fn ordinal(self) -> i32 {
        match self {
            Self::Handshake => 0,
            Self::Play => 1,
            Self::Status => 2,
            Self::Login => 3,
            Self::Configuration => 4,
        }
    }

    #[must_use]
    pub const fn from_ordinal(ordinal: i32) -> Option<Self> {
        Some(match ordinal {
            0 => Self::Handshake,
            1 => Self::Play,
            2 => Self::Status,
            3 => Self::Login,
            4 => Self::Configuration,
            _ => return None,
        })
    }

    /// `ConnectionProtocol.id()`, what `c:register` uses.
    #[must_use]
    pub const fn id(self) -> &'static str {
        match self {
            Self::Handshake => "handshake",
            Self::Play => "play",
            Self::Status => "status",
            Self::Login => "login",
            Self::Configuration => "configuration",
        }
    }

    #[must_use]
    pub fn from_id(id: &str) -> Option<Self> {
        [
            Self::Handshake,
            Self::Play,
            Self::Status,
            Self::Login,
            Self::Configuration,
        ]
        .into_iter()
        .find(|phase| phase.id() == id)
    }
}

/// Minecraft's `PacketFlow`. The ordinal is the wire value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Flow {
    Serverbound,
    Clientbound,
}

impl Flow {
    #[must_use]
    pub const fn ordinal(self) -> i32 {
        match self {
            Self::Serverbound => 0,
            Self::Clientbound => 1,
        }
    }

    #[must_use]
    pub const fn from_ordinal(ordinal: i32) -> Option<Self> {
        match ordinal {
            0 => Some(Self::Serverbound),
            1 => Some(Self::Clientbound),
            _ => None,
        }
    }

    /// `PacketFlow.toString()`, the constant name.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Serverbound => "SERVERBOUND",
            Self::Clientbound => "CLIENTBOUND",
        }
    }
}

// ---------------------------------------------------------------------------
// minecraft:register / minecraft:unregister
// ---------------------------------------------------------------------------

/// The payload of `minecraft:register` and `minecraft:unregister`: every channel followed by a
/// zero byte, no length prefix. Duplicates are written once.
#[must_use]
pub fn encode_register_channels<S: AsRef<str>>(channels: &[S]) -> Vec<u8> {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for channel in channels {
        let channel = channel.as_ref();
        if !seen.insert(channel.to_string()) {
            continue;
        }
        out.extend_from_slice(channel.as_bytes());
        out.push(0);
    }
    out
}

/// Decodes like `NeoForge`: names are split at zero bytes and a name that is not a valid
/// identifier is ignored.
#[must_use]
pub fn decode_register_channels(data: &[u8]) -> Vec<String> {
    let mut result: Vec<String> = Vec::new();
    for name in data.split(|&byte| byte == 0) {
        if name.is_empty() {
            continue;
        }
        let Ok(text) = std::str::from_utf8(name) else {
            continue;
        };
        if let Ok(id) = normalize(text)
            && !result.contains(&id)
        {
            result.push(id);
        }
    }
    result
}

// ---------------------------------------------------------------------------
// neoforge:register (ModdedNetworkQueryPayload)
// ---------------------------------------------------------------------------

/// What one side registered for a payload channel (`ModdedNetworkQueryComponent`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QueryComponent {
    pub id: String,
    pub version: String,
    /// The direction the payload flows in, none for both.
    pub flow: Option<Flow>,
    /// Whether the other side may lack the channel.
    pub optional: bool,
}

/// The query: sent by the server with an empty map, answered by the client with everything it
/// registered, per protocol.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ModdedNetworkQuery {
    pub components: BTreeMap<Phase, Vec<QueryComponent>>,
}

impl ModdedNetworkQuery {
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        let mut w = Writer::new();
        w.var_len(self.components.len());
        for (phase, list) in &self.components {
            w.var_int(phase.ordinal());
            w.var_len(list.len());
            for component in list {
                w.string(&component.id);
                w.string(&component.version);
                match component.flow {
                    Some(flow) => {
                        w.bool(true);
                        w.var_int(flow.ordinal());
                    }
                    None => w.bool(false),
                }
                w.bool(component.optional);
            }
        }
        w.into_bytes()
    }

    pub fn decode(data: &[u8]) -> Result<Self, CodecError> {
        let mut r = Reader::new(data);
        let mut components = BTreeMap::new();
        for _ in 0..r.var_len(5)? {
            let ordinal = r.var_int()?;
            let phase = Phase::from_ordinal(ordinal)
                .ok_or_else(|| CodecError::new(format!("unknown connection protocol {ordinal}")))?;
            let mut list = Vec::new();
            for _ in 0..r.var_len(MAX_ELEMENTS)? {
                let id = normalize(&r.string()?)?;
                let version = r.string()?;
                let flow =
                    if r.bool()? {
                        let ordinal = r.var_int()?;
                        Some(Flow::from_ordinal(ordinal).ok_or_else(|| {
                            CodecError::new(format!("unknown packet flow {ordinal}"))
                        })?)
                    } else {
                        None
                    };
                let optional = r.bool()?;
                list.push(QueryComponent {
                    id,
                    version,
                    flow,
                    optional,
                });
            }
            // Maps overwrite duplicate keys.
            components.insert(phase, list);
        }
        r.finish()?;
        Ok(Self { components })
    }
}

// ---------------------------------------------------------------------------
// neoforge:network (ModdedNetworkPayload)
// ---------------------------------------------------------------------------

/// The channels the server negotiated, per protocol: id to chosen version.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ModdedNetworkSetup {
    pub channels: BTreeMap<Phase, BTreeMap<String, String>>,
}

impl ModdedNetworkSetup {
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        let mut w = Writer::new();
        w.var_len(self.channels.len());
        for (phase, map) in &self.channels {
            w.var_int(phase.ordinal());
            w.var_len(map.len());
            for (id, version) in map {
                w.string(id);
                // NetworkChannel(id, chosenVersion): the id is repeated inside the value.
                w.string(id);
                w.string(version);
            }
        }
        w.into_bytes()
    }

    /// The negotiated configuration channels.
    #[must_use]
    pub fn configuration_channels(&self) -> Vec<String> {
        self.channels
            .get(&Phase::Configuration)
            .map(|map| map.keys().cloned().collect())
            .unwrap_or_default()
    }
}

/// `neoforge:modded_network_setup_failed`: why the negotiation failed, per channel.
pub fn encode_setup_failed(reasons: &BTreeMap<String, NbtText>) -> Result<Vec<u8>, CodecError> {
    let mut w = Writer::new();
    w.var_len(reasons.len());
    for (id, reason) in reasons {
        w.string(id);
        reason.write(&mut w)?;
    }
    Ok(w.into_bytes())
}

// ---------------------------------------------------------------------------
// c:version and c:register
// ---------------------------------------------------------------------------

/// `c:version` with the supported versions: `[1]`.
#[must_use]
pub fn encode_common_version() -> Vec<u8> {
    let mut w = Writer::new();
    w.var_len(1);
    w.var_int(COMMON_NETWORK_VERSION);
    w.into_bytes()
}

/// Decodes `c:version`, the versions a side supports.
pub fn decode_common_version(data: &[u8]) -> Result<Vec<i32>, CodecError> {
    let mut r = Reader::new(data);
    let count = r.var_len(MAX_ELEMENTS)?;
    let versions = (0..count)
        .map(|_| r.var_int())
        .collect::<Result<Vec<_>, _>>()?;
    r.finish()?;
    Ok(versions)
}

/// `c:register`: the channels a side can receive in `phase`.
#[must_use]
pub fn encode_common_register(phase: Phase, channels: &[String]) -> Vec<u8> {
    let mut w = Writer::new();
    w.var_int(COMMON_NETWORK_VERSION);
    w.string(phase.id());
    w.var_len(channels.len());
    for channel in channels {
        w.string(channel);
    }
    w.into_bytes()
}

/// Decodes `c:register` into the phase and the channels.
pub fn decode_common_register(data: &[u8]) -> Result<(Phase, Vec<String>), CodecError> {
    let mut r = Reader::new(data);
    let _version = r.var_int()?;
    let id = r.string()?;
    let phase = Phase::from_id(&id)
        .ok_or_else(|| CodecError::new(format!("unknown connection protocol '{id}'")))?;
    let count = r.var_len(MAX_ELEMENTS)?;
    let channels = (0..count)
        .map(|_| r.string().and_then(|text| normalize(&text)))
        .collect::<Result<Vec<_>, _>>()?;
    r.finish()?;
    Ok((phase, channels))
}

// ---------------------------------------------------------------------------
// Registry synchronisation
// ---------------------------------------------------------------------------

/// `neoforge:frozen_registry_sync_start`: the registries that follow.
#[must_use]
pub fn encode_sync_start<S: AsRef<str>>(registries: &[S]) -> Vec<u8> {
    let mut w = Writer::new();
    w.var_len(registries.len());
    for registry in registries {
        w.string(registry.as_ref());
    }
    w.into_bytes()
}

/// `neoforge:frozen_registry`: the registry name, then the snapshot.
///
/// The id of an entry is its
/// index in `entries`, the ids are written in increasing order (the client applies them in that
/// order), and no aliases are sent.
#[must_use]
pub fn encode_frozen_registry<S: AsRef<str>>(registry: &str, entries: &[S]) -> Vec<u8> {
    let mut w = Writer::new();
    w.string(registry);
    w.var_len(entries.len());
    for (id, entry) in entries.iter().enumerate() {
        w.var_len(id);
        w.string(entry.as_ref());
    }
    // Aliases.
    w.var_len(0);
    w.into_bytes()
}

// ---------------------------------------------------------------------------
// Data maps, feature flags, config files
// ---------------------------------------------------------------------------

/// `neoforge:known_registry_data_maps`: registry to its data maps and whether each is mandatory.
#[must_use]
pub fn encode_known_data_maps(maps: &BTreeMap<String, Vec<(String, bool)>>) -> Vec<u8> {
    let mut w = Writer::new();
    w.var_len(maps.len());
    for (registry, list) in maps {
        w.string(registry);
        w.var_len(list.len());
        for (id, mandatory) in list {
            w.string(id);
            w.bool(*mandatory);
        }
    }
    w.into_bytes()
}

/// Decodes `neoforge:known_registry_data_maps_reply`: registry to the data maps the client has.
pub fn decode_known_data_maps_reply(
    data: &[u8],
) -> Result<BTreeMap<String, Vec<String>>, CodecError> {
    let mut r = Reader::new(data);
    let mut result = BTreeMap::new();
    for _ in 0..r.var_len(MAX_ELEMENTS)? {
        let registry = normalize(&r.string()?)?;
        let mut list = Vec::new();
        for _ in 0..r.var_len(MAX_ELEMENTS)? {
            list.push(normalize(&r.string()?)?);
        }
        result.insert(registry, list);
    }
    r.finish()?;
    Ok(result)
}

/// `neoforge:feature_flags`: the modded feature flags.
#[must_use]
pub fn encode_feature_flags<S: AsRef<str>>(flags: &[S]) -> Vec<u8> {
    encode_sync_start(flags)
}

/// `neoforge:config_file`: the file name and the contents as a byte array.
#[must_use]
pub fn encode_config_file(name: &str, content: &[u8]) -> Vec<u8> {
    let mut w = Writer::new();
    w.string(name);
    w.byte_array(content);
    w.into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lp(text: &str) -> Vec<u8> {
        let mut out = vec![u8::try_from(text.len()).unwrap()];
        out.extend_from_slice(text.as_bytes());
        out
    }

    #[test]
    fn register_channels_match_the_documented_example() {
        let bytes = encode_register_channels(&["minecraft:register", "c:version", "c:version"]);
        let mut expected = b"minecraft:register".to_vec();
        expected.push(0);
        expected.extend_from_slice(b"c:version");
        expected.push(0);
        assert_eq!(bytes, expected);
        assert_eq!(
            decode_register_channels(&bytes),
            ["minecraft:register", "c:version"]
        );
        // Invalid names are ignored, the empty payload is valid.
        assert_eq!(decode_register_channels(b"BAD\0ok:x\0"), ["ok:x"]);
        assert!(decode_register_channels(&[]).is_empty());
    }

    #[test]
    fn empty_query_is_a_zero_byte() {
        assert_eq!(ModdedNetworkQuery::default().encode(), [0]);
    }

    #[test]
    fn query_matches_the_documented_example() {
        let mut components = BTreeMap::new();
        components.insert(
            Phase::Configuration,
            vec![QueryComponent {
                id: "neoforge:frozen_registry_sync_completed".into(),
                version: "1".into(),
                flow: None,
                optional: true,
            }],
        );
        let query = ModdedNetworkQuery { components };
        let mut expected = vec![0x01, 0x04, 0x01];
        expected.extend(lp("neoforge:frozen_registry_sync_completed"));
        expected.extend([0x01, 0x31, 0x00, 0x01]);
        assert_eq!(query.encode(), expected);
        assert_eq!(ModdedNetworkQuery::decode(&expected).unwrap(), query);
    }

    #[test]
    fn query_with_flow_round_trips_and_rejects_garbage() {
        let mut components = BTreeMap::new();
        components.insert(
            Phase::Play,
            vec![QueryComponent {
                id: "mod:chan".into(),
                version: "2".into(),
                flow: Some(Flow::Clientbound),
                optional: false,
            }],
        );
        let query = ModdedNetworkQuery { components };
        let bytes = query.encode();
        assert_eq!(ModdedNetworkQuery::decode(&bytes).unwrap(), query);
        assert!(ModdedNetworkQuery::decode(&[0x01, 0x09, 0x00]).is_err());
        assert!(ModdedNetworkQuery::decode(&[0x00, 0x00]).is_err());
    }

    #[test]
    fn network_matches_the_documented_example() {
        let mut map = BTreeMap::new();
        map.insert("neoforge:frozen_registry".to_string(), "1".to_string());
        let mut channels = BTreeMap::new();
        channels.insert(Phase::Configuration, map);
        let setup = ModdedNetworkSetup { channels };
        let mut expected = vec![0x01, 0x04, 0x01];
        expected.extend(lp("neoforge:frozen_registry"));
        expected.extend(lp("neoforge:frozen_registry"));
        expected.extend([0x01, 0x31]);
        assert_eq!(setup.encode(), expected);
    }

    #[test]
    fn frozen_registry_matches_the_documented_example() {
        // The documented example has ids 0, 1 and 300; ids are contiguous here, so check the
        // layout with three entries and the multi byte id separately through the writer.
        let bytes = encode_frozen_registry("minecraft:item", &["minecraft:air", "minecraft:stone"]);
        let mut expected = lp("minecraft:item");
        expected.push(2);
        expected.push(0);
        expected.extend(lp("minecraft:air"));
        expected.push(1);
        expected.extend(lp("minecraft:stone"));
        expected.push(0);
        assert_eq!(bytes, expected);

        let entries: Vec<String> = (0..301).map(|i| format!("a:e{i}")).collect();
        let bytes = encode_frozen_registry("a:r", &entries);
        let mut reader = Reader::new(&bytes);
        assert_eq!(reader.string().unwrap(), "a:r");
        assert_eq!(reader.var_len(1000).unwrap(), 301);
        for (i, entry) in entries.iter().enumerate() {
            assert_eq!(reader.var_len(1000).unwrap(), i);
            assert_eq!(&reader.string().unwrap(), entry);
        }
        assert_eq!(reader.var_len(10).unwrap(), 0);
        reader.finish().unwrap();
    }

    #[test]
    fn sync_start_and_config_file() {
        let mut expected = vec![1];
        expected.extend(lp("minecraft:item"));
        assert_eq!(encode_sync_start(&["minecraft:item"]), expected);

        let mut expected = lp("neoforge-server.toml");
        expected.push(0);
        assert_eq!(encode_config_file("neoforge-server.toml", &[]), expected);
        let mut expected = lp("a.toml");
        expected.extend([2, b'h', b'i']);
        assert_eq!(encode_config_file("a.toml", b"hi"), expected);
    }

    #[test]
    fn common_payloads_match_the_documented_examples() {
        assert_eq!(encode_common_version(), [0x01, 0x01]);
        assert_eq!(decode_common_version(&[0x01, 0x01]).unwrap(), [1]);
        assert!(decode_common_version(&[0x01]).is_err());

        let mut expected = vec![0x01];
        expected.extend(lp("play"));
        expected.extend([0x01]);
        expected.extend(lp("c:foo"));
        let bytes = encode_common_register(Phase::Play, &["c:foo".to_string()]);
        assert_eq!(bytes, expected);
        assert_eq!(
            decode_common_register(&bytes).unwrap(),
            (Phase::Play, vec!["c:foo".to_string()])
        );
        let empty = encode_common_register(Phase::Play, &[]);
        assert_eq!(decode_common_register(&empty).unwrap().1.len(), 0);
    }

    #[test]
    fn data_maps_round_trip() {
        let mut maps = BTreeMap::new();
        maps.insert(
            "minecraft:item".to_string(),
            vec![("mod:fuel".to_string(), true)],
        );
        let mut expected = vec![1];
        expected.extend(lp("minecraft:item"));
        expected.push(1);
        expected.extend(lp("mod:fuel"));
        expected.push(1);
        assert_eq!(encode_known_data_maps(&maps), expected);

        let mut reply = vec![1];
        reply.extend(lp("minecraft:item"));
        reply.push(1);
        reply.extend(lp("mod:fuel"));
        let decoded = decode_known_data_maps_reply(&reply).unwrap();
        assert_eq!(decoded["minecraft:item"], ["mod:fuel"]);
    }

    #[test]
    fn setup_failed_carries_text_nbt() {
        let mut reasons = BTreeMap::new();
        reasons.insert("a:b".to_string(), NbtText::literal("x"));
        let bytes = encode_setup_failed(&reasons).unwrap();
        let mut expected = vec![1];
        expected.extend(lp("a:b"));
        expected.extend([8, 0, 1, b'x']);
        assert_eq!(bytes, expected);
    }
}
