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
fn box_between(min: (f64, f64, f64), max: (f64, f64, f64)) -> BoundingBox {
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

fn snapshot(entity: &dyn EntityBase) -> wasmtime::Result<WitEntitySnapshot> {
    let base = entity.get_entity();
    let bb = base.bounding_box.load();
    let item = entity.get_item_entity().map(|item| {
        let stack = item.get_item_stack().lock_ignore_poison();
        WitItemSummary {
            registry_key: stack.item.registry_key.to_string(),
            count: stack.item_count,
        }
    });
    Ok(WitEntitySnapshot {
        id: base.entity_id as u32,
        uuid: crate::pumpkin::plugin::uuid::Uuid::to_wit(&base.entity_uuid),
        entity_type: super::entity::to_wit_entity_type(base.entity_type.resource_name)?,
        position: to_wasm_position(base.pos.load()),
        bounding_box: WitBoundingBox {
            min: to_wasm_position(bb.min),
            max: to_wasm_position(bb.max),
        },
        velocity: to_wasm_position(base.velocity.load()),
        item,
    })
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
                result.push(snapshot(entity.as_ref())?);
            }
        }
    }
    if matcher.includes_players() {
        for player in world.players.load().iter() {
            let entity: &dyn EntityBase = player.as_ref();
            if in_box(entity, &aabb) && matcher.matches(entity) {
                result.push(snapshot(entity)?);
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
    let item = match velocity {
        Some(velocity) => ItemEntity::new_with_velocity(
            entity,
            stack,
            from_wasm_position(velocity),
            pickup_delay.unwrap_or(ItemEntity::DEFAULT_PICKUP_DELAY),
        ),
        None => {
            let item = ItemEntity::new(entity, stack);
            if let Some(delay) = pickup_delay {
                item.set_pickup_delay(delay);
            }
            item
        }
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
