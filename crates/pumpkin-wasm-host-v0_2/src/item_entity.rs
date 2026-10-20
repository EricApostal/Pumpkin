use super::{AccessorExt, LockIgnorePoison, run_blocking};
use crate::events::{from_wasm_position, to_wasm_position};
use crate::pumpkin::plugin::{
    item_stack::ItemStack as WitHostItemStack,
    world::{
        BoundingBox as WitBoundingBox, Entity, EntityFilter as WitEntityFilter,
        EntitySnapshot as WitEntitySnapshot, HostItemEntity, HostItemEntityWithStore,
        ItemEntity as WitItemEntity, ItemSummary as WitItemSummary,
    },
};
use crate::uuid::UuidExt;
use pumpkin_core::entity::{Entity as InternalEntity, EntityBase, item::ItemEntity};
use pumpkin_core::world::World;
use pumpkin_data::entity::EntityType;
use pumpkin_util::math::{boundingbox::BoundingBox, vector3::Vector3};
use pumpkin_wasm_host_common::state::PluginHostState;
use std::sync::{Arc, atomic::Ordering};
use tokio::sync::Mutex;
use wasmtime::component::{Accessor, HasSelf, Resource};

fn item_of(entity: &dyn EntityBase) -> wasmtime::Result<&ItemEntity> {
    entity
        .get_item_entity()
        .ok_or_else(|| wasmtime::Error::msg("entity is not an item entity"))
}

/// A [`WitEntityFilter`] with its entity types resolved.
enum Matcher {
    All,
    Items,
    Living,
    Players,
    Types(Vec<&'static EntityType>),
}

impl Matcher {
    fn new(filter: WitEntityFilter) -> wasmtime::Result<Self> {
        Ok(match filter {
            WitEntityFilter::All => Self::All,
            WitEntityFilter::Items => Self::Items,
            WitEntityFilter::Living => Self::Living,
            WitEntityFilter::Players => Self::Players,
            WitEntityFilter::Types(types) => Self::Types(
                types
                    .into_iter()
                    .map(super::entity::from_wit_entity_type)
                    .collect::<wasmtime::Result<_>>()?,
            ),
        })
    }

    const fn includes_players(&self) -> bool {
        !matches!(self, Self::Items)
    }

    const fn includes_non_players(&self) -> bool {
        !matches!(self, Self::Players)
    }

