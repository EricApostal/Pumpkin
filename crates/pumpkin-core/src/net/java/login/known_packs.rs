#[allow(clippy::wildcard_imports)]
use super::*;

impl PendingConnection {
    pub async fn handle_known_packs(&mut self, server: &Arc<Server>) {
        let version = CURRENT_MC_VERSION;
        let registry = pumpkin_data::registry::Registry::get_synced(version);
        let mut sent = Vec::with_capacity(registry.len());
        for reg in &registry {
            sent.push(reg.registry_id.clone());
            if reg.registry_id == "minecraft:damage_type" {
                let merged = server
                    .datapack_manager
                    .merge_damage_type_entries(&reg.registry_entries);
                let merged = self.with_neoforge_datapack_entries(&reg.registry_id, merged);
                self.send_packet_now(&CRegistryData::new(&reg.registry_id, &merged))
                    .await;
                continue;
            }
            let mut merged = server
                .datapack_manager
                .merge_registry_entries(&reg.registry_id, &reg.registry_entries);
            if reg.registry_id == "minecraft:enchantment" {
                // Plugin enchantments follow the vanilla ones, which is what their ids say.
                let at = pumpkin_data::Enchantment::ALL.len().min(merged.len());
                let plugin = server.enchantment_manager.registry_entries().await;
                merged.splice(at..at, plugin);
            }
            let merged = self.with_neoforge_datapack_entries(&reg.registry_id, merged);
            self.send_packet_now(&CRegistryData::new(&reg.registry_id, &merged))
                .await;
        }
        // Registries only a plugin declared entries for.
        for (registry_id, entries) in self.neoforge_extra_datapack_registries(&sent) {
            self.send_packet_now(&CRegistryData::new(&registry_id, &entries))
                .await;
        }
        let mut tags = Vec::new();
        for &key in pumpkin_data::tag::RegistryKey::NETWORK_KEYS {
            if pumpkin_data::tag::get_registry_key_tags(version, key)
                .is_some_and(|map| !map.is_empty())
            {
                tags.push(key);
            }
        }
        self.send_packet_now(&CUpdateTags::new(&tags)).await;
        // The NeoForge tasks that follow the registry data and tags.
        if self.neoforge_begin_finish().await {
            return;
        }
        self.complete_configuration(server).await;
    }

    /// `entries` plus the entries of this registry that plugins declared for `NeoForge` clients.
    fn with_neoforge_datapack_entries(
        &self,
        registry_id: &str,
        mut entries: Vec<pumpkin_data::registry::RegistryEntryData>,
    ) -> Vec<pumpkin_data::registry::RegistryEntryData> {
        for added in self.neoforge_datapack_entries(registry_id) {
            if !entries.iter().any(|entry| entry.entry_id == added.entry_id) {
                entries.push(added);
            }
        }
        entries
    }

    /// Everything after the registry data and tags: the finish hook, then "finish configuration".
    pub(in crate::net::java) async fn complete_configuration(&mut self, server: &Arc<Server>) {
        if self.begin_configuration_finish_hook(server).await {
            return;
        }
        self.send_finish_configuration().await;
    }
}
