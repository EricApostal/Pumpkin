use std::{
    collections::{BTreeMap, BTreeSet},
    future::Future,
    sync::{Arc, Weak},
};

use wasmtime::component::{Resource, ResourceTable};
use wasmtime_wasi::{WasiCtx, WasiCtxBuilder, WasiCtxView, WasiView};
use wasmtime_wasi_http::{
    RequestOptions, WasiBody, WasiHttpCtx, WasiHttpCtxView, WasiHttpHooks, WasiHttpView,
};

use crate::plugin::WasmPlugin;
use pumpkin_core::{command::CommandSender, entity::player::Player, server::Server, world::World};

pub struct WasmCommand {
    pub names: Vec<String>,
    pub builder: pumpkin_core::command::argument_builder::CommandArgumentBuilder,
}

impl WasmCommand {
    #[must_use]
    pub fn new(names: Vec<String>, description: String) -> Self {
        let primary = names.first().cloned().unwrap_or_default();
        let builder = pumpkin_core::command::argument_builder::command(primary, description);
        Self { names, builder }
    }

    #[must_use]
    pub fn then(mut self, child: WasmCommandNode) -> Self {
        use pumpkin_core::command::argument_builder::ArgumentBuilder;
        self.builder = self.builder.then(child.into_detached_node());
        self
    }

    #[must_use]
    pub fn executes(
        mut self,
        executor: impl pumpkin_core::command::node::CommandExecutor + 'static,
    ) -> Self {
        use pumpkin_core::command::argument_builder::ArgumentBuilder;
        self.builder = self.builder.executes(executor);
        self
    }
}

pub enum WasmCommandNode {
    Literal(pumpkin_core::command::argument_builder::LiteralArgumentBuilder),
    Argument(pumpkin_core::command::argument_builder::RequiredArgumentBuilder),
}

impl WasmCommandNode {
    #[must_use]
    pub fn then(self, child: Self) -> Self {
        use pumpkin_core::command::argument_builder::ArgumentBuilder;
        match self {
            Self::Literal(b) => Self::Literal(b.then(child.into_detached_node())),
            Self::Argument(b) => Self::Argument(b.then(child.into_detached_node())),
        }
    }

    #[must_use]
    pub fn executes(
        self,
        executor: impl pumpkin_core::command::node::CommandExecutor + 'static,
    ) -> Self {
        use pumpkin_core::command::argument_builder::ArgumentBuilder;
        match self {
            Self::Literal(b) => Self::Literal(b.executes(executor)),
            Self::Argument(b) => Self::Argument(b.executes(executor)),
        }
    }

    #[must_use]
    pub fn suggests(
        self,
        provider: impl pumpkin_core::command::suggestion::provider::SuggestionProvider + 'static,
    ) -> Self {
        match self {
            Self::Literal(b) => Self::Literal(b),
            Self::Argument(b) => Self::Argument(b.suggests(provider)),
        }
    }

    #[must_use]
    pub fn into_detached_node(self) -> pumpkin_core::command::node::detached::DetachedNode {
        use pumpkin_core::command::argument_builder::ArgumentBuilder;
        match self {
            Self::Literal(b) => {
                pumpkin_core::command::node::detached::DetachedNode::Literal(b.build())
            }
            Self::Argument(b) => {
                pumpkin_core::command::node::detached::DetachedNode::Argument(b.build())
            }
        }
    }
}

/// A command sender as handed to a plugin, together with the world the command
/// is being executed in.
///
/// [`CommandSender`] only knows a world for the senders that are bound to one
/// (players and command blocks). Console and dummy senders operate in the world
/// of the command source that dispatched them, which is only known while the
/// command runs, so it is recorded here instead of being dropped.
#[derive(Clone)]
pub struct CommandSenderContext {
    pub sender: CommandSender,
    pub world: Option<Arc<World>>,
}

impl CommandSenderContext {
    #[must_use]
    pub const fn new(sender: CommandSender, world: Option<Arc<World>>) -> Self {
        Self { sender, world }
    }

    /// The world this command runs in: the sender's own world if it is bound to
    /// one, otherwise the world of the command source that dispatched it.
    #[must_use]
    pub fn world(&self) -> Option<Arc<World>> {
        self.sender.world().or_else(|| self.world.clone())
    }
}

impl std::ops::Deref for CommandSenderContext {
    type Target = CommandSender;

    fn deref(&self) -> &Self::Target {
        &self.sender
    }
}

#[derive(Clone)]
pub enum ScoreboardProvider {
    World(Arc<World>),
    Player(Arc<Player>),
}

