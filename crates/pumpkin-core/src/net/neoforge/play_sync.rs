//! Play phase payloads sent to NeoForge clients that are already connected.

use pumpkin_protocol::{ConnectionFlavour, java::client::play::CCustomPayload};

use super::{channels, payloads::encode_config_file, recipe_sync};
use crate::{net::ClientPlatform, server::Server};

/// Sends one play payload to every NeoForge player. Returns how many players it was queued for.
fn broadcast(server: &Server, channel: &str, data: &[u8]) -> usize {
    let packet = CCustomPayload::new(channel, data);
    let mut sent = 0;
    for player in server.get_all_players() {
        let ClientPlatform::Java(client) = player.client.as_ref() else {
            continue;
        };
        if client.flavour.load() != ConnectionFlavour::NeoForge {
            continue;
        }
        if let Ok(bytes) = client.serialize_packet(&packet) {
            client.try_enqueue_packet(bytes);
            sent += 1;
        }
    }
    sent
}

/// Sends a synced config file (`neoforge:config_file`) to the connected NeoForge players.
pub fn broadcast_config_file(server: &Server, name: &str, content: &[u8]) -> usize {
    broadcast(server, channels::CONFIG_FILE, &encode_config_file(name, content))
}

/// Sends the current `neoforge:recipe_content` to the connected NeoForge players, after a
/// datapack reload.
pub fn broadcast_recipe_content(server: &Server) -> usize {
    broadcast(
        server,
        channels::RECIPE_CONTENT,
        &recipe_sync::recipe_content(&server.recipe_manager),
    )
}
