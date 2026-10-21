//! What a **relogin** is told about the light around a plugin block.
//!
//! A client gets light in exactly two ways: the light arrays inside `LEVEL_CHUNK_WITH_LIGHT`
//! (`pumpkin-core/src/net/java/chunk_sender.rs:299`, `pumpkin-core/src/net/java/mod.rs:414`) and
//! its own relight of the cells a block update touches. `LIGHT_UPDATE` is never sent
//! (`CLightUpdate::from_chunk` is only reachable through the chunk packet), so a client that
//! was *present* when the world changed keeps whatever it computed itself, while the same world
//! seen again after a relogin is whatever the chunk packet carries.
//!
//! That makes "it looks right when I place it and dark when I relog" a statement about the chunk
//! payload, not about rendering. This test reads the payload the way the sender builds it
//! (`CLightUpdate::from_chunk`, the same call the chunk sender makes) for the same chunk before
//! and after a plugin block is placed, and asserts the placed block does not lose its light.
//!
//! Run from the repository root (`./plugins` is cwd-relative):
//!
//! ```text
//! cargo test -p pumpkin-tester --test create_belt_lighting
//! ```

use std::sync::Arc;
use std::time::Duration;

use pumpkin_core::net::java::ChunkLightExt;
use pumpkin_core::server::Server;
use pumpkin_data::Block;
use pumpkin_data::packet::CURRENT_MC_VERSION;
use pumpkin_protocol::java::client::play::{CLightUpdate, LightData};
use pumpkin_tester::{harness, scenario::Scenario};
use pumpkin_util::math::position::BlockPos;
use pumpkin_util::math::vector2::Vector2;

/// The light the packet carries for one cell of one section.
///
/// A section in the non-empty mask has its own array; a section in the empty mask is documented
/// as *all zeros* (`Java Edition protocol/Data Types`, "Empty Sky Light Mask"), which is what
/// `ClientPacketListener.readSectionList` stores (`new DataLayer()`); a section in neither mask
/// keeps what the client has.
fn sky_light_at(data: &LightData, section_index: usize, lx: usize, ly: usize, lz: usize) -> u8 {
    let bit = section_index + 1;
    if data.empty_sky_light_mask.get_bit(bit) {
        return 0;
    }
    assert!(
        data.sky_light_mask.get_bit(bit),
        "section {section_index} is in neither sky light mask, the client keeps its own light"
    );
    let array_index = (0..bit).filter(|i| data.sky_light_mask.get_bit(*i)).count();
    let array = &data.sky_light_arrays[array_index];
    let index = ly * 256 + lz * 16 + lx;
    let byte = array[index >> 1];
    if index & 1 == 0 { byte & 0x0F } else { byte >> 4 }
}

struct Running {
    server: Arc<Server>,
    _temp: tempfile::TempDir,
}

impl Running {
    async fn boot() -> Self {
        // `PLUGIN_DIR` is the cwd-relative `./plugins`, and cargo runs a test binary from its
        // *package* directory, so the workspace root has to be established explicitly.
        let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let root = manifest
            .parent()
            .and_then(std::path::Path::parent)
            .expect("workspace root");
        std::env::set_current_dir(root).expect("cwd is the workspace root");

        let temp = tempfile::tempdir().expect("temp world dir");
        let server = harness::create_test_server_with_plugin(&temp)
            .await
            .expect("plugin server boots");
        Self {
            server,
            _temp: temp,
        }
    }

    /// Dispatches console commands, ticks, and lets the async chunk work land.
    async fn run(&self, scenario: Scenario) {
        scenario.run_with_ticks(&self.server, scenario.settle_ticks);
        tokio::time::sleep(Duration::from_millis(300)).await;
    }

    fn world(&self) -> Arc<pumpkin_core::world::World> {
        self.server
            .worlds
            .load()
            .first()
            .cloned()
            .expect("overworld loaded")
    }

    /// The chunk payload's light data for the chunk holding `pos`, built by the same call the
    /// chunk sender uses.
    fn light_payload(&self, pos: BlockPos) -> LightData {
        let world = self.world();
        let chunk_pos = Vector2::new(pos.0.x >> 4, pos.0.z >> 4);
        let chunk = world
            .level
            .loaded_chunks
            .get(&chunk_pos)
            .unwrap_or_else(|| panic!("chunk {chunk_pos:?} is loaded"));
        CLightUpdate::from_chunk(&chunk, CURRENT_MC_VERSION)
            .expect("light data builds")
            .light_data
    }

    fn section_index(&self, y: i32) -> usize {
        let world = self.world();
        ((y - world.min_y) >> 4) as usize
    }
}

