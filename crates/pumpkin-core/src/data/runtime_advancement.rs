//! Advancements that are not part of the generated tree: the ones of datapacks
//! (`data/<namespace>/advancement/**/*.json`) and the ones plugins supply as JSON.
//!
//! The definitions are kept as data and turned into leaked [`Advancement`]s, so the rest of the
//! advancement code (progress, packets, saving) treats them like generated ones. The leaked values
//! are rebuilt lazily when a definition changed, unchanged definitions keep theirs.
//!
//! Criteria keep their `trigger` id and raw `conditions`. The host evaluates none of the vanilla
//! condition types: the plugin that owns a trigger decides when to fire it (see
//! [`criteria_for_trigger`]). The only condition the host reads is the optional string
//! `plugin_tag`, matched exactly against the matcher a plugin passes when firing. Triggers no plugin
//! registered are accepted and never fire.

use std::borrow::Cow;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::path::Path;
use std::sync::{LazyLock, Mutex, PoisonError, RwLock, RwLockReadGuard};

use pumpkin_data::advancement_data::{AdvancementDisplay, AdvancementReward, FrameType};
use pumpkin_data::data_component_impl::ItemModelImpl;
use pumpkin_data::item::Item;
use pumpkin_data::item_stack::ItemStack;
use pumpkin_data::{ADVANCEMENT_TREE, Advancement};
use pumpkin_util::identifier::Identifier;
use pumpkin_util::text::TextComponent;
use serde_json::Value;
use tracing::{debug, warn};

/// Most criteria one advancement can have.
const MAX_CRITERIA: usize = 256;
/// Deepest parent chain that is followed.
const MAX_DEPTH: usize = 64;

/// The display of a runtime advancement.
#[derive(Clone, Debug, PartialEq)]
struct DisplayDef {
    title: TextComponent,
    description: TextComponent,
    icon_item: String,
    icon_count: u8,
    icon_model: Option<String>,
    /// 0 task, 1 challenge, 2 goal.
    frame: u8,
    show_toast: bool,
    announce_to_chat: bool,
    hidden: bool,
    background: Option<String>,
}