#[derive(Clone)]
pub enum InventoryProvider {
    Generic(Arc<dyn pumpkin_inventory::Inventory>),
    PlayerMain(Arc<Player>),
    PlayerEnderChest(Arc<Player>),
}

#[derive(Clone)]
pub struct ContainerBlockEntity {
    pub provider: Arc<dyn pumpkin_core::block::entities::BlockEntity>,
    pub inventory: Arc<dyn pumpkin_inventory::Inventory>,
}

#[derive(Clone)]
pub struct ChunkBuffer {
    pub x: i32,
    pub z: i32,
    pub min_y: i32,
    pub height: u32,
    pub proto_chunk: Arc<std::sync::Mutex<pumpkin_world::ProtoChunk>>,
}

/// What a plugin registered through the host API while it was loading.
///
/// Every successful registration used to be logged at `info!`. A large plugin
/// registers thousands of entries, and one line each buried the startup log, so
/// those lines are `debug!` now and this tally is what the single summary line
/// emitted when the load finishes reports. Nothing is lost: `debug` still has the
/// per-entry detail, the summary has the totals.
///
/// The tally keeps the keys rather than a bare counter because the count must be
/// the number of entries the plugin added, not the number of calls it made. The
/// dynamic registries return an already-registered entry unchanged when the same
/// definition is registered again (see `DynamicBlockRegistry::register`), so a
/// plugin that does that must not be reported as having registered the entry
/// twice. Things that append rather than insert — a feature-to-biome rule, a
/// recipe — are counted per call, because each call really does add another one.
///
/// It lives in the store rather than being derived from the dynamic registries
/// after the load because part of what it counts (item→block links, block callback
/// and placement assignments, feature rules) is not enumerable from any registry,
/// and because the registries are global: a scan cannot tell which plugin added
/// what without guessing from namespaces.
#[derive(Debug, Default)]
pub struct RegistrationTally {
    /// Block key to the number of states the block was registered with.
    blocks: BTreeMap<String, usize>,
    block_callbacks: BTreeSet<String>,
    /// Block key to the number of placement rules it was last given.
    block_placement: BTreeMap<String, usize>,
    /// `(item key, block key)` pairs.
    item_block_links: BTreeSet<(String, String)>,
    items: BTreeSet<String>,
    block_entity_types: BTreeSet<String>,
    fluids: BTreeSet<String>,
    entity_types: BTreeSet<String>,
    menu_types: BTreeSet<String>,
    damage_types: BTreeSet<String>,
    data_component_types: BTreeSet<String>,
    sounds: BTreeSet<String>,
    particles: BTreeSet<String>,
    mob_effects: BTreeSet<String>,
    features: BTreeSet<String>,
    ore_features: BTreeSet<String>,
    biome_feature_rules: usize,
    recipe_types: BTreeSet<String>,
    recipes: BTreeSet<String>,
}

impl RegistrationTally {
    pub fn block(&mut self, key: &str, states: usize) {
        self.blocks.insert(key.to_string(), states);
    }

    pub fn block_callbacks(&mut self, block_key: &str) {
        self.block_callbacks.insert(block_key.to_string());
    }

    pub fn block_placement_rules(&mut self, block_key: &str, rules: usize) {
        self.block_placement.insert(block_key.to_string(), rules);
    }

    pub fn item_block_link(&mut self, item_key: &str, block_key: &str) {
        self.item_block_links
            .insert((item_key.to_string(), block_key.to_string()));
    }

    pub fn item(&mut self, key: &str) {
        self.items.insert(key.to_string());
    }

    pub fn block_entity_type(&mut self, key: &str) {
        self.block_entity_types.insert(key.to_string());
    }

    pub fn fluid(&mut self, key: &str) {
        self.fluids.insert(key.to_string());
    }

    pub fn entity_type(&mut self, key: &str) {
        self.entity_types.insert(key.to_string());
    }

    pub fn menu_type(&mut self, key: &str) {
        self.menu_types.insert(key.to_string());
    }

    pub fn damage_type(&mut self, key: &str) {
        self.damage_types.insert(key.to_string());
    }

    pub fn data_component_type(&mut self, key: &str) {
        self.data_component_types.insert(key.to_string());
    }

    pub fn sound(&mut self, key: &str) {
        self.sounds.insert(key.to_string());
    }

    pub fn particle(&mut self, key: &str) {
        self.particles.insert(key.to_string());
    }