/// The block the user places, above the terrain so its column has nothing but sky over it.
const PROBE: BlockPos = BlockPos::new(8, 120, 8);
/// A block the plugin registers as non-occluding, kept as the control.
const CONTROL: BlockPos = BlockPos::new(12, 120, 12);

fn sky_light_at_pos(data: &LightData, running: &Running, pos: BlockPos) -> u8 {
    sky_light_at(
        data,
        running.section_index(pos.0.y),
        (pos.0.x & 15) as usize,
        (pos.0.y & 15) as usize,
        (pos.0.z & 15) as usize,
    )
}

/// **A placed block must not be darker on the next relogin than it was when it was placed.**
///
/// `create:belt` is a non-full block in Create — `BeltBlock.getShape`/`getCollisionShape` are
/// `BeltShapes`, and vanilla derives `getLightDampening() == 0` from that shape — so a client
/// that places one sees it lit. The light arrays of the chunk it lives in must say the same.
#[tokio::test(flavor = "multi_thread")]
async fn a_placed_belt_keeps_the_light_a_client_relighting_it_would_have() {
    let running = Running::boot().await;

    // The registered states are what the host's light engines read: `opacity` is vanilla's
    // `getLightDampening`, `is_solid_render` is vanilla's `isSolidRender`.
    let belt = Block::from_name("create:belt").expect("the plugin registered create:belt");
    let pipe = Block::from_name("create:fluid_pipe").expect("the plugin registered create:fluid_pipe");
    let belt_state = belt.default_state;
    let pipe_state = pipe.default_state;
    assert!(
        !belt_state.is_solid_render(),
        "create:belt is not a full cube in Java (`BeltShapes`), so it is not a solid render: \
         opacity={} solid_render={} full_cube={}",
        belt_state.opacity,
        belt_state.is_solid_render(),
        belt_state.is_full_cube(),
    );
    assert!(
        belt_state.opacity == pipe_state.opacity,
        "create:belt registered opacity {} where the known-good non-occluding plugin block \
         create:fluid_pipe registered {}",
        belt_state.opacity,
        pipe_state.opacity,
    );

    running
        .run(
            Scenario::new("load the probe chunks")
                .command("forceload add 0 0 0 0")
                .settle(60),
        )
        .await;

    // What a client joining now would be told: the chunk as generated, nothing placed yet.
    let before = running.light_payload(PROBE);
    let before_probe = sky_light_at_pos(&before, &running, PROBE);
    assert_eq!(
        before_probe, 15,
        "the probe column is open sky, the chunk payload must say sky light 15 before anything \
         is placed"
    );

    running
        .run(
            Scenario::new("place a conveyor and a pipe")
                .command("setblock 8 120 8 create:belt")
                .command("setblock 12 120 12 create:fluid_pipe")
                .settle(40),
        )
        .await;

    // The blocks really landed, or the rest of the test proves nothing.
    for (pos, block, name) in [(PROBE, belt, "belt"), (CONTROL, pipe, "pipe")] {
        let placed = running.world().get_block_state(&pos);
        assert_eq!(
            placed.id.to_block_id(),
            block.id,
            "{name} was not placed at {pos:?}, found block {:?}",
            placed.id.to_block_id()
        );
    }

    // What a client **relogging** is told: the same chunk after the change.
    let after = running.light_payload(PROBE);
    let after_probe = sky_light_at_pos(&after, &running, PROBE);
    let after_control = sky_light_at_pos(&after, &running, CONTROL);
    eprintln!(
        "create:belt registered opacity {} (can_occlude {}, solid_render {}, full_cube {}), \
         create:fluid_pipe opacity {}; chunk light for the belt cell: {before_probe} before it \
         was placed, {after_probe} after; control cell {after_control}",
        belt_state.opacity,
        belt_state.can_occlude(),
        belt_state.is_solid_render(),
        belt_state.is_full_cube(),
        pipe_state.opacity,
    );

    assert_eq!(
        after_control, 15,
        "create:fluid_pipe is a non-occluding plugin block; it must leave its own cell at sky \
         light 15"
    );
    assert!(
        after_probe >= 13,
        "a relogin is sent sky light {after_probe} at the conveyor at {PROBE:?}, while the same \
         chunk was sent sky light {before_probe} before the conveyor was placed: the block \
         swallows the light of its own cell (registered opacity {}, can_occlude {}, \
         solid_render {}), so the conveyor renders dark only after a relog",
        belt_state.opacity,
        belt_state.can_occlude(),
        belt_state.is_solid_render(),
    );
}