/// A criterion with the trigger that fires it.
#[derive(Clone, Debug, PartialEq)]
struct CriterionDef {
    name: String,
    /// Full `namespace:path` trigger id.
    trigger: String,
    /// The raw conditions, never evaluated except `plugin_tag`.
    conditions: Value,
    plugin_tag: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
struct Def {
    /// Full `namespace:path` id.
    id: String,
    /// Full id of the parent.
    parent: Option<String>,
    display: Option<DisplayDef>,
    criteria: Vec<CriterionDef>,
    requirements: Vec<Vec<String>>,
    experience: i32,
    telemetry: bool,
}

struct Entry {
    def: Def,
    x: f32,
    y: f32,
    advancement: &'static Advancement,
}

/// An advancement, the criterion and its `plugin_tag`.
type TriggerEntry = (&'static Advancement, String, Option<String>);

#[derive(Default)]
struct Built {
    entries: HashMap<String, Entry>,
    /// Runtime children by full id of the parent (generated or runtime).
    children: HashMap<String, Vec<&'static Advancement>>,
    /// Criteria by trigger id: the advancement, the criterion and its `plugin_tag`.
    by_trigger: HashMap<String, Vec<TriggerEntry>>,
}

#[derive(Default)]
struct State {
    datapack: BTreeMap<String, Def>,
    plugin: BTreeMap<String, Def>,
    triggers: HashSet<String>,
    built: Built,
    dirty: bool,
    version: u64,
}

static STATE: LazyLock<RwLock<State>> = LazyLock::new(|| RwLock::new(State::default()));
static PENDING: Mutex<Vec<Def>> = Mutex::new(Vec::new());

fn leak_str(value: &str) -> &'static str {
    Box::leak(value.to_owned().into_boxed_str())
}

/// Turns `path` and `namespace:path` into the full id.
fn full_id(id: &str) -> String {
    if id.contains(':') {
        id.to_owned()
    } else {
        format!("minecraft:{id}")
    }
}

fn valid_id(id: &str) -> Result<String, String> {
    let full = full_id(id);
    Identifier::parse(&full)
        .map(|identifier| identifier.to_string())
        .map_err(|_| format!("invalid id '{id}'"))
}

fn in_generated_tree(id: &str) -> bool {
    Identifier::parse(id)
        .is_ok_and(|identifier| ADVANCEMENT_TREE.get_node_from_id(&identifier).is_some())
}

fn parse_text(value: &Value, what: &str) -> Result<TextComponent, String> {
    serde_json::from_value::<TextComponent>(value.clone())
        .map_err(|e| format!("invalid {what}: {e}"))
}

fn parse_display(display: &Value) -> Result<DisplayDef, String> {
    let object = display.as_object().ok_or("display must be an object")?;
    let title = parse_text(object.get("title").ok_or("display needs a title")?, "title")?;
    let description = object.get("description").map_or_else(
        || Ok(TextComponent::text("")),
        |v| parse_text(v, "description"),
    )?;
    let icon = object.get("icon");
    let (icon_item, icon_count, icon_model) = match icon {
        Some(Value::String(item)) => (valid_id(item)?, 1, None),
        Some(Value::Object(icon)) => {
            let item = icon
                .get("id")
                .or_else(|| icon.get("item"))
                .and_then(Value::as_str)
                .ok_or("icon needs an id")?;
            let count = icon
                .get("count")
                .and_then(Value::as_u64)
                .map_or(1, |c| c.clamp(1, 99) as u8);
            // Of the components only the item model is applied.
            let model = icon
                .get("components")
                .and_then(|c| {
                    c.get("minecraft:item_model")
                        .or_else(|| c.get("item_model"))
                })
                .and_then(Value::as_str)
                .map(ToOwned::to_owned);
            (valid_id(item)?, count, model)
        }
        _ => return Err("display needs an icon".to_owned()),
    };
    let frame = match object.get("frame").and_then(Value::as_str) {
        None | Some("task") => 0,
        Some("challenge") => 1,
        Some("goal") => 2,
        Some(other) => return Err(format!("unknown frame '{other}'")),
    };
    let flag =
        |key: &str, default: bool| object.get(key).and_then(Value::as_bool).unwrap_or(default);
    Ok(DisplayDef {
        title,
        description,
        icon_item,
        icon_count,
        icon_model,
        frame,
        show_toast: flag("show_toast", true),
        announce_to_chat: flag("announce_to_chat", true),
        hidden: flag("hidden", false),
        background: object
            .get("background")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned),
    })
}

/// Parses an advancement in the datapack JSON format.
fn parse_def(id: &str, json: &Value) -> Result<Def, String> {
    let id = valid_id(id)?;
    let object = json.as_object().ok_or("an advancement must be an object")?;
    let parent = match object.get("parent") {
        None | Some(Value::Null) => None,
        Some(Value::String(parent)) => Some(valid_id(parent)?),
        Some(_) => return Err("parent must be a string".to_owned()),
    };
    let criteria_json = object
        .get("criteria")
        .and_then(Value::as_object)
        .ok_or("an advancement needs criteria")?;
    if criteria_json.len() > MAX_CRITERIA {
        return Err("too many criteria".to_owned());
    }
    let mut criteria = Vec::with_capacity(criteria_json.len());
    for (name, criterion) in criteria_json {
        let trigger = criterion
            .get("trigger")
            .and_then(Value::as_str)
            .ok_or_else(|| format!("criterion '{name}' needs a trigger"))?;
        let conditions = criterion.get("conditions").cloned().unwrap_or(Value::Null);
        let plugin_tag = conditions
            .get("plugin_tag")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned);
        criteria.push(CriterionDef {
            name: name.clone(),
            trigger: valid_id(trigger)?,
            conditions,
            plugin_tag,
        });
    }
    let requirements: Vec<Vec<String>> = match object.get("requirements") {
        None | Some(Value::Null) => criteria.iter().map(|c| vec![c.name.clone()]).collect(),
        Some(Value::Array(groups)) => groups
            .iter()
            .map(|group| {
                group
                    .as_array()
                    .ok_or("requirements must be arrays of criteria")?
                    .iter()
                    .map(|name| {
                        let name = name.as_str().ok_or("requirement names must be strings")?;
                        if criteria.iter().any(|c| c.name == name) {
                            Ok(name.to_owned())
                        } else {
                            Err("requirements name an unknown criterion")
                        }
                    })
                    .collect::<Result<Vec<_>, _>>()
            })
            .collect::<Result<_, _>>()?,
        Some(_) => return Err("requirements must be an array".to_owned()),
    };
    let display = object.get("display").map(parse_display).transpose()?;
    let experience = object
        .get("rewards")
        .and_then(|r| r.get("experience"))
        .and_then(Value::as_i64)
        .map_or(0, |e| e.clamp(0, i64::from(i32::MAX)) as i32);
    Ok(Def {
        id,
        parent,
        display,
        criteria,
        requirements,
        experience,
        telemetry: object
            .get("sends_telemetry_event")
            .and_then(Value::as_bool)
            .unwrap_or(false),
    })
}