    pub fn mob_effect(&mut self, key: &str) {
        self.mob_effects.insert(key.to_string());
    }

    pub fn feature(&mut self, key: &str) {
        self.features.insert(key.to_string());
    }

    pub fn ore_feature(&mut self, key: &str) {
        self.ore_features.insert(key.to_string());
    }

    pub fn biome_feature_rule(&mut self) {
        self.biome_feature_rules += 1;
    }

    pub fn recipe_type(&mut self, key: &str) {
        self.recipe_types.insert(key.to_string());
    }

    pub fn recipe(&mut self, id: &str) {
        self.recipes.insert(id.to_string());
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.blocks.is_empty()
            && self.block_callbacks.is_empty()
            && self.block_placement.is_empty()
            && self.item_block_links.is_empty()
            && self.items.is_empty()
            && self.block_entity_types.is_empty()
            && self.fluids.is_empty()
            && self.entity_types.is_empty()
            && self.menu_types.is_empty()
            && self.damage_types.is_empty()
            && self.data_component_types.is_empty()
            && self.sounds.is_empty()
            && self.particles.is_empty()
            && self.mob_effects.is_empty()
            && self.features.is_empty()
            && self.ore_features.is_empty()
            && self.biome_feature_rules == 0
            && self.recipe_types.is_empty()
            && self.recipes.is_empty()
    }

    /// The one line that replaces this plugin's per-entry registration log.
    ///
    /// `None` when the plugin registered nothing, so a plugin that only declares
    /// permissions does not gain a line that says nothing. The tally is emptied,
    /// so a second call cannot report the same entries again.
    #[must_use]
    pub fn take_summary(&mut self, plugin: &str) -> Option<String> {
        if self.is_empty() {
            return None;
        }
        let mut parts: Vec<String> = Vec::new();
        if !self.blocks.is_empty() {
            let states: usize = self.blocks.values().sum();
            parts.push(format!(
                "{} ({states} states)",
                count(self.blocks.len(), "block", "blocks")
            ));
        }
        push_count(&mut parts, self.items.len(), "item", "items");
        push_count(
            &mut parts,
            self.item_block_links.len(),
            "item→block link",
            "item→block links",
        );
        push_count(
            &mut parts,
            self.block_entity_types.len(),
            "block entity type",
            "block entity types",
        );
        push_count(&mut parts, self.fluids.len(), "fluid", "fluids");
        push_count(&mut parts, self.entity_types.len(), "entity type", "entity types");
        push_count(&mut parts, self.menu_types.len(), "menu type", "menu types");
        push_count(
            &mut parts,
            self.damage_types.len(),
            "damage type",
            "damage types",
        );
        push_count(
            &mut parts,
            self.data_component_types.len(),
            "data component type",
            "data component types",
        );
        push_count(&mut parts, self.sounds.len(), "sound", "sounds");
        push_count(&mut parts, self.particles.len(), "particle", "particles");
        push_count(&mut parts, self.mob_effects.len(), "mob effect", "mob effects");
        push_count(&mut parts, self.features.len(), "feature", "features");
        push_count(
            &mut parts,
            self.ore_features.len(),
            "ore feature",
            "ore features",
        );
        push_count(
            &mut parts,
            self.biome_feature_rules,
            "feature-to-biome rule",
            "feature-to-biome rules",
        );
        push_count(
            &mut parts,
            self.recipe_types.len(),
            "recipe type",
            "recipe types",
        );
        push_count(&mut parts, self.recipes.len(), "recipe", "recipes");
        push_count(
            &mut parts,
            self.block_callbacks.len(),
            "block with callbacks",
            "blocks with callbacks",
        );
        if !self.block_placement.is_empty() {
            let rules: usize = self.block_placement.values().sum();
            parts.push(format!(
                "{} with placement rules ({rules} rules)",
                count(self.block_placement.len(), "block", "blocks")
            ));
        }
        *self = Self::default();
        Some(format!("Plugin {plugin} registered {}.", parts.join(", ")))
    }
}

fn count(n: usize, singular: &str, plural: &str) -> String {
    format!("{n} {}", if n == 1 { singular } else { plural })
}

fn push_count(parts: &mut Vec<String>, n: usize, singular: &str, plural: &str) {
    if n > 0 {
        parts.push(count(n, singular, plural));
    }
}

