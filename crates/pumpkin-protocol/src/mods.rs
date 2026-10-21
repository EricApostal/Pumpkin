//! Packet extensions contributed by the mods a connection negotiated.
//!
//! Some mods change the wire format of a vanilla packet for the clients that have them installed:
//! the client that receives such a packet reads the vanilla fields and then one (or more) extra
//! field, so a host that writes only the vanilla fields leaves the decoder running off the end of
//! the frame, which drops the connection. `ConnectionFlavour` cannot express this: it says how the
//! client encodes in general (vanilla vs `NeoForge`), and two `NeoForge` clients differ by the mods
//! they have. What decides is the mod set the connection *negotiated*, which is what this module
//! keeps.
//!
//! # How it is applied
//!
//! * [`ModSet`] is the per-connection answer: the subset of [`MOD_EXTENSIONS`] the client declared.
//!   It is built from the channel ids the client announced (`minecraft:register` /
//!   `minecraft:unregister`, in any phase) and from the payload ids of its `neoforge:register`
//!   answer (`crates/pumpkin-core/src/net/java/neoforge_session.rs`).
//! * The code that serializes a packet for one connection wraps the call in
//!   [`PacketEncoding::scope`](crate::PacketEncoding::scope), so the set reaches the writer through
//!   a thread local, like the flavour.
//! * `crate::java::packet_encoder::write_packet` — the single place every clientbound packet goes
//!   through — appends the fields of every extended packet that matches, after the vanilla payload.
//!   A packet writer therefore carries no mod code at all, and a packet that only a mod extends
//!   needs no change here.
//!
//! # Adding a mod
//!
//! Add one [`ModExtension`] row to [`MOD_EXTENSIONS`]: the mod's id, the channel ids or namespaces
//! that identify it in what the client announced, and one [`ExtendedPacket`] per packet it extends
//! with the fields it appends. Nothing else — no packet writer changes, no new `if` anywhere.
//! Nothing outside this file names a mod.
//!
//! Order matters for two mods that extend the same packet: the fields are applied in the order of
//! [`MOD_EXTENSIONS`], which is the order the reader has to have, so a row is inserted where its
//! client reads it, not appended at the end blindly.
//!
//! # Limits
//!
//! The table describes what a mod appends to a *clientbound* packet, which is what the client's
//! reader demands. A mod that changes a *serverbound* packet would need a direction in the table
//! and the same lookup in a read path; no mod here does (Sable's only serverbound `PacketMixin`
//! changes the values of the move packet, not its format).

use std::io::Write;

use pumpkin_data::packet::PacketId;
use pumpkin_util::version::JavaMinecraftVersion;

use crate::{ser::NetworkWriteExt, WritingError};

/// `minecraft:register`: the channel ids a peer can receive, `\0` separated.
pub const MINECRAFT_REGISTER: &str = "minecraft:register";
/// `minecraft:unregister`: the same list, removed instead of added.
pub const MINECRAFT_UNREGISTER: &str = "minecraft:unregister";

/// One field a mod appends to a packet it extends.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExtensionField {
    /// A boolean: one byte, `1` or `0`.
    Bool(bool),
}

impl ExtensionField {
    fn write(self, write: &mut impl Write) -> Result<(), WritingError> {
        match self {
            Self::Bool(value) => write.write_bool(value),
        }
    }
}

/// One packet a mod extends.
#[derive(Clone, Copy, Debug)]
pub struct ExtendedPacket {
    /// The vanilla packet whose payload the fields follow. The id is the one of the version being
    /// written ([`PacketId::to_id`]).
    pub packet: PacketId,
    /// The fields, in wire order, after the vanilla payload.
    pub fields: &'static [ExtensionField],
}

/// A mod whose client changes the wire format of some of the packets it receives.
#[derive(Clone, Copy, Debug)]
pub struct ModExtension {
    /// The mod id, for diagnostics, errors and the tests. Nothing depends on its value.
    pub id: &'static str,
    /// Channel ids of the mod whose presence in what the client announced means it is installed.
    pub channels: &'static [&'static str],
    /// Channel namespaces of the mod (`sable` matches `sable:anything`), for the same purpose. A
    /// mod registers its payloads under its own namespace, so one entry covers a mod whose channel
    /// list changes with its version.
    pub namespaces: &'static [&'static str],
    /// The packets it extends.
    pub packets: &'static [ExtendedPacket],
}