    fn matches(&self, entity: &dyn EntityBase) -> bool {
        match self {
            Self::All | Self::Players => true,
            Self::Items => entity.get_item_entity().is_some(),
            Self::Living => entity.get_living_entity().is_some(),
            Self::Types(types) => {
                let id = entity.get_entity().entity_type.id;
                types.iter().any(|t| t.id == id)
            }
        }
    }
}

/// The box between two corners given in any order.
const fn box_between(min: (f64, f64, f64), max: (f64, f64, f64)) -> BoundingBox {
    let (a, b) = (from_wasm_position(min), from_wasm_position(max));
    BoundingBox::new(
        Vector3::new(a.x.min(b.x), a.y.min(b.y), a.z.min(b.z)),
        Vector3::new(a.x.max(b.x), a.y.max(b.y), a.z.max(b.z)),
    )
}

fn in_box(entity: &dyn EntityBase, aabb: &BoundingBox) -> bool {
    let base = entity.get_entity();
    !base.removed.load(Ordering::SeqCst) && base.bounding_box.load().intersects(aabb)
}

/// The snapshot of an entity. It cannot fail: the type is carried as its registry key, which is
/// defined for vanilla and plugin types alike. Mapping it to the closed `entity-type` enum instead
/// would fail for an entity of a plugin type (a contraption next to the one being ticked) and fail
/// the whole `get-entities-in-box` call, see `snapshot_parts`.
fn snapshot(entity: &dyn EntityBase) -> WitEntitySnapshot {
    let base = entity.get_entity();
    let bb = base.bounding_box.load();
    let item = entity.get_item_entity().map(|item| {
        let stack = item.get_item_stack().lock_ignore_poison();
        WitItemSummary {
            registry_key: crate::item_stack::namespaced_key(stack.item.registry_key),
            count: stack.item_count,
        }
    });
    snapshot_parts(
        base.entity_id,
        &base.entity_uuid,
        base.entity_type,
        base.pos.load(),
        &bb,
        base.velocity.load(),
        item,
    )
}

/// Builds a snapshot from the state of one entity.
///
/// `get-entities-in-box` used to resolve the type with `to_wit_entity_type`, which only knows the
/// vanilla table in [`pumpkin_data::entity::EntityType::ALL`]: an entity of a plugin type (the
/// Create contraption entity itself, its seats, or the stabilized sub-contraptions of a bearing)
/// made the whole call return an error, which the wasm runtime reports as a trap and which poisons
/// the plugin store. The key is defined for both kinds of type.
fn snapshot_parts(
    id: i32,
    uuid: &uuid::Uuid,
    entity_type: &'static EntityType,
    position: Vector3<f64>,
    bounding_box: &BoundingBox,
    velocity: Vector3<f64>,
    item: Option<WitItemSummary>,
) -> WitEntitySnapshot {
    WitEntitySnapshot {
        id: id as u32,
        uuid: crate::pumpkin::plugin::uuid::Uuid::to_wit(uuid),
        entity_type_key: entity_type.resource_location().into_owned(),
        position: to_wasm_position(position),
        bounding_box: WitBoundingBox {
            min: to_wasm_position(bounding_box.min),
            max: to_wasm_position(bounding_box.max),
        },
        velocity: to_wasm_position(velocity),
        item,
    }
}

/// Snapshots of the entities matching `filter` that intersect the box, scanning the world's
/// entities once without cloning any of them.
pub(crate) fn entity_snapshots_in_box(
    world: &World,
    min: (f64, f64, f64),
    max: (f64, f64, f64),
    filter: WitEntityFilter,
) -> wasmtime::Result<Vec<WitEntitySnapshot>> {
    let aabb = box_between(min, max);
    let matcher = Matcher::new(filter)?;
    let mut result = Vec::new();
    if matcher.includes_non_players() {
        for entity in world.entities.load().iter() {
            if in_box(entity.as_ref(), &aabb) && matcher.matches(entity.as_ref()) {
                result.push(snapshot(entity.as_ref()));
            }
        }
    }
    if matcher.includes_players() {
        for player in world.players.load().iter() {
            let entity: &dyn EntityBase = player.as_ref();
            if in_box(entity, &aabb) && matcher.matches(entity) {
                result.push(snapshot(entity));
            }
        }
    }
    Ok(result)
}

/// The dropped items that intersect the box.
pub(crate) fn item_entities_in_box(
    state: &mut PluginHostState,
    world: &World,
    min: (f64, f64, f64),
    max: (f64, f64, f64),
) -> wasmtime::Result<Vec<Resource<WitItemEntity>>> {
    let aabb = box_between(min, max);
    let mut result = Vec::new();
    for entity in world.entities.load().iter() {
        if entity.get_item_entity().is_some() && in_box(entity.as_ref(), &aabb) {
            result.push(state.add(entity.clone())?);
        }
    }
    Ok(result)
}

/// Looks an entity up by uuid, players included.
pub(crate) fn entity_by_uuid(world: &World, id: uuid::Uuid) -> Option<Arc<dyn EntityBase>> {
    if let Some(entity) = world
        .entities
        .load()
        .iter()
        .find(|e| e.get_entity().entity_uuid == id)
    {
        return Some(entity.clone());
    }
    world
        .players
        .load()
        .iter()
        .find(|p| p.get_entity().entity_uuid == id)
        .map(|p| p.clone() as Arc<dyn EntityBase>)
}

/// Spawns a dropped item. `None` when the stack is empty or the spawn was cancelled.
pub(crate) fn spawn_item(
    world: &Arc<World>,
    pos: (f64, f64, f64),
    stack: pumpkin_data::item_stack::ItemStack,
    velocity: Option<(f64, f64, f64)>,
    pickup_delay: Option<u8>,
) -> Option<Arc<dyn EntityBase>> {
    if stack.is_empty() {
        return None;
    }
    let entity = InternalEntity::new(world.clone(), from_wasm_position(pos), &EntityType::ITEM);
    let item = if let Some(velocity) = velocity {
        ItemEntity::new_with_velocity(
            entity,
            stack,
            from_wasm_position(velocity),
            pickup_delay.unwrap_or(ItemEntity::DEFAULT_PICKUP_DELAY),
        )
    } else {
        let item = ItemEntity::new(entity, stack);
        if let Some(delay) = pickup_delay {
            item.set_pickup_delay(delay);
        }
        item
    };
    let item: Arc<dyn EntityBase> = Arc::new(item);
    world.spawn_entity(item.clone()).then_some(item)
}

impl HostItemEntity for PluginHostState {
    fn as_entity(&mut self, this: Resource<WitItemEntity>) -> wasmtime::Result<Resource<Entity>> {
        self.add(self.get(&this)?.clone())
    }