fn resolve_item(id: &str) -> &'static Item {
    Item::from_registry_key(id).unwrap_or_else(|| {
        warn!("Advancement icon {id} is not an item, using a barrier");
        &Item::BARRIER
    })
}

fn make_advancement(def: &Def, x: f32, y: f32) -> &'static Advancement {
    let display = def.display.as_ref().map(|d| {
        let mut stack = ItemStack::new(d.icon_count, resolve_item(&d.icon_item));
        if let Some(model) = &d.icon_model {
            stack.set_data_component(ItemModelImpl {
                id: Cow::Owned(model.clone()),
            });
        }
        let frame = match d.frame {
            1 => FrameType::Challenge,
            2 => FrameType::Goal,
            _ => FrameType::Task,
        };
        let mut display = AdvancementDisplay::new(
            "",
            "",
            stack,
            frame,
            d.background.as_deref().map(leak_str),
            d.show_toast,
            d.hidden,
            d.announce_to_chat,
            x,
            y,
        );
        display.title_text = Some(Box::leak(Box::new(d.title.clone())));
        display.description_text = Some(Box::leak(Box::new(d.description.clone())));
        &*Box::leak(Box::new(display))
    });
    let requirements: Vec<&'static [&'static str]> = def
        .requirements
        .iter()
        .map(|group| {
            let names: Vec<&'static str> = group.iter().map(|n| leak_str(n)).collect();
            &*Box::leak(names.into_boxed_slice())
        })
        .collect();
    let criteria: Vec<&'static str> = def.criteria.iter().map(|c| leak_str(&c.name)).collect();
    // The ids were validated when the definition was parsed.
    let id = Identifier::parse(&def.id)
        .unwrap_or_else(|_| Identifier::from_static("minecraft", "invalid"));
    let parent = def
        .parent
        .as_deref()
        .and_then(|parent| Identifier::parse(parent).ok());
    Box::leak(Box::new(Advancement {
        id,
        parent,
        send_telemetry: def.telemetry,
        display,
        reward: Box::leak(Box::new(AdvancementReward {
            experience: def.experience,
            recipes: &[],
        })),
        requirements: Box::leak(requirements.into_boxed_slice()),
        criteria: Box::leak(criteria.into_boxed_slice()),
    }))
}

/// Places the subtree of `id`, returns its row.
fn place(
    id: &str,
    depth: f32,
    cursor: &mut f32,
    children: &HashMap<&str, Vec<&str>>,
    out: &mut HashMap<String, (f32, f32)>,
) -> f32 {
    let row = match children.get(id) {
        Some(kids) if !kids.is_empty() => {
            let rows: Vec<f32> = kids
                .iter()
                .map(|kid| place(kid, depth + 1.0, cursor, children, out))
                .collect();
            f32::midpoint(rows[0], rows[rows.len() - 1])
        }
        _ => {
            let row = *cursor;
            *cursor += 1.0;
            row
        }
    };
    out.insert(id.to_owned(), (depth, row));
    row
}

