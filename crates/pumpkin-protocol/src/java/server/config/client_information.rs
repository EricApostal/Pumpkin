use pumpkin_data::packet::serverbound::config::CLIENT_INFORMATION;
use pumpkin_macros::java_packet;

use crate::VarInt;

use crate::{
    ServerPacket,
    ser::{NetworkReadExt, NetworkReadSliceExt, ReadingError},
};
use pumpkin_util::version::JavaMinecraftVersion;

/// Sent by the client to inform the server about its local settings
#[java_packet(CLIENT_INFORMATION)]
pub struct SClientInformationConfig<'a> {
    /// The language code used by the client (e.g., "`en_us`")
    pub locale: &'a str,
    /// The maximum number of chunks the client renders
    pub view_distance: i8,
    /// Visibility of chat messages (0: Enabled, 1: Commands Only, 2: Hidden)
    pub chat_mode: VarInt,
    /// Whether the client wants chat colors/formatting rendered
    pub chat_colors: bool,
    /// Bitmask representing displayed skin parts (e.g., cape, jacket, sleeves)
    pub skin_parts: u8,
    /// The player's dominant hand (0: Left, 1: Right)
    pub main_hand: VarInt,
    /// Whether the client wants text filtering (e.g., for profanity) enabled
    pub text_filtering: bool,
    /// Whether the player should appear in the server's online player list
    pub server_listing: bool,
    /// How much of the client's particle effects the server may send (`ALL`, `DECREASED`,
    /// `MINIMAL`, by ordinal).
    ///
    /// A `NeoForge` server of 26.3 decodes this from `client_information` (its patched
    /// `ClientInformation`), and a client that leaves it out gets its packet refused with
    /// `Failed to decode packet 'serverbound/minecraft:client_information'`. A peer that does not
    /// send it (a plain vanilla 26.3 client) means `ALL`, so the field is only read when the
    /// payload still has bytes for it.
    pub particle_status: VarInt,
}

impl<'a> ServerPacket<'a> for SClientInformationConfig<'a> {
    fn read(bytebuf: &mut &'a [u8], version: &JavaMinecraftVersion) -> Result<Self, ReadingError> {
        let locale = bytebuf.get_str_borrowed()?;
        let view_distance = bytebuf.get_i8()?;
        let chat_mode = bytebuf.get_var_int()?;
        let chat_colors = bytebuf.get_bool()?;
        let skin_parts = bytebuf.get_u8()?;
        let main_hand = if version >= &JavaMinecraftVersion::V_1_9 {
            bytebuf.get_var_int()?
        } else {
            VarInt(1)
        };
        let text_filtering = if version >= &JavaMinecraftVersion::V_1_17 {
            bytebuf.get_bool()?
        } else {
            false
        };
        let server_listing = if version >= &JavaMinecraftVersion::V_1_18 {
            bytebuf.get_bool()?
        } else {
            true
        };

        let particle_status = if version >= &JavaMinecraftVersion::V_26_3 && !bytebuf.is_empty() {
            bytebuf.get_var_int()?
        } else {
            VarInt(0)
        };

        Ok(Self {
            locale,
            view_distance,
            chat_mode,
            chat_colors,
            skin_parts,
            main_hand,
            text_filtering,
            server_listing,
            particle_status,
        })
    }
}

impl crate::ClientPacket for SClientInformationConfig<'_> {
    fn write_packet_data(
        &self,
        mut write: impl std::io::Write,
        version: &JavaMinecraftVersion,
    ) -> Result<(), crate::ser::WritingError> {
        use crate::ser::NetworkWriteExt;
        write.write_string(self.locale)?;
        write.write_i8(self.view_distance)?;
        write.write_var_int(&self.chat_mode)?;
        write.write_bool(self.chat_colors)?;
        write.write_u8(self.skin_parts)?;
        if version >= &JavaMinecraftVersion::V_1_9 {
            write.write_var_int(&self.main_hand)?;
        }
        if version >= &JavaMinecraftVersion::V_1_17 {
            write.write_bool(self.text_filtering)?;
        }
        if version >= &JavaMinecraftVersion::V_1_18 {
            write.write_bool(self.server_listing)?;
        }
        if version >= &JavaMinecraftVersion::V_26_3 {
            write.write_var_int(&self.particle_status)?;
        }
        Ok(())
    }
}