    fn get_stack(
        &mut self,
        this: Resource<WitItemEntity>,
    ) -> wasmtime::Result<Resource<WitHostItemStack>> {
        let stack = item_of(self.get(&this)?.as_ref())?
            .get_item_stack()
            .lock_ignore_poison()
            .clone();
        self.add(Arc::new(Mutex::new(stack)))
    }

    fn get_pickup_delay(&mut self, this: Resource<WitItemEntity>) -> wasmtime::Result<u8> {
        Ok(item_of(self.get(&this)?.as_ref())?.get_pickup_delay())
    }

    fn set_pickup_delay(
        &mut self,
        this: Resource<WitItemEntity>,
        ticks: u8,
    ) -> wasmtime::Result<()> {
        item_of(self.get(&this)?.as_ref())?.set_pickup_delay(ticks);
        Ok(())
    }

    fn is_never_pickup(&mut self, this: Resource<WitItemEntity>) -> wasmtime::Result<bool> {
        Ok(item_of(self.get(&this)?.as_ref())?.is_never_pickup())
    }

    fn set_never_pickup(
        &mut self,
        this: Resource<WitItemEntity>,
        never_pickup: bool,
    ) -> wasmtime::Result<()> {
        item_of(self.get(&this)?.as_ref())?.set_never_pickup(never_pickup);
        Ok(())
    }

    fn is_never_despawn(&mut self, this: Resource<WitItemEntity>) -> wasmtime::Result<bool> {
        Ok(item_of(self.get(&this)?.as_ref())?.is_never_despawn())
    }

    fn set_never_despawn(
        &mut self,
        this: Resource<WitItemEntity>,
        never_despawn: bool,
    ) -> wasmtime::Result<()> {
        item_of(self.get(&this)?.as_ref())?.set_never_despawn(never_despawn);
        Ok(())
    }

    fn discard(&mut self, this: Resource<WitItemEntity>) -> wasmtime::Result<()> {
        self.get(&this)?.get_entity().remove();
        Ok(())
    }

