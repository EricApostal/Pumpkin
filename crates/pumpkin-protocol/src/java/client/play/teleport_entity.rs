use std::io::Write;

use pumpkin_data::packet::clientbound::play::TELEPORT_ENTITY;
use pumpkin_macros::java_packet;
use pumpkin_util::{math::vector3::Vector3, version::JavaMinecraftVersion};

use crate::{ClientPacket, PositionFlag, VarInt, WritingError, ser::NetworkWriteExt};

/// Only used when teleporting a player's vehicle, this packet is sent to the player.
#[java_packet(TELEPORT_ENTITY)]
pub struct CTeleportEntity<'a> {
    pub entity_id: VarInt,
    pub position: Vector3<f64>,
    pub delta: Vector3<f64>,
    pub yaw: f32,
    pub pitch: f32,
    pub relatives: &'a [PositionFlag],
    pub on_ground: bool,
}

impl<'a> CTeleportEntity<'a> {
    #[must_use]
    pub const fn new(
        entity_id: VarInt,
        position: Vector3<f64>,
        delta: Vector3<f64>,
        yaw: f32,
        pitch: f32,
        relatives: &'a [PositionFlag],
        on_ground: bool,
    ) -> Self {
        Self {
            entity_id,
            position,
            delta,
            yaw,
            pitch,
            relatives,
            on_ground,
        }
    }
}

// TODO: Do we need a custom impl?
// Fields a mod of the receiving client appends to this packet do not appear here: they are applied
// from `crate::mods::MOD_EXTENSIONS` where every clientbound packet is written.
impl ClientPacket for CTeleportEntity<'_> {
    fn write_packet_data(
        &self,
        write: impl Write,
        version: &JavaMinecraftVersion,
    ) -> Result<(), WritingError> {
        let mut write = write;

        if *version <= JavaMinecraftVersion::V_1_7_6 {
            write.write_i32_be(self.entity_id.0)?;
        } else {
            write.write_var_int(&self.entity_id)?;
        }

        if version >= &JavaMinecraftVersion::V_1_9 {
            write.write_f64_be(self.position.x)?;
            write.write_f64_be(self.position.y)?;
            write.write_f64_be(self.position.z)?;
        } else {
            write.write_i32_be((self.position.x * 32.0).floor() as i32)?;
            write.write_i32_be((self.position.y * 32.0).floor() as i32)?;
            write.write_i32_be((self.position.z * 32.0).floor() as i32)?;
        }

        if version >= &JavaMinecraftVersion::V_1_21_2 {
            write.write_f64_be(self.delta.x)?;
            write.write_f64_be(self.delta.y)?;
            write.write_f64_be(self.delta.z)?;
            write.write_f32_be(self.yaw)?;
            write.write_f32_be(self.pitch)?;
            write.write_i32_be(PositionFlag::get_bitfield(self.relatives))?;
        } else {
            write.write_u8((self.yaw.rem_euclid(360.0) * 256.0 / 360.0).floor() as u8)?;
            write.write_u8((self.pitch.rem_euclid(360.0) * 256.0 / 360.0).floor() as u8)?;
        }

        if version >= &JavaMinecraftVersion::V_1_8 {
            write.write_bool(self.on_ground)?;
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::CTeleportEntity;
    use crate::{ClientPacket, ConnectionFlavour, ModSet, PacketEncoding, VarInt};
    use pumpkin_util::{math::vector3::Vector3, version::JavaMinecraftVersion};

    /// A channel of the mod whose row in `crate::mods::MOD_EXTENSIONS` extends this packet.
    const MODDED: &str = "sable:snapshot_packet";

    /// The whole frame (packet id included) as one connection reads it.
    fn frame(encoding: PacketEncoding) -> Vec<u8> {
        let packet = CTeleportEntity::new(
            VarInt(5),
            Vector3::new(1.5, 64.0, -2.25),
            Vector3::new(0.0, 0.0, 0.0),
            90.0,
            45.0,
            &[],
            true,
        );
        encoding
            .scope(|| {
                packet
                    .serialize_packet(&JavaMinecraftVersion::V_26_3)
                    .unwrap()
            })
            .to_vec()
    }

    #[test]
    fn vanilla_is_unchanged_and_a_mod_of_this_packet_appends_one_boolean() {
        let vanilla = frame(PacketEncoding::VANILLA);
        assert_eq!(
            vanilla,
            [
                0x80, 0x01, 0x05, // the packet id (128) and the entity id
                0x3f, 0xf8, 0, 0, 0, 0, 0, 0, // 1.5
                0x40, 0x50, 0, 0, 0, 0, 0, 0, // 64.0
                0xc0, 0x02, 0, 0, 0, 0, 0, 0, // -2.25
                0, 0, 0, 0, 0, 0, 0, 0, // the velocity x
                0, 0, 0, 0, 0, 0, 0, 0, // y
                0, 0, 0, 0, 0, 0, 0, 0, // z
                0x42, 0xb4, 0, 0, // yaw 90
                0x42, 0x34, 0, 0, // pitch 45
                0, 0, 0, 0, // no relative flag
                0x01, // on ground
            ],
            "the vanilla frame"
        );

        let modded = frame(PacketEncoding::new(
            ConnectionFlavour::Vanilla,
            ModSet::from_channels([MODDED]),
        ));
        assert_eq!(modded.len(), vanilla.len() + 1);
        assert_eq!(modded[..vanilla.len()], vanilla[..], "the vanilla bytes stay");
        assert_eq!(
            modded[vanilla.len()],
            0x00,
            "the appended boolean is false: Pumpkin has no sub-level to be in"
        );

        // A connection without the mod, and one whose mods extend other packets, are untouched.
        assert_eq!(frame(PacketEncoding::VANILLA), vanilla);
        assert_eq!(
            frame(PacketEncoding::new(
                ConnectionFlavour::NeoForge,
                ModSet::from_channels(["neoforge:register"]),
            )),
            vanilla
        );
    }
}