/// Every mod whose packet extensions this host knows, as one table.
///
/// A row is data only: it names the mod and what it appends, and the mechanism in this module
/// applies it. `pumpkin_data::packet::clientbound` gives the packet ids, so a row never repeats a
/// number.
pub static MOD_EXTENSIONS: &[ModExtension] = &[ModExtension {
    id: "sable",
    channels: &[],
    namespaces: &["sable"],
    // Sable's client appends `sable$actuallyInSubLevel` to the three packets that carry an entity
    // position (`dev/ryanhcode/sable/mixin/entity/entities_stick_sub_levels/packet_mixin/`), and
    // reads it back after the vanilla fields.
    //
    // The field says whether the entity is inside one of Sable's *sub-levels*, which only a Sable
    // server has: it is set from the sub-level an entity is attached to. Pumpkin has no sub-levels
    // and cannot have an entity inside one, so `false` is the truthful value for every entity of
    // this host — not a placeholder for a value that is missing, and not a value a later change
    // may decide differently. (A client that reads `true` places the entity in a sub-level's space
    // and moves it with that sub-level's pose; `false` is what the client expects of an entity
    // networked aside one.)
    packets: &[
        ExtendedPacket {
            packet: pumpkin_data::packet::clientbound::play::MOVE_ENTITY_POS,
            fields: &[ExtensionField::Bool(false)],
        },
        ExtendedPacket {
            packet: pumpkin_data::packet::clientbound::play::MOVE_ENTITY_POS_ROT,
            fields: &[ExtensionField::Bool(false)],
        },
        ExtendedPacket {
            packet: pumpkin_data::packet::clientbound::play::TELEPORT_ENTITY,
            fields: &[ExtensionField::Bool(false)],
        },
    ],
}];

/// The mods of one connection, as a bit set over the indices of [`MOD_EXTENSIONS`].
///
/// [`Self::NONE`] is the vanilla answer and the default: no extension is applied.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct ModSet(u64);

/// The table has one bit per mod; a row past it would silently never be applied.
const _: () = assert!(MOD_EXTENSIONS.len() <= 64);

thread_local! {
    /// The mods of the packet being written on this thread.
    static ACTIVE: std::cell::Cell<ModSet> = const { std::cell::Cell::new(ModSet::NONE) };
}

/// Puts the previous set back, also when the scope unwinds.
struct Restore(ModSet);

impl Drop for Restore {
    fn drop(&mut self) {
        ACTIVE.set(self.0);
    }
}

/// Whether `channel` names `extension` — one of its ids, or any channel of one of its namespaces.
fn names(extension: &ModExtension, channel: &str) -> bool {
    extension.channels.contains(&channel)
        || extension.namespaces.iter().any(|namespace| {
            channel
                .strip_prefix(namespace)
                .is_some_and(|rest| rest.starts_with(':'))
        })
}

impl ModSet {
    /// No mod: the vanilla bytes.
    pub const NONE: Self = Self(0);

    /// The mods identified by `channels` (channel ids, e.g. from `minecraft:register`).
    #[must_use]
    pub fn from_channels<'a>(channels: impl IntoIterator<Item = &'a str>) -> Self {
        let mut set = Self::NONE;
        set.add_channels(channels);
        set
    }

    /// Adds every mod identified by `channels`.
    pub fn add_channels<'a>(&mut self, channels: impl IntoIterator<Item = &'a str>) {
        for channel in channels {
            for (index, extension) in MOD_EXTENSIONS.iter().enumerate() {
                if names(extension, channel) {
                    self.0 |= 1 << index;
                }
            }
        }
    }

    /// Removes every mod identified by `channels` (`minecraft:unregister`).
    pub fn remove_channels<'a>(&mut self, channels: impl IntoIterator<Item = &'a str>) {
        for channel in channels {
            for (index, extension) in MOD_EXTENSIONS.iter().enumerate() {
                if names(extension, channel) {
                    self.0 &= !(1 << index);
                }
            }
        }
    }

    /// Applies a client's channel list payload to the set.
    ///
    /// `true` when `channel` was one of [`MINECRAFT_REGISTER`]/[`MINECRAFT_UNREGISTER`] (and was
    /// consumed). A payload that is not valid UTF-8, or an empty entry, teaches nothing.
    pub fn apply_channel_list(&mut self, channel: &str, data: &[u8]) -> bool {
        let unregister = channel == MINECRAFT_UNREGISTER;
        if !unregister && channel != MINECRAFT_REGISTER {
            return false;
        }
        if let Ok(list) = std::str::from_utf8(data) {
            let channels = list.split('\0').filter(|entry| !entry.is_empty());
            if unregister {
                self.remove_channels(channels);
            } else {
                self.add_channels(channels);
            }
        }
        true
    }

    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// Whether the mod at `index` of [`MOD_EXTENSIONS`] is part of the set.
    #[must_use]
    pub const fn has(self, index: usize) -> bool {
        index < 64 && self.0 & (1 << index) != 0
    }

    /// The mods packets are being written for on this thread, [`NONE`](Self::NONE) outside of a
    /// [`scope`](Self::scope).
    #[must_use]
    pub fn current() -> Self {
        ACTIVE.get()
    }

    /// Runs `packets` with `self` as the active mod set.
    pub fn scope<R>(self, packets: impl FnOnce() -> R) -> R {
        let _restore = Restore(ACTIVE.replace(self));
        packets()
    }

    /// Appends the fields of every extended packet of the active mods that matches `packet_id`.
    ///
    /// The id is the one of `version` (the writer knows the packet it is writing, the table knows
    /// the ids of every version).
    pub fn write_extensions(
        self,
        packet_id: i32,
        version: &JavaMinecraftVersion,
        write: &mut impl Write,
    ) -> Result<(), WritingError> {
        if self.is_empty() {
            return Ok(());
        }
        for (index, extension) in MOD_EXTENSIONS.iter().enumerate() {
            if !self.has(index) {
                continue;
            }
            for packet in extension.packets {
                if packet.packet.to_id(*version) != packet_id {
                    continue;
                }
                for field in packet.fields {
                    field.write(write)?;
                }
            }
        }
        Ok(())
    }
}

