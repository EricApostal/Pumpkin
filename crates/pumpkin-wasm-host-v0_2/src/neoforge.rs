use crate::pumpkin::plugin::neoforge::{
    ChannelFlow as WitChannelFlow, ChannelPhase as WitChannelPhase, Declaration as WitDeclaration,
    Host, NonNeoforgePolicy,
};
use pumpkin_core::net::neoforge::{
    declaration::{
        self, ChannelDecl, ConfigFileDecl, DataMapDecl, DatapackEntryDecl, Declaration, ModInfo,
        RegistryDecl,
    },
    payloads::{Flow, Phase},
    play_sync, recipe_sync,
};
use pumpkin_core::plugin::permissions;
use pumpkin_wasm_host_common::state::PluginHostState;
use tracing::info;

/// The key of the plugin's declaration.
fn owner(state: &PluginHostState) -> String {
    state
        .name
        .clone()
        .unwrap_or_else(|| "<unknown>".to_string())
}

const fn phase(phase: WitChannelPhase) -> Phase {
    match phase {
        WitChannelPhase::Configuration => Phase::Configuration,
        WitChannelPhase::Play => Phase::Play,
    }
}

const fn flow(flow: WitChannelFlow) -> Option<Flow> {
    match flow {
        WitChannelFlow::Clientbound => Some(Flow::Clientbound),
        WitChannelFlow::Serverbound => Some(Flow::Serverbound),
        WitChannelFlow::Both => None,
    }
}

fn to_declaration(declaration: WitDeclaration) -> Declaration {
    Declaration {
        mod_info: ModInfo {
            id: declaration.mod_info.id,
            version: declaration.mod_info.version,
            display_name: declaration.mod_info.display_name.unwrap_or_default(),
        },
        channels: declaration
            .channels
            .into_iter()
            .map(|channel| ChannelDecl {
                id: channel.id,
                version: channel.version,
                phases: channel.phases.into_iter().map(phase).collect(),
                flow: flow(channel.flow),
                optional: channel.optional,
            })
            .collect(),
        registries: declaration
            .registries
            .into_iter()
            .map(|registry| RegistryDecl {
                key: registry.key,
                entries: registry.entries,
            })
            .collect(),
        config_files: declaration
            .config_files
            .into_iter()
            .map(|file| ConfigFileDecl {
                name: file.name,
                content: file.content,
            })
            .collect(),
        datapack_entries: declaration
            .datapack_entries
            .into_iter()
            .map(|entry| DatapackEntryDecl {
                registry: entry.registry,
                id: entry.id,
                data: entry.data,
            })
            .collect(),
        data_maps: declaration
            .data_maps
            .into_iter()
            .map(|map| DataMapDecl {
                registry: map.registry,
                id: map.id,
                mandatory: map.mandatory,
            })
            .collect(),
        feature_flags: declaration.feature_flags,
        allow_non_neoforge: matches!(declaration.non_neoforge, NonNeoforgePolicy::Allow),
        reject_message: declaration.reject_message,
    }
}

impl Host for PluginHostState {
    fn declare(&mut self, declaration: WitDeclaration) -> wasmtime::Result<Result<(), String>> {
        if !self.permissions.iter().any(|p| p == permissions::NEOFORGE) {
            return Ok(Err(self.missing_permission(
                "declaring NeoForge content",
                permissions::NEOFORGE,
            )));
        }
        let owner = owner(self);
        let declaration = to_declaration(declaration);
        let mod_id = declaration.mod_info.id.clone();
        let result = declaration::declare(&owner, declaration);
        if result.is_ok() {
            info!("Plugin {owner} declared NeoForge mod {mod_id}");
        }
        Ok(result)
    }

    fn withdraw(&mut self) -> wasmtime::Result<()> {
        declaration::withdraw(&owner(self));
        recipe_sync::withdraw(&owner(self));
        Ok(())
    }

    fn declare_synced_recipe_types(
        &mut self,
        types: Vec<String>,
    ) -> wasmtime::Result<Result<(), String>> {
        if !self.permissions.iter().any(|p| p == permissions::NEOFORGE) {
            return Ok(Err(self.missing_permission(
                "declaring NeoForge content",
                permissions::NEOFORGE,
            )));
        }
        Ok(recipe_sync::declare_synced_types(&owner(self), &types))
    }

    fn update_config_file(
        &mut self,
        mod_id: String,
        file_name: String,
        content: Vec<u8>,
    ) -> wasmtime::Result<Result<(), String>> {
        if !self.permissions.iter().any(|p| p == permissions::NEOFORGE) {
            return Ok(Err(self.missing_permission(
                "updating NeoForge config files",
                permissions::NEOFORGE,
            )));
        }
        if let Err(message) =
            declaration::update_config_file(&owner(self), &mod_id, &file_name, content.clone())
        {
            return Ok(Err(message));
        }
        if let Some(server) = self.server.as_ref() {
            play_sync::broadcast_config_file(server, &file_name, &content);
        }
        Ok(Ok(()))
    }

    fn is_neoforge_player(&mut self, player_uuid: String) -> wasmtime::Result<bool> {
        let Some(server) = self.server.as_ref() else {
            return Ok(false);
        };
        let Ok(uuid) = uuid::Uuid::parse_str(&player_uuid) else {
            return Ok(false);
        };
        Ok(server.get_player_by_uuid(uuid).is_some_and(|player| {
            player.client.java().is_some_and(|client| {
                client.flavour.load() == pumpkin_protocol::ConnectionFlavour::NeoForge
            })
        }))
    }
}