fn rebuild(state: &mut State) {
    let mut merged: BTreeMap<&str, &Def> = BTreeMap::new();
    for (id, def) in state.datapack.iter().chain(state.plugin.iter()) {
        if in_generated_tree(id) {
            debug!("Advancement {id} replaces a generated one, skipped");
            continue;
        }
        merged.insert(id, def);
    }

    // Drop those whose parent chain does not end at a root.
    let resolves = |start: &str| -> bool {
        let mut current = start;
        for _ in 0..MAX_DEPTH {
            let Some(def) = merged.get(current) else {
                return in_generated_tree(current);
            };
            match &def.parent {
                None => return true,
                Some(parent) => current = parent,
            }
        }
        false
    };
    let valid: Vec<&str> = merged
        .keys()
        .copied()
        .filter(|id| {
            let ok = resolves(id);
            if !ok {
                warn!("Advancement {id} has no valid parent chain, skipped");
            }
            ok
        })
        .collect();
    let valid_set: HashSet<&str> = valid.iter().copied().collect();

    // Layout: runtime subtrees below a generated parent continue its row and column.
    let mut kids: HashMap<&str, Vec<&str>> = HashMap::new();
    let mut tops: Vec<&str> = Vec::new();
    for id in &valid {
        match merged[id].parent.as_deref() {
            Some(parent) if valid_set.contains(parent) => kids.entry(parent).or_default().push(id),
            _ => tops.push(id),
        }
    }
    let mut positions: HashMap<String, (f32, f32)> = HashMap::new();
    let mut below_parent: HashMap<&str, f32> = HashMap::new();
    for top in &tops {
        let (depth, mut cursor) = merged[top]
            .parent
            .as_deref()
            .and_then(|parent| {
                let identifier = Identifier::parse(parent).ok()?;
                let node = ADVANCEMENT_TREE.get_node_from_id(&identifier)?;
                let display = node.value.display?;
                let used = below_parent.get(parent).copied().unwrap_or(0.0);
                Some((display.x + 1.0, display.y + used))
            })
            .unwrap_or((0.0, 0.0));
        let start = cursor;
        place(top, depth, &mut cursor, &kids, &mut positions);
        if let Some(parent) = merged[top].parent.as_deref() {
            *below_parent.entry(parent).or_insert(0.0) += cursor - start;
        }
    }

    let mut old = std::mem::take(&mut state.built.entries);
    let mut built = Built::default();
    for id in valid {
        let def = merged[id];
        let (x, y) = positions.get(id).copied().unwrap_or((0.0, 0.0));
        let advancement = match old.remove(id) {
            Some(entry) if entry.def == *def && entry.x == x && entry.y == y => entry.advancement,
            _ => make_advancement(def, x, y),
        };
        if let Some(parent) = &def.parent {
            built
                .children
                .entry(parent.clone())
                .or_default()
                .push(advancement);
        }
        for criterion in &def.criteria {
            built
                .by_trigger
                .entry(criterion.trigger.clone())
                .or_default()
                .push((
                    advancement,
                    criterion.name.clone(),
                    criterion.plugin_tag.clone(),
                ));
        }
        built.entries.insert(
            id.to_owned(),
            Entry {
                def: def.clone(),
                x,
                y,
                advancement,
            },
        );
    }
    state.built = built;
    state.dirty = false;
    state.version += 1;
}

/// The state with the built tables up to date.
fn fresh() -> RwLockReadGuard<'static, State> {
    {
        let guard = STATE.read().unwrap_or_else(PoisonError::into_inner);
        if !guard.dirty {
            return guard;
        }
    }
    {
        let mut guard = STATE.write().unwrap_or_else(PoisonError::into_inner);
        if guard.dirty {
            rebuild(&mut guard);
        }
    }
    STATE.read().unwrap_or_else(PoisonError::into_inner)
}

fn mutate(f: impl FnOnce(&mut State)) {
    let mut guard = STATE.write().unwrap_or_else(PoisonError::into_inner);
    f(&mut guard);
    guard.dirty = true;
}

/// Increases whenever the set of runtime advancements changed, players resend their tree then.
#[must_use]
pub fn version() -> u64 {
    fresh().version
}

/// Registers a trigger id plugins fire with `Player::trigger_criterion`.
pub fn register_trigger(key: &str) -> Result<(), String> {
    let key = valid_id(key)?;
    STATE
        .write()
        .unwrap_or_else(PoisonError::into_inner)
        .triggers
        .insert(key);
    Ok(())
}

#[must_use]
pub fn is_trigger_registered(key: &str) -> bool {
    STATE
        .read()
        .unwrap_or_else(PoisonError::into_inner)
        .triggers
        .contains(&full_id(key))
}

/// Adds or replaces an advancement of a plugin, given in the datapack JSON format. They stay
/// through datapack reloads.
pub fn add_plugin_advancement(id: &str, json: &str) -> Result<(), String> {
    let value: Value = serde_json::from_str(json).map_err(|e| format!("invalid JSON: {e}"))?;
    let def = parse_def(id, &value)?;
    for criterion in &def.criteria {
        if !is_trigger_registered(&criterion.trigger)
            && !criterion.trigger.starts_with("minecraft:")
        {
            debug!(
                "Advancement {} uses the unregistered trigger {}",
                def.id, criterion.trigger
            );
        }
    }
    mutate(|state| {
        state.plugin.insert(def.id.clone(), def);
    });
    Ok(())
}