pub struct PluginHostState {
    pub wasi_ctx: WasiCtx,
    pub wasi_http_ctx: WasiHttpCtx,
    pub wasi_http_hooks: PluginHttpHooks,
    pub resource_table: ResourceTable,
    pub limits: wasmtime::StoreLimits,
    pub plugin: Option<Weak<WasmPlugin>>,
    pub server: Option<Arc<Server>>,
    pub permissions: Vec<String>,
    pub name: Option<String>,
    pub marketplace_metadata: Option<crate::signature::PumpkinMetadata>,
    /// What this plugin registered, reported as one line when its load finishes.
    pub registrations: RegistrationTally,
}

impl Default for PluginHostState {
    fn default() -> Self {
        Self::new()
    }
}

impl PluginHostState {
    #[must_use]
    pub fn new() -> Self {
        let resource_table = ResourceTable::new();
        Self {
            wasi_ctx: WasiCtxBuilder::new()
                .inherit_stdout() // allow messages & errors to be printed
                .inherit_stderr() // before `on_load`, e.g. during metadata retrieval
                .build(),
            wasi_http_ctx: WasiHttpCtx::new(),
            wasi_http_hooks: PluginHttpHooks::new(),
            resource_table,
            limits: wasmtime::StoreLimitsBuilder::new().build(),
            plugin: None,
            server: None,
            permissions: Vec::new(),
            name: None,
            marketplace_metadata: None,
            registrations: RegistrationTally::default(),
        }
    }

    /// Installs the permissions a plugin declared in its metadata.
    ///
    /// Every host call that needs a permission reads this list, so it must be
    /// installed before the guest export that makes the call runs: the metadata is
    /// read and the list set in `init_plugin`, and `on_load` replaces it with the
    /// configured permissions minus the blocked ones before `on-load` is called.
    ///
    /// The list is replaced whole, never appended to, so nothing carries over from
    /// an earlier load of the same store.
    pub fn set_declared_permissions(&mut self, declared: &[String]) {
        self.permissions.clear();
        self.permissions.extend_from_slice(declared);
    }

    /// The error for a host call the plugin's declared permissions do not allow.
    ///
    /// The declared list is part of the message on purpose. A plugin whose sources
    /// declare the permission but whose `.wasm` was built before that change exports
    /// the older list, and the host rejects the call exactly like a plugin that never
    /// declared it. Naming what the guest actually exported tells the two apart:
    /// `declares ["registry.items", ...]` is a stale binary, an empty list is a
    /// plugin that declares nothing.
    #[must_use]
    pub fn missing_permission(&self, what: &str, needed: &str) -> String {
        let declared = if self.permissions.is_empty() {
            "no permissions".to_string()
        } else {
            format!("{:?}", self.permissions)
        };
        format!(
            "{what} needs the '{needed}' permission, but the plugin declares {declared}. \
             A plugin whose sources declare it was built before that change; rebuild its .wasm."
        )
    }

    /// get a **borrowed** (or **owned**) [`Resource`]'s value from the resource table
    pub fn get<T: FromResource>(&self, res: &Resource<T>) -> wasmtime::Result<&T::Internal> {
        Ok(self.resource_table.get(&Resource::new_borrow(res.rep()))?)
    }

    pub fn get_mut<T: FromResource>(
        &mut self,
        res: &Resource<T>,
    ) -> wasmtime::Result<&mut T::Internal> {
        Ok(self
            .resource_table
            .get_mut(&Resource::new_borrow(res.rep()))?)
    }

    /// always returns an owned [`Resource`]
    pub fn add<T: FromResource>(&mut self, item: T::Internal) -> wasmtime::Result<Resource<T>> {
        Ok(Resource::new_own(self.resource_table.push(item)?.rep()))
    }

    /// take the value of an **owned** [`Resource`] from the resource table
    #[expect(
        clippy::needless_pass_by_value,
        reason = "Our 'cloning' then deletion of the handle wouldn't be sound if it still exists"
    )]
    pub fn take<T: FromResource>(&mut self, res: Resource<T>) -> wasmtime::Result<T::Internal> {
        debug_assert!(res.owned());
        Ok(self.resource_table.delete(Resource::new_own(res.rep()))?)
    }

    /// requires an **owned** [`Resource`]
    pub fn drop<T: FromResource>(&mut self, res: Resource<T>) -> wasmtime::Result<()> {
        self.take(res)?;
        Ok(())
    }

    pub fn discard<T: FromResource>(&mut self, res: Resource<T>) {
        let _ = self.drop(res);
    }

    pub fn discard_to_be_removed<T: FromResource>(&mut self, res: &Resource<T>) {
        assert!(res.owned());
        let _ = self
            .resource_table
            .delete::<T::Internal>(Resource::new_own(res.rep()));
    }
}

