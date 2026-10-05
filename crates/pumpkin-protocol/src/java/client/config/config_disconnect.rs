use pumpkin_data::packet::clientbound::config::DISCONNECT;
use pumpkin_macros::java_packet;

use crate::ClientPacket;
use crate::ser::NetworkWriteExt;
use pumpkin_util::text::TextComponent;
use pumpkin_util::version::JavaMinecraftVersion;

/// Disconnects the client in the configuration state.
///
/// The client reads the reason as a text component (network NBT), like the play disconnect.
#[java_packet(DISCONNECT)]
pub struct CConfigDisconnect<'a> {
    /// Plain text shown to the player.
    pub reason: &'a str,
}

impl<'a> CConfigDisconnect<'a> {
    #[must_use]
    pub const fn new(reason: &'a str) -> Self {
        Self { reason }
    }
}

impl ClientPacket for CConfigDisconnect<'_> {
    fn write_packet_data(
        &self,
        mut write: impl std::io::Write,
        version: &JavaMinecraftVersion,
    ) -> Result<(), crate::ser::WritingError> {
        write.write_component(&TextComponent::text(self.reason.to_string()), version)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reason_is_written_as_a_network_nbt_text_component() {
        let mut bytes = Vec::new();
        CConfigDisconnect::new("bye")
            .write_packet_data(&mut bytes, &JavaMinecraftVersion::V_26_3)
            .expect("write");
        // Nameless string tag: type 8, u16 length, text.
        assert_eq!(bytes, [8, 0, 3, b'b', b'y', b'e']);
    }
}