/// Removes an advancement a plugin added.
#[must_use]
pub fn remove_plugin_advancement(id: &str) -> bool {
    let Ok(id) = valid_id(id) else {
        return false;
    };
    let mut removed = false;
    mutate(|state| removed = state.plugin.remove(&id).is_some());
    removed
}

fn visit_json(dir: &Path, prefix: &str, f: &mut impl FnMut(String, &Path)) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        if path.is_dir() {
            visit_json(&path, &format!("{prefix}{name}/"), f);
        } else if let Some(stem) = name.strip_suffix(".json") {
            f(format!("{prefix}{stem}"), &path);
        }
    }
}

/// Starts a datapack reload, forgets what the previous pass read.
pub fn clear() {
    PENDING
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clear();
}

/// Reads the advancements of one namespace directory of a datapack.
pub fn load_namespace(namespace: &str, ns_path: &Path) {
    let mut pending = PENDING.lock().unwrap_or_else(PoisonError::into_inner);
    for sub in ["advancement", "advancements"] {
        visit_json(&ns_path.join(sub), "", &mut |path, file| {
            let id = format!("{namespace}:{path}");
            let result = fs::read_to_string(file)
                .map_err(|e| e.to_string())
                .and_then(|text| serde_json::from_str::<Value>(&text).map_err(|e| e.to_string()))
                .and_then(|json| parse_def(&id, &json));
            match result {
                Ok(def) => pending.push(def),
                Err(error) => warn!("Invalid advancement {id}: {error}"),
            }
        });
    }
}

/// Replaces the advancements of the datapacks with what was read since [`clear`].
pub fn finish() {
    let pending = std::mem::take(&mut *PENDING.lock().unwrap_or_else(PoisonError::into_inner));
    mutate(|state| {
        state.datapack = pending
            .into_iter()
            .map(|def| (def.id.clone(), def))
            .collect();
    });
}

/// Finds an advancement by id: generated ones with or without the `minecraft:` prefix, runtime ones
/// by their full id.
#[must_use]
pub fn find(name: &str) -> Option<&'static Advancement> {
    Advancement::from_name(name)
        .or_else(|| Advancement::from_minecraft_name(name))
        .or_else(|| {
            name.strip_prefix("minecraft:")
                .and_then(Advancement::from_name)
        })
        .or_else(|| find_runtime(name))
}

/// Finds a runtime advancement by id.
#[must_use]
pub fn find_runtime(name: &str) -> Option<&'static Advancement> {
    fresh()
        .built
        .entries
        .get(&full_id(name))
        .map(|e| e.advancement)
}

/// Whether the advancement is part of the generated tree.
#[must_use]
pub fn is_generated(advancement: &Advancement) -> bool {
    ADVANCEMENT_TREE.get_node_from_id(&advancement.id).is_some()
}

#[must_use]
pub fn is_runtime(advancement: &Advancement) -> bool {
    fresh()
        .built
        .entries
        .contains_key(&advancement.id.to_string())
}

/// All runtime advancements.
#[must_use]
pub fn all() -> Vec<&'static Advancement> {
    let guard = fresh();
    let mut list: Vec<_> = guard
        .built
        .entries
        .values()
        .map(|e| e.advancement)
        .collect();
    list.sort_by_key(|a| a.id.to_string());
    list
}

/// The runtime advancements without a parent, the tabs of plugins and datapacks.
#[must_use]
pub fn roots() -> Vec<&'static Advancement> {
    all().into_iter().filter(|a| a.parent.is_none()).collect()
}

/// The parent of an advancement, generated or runtime.
#[must_use]
pub fn parent_of(advancement: &Advancement) -> Option<&'static Advancement> {
    let parent = advancement.parent.as_ref()?;
    if let Some(node) = ADVANCEMENT_TREE.get_node_from_id(parent) {
        return Some(node.value);
    }
    find_runtime(&parent.to_string())
}