    fn drop(&mut self, rep: Resource<WitItemEntity>) -> wasmtime::Result<()> {
        self.drop(rep)
    }
}

impl HostItemEntityWithStore<PluginHostState> for HasSelf<PluginHostState> {
    async fn set_stack(
        accessor: &Accessor<PluginHostState, Self>,
        this: Resource<WitItemEntity>,
        stack: Resource<WitHostItemStack>,
    ) -> wasmtime::Result<()> {
        let entity = accessor.get_res(&this)?;
        let stack = accessor.get_res(&stack)?.lock().await.clone();
        run_blocking(accessor, move || {
            item_of(entity.as_ref()).map(|item| item.set_item_stack(stack))
        })
        .await?
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pumpkin_data::entity_registry::{
        PluginEntityCategory, PluginEntityDefinition, register_plugin_entity_type,
    };

    fn plugin_definition() -> PluginEntityDefinition {
        PluginEntityDefinition {
            category: PluginEntityCategory::Misc,
            width: 2.0,
            height: 2.0,
            eye_height: None,
            fire_immune: false,
            tracking_range: 10,
            update_interval: 3,
            track_deltas: true,
            summonable: false,
            saveable: true,
            pickable: false,
            collidable: true,
            pushable: false,
            ignore_riders_hit: false,
            default_synced_data: vec![],
            spawn_data_channel: None,
        }
    }

    fn box_of(min: (f64, f64, f64), max: (f64, f64, f64)) -> BoundingBox {
        BoundingBox::new(
            Vector3::new(min.0, min.1, min.2),
            Vector3::new(max.0, max.1, max.2),
        )
    }

    /// Regression: a Create contraption entity has a plugin type (`create:contraption`), which is
    /// not in `EntityType::ALL`. `get-entities-in-box` used to map the type through
    /// `to_wit_entity_type`, so the query of `ContraptionCollider.collideEntities` failed on the
    /// contraption's own entity every tick and poisoned the plugin store. The snapshot now carries
    /// the registry key, which is defined for a plugin type too.
    #[test]
    fn a_plugin_entity_type_gets_a_snapshot_key() {
        let plugin =
            register_plugin_entity_type("test_host:contraption", plugin_definition()).unwrap();

        // What the old code did: the vanilla table does not know the type, so the whole
        // `get-entities-in-box` call errored.
        assert!(
            super::super::entity::to_wit_entity_type(plugin.entity_type.resource_name).is_err()
        );

        let snapshot = snapshot_parts(
            7,
            &uuid::Uuid::new_v4(),
            plugin.entity_type,
            Vector3::new(1.0, 2.0, 3.0),
            &box_of((0.0, 0.0, 0.0), (2.0, 4.0, 4.0)),
            Vector3::new(0.0, 0.0, 0.0),
            None,
        );
        assert_eq!(snapshot.id, 7);
        assert_eq!(snapshot.entity_type_key, "test_host:contraption");
    }

    #[test]
    fn a_vanilla_entity_type_keeps_its_namespaced_key() {
        let snapshot = snapshot_parts(
            8,
            &uuid::Uuid::new_v4(),
            &EntityType::COW,
            Vector3::new(1.0, 2.0, 3.0),
            &box_of((0.0, 0.0, 0.0), (2.0, 4.0, 4.0)),
            Vector3::new(0.0, 0.0, 0.0),
            None,
        );
        assert_eq!(snapshot.entity_type_key, "minecraft:cow");
    }

    /// The box is only used for `intersects`, which is false for any comparison with a NaN corner,
    /// so a degenerate box yields no entities instead of a trap. The corners are swapped when
    /// needed.
    #[test]
    fn a_degenerate_box_selects_nothing() {
        let nan = f64::NAN;
        for (min, max) in [
            ((nan, 0.0, 0.0), (1.0, 1.0, 1.0)),
            ((0.0, 0.0, 0.0), (nan, nan, nan)),
            ((0.0, 0.0, 0.0), (0.0, 0.0, 0.0)),
            ((f64::INFINITY, 0.0, 0.0), (1.0, 1.0, 1.0)),
        ] {
            let aabb = box_between(min, max);
            let entity_box = box_of((0.0, 0.0, 0.0), (1.0, 1.0, 1.0));
            assert!(!entity_box.intersects(&aabb), "{min:?} {max:?}");
        }
        let swapped = box_between((2.0, 2.0, 2.0), (1.0, 1.0, 1.0));
        assert_eq!(swapped.min, Vector3::new(1.0, 1.0, 1.0));
        assert_eq!(swapped.max, Vector3::new(2.0, 2.0, 2.0));
    }

    /// A world with no blocks and no server: enough for `get-entities-in-box`, which only reads the
    /// entity list.
    fn test_world() -> Arc<World> {
        use pumpkin_config::world::LevelConfig;
        use pumpkin_core::block::registry::BlockRegistry;
        use pumpkin_data::dimension::Dimension;
        use pumpkin_world::level::Level;
        use pumpkin_world::world_info::LevelData;

        let root = std::env::temp_dir().join(format!("pumpkin-host-{}", uuid::Uuid::new_v4()));
        let level = Level::from_root_folder(&LevelConfig::default(), root, 0, Dimension::OVERWORLD);
        let info = Arc::new(arc_swap::ArcSwap::from_pointee(LevelData::default(
            pumpkin_util::world_seed::Seed(0),
        )));
        let world = Arc::new(World::load(
            level,
            info,
            Dimension::OVERWORLD,
            Arc::new(BlockRegistry::default()),
            std::sync::Weak::<pumpkin_core::server::Server>::new(),
        ));
        world
    }

    /// Regression, end to end: the contraption's own entity is in the queried box (the Dart side
    /// filters it out only after the snapshots are built), and its type is a plugin type. The query
    /// has to return it, not fail. Fails before the fix with
    /// `Unknown entity type: test_host:boxed_contraption`.
    #[tokio::test]
    async fn a_plugin_entity_in_the_box_is_snapshotted() {
        let plugin =
            register_plugin_entity_type("test_host:boxed_contraption", plugin_definition()).unwrap();
        let world = test_world();
        let entity = pumpkin_core::entity::r#type::from_type(
            plugin.entity_type,
            Vector3::new(1.0, 2.0, 3.0),
            &world,
            uuid::Uuid::new_v4(),
        );
        world.entities.store(Arc::new(vec![entity]));

        let snapshots =
            entity_snapshots_in_box(&world, (0.0, 0.0, 0.0), (4.0, 4.0, 4.0), WitEntityFilter::All)
                .expect("a plugin entity type must not fail the query");
        assert_eq!(snapshots.len(), 1);
        assert_eq!(snapshots[0].entity_type_key, "test_host:boxed_contraption");

        // A box away from it, and one with a NaN corner, select nothing instead of failing.
        assert!(
            entity_snapshots_in_box(
                &world,
                (10.0, 10.0, 10.0),
                (11.0, 11.0, 11.0),
                WitEntityFilter::All
            )
            .unwrap()
            .is_empty()
        );
        assert!(
            entity_snapshots_in_box(
                &world,
                (f64::NAN, 0.0, 0.0),
                (4.0, 4.0, 4.0),
                WitEntityFilter::All
            )
            .unwrap()
            .is_empty()
        );
    }
}
