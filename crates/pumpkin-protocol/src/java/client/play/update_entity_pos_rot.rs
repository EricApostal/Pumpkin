use pumpkin_data::packet::clientbound::play::MOVE_ENTITY_POS_ROT;
use pumpkin_macros::java_packet;
use pumpkin_util::{math::vector3::Vector3, version::JavaMinecraftVersion};

use crate::{
    ClientPacket, ServerPacket, VarInt,
    ser::{NetworkReadExt, NetworkWriteExt, ReadingError, WritingError},
};

#[java_packet(MOVE_ENTITY_POS_ROT)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CUpdateEntityPosRot {
    pub entity_id: VarInt,
    pub delta: Vector3<i16>,
    pub yaw: u8,
    pub pitch: u8,
    pub on_ground: bool,
}

impl CUpdateEntityPosRot {
    #[must_use]
    pub const fn new(
        entity_id: VarInt,
        delta: Vector3<i16>,
        yaw: u8,
        pitch: u8,
        on_ground: bool,
    ) -> Self {
        Self {
            entity_id,
            delta,
            yaw,
            pitch,
            on_ground,
        }
    }
}

// Fields a mod of the receiving client appends to this packet do not appear here: they are applied
// from `crate::mods::MOD_EXTENSIONS` where every clientbound packet is written.
impl ClientPacket for CUpdateEntityPosRot {
    fn write_packet_data(
        &self,
        mut write: impl std::io::Write,
        version: &JavaMinecraftVersion,
    ) -> Result<(), WritingError> {
        if *version <= JavaMinecraftVersion::V_1_7_6 {
            write.write_i32_be(self.entity_id.0)?;
        } else {
            write.write_var_int(&self.entity_id)?;
        }
        // Since 26.3 the on ground flag and the delta step count are packed into a var int in
        // front of the delta, 0 steps being a single linear delta.
        if *version >= JavaMinecraftVersion::V_26_3 {
            write.write_var_int(&VarInt(i32::from(self.on_ground)))?;
        }
        if *version >= JavaMinecraftVersion::V_1_9 {
            write.write_i16_be(self.delta.x)?;
            write.write_i16_be(self.delta.y)?;
            write.write_i16_be(self.delta.z)?;
        } else {
            write.write_i8((self.delta.x / 128) as i8)?;
            write.write_i8((self.delta.y / 128) as i8)?;
            write.write_i8((self.delta.z / 128) as i8)?;
        }
        write.write_u8(self.yaw)?;
        write.write_u8(self.pitch)?;
        if *version >= JavaMinecraftVersion::V_1_8 && *version < JavaMinecraftVersion::V_26_3 {
            write.write_bool(self.on_ground)?;
        }
        Ok(())
    }
}

impl<'a> ServerPacket<'a> for CUpdateEntityPosRot {
    fn read(bytebuf: &mut &'a [u8], version: &JavaMinecraftVersion) -> Result<Self, ReadingError> {
        let entity_id = if *version <= JavaMinecraftVersion::V_1_7_6 {
            VarInt(bytebuf.get_i32_be()?)
        } else {
            bytebuf.get_var_int()?
        };
        let delta = if *version >= JavaMinecraftVersion::V_1_9 {
            Vector3::new(
                bytebuf.get_i16_be()?,
                bytebuf.get_i16_be()?,
                bytebuf.get_i16_be()?,
            )
        } else {
            Vector3::new(
                i16::from(bytebuf.get_i8()?) * 128,
                i16::from(bytebuf.get_i8()?) * 128,
                i16::from(bytebuf.get_i8()?) * 128,
            )
        };
        let yaw = bytebuf.get_u8()?;
        let pitch = bytebuf.get_u8()?;
        let on_ground = if *version >= JavaMinecraftVersion::V_1_8 {
            bytebuf.get_bool()?
        } else {
            false
        };
        Ok(Self {
            entity_id,
            delta,
            yaw,
            pitch,
            on_ground,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::CUpdateEntityPosRot;
    use crate::{ClientPacket, ConnectionFlavour, ModSet, PacketEncoding, VarInt};
    use pumpkin_util::{math::vector3::Vector3, version::JavaMinecraftVersion};

    /// A channel of the mod whose row in `crate::mods::MOD_EXTENSIONS` extends this packet.
    const MODDED: &str = "sable:snapshot_packet";

    /// The whole frame (packet id included) as one connection reads it.
    fn frame(encoding: PacketEncoding) -> Vec<u8> {
        let packet = CUpdateEntityPosRot::new(VarInt(5), Vector3::new(1, 2, 3), 90, 45, true);
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
            [0x37, 0x05, 0x01, 0x00, 0x01, 0x00, 0x02, 0x00, 0x03, 90, 45],
            "the vanilla frame: id, entity id, the packed on-ground, the delta, the angles"
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