/// [`ModSet::write_extensions`] for the mods active on this thread.
pub fn write_extensions(
    packet_id: i32,
    version: &JavaMinecraftVersion,
    write: &mut impl Write,
) -> Result<(), WritingError> {
    ModSet::current().write_extensions(packet_id, version, write)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The namespace of the only row in the table; the tests below use it to build a set.
    const SABLE: &str = "sable:snapshot_packet";

    #[test]
    fn defaults_to_no_mods_outside_a_scope() {
        assert_eq!(ModSet::current(), ModSet::NONE);
    }

    #[test]
    fn scope_sets_and_restores_the_mods() {
        let sable = ModSet::from_channels([SABLE]);
        let seen = sable.scope(|| {
            let outer = ModSet::current();
            let inner = ModSet::NONE.scope(ModSet::current);
            (outer, inner, ModSet::current())
        });
        assert_eq!(seen, (sable, ModSet::NONE, sable));
        assert_eq!(ModSet::current(), ModSet::NONE);
    }

    #[test]
    fn scope_restores_after_a_panic() {
        let result = std::panic::catch_unwind(|| {
            ModSet::from_channels([SABLE]).scope(|| panic!("writer failed"));
        });
        assert!(result.is_err());
        assert_eq!(ModSet::current(), ModSet::NONE);
    }

    #[test]
    fn a_channel_of_a_mods_namespace_registers_it() {
        assert!(!ModSet::from_channels([SABLE]).is_empty());
        // Any channel of the namespace, not just the one in the table.
        assert!(!ModSet::from_channels(["sable:udp_activation"]).is_empty());
        // Another namespace, and a name that only starts like one.
        assert!(ModSet::from_channels(["neoforge:register"]).is_empty());
        assert!(ModSet::from_channels(["sables:thing"]).is_empty());
        assert!(ModSet::from_channels(["minecraft:brand"]).is_empty());
    }

    #[test]
    fn a_register_payload_adds_and_an_unregister_removes() {
        let mut set = ModSet::NONE;
        let register = b"sable:punch_sub_level\0minecraft:brand\0";
        assert!(set.apply_channel_list(MINECRAFT_REGISTER, register));
        assert_eq!(set, ModSet::from_channels([SABLE]));
        assert!(!set.apply_channel_list("minecraft:brand", register));
        assert!(set.apply_channel_list(MINECRAFT_UNREGISTER, b"sable:punch_sub_level\0"));
        assert!(set.is_empty());
    }

    #[test]
    fn only_the_packets_in_the_table_are_extended() {
        let set = ModSet::from_channels([SABLE]);
        let version = JavaMinecraftVersion::V_26_3;
        let mut out = Vec::new();
        set.write_extensions(
            pumpkin_data::packet::clientbound::play::MOVE_ENTITY_ROT.to_id(version),
            &version,
            &mut out,
        )
        .unwrap();
        assert!(out.is_empty(), "a packet no mod extends is unchanged");
    }

    #[test]
    fn a_packet_of_the_table_gets_the_field() {
        let version = JavaMinecraftVersion::V_26_3;
        let mut out = Vec::new();
        ModSet::from_channels([SABLE])
            .write_extensions(
                pumpkin_data::packet::clientbound::play::MOVE_ENTITY_POS.to_id(version),
                &version,
                &mut out,
            )
            .unwrap();
        assert_eq!(out, [0x00]);
        // The vanilla answer writes nothing.
        let mut out = Vec::new();
        ModSet::NONE
            .write_extensions(
                pumpkin_data::packet::clientbound::play::MOVE_ENTITY_POS.to_id(version),
                &version,
                &mut out,
            )
            .unwrap();
        assert!(out.is_empty());
    }
}