/// This trait could also be replaced with a bunch of `with: {}` definitions in the-
/// bindgen! macro. That would however enforce that said macro stays in this crate.
pub trait FromResource: Sized + 'static
where
    Resource<Self>: Send + Sync + 'static,
{
    type Internal: Send + Sync + 'static;
}

pub struct PluginHttpHooks {
    pub allow_outbound: bool,
}

impl PluginHttpHooks {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            allow_outbound: false,
        }
    }
}

impl Default for PluginHttpHooks {
    fn default() -> Self {
        Self::new()
    }
}

impl WasiHttpHooks for PluginHttpHooks {
    fn send_request(
        &mut self,
        request: hyper::Request<WasiBody>,
        options: Option<RequestOptions>,
        fut: Box<dyn Future<Output = wasmtime_wasi_http::Result<()>> + Send>,
    ) -> Box<
        dyn Future<
                Output = wasmtime_wasi_http::Result<(
                    hyper::Response<WasiBody>,
                    Box<dyn Future<Output = wasmtime_wasi_http::Result<()>> + Send>,
                )>,
            > + Send,
    > {
        if !self.allow_outbound {
            return Box::new(async { Err(wasmtime_wasi_http::Error::HttpRequestDenied) });
        }

        wasmtime_wasi_http::default_hooks().send_request(request, options, fut)
    }
}

impl WasiView for PluginHostState {
    fn ctx(&mut self) -> WasiCtxView<'_> {
        WasiCtxView {
            ctx: &mut self.wasi_ctx,
            table: &mut self.resource_table,
        }
    }
}

impl WasiHttpView for PluginHostState {
    fn http(&mut self) -> WasiHttpCtxView<'_> {
        WasiHttpCtxView {
            ctx: &mut self.wasi_http_ctx,
            table: &mut self.resource_table,
            hooks: &mut self.wasi_http_hooks,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::RegistrationTally;

    #[test]
    fn an_empty_tally_has_no_summary() {
        let mut tally = RegistrationTally::default();
        assert_eq!(tally.take_summary("quiet"), None);
    }

    #[test]
    fn the_summary_names_every_category_that_was_registered() {
        let mut tally = RegistrationTally::default();
        tally.block("test:one", 4);
        tally.block("test:two", 1);
        tally.item("test:item");
        tally.item_block_link("test:item", "test:one");
        tally.block_entity_type("test:entity");
        tally.mob_effect("test:effect");
        tally.recipe_type("test:type");
        tally.recipe("test:recipe");
        tally.biome_feature_rule();

        assert_eq!(
            tally.take_summary("test"),
            Some(
                "Plugin test registered 2 blocks (5 states), 1 item, 1 item→block link, \
                 1 block entity type, 1 mob effect, 1 feature-to-biome rule, 1 recipe type, \
                 1 recipe."
                    .to_string()
            )
        );
    }

    /// The registries hand back an already-registered entry unchanged when the same
    /// definition is registered twice, so the summary must report the entry once.
    #[test]
    fn re_registering_the_same_entry_counts_once() {
        let mut tally = RegistrationTally::default();
        tally.item("test:item");
        tally.item("test:item");
        tally.block("test:block", 2);
        tally.block("test:block", 2);
        tally.item_block_link("test:item", "test:block");
        tally.item_block_link("test:item", "test:block");

        assert_eq!(
            tally.take_summary("test"),
            Some(
                "Plugin test registered 1 block (2 states), 1 item, 1 item→block link.".to_string()
            )
        );
    }

    /// A second summary must not report the same entries again.
    #[test]
    fn taking_the_summary_empties_the_tally() {
        let mut tally = RegistrationTally::default();
        tally.sound("test:sound");
        assert!(tally.take_summary("test").is_some());
        assert_eq!(tally.take_summary("test"), None);
    }

    #[test]
    fn plural_forms_are_used_for_counts_other_than_one() {
        let mut tally = RegistrationTally::default();
        tally.sound("test:a");
        tally.sound("test:b");
        assert_eq!(
            tally.take_summary("test"),
            Some("Plugin test registered 2 sounds.".to_string())
        );
    }

    #[test]
    fn placement_rules_report_the_blocks_and_the_rules() {
        let mut tally = RegistrationTally::default();
        tally.block_placement_rules("test:one", 3);
        tally.block_placement_rules("test:two", 1);
        assert_eq!(
            tally.take_summary("test"),
            Some("Plugin test registered 2 blocks with placement rules (4 rules).".to_string())
        );
    }
}