/// The children of an advancement: the generated ones and the runtime ones.
#[must_use]
pub fn children_of(advancement: &Advancement) -> Vec<&'static Advancement> {
    let mut list: Vec<&'static Advancement> = ADVANCEMENT_TREE
        .get_node_from_id(&advancement.id)
        .map(|node| {
            node.children
                .iter()
                .map(|child| ADVANCEMENT_TREE.nodes_vector[*child].value)
                .collect()
        })
        .unwrap_or_default();
    if let Some(runtime) = fresh().built.children.get(&advancement.id.to_string()) {
        list.extend(runtime.iter().copied());
    }
    list
}

/// The root of the tree an advancement is in.
#[must_use]
pub fn root_of(advancement: &'static Advancement) -> &'static Advancement {
    let mut current = advancement;
    for _ in 0..MAX_DEPTH {
        match parent_of(current) {
            Some(parent) => current = parent,
            None => break,
        }
    }
    current
}

/// The criteria that fire for a trigger: the advancement and the criterion name. A criterion with
/// a `plugin_tag` condition only fires when `matcher` equals it, one without fires for any matcher.
#[must_use]
pub fn criteria_for_trigger(
    trigger: &str,
    matcher: Option<&str>,
) -> Vec<(&'static Advancement, String)> {
    fresh()
        .built
        .by_trigger
        .get(&full_id(trigger))
        .map(|list| {
            list.iter()
                .filter(|(_, _, tag)| tag.as_deref().is_none_or(|tag| Some(tag) == matcher))
                .map(|(advancement, criterion, _)| (*advancement, criterion.clone()))
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn sample(parent: Option<&str>, tag: Option<&str>) -> Value {
        let mut conditions = json!({ "items": [{ "items": "create:andesite_alloy" }] });
        if let Some(tag) = tag {
            conditions["plugin_tag"] = json!(tag);
        }
        let mut value = json!({
            "criteria": { "0": { "trigger": "create:test_trigger", "conditions": conditions } },
            "display": {
                "announce_to_chat": false,
                "background": "create:textures/gui/advancements.png",
                "description": { "color": "#DBA213", "translate": "advancement.create.root.desc" },
                "icon": { "count": 1, "id": "minecraft:stone" },
                "title": { "translate": "advancement.create.root" }
            },
            "requirements": [["0"]],
            "sends_telemetry_event": true,
            "rewards": { "experience": 5 }
        });
        if let Some(parent) = parent {
            value["parent"] = json!(parent);
        }
        value
    }

    #[test]
    fn parses_a_create_advancement() {
        let def = parse_def("rt_test:root", &sample(None, Some("x"))).unwrap();
        assert_eq!(def.id, "rt_test:root");
        assert_eq!(def.criteria[0].trigger, "create:test_trigger");
        assert_eq!(def.criteria[0].plugin_tag.as_deref(), Some("x"));
        assert_eq!(def.experience, 5);
        assert!(def.display.is_some());
    }

    #[test]
    fn rejects_bad_requirements() {
        let mut json = sample(None, None);
        json["requirements"] = json!([["nope"]]);
        assert!(parse_def("rt_test:bad", &json).is_err());
    }

    #[test]
    fn builds_tree_and_matches_triggers() {
        add_plugin_advancement("rt_test2:root", &sample(None, None).to_string()).unwrap();
        add_plugin_advancement(
            "rt_test2:child",
            &sample(Some("rt_test2:root"), Some("tag")).to_string(),
        )
        .unwrap();
        add_plugin_advancement(
            "rt_test2:orphan",
            &sample(Some("rt_test2:missing"), None).to_string(),
        )
        .unwrap();
        let root = find("rt_test2:root").unwrap();
        let child = find("rt_test2:child").unwrap();
        assert!(find("rt_test2:orphan").is_none());
        assert!(is_runtime(root));
        assert_eq!(root_of(child).id, root.id);
        assert_eq!(children_of(root).len(), 1);
        assert_eq!(child.display.unwrap().x, 1.0);

        let hits = criteria_for_trigger("create:test_trigger", Some("tag"));
        assert!(hits.iter().any(|(a, _)| a.id == child.id));
        let hits = criteria_for_trigger("create:test_trigger", Some("other"));
        assert!(!hits.iter().any(|(a, _)| a.id == child.id));
        assert!(hits.iter().any(|(a, _)| a.id == root.id));
        assert!(remove_plugin_advancement("rt_test2:child"));
        assert!(find("rt_test2:child").is_none());
    }
}
