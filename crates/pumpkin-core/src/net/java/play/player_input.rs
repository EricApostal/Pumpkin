#[allow(clippy::wildcard_imports)]
use super::*;

impl JavaClient {
    pub fn handle_player_input(
        &self,
        player: &Arc<Player>,
        input: &SPlayerInput,
        server: &Arc<Server>,
    ) {
        let mut input_event =
            crate::plugin::api::events::player::player_input::PlayerInputEvent::new(
                player.clone(),
                format!("{:b}", input.input),
            );
        server
            .plugin_manager
            .fire_blocking(server, &mut input_event);
        if input_event.cancelled {
            return;
        }

        player.last_input.store(input.input, Ordering::Relaxed);

        // Riders of plugin vehicles tell the plugin what they press.
        if let Some(vehicle) = player.get_entity().get_vehicle()
            && vehicle.get_plugin_entity().is_some()
            && server
                .plugin_manager
                .has_handlers::<crate::plugin::api::events::entity::PluginVehicleInputEvent>()
        {
            let axis = |positive: i8, negative: i8| {
                f32::from(i8::from(input.input & positive != 0) - i8::from(input.input & negative != 0))
            };
            let mut vehicle_input =
                crate::plugin::api::events::entity::PluginVehicleInputEvent::new(
                    vehicle.get_entity().entity_id,
                    player.clone(),
                    axis(SPlayerInput::FORWARD, SPlayerInput::BACKWARD),
                    axis(SPlayerInput::LEFT, SPlayerInput::RIGHT),
                    input.input & SPlayerInput::JUMP != 0,
                    input.input & SPlayerInput::SNEAK != 0,
                    input.input & SPlayerInput::SPRINT != 0,
                );
            server
                .plugin_manager
                .fire_blocking(server, &mut vehicle_input);
        }

        let sneak = input.input & SPlayerInput::SNEAK != 0;
        if sneak
            && player.gamemode.load() == GameMode::Spectator
            && player.camera_target_id.load().is_some()
        {
            player.camera_target_id.store(None);
            player.try_send_client_packet(&CSetCamera::new(player.entity_id().into()));
        }

        if player.get_entity().is_sneaking() != sneak {
            send_cancellable_blocking! {{
                server;
                PlayerToggleSneakEvent::new(player.clone(), sneak);
                'after: {
                    player.get_entity().set_sneaking(event.is_sneaking);
                    if event.is_sneaking {
                        let vehicle = player
                            .get_entity()
                            .vehicle
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                            .clone();
                        if let Some(vehicle) = vehicle {
                            vehicle.get_entity().remove_passenger(player.entity_id());
                        }
                    }
                }
            }}
        } else if sneak {
            let vehicle = player
                .get_entity()
                .vehicle
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone();
            if let Some(vehicle) = vehicle {
                vehicle.get_entity().remove_passenger(player.entity_id());
            }
        }
    }
}
