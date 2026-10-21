use arc_swap::ArcSwap;
use futures::future::join_all;
use loader::{LoaderError, PluginLoader, native::NativePluginLoader};
use notify::{EventKind, RecursiveMode, Watcher, event::ModifyKind};
use std::{
    any::Any,
    cell::Cell,
    collections::{HashMap, HashSet, VecDeque},
    path::{Path, PathBuf},
    pin::Pin,
    sync::{Arc, Mutex, OnceLock, RwLock as SyncRwLock, atomic::AtomicBool},
    thread::ThreadId,
    time::Duration,
};
use thiserror::Error;
use tokio::{
    sync::{Notify, RwLock},
    task::JoinHandle,
};
use tracing::{debug, error, info, warn};

pub mod api;
pub mod cache;
pub mod loader;
/// Constants for plugin permissions.
///
/// Plugins can request these permissions in their metadata to access specific
/// host features.
pub mod perf;
pub mod permissions;

use crate::{LOGGER_IMPL, server::Server};
pub use api::*;

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Bump this whenever the public plugin API or any event layout changes in a way
/// that makes old binary plugins incompatible.
pub const PLUGIN_API_VERSION: u32 = 2;

const PLUGIN_DIR: &str = "./plugins";

/// Probe the plugin runtime installs to tell whether the current thread runs a plugin guest call.
static GUEST_CALL_PROBE: OnceLock<fn() -> bool> = OnceLock::new();

/// A plugin event whose dispatch had to wait for the guest call that fired it to return.
struct DeferredEvent {
    server: Arc<Server>,
    event_name: &'static str,
    event: Box<dyn Payload + Send + Sync>,
}

/// Events fired from inside a guest call, oldest first, waiting for a dispatch outside one.
static DEFERRED_EVENTS: Mutex<VecDeque<DeferredEvent>> = Mutex::new(VecDeque::new());

std::thread_local! {
    /// Set while this thread dispatches deferred events, so that dispatch does not recurse.
    static DISPATCHING_DEFERRED: Cell<bool> = const { Cell::new(false) };
}

/// Registers the probe that reports whether the current thread is running a plugin guest call.
///
/// The plugin runtime calls this once at startup. Without it a dispatch inside a guest call would
/// always be attempted, which deadlocks the plugin it re-enters.
pub fn install_guest_call_probe(probe: fn() -> bool) {
    let _ = GUEST_CALL_PROBE.set(probe);
}

/// Whether a plugin guest call is running on this thread right now.
fn in_guest_call() -> bool {
    GUEST_CALL_PROBE.get().is_some_and(|probe| probe())
}

/// Dispatches the plugin events that were deferred out of guest calls, oldest first.
///
/// Host code that fires an event outside a guest call is the point where a plugin can be
/// re-entered, so it delivers what the guest calls had to postpone.
fn dispatch_deferred_events() {
    if DISPATCHING_DEFERRED.with(|dispatching| dispatching.replace(true)) {
        return;
    }
    let _guard = DeferredDispatchGuard;

    loop {
        let deferred = DEFERRED_EVENTS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .pop_front();
        let Some(mut deferred) = deferred else {
            return;
        };
        deferred.server.plugin_manager.fire_blocking_dyn(
            &deferred.server,
            deferred.event_name,
            &mut *deferred.event,
        );
    }
}

/// Dispatches the events deferred out of guest calls, oldest first, at the end of the guest call
/// that queued them.
///
/// The plugin runtime installs this as its guest-call-return hook
/// (`pumpkin_plugin_runtime::install_guest_call_return_hook`): it calls it once the outermost
/// guest call of a chain has returned, which is the earliest moment the queue can be dispatched
/// without re-entering a plugin from inside its own call. Until then the queue waits for the next
/// host dispatch outside a guest call — in practice the next server tick, so a plugin-initiated
/// write of N blocks pays a tick per block for callbacks that do microseconds of work.
pub fn drain_deferred_events_after_guest_call() {
    dispatch_deferred_events();
}

/// Clears [`DISPATCHING_DEFERRED`] when the dispatch that set it leaves, even on a panic.
struct DeferredDispatchGuard;

impl Drop for DeferredDispatchGuard {
    fn drop(&mut self) {
        DISPATCHING_DEFERRED.with(|dispatching| dispatching.set(false));
    }
}

/// A trait for handling events dynamically.
///
/// This trait allows for handling events of any type that implements the `Event` trait.
pub trait DynEventHandler: Send + Sync {
    /// Asynchronously handles a dynamic event.
    ///
    /// # Arguments
    /// - `event`: A reference to the event to handle.
    fn handle_dyn<'a>(
        &'a self,
        _server: &'a Arc<Server>,
        event: &'a (dyn Payload + Send + Sync),
    ) -> BoxFuture<'a, ()>;

    /// Asynchronously handles a blocking dynamic event.
    ///
    /// # Arguments
    /// - `event`: A mutable reference to the event to handle.
    fn handle_blocking_dyn<'a>(
        &'a self,
        _server: &'a Arc<Server>,
        _event: &'a mut (dyn Payload + Send + Sync),
    ) -> BoxFuture<'a, ()>;

    /// Checks if the event handler is blocking.
    ///
    /// # Returns
    /// A boolean indicating whether the handler is blocking.
    fn is_blocking(&self) -> bool;

    /// Retrieves the priority of the event handler.
    ///
    /// # Returns
    /// The priority of the event handler.
    fn get_priority(&self) -> &EventPriority;

    /// Returns the plugin that registered this handler, when applicable.
    fn source(&self) -> Option<&str> {
        None
    }
}

/// A trait for handling specific events.
///
/// This trait allows for handling events of a specific type that implements the `Event` trait.
pub trait EventHandler<E: Payload>: Send + Sync {
    /// Asynchronously handles an event of type `E`.
    ///
    /// # Arguments
    /// - `event`: A reference to the event to handle.
    fn handle<'a>(&'a self, _server: &'a Arc<Server>, _event: &'a E) -> BoxFuture<'a, ()> {
        Box::pin(async {})
    }

    /// Asynchronously handles a blocking event of type `E`.
    ///
    /// # Arguments
    /// - `event`: A mutable reference to the event to handle.
    fn handle_blocking<'a>(
        &'a self,
        _server: &'a Arc<Server>,
        _event: &'a mut E,
    ) -> BoxFuture<'a, ()> {
        Box::pin(async {})
    }
}

/// A struct representing a typed event handler.
///
/// This struct holds a reference to an event handler, its priority, and whether it is blocking.
pub struct TypedEventHandler<E, H>
where
    E: Payload + Send + Sync + 'static,
    H: EventHandler<E> + Send + Sync,
{
    pub handler: Arc<H>,
    pub priority: EventPriority,
    pub blocking: bool,
    pub source: Option<String>,
    pub _phantom: std::marker::PhantomData<E>,
}

impl<E, H> DynEventHandler for TypedEventHandler<E, H>
where
    E: Payload + Send + Sync + 'static,
    H: EventHandler<E> + Send + Sync,
{
    /// Asynchronously handles a blocking dynamic event.
    fn handle_blocking_dyn<'a>(
        &'a self,
        server: &'a Arc<Server>,
        event: &'a mut (dyn Payload + Send + Sync),
    ) -> BoxFuture<'a, ()> {
        Box::pin(async move {
            if let Some(typed_event) = <dyn Payload>::downcast_mut(event) {
                // The handler.handle_blocking call now returns a Future, which we await.
                self.handler.handle_blocking(server, typed_event).await;
            }
        })
    }

    /// Asynchronously handles a dynamic event.
    fn handle_dyn<'a>(
        &'a self,
        server: &'a Arc<Server>,
        event: &'a (dyn Payload + Send + Sync),
    ) -> BoxFuture<'a, ()> {
        Box::pin(async move {
            if let Some(typed_event) = <dyn Payload>::downcast_ref(event) {
                // The handler.handle call now returns a Future, which we await.
                self.handler.handle(server, typed_event).await;
            }
        })
    }

    /// Checks if the handler is blocking.
    fn is_blocking(&self) -> bool {
        self.blocking
    }

    /// Retrieves the priority of the handler.
    fn get_priority(&self) -> &EventPriority {
        &self.priority
    }

    fn source(&self) -> Option<&str> {
        self.source.as_deref()
    }
}

/// A type alias for a map of event handlers, where the key is a static string
/// and the value is a vector of dynamic event handlers.
pub type HandlerMap = HashMap<&'static str, Vec<Arc<dyn DynEventHandler>>>;

/// Plugin loading state
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PluginState {
    Loading,
    Loaded,
    Failed(String),
}

/// Core plugin management system
pub struct PluginManager {
    plugins: SyncRwLock<Vec<LoadedPlugin>>,
    loaders: RwLock<Vec<Arc<dyn PluginLoader>>>,
    handlers: Arc<ArcSwap<HandlerMap>>,
    unloaded_files: RwLock<HashSet<PathBuf>>,
    services: Arc<RwLock<HashMap<String, Arc<dyn Payload>>>>,
    // Plugin state tracking
    plugin_states: RwLock<HashMap<String, PluginState>>,
    // Notification for plugin state changes
    state_notify: Arc<Notify>,
    // Background task for hot reloading
    hot_reload_task: RwLock<Option<JoinHandle<()>>>,
    hot_reload_enabled: AtomicBool,
    // Thread ID of the thread that created the plugin manager.
    // Permission prompts use rustyline, which is only safe on this thread.
    main_thread_id: ThreadId,
}

/// Represents a successfully loaded plugin
///
/// OS specific issues
/// - Windows: Plugin cannot be unloaded, it can be only active or not
struct LoadedPlugin {
    metadata: PluginMetadata,
    instance: Option<Arc<dyn Plugin>>,
    loader: Arc<dyn PluginLoader>,
    loader_data: Option<Box<dyn Any + Send + Sync>>,
    is_active: bool,
    context: Arc<Context>,
    path: PathBuf,
}

/// Error types for plugin management
#[derive(Error, Debug)]
pub enum ManagerError {
    #[error("Plugin not found: {0}")]
    PluginNotFound(String),
    #[error("Loader error: {0}")]
    LoaderError(#[from] LoaderError),
    #[error("IO error: {0}")]
    IoError(#[from] std::io::Error),
    #[error("Dependency error: {0}")]
    DependencyError(String),
}

impl Default for PluginManager {
    fn default() -> Self {
        Self::new(Vec::new())
    }
}

/// Queues an event that was fired from inside a guest call.
///
/// The guest call runs on the thread that polls the plugin's store driver, so the nested call this
/// dispatch needs would wait for the driver that is waiting for the write that fired the event:
/// dispatching it there deadlocks the plugin. The write itself has already happened, so the
/// notification is deferred to the next dispatch outside a guest call instead of being lost. A
/// handler can therefore not cancel or reshape the write that fired the event: its answer arrives
/// after the fact.
fn defer_event(
    server: &Arc<Server>,
    event_name: &'static str,
    event: &(dyn Payload + Send + Sync),
) {
    let Some(event) = event.duplicate() else {
        tracing::warn!(
            wasm_plugin_event = event_name,
            "An event fired inside a guest call cannot be copied and did not reach plugins"
        );
        return;
    };

    tracing::trace!(
        wasm_plugin_event = event_name,
        "Deferred a plugin event fired inside a guest call"
    );
    DEFERRED_EVENTS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .push_back(DeferredEvent {
            server: Arc::clone(server),
            event_name,
            event,
        });
}

impl PluginManager {
    /// Create a new plugin manager with the native loader and the given extra loaders
    #[must_use]
    pub fn new(extra_loaders: Vec<Arc<dyn PluginLoader>>) -> Self {
        let mut loaders: Vec<Arc<dyn PluginLoader>> = vec![Arc::new(NativePluginLoader)];
        loaders.extend(extra_loaders);
        Self {
            plugins: SyncRwLock::new(Vec::new()),
            loaders: RwLock::new(loaders),
            handlers: Arc::new(ArcSwap::from_pointee(HashMap::new())),
            unloaded_files: RwLock::new(HashSet::new()),
            services: Arc::new(RwLock::new(HashMap::new())),
            plugin_states: RwLock::new(HashMap::new()),
            state_notify: Arc::new(Notify::new()),
            hot_reload_task: RwLock::new(None),
            hot_reload_enabled: AtomicBool::new(false),
            main_thread_id: std::thread::current().id(),
        }
    }

    /// Unload all loaded plugins
    pub async fn unload_all_plugins(&self) -> Result<(), ManagerError> {
        let plugin_names: Vec<String> = {
            let plugins = self
                .plugins
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            plugins
                .iter()
                .filter(|p| p.is_active)
                .map(|p| p.metadata.name.clone())
                .collect()
        };

        for name in plugin_names {
            if let Err(e) = self.unload_plugin(&name).await {
                error!("Failed to unload plugin {name}: {e}");
            }
        }

        Ok(())
    }

    /// Add a new plugin loader implementation
    pub async fn add_loader(self: &Arc<Self>, server: &Arc<Server>, loader: Arc<dyn PluginLoader>) {
        self.loaders.write().await.push(loader);

        // Try to load previously unloaded files with the new loader
        self.retry_unloaded_files(server).await;
    }

    /// Start watching the plugins directory for changes
    pub async fn start_watcher(self: &Arc<Self>, server: &Arc<Server>) -> Result<(), ManagerError> {
        if self.hot_reload_task.read().await.is_some() {
            return Ok(());
        }

        let (tx, mut rx) = tokio::sync::mpsc::channel(100);
        let mut watcher = notify::recommended_watcher(move |res| {
            if let Ok(event) = res {
                let _ = tx.blocking_send(event);
            }
        })
        .map_err(|e| ManagerError::IoError(std::io::Error::other(e)))?;

        let plugin_dir = Path::new(PLUGIN_DIR);
        if !plugin_dir.exists() {
            std::fs::create_dir_all(plugin_dir)?;
        }

        watcher
            .watch(plugin_dir, RecursiveMode::NonRecursive)
            .map_err(|e| ManagerError::IoError(std::io::Error::other(e)))?;

        let manager = self.clone();
        let server_clone = Arc::clone(server);
        let task = server.spawn_task(async move {
            // Keep watcher alive by moving it into the task
            let _watcher = watcher;

            while let Some(event) = rx.recv().await {
                if !manager
                    .hot_reload_enabled
                    .load(std::sync::atomic::Ordering::Relaxed)
                {
                    continue;
                }

                match event.kind {
                    EventKind::Modify(ModifyKind::Data(_)) | EventKind::Create(_) => {
                        for path in event.paths {
                            if path.extension().is_some_and(|ext| ext == "wasm") {
                                debug!("Detected change in plugin: {:?}", path);
                                // Give it a small delay to ensure file is completely written
                                tokio::time::sleep(Duration::from_millis(100)).await;

                                // We need to find if this plugin is already loaded to unload it first
                                let plugin_name = {
                                    let plugins = manager
                                        .plugins
                                        .read()
                                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                                    plugins
                                        .iter()
                                        .find(|p| p.path == path)
                                        .map(|p| p.metadata.name.clone())
                                };

                                if let Some(name) = plugin_name {
                                    info!("Hot-reloading plugin: {}", name);
                                    let _ = manager.unload_plugin(&name).await;
                                }

                                // For now, we just try to load it. If it's already loaded,
                                // the loader might handle it or we might get a duplicate.
                                // Most WASM loaders will just create a new instance.
                                if let Err(e) =
                                    manager.start_loading_plugin(&server_clone, &path).await
                                {
                                    error!("Failed to hot-reload plugin {:?}: {}", path, e);
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }
        });

        *self.hot_reload_task.write().await = Some(task);
        self.set_hot_reload_enabled(true);
        Ok(())
    }

    /// Stop watching the plugins directory for changes
    pub async fn stop_watcher(&self) {
        let mut task_lock = self.hot_reload_task.write().await;
        if let Some(handle) = task_lock.take() {
            handle.abort();
        }
        self.set_hot_reload_enabled(false);
    }

    pub fn set_hot_reload_enabled(&self, enabled: bool) {
        self.hot_reload_enabled
            .store(enabled, std::sync::atomic::Ordering::Relaxed);
    }

    pub fn is_hot_reload_enabled(&self) -> bool {
        self.hot_reload_enabled
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Retry loading files that couldn't be loaded previously
    async fn retry_unloaded_files(self: &Arc<Self>, server: &Arc<Server>) {
        let files_to_retry: Vec<PathBuf> =
            { self.unloaded_files.read().await.iter().cloned().collect() };
        let mut retry_tasks = Vec::new();

        for path in files_to_retry {
            if let Ok(task) = self.start_loading_plugin(server, &path).await {
                retry_tasks.push(task);
            }
        }

        // Wait for all retry tasks to complete
        join_all(retry_tasks).await;
    }

    /// Lets every loader run its per-tick work.
    pub fn tick_loaders(&self, server: &Arc<Server>) {
        if let Ok(loaders) = self.loaders.try_read() {
            for loader in loaders.iter() {
                loader.on_tick(server);
            }
        }
    }

    /// Get a clone of the loaders for context use
    #[must_use]
    pub async fn get_loaders(&self) -> Vec<Arc<dyn PluginLoader>> {
        self.loaders.read().await.clone()
    }

    /// Helper for topological sort of plugins based on dependencies.
    ///
    /// The load order is not just a scheduling detail: the host numbers every registry entry a
    /// plugin registers `vanilla count + registration index`, and that numbering is what the
    /// registry sync puts on the wire (see `net::neoforge::registries`). So the same set of
    /// plugins has to load in the same order on every start, or the same world would number its
    /// blocks and items differently after a restart.
    ///
    /// The order is therefore a function of the plugin set alone — names and declared
    /// dependencies — never of the order the filesystem returned: plugins and their dependencies
    /// are visited by name, and a duplicate name (which would make the winner depend on the
    /// directory order) is rejected.
    fn topological_sort(plugins: &[(String, Vec<String>)]) -> Result<Vec<String>, String> {
        fn visit(
            name: &str,
            deps_map: &HashMap<String, &Vec<String>>,
            plugin_names: &HashSet<String>,
            visited: &mut HashSet<String>,
            current_path: &mut HashSet<String>,
            sorted: &mut Vec<String>,
        ) -> Result<(), String> {
            if current_path.contains(name) {
                return Err(format!(
                    "Circular dependency detected involving plugin: {name}"
                ));
            }
            if !visited.contains(name) {
                current_path.insert(name.to_string());
                if let Some(deps) = deps_map.get(name) {
                    let mut deps = deps.to_vec();
                    deps.sort();
                    for dep in &deps {
                        if !plugin_names.contains(dep) {
                            return Err(format!("Plugin {name} depends on missing plugin: {dep}"));
                        }
                        visit(dep, deps_map, plugin_names, visited, current_path, sorted)?;
                    }
                }
                current_path.remove(name);
                visited.insert(name.to_string());
                sorted.push(name.to_string());
            }
            Ok(())
        }
        let mut sorted = Vec::new();
        let mut visited = HashSet::new();
        let mut current_path = HashSet::new();
        let plugin_names: HashSet<String> = plugins.iter().map(|(n, _)| n.clone()).collect();
        let deps_map: HashMap<String, &Vec<String>> =
            plugins.iter().map(|(n, d)| (n.clone(), d)).collect();

        let mut ordered: Vec<&(String, Vec<String>)> = plugins.iter().collect();
        ordered.sort_by(|a, b| a.0.cmp(&b.0));
        let mut seen = HashSet::with_capacity(ordered.len());
        for (name, _) in &ordered {
            if !seen.insert(name.as_str()) {
                return Err(format!("Duplicate plugin name: {name}"));
            }
        }

        for (name, _) in ordered {
            visit(
                name,
                &deps_map,
                &plugin_names,
                &mut visited,
                &mut current_path,
                &mut sorted,
            )?;
        }

        Ok(sorted)
    }

    /// Ask the server owner if they allow the permissions requested by a plugin
    ///
    /// This must only be called from the main thread, because the underlying
    /// `rustyline` console handle is neither `Send` nor `Sync`. Calling it from a
    /// different thread (e.g. the hot-reload watcher task) will panic.
    ///
    /// Returns `(allowed, wait_time, cacheable)`. Non-main-thread denials are not
    /// cacheable so the user gets prompted again on the next cold load.
    #[expect(clippy::print_stdout)]
    fn ask_permission_confirmation(
        &self,
        metadata: &PluginMetadata,
    ) -> (bool, std::time::Duration, bool) {
        use colored::Colorize;

        if metadata.permissions.is_empty() {
            return (true, std::time::Duration::ZERO, true);
        }

        if std::thread::current().id() != self.main_thread_id {
            warn!(
                "Plugin \"{}\" ({}) requests permissions from a non-main thread. \
                 Permission prompts are only supported on the main thread. \
                 Denying permissions. To load this plugin interactively, restart the server.",
                metadata.name, metadata.version
            );
            return (false, Duration::ZERO, false);
        }

        let start_time = std::time::Instant::now();

        println!(
            "\n{} \"{}\" ({}) requests the following permissions:",
            "Plugin".bold(),
            metadata.name.cyan(),
            metadata.version.green()
        );
        for permission in &metadata.permissions {
            println!(
                "  - {}: {}",
                permission.yellow().bold(),
                permissions::get_permission_description(permission)
                    .unwrap_or("<unknown permission>")
                    .italic()
            );
        }

        let prompt = format!(
            "\n{} [y/N]: ",
            "Do you want to allow these permissions and load the plugin?".bold()
        );

        let mut rl_taken = if let Some(logger_option) = crate::LOGGER_IMPL.get()
            && let Some((wrapper, _, _)) = logger_option
            && let Some(rl) = wrapper.take_readline()
        {
            Some((wrapper, rl))
        } else {
            None
        };

        let result = if let Some((_, ref mut rl)) = rl_taken {
            rl.readline(&prompt).is_ok_and(|line| {
                let input = line.trim().to_lowercase();
                input == "y" || input == "yes"
            })
        } else {
            warn!(
                "Console readline is not available; cannot prompt for plugin \"{}\" permissions",
                metadata.name
            );
            false
        };

        if let Some((wrapper, rl)) = rl_taken {
            wrapper.return_readline(rl);
        }

        (result, start_time.elapsed(), true)
    }

    /// Spawn initialization for a single plugin
    #[allow(clippy::too_many_lines)]
    async fn spawn_plugin_initialization(
        self: &Arc<Self>,
        server: Arc<Server>,
        instance: Arc<dyn Plugin>,
        metadata: PluginMetadata,
        loader_data: Box<dyn Any + Send + Sync>,
        loader: Arc<dyn PluginLoader>,
        path: PathBuf,
    ) -> Result<tokio::task::JoinHandle<()>, ManagerError> {
        // Mark plugin as loading
        self.plugin_states
            .write()
            .await
            .insert(metadata.name.clone(), PluginState::Loading);

        let context = Arc::new(Context::new(
            metadata.clone(),
            server.clone(),
            Arc::clone(&self.handlers),
            Arc::clone(self),
            Arc::clone(&LOGGER_IMPL),
        ));

        // Create the plugin structure first
        let plugin = LoadedPlugin {
            metadata: metadata.clone(),
            instance: None, // Will be set after successful initialization
            loader: loader.clone(),
            loader_data: Some(loader_data),
            is_active: false, // Will be set to true after successful initialization
            context: context.clone(),
            path,
        };

        let plugin_index = {
            let mut plugins = self
                .plugins
                .write()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            plugins.push(plugin);
            plugins.len() - 1
        };

        // Spawn async task for plugin initialization
        let self_ref_clone = Arc::clone(self);
        let state_notify = Arc::clone(&self.state_notify);
        let plugin_name = metadata.name.clone();
        let loader_clone = loader.clone();

        let task = server.spawn_task(async move {
            // Initialize the plugin
            match instance.on_load(context.clone()).await {
                Ok(()) => {
                    // Update plugin state to loaded
                    {
                        let mut plugins = self_ref_clone
                            .plugins
                            .write()
                            .unwrap_or_else(std::sync::PoisonError::into_inner);
                        if let Some(plugin) = plugins.get_mut(plugin_index) {
                            plugin.instance = Some(instance);
                            plugin.is_active = true;
                        }
                    }
                    self_ref_clone
                        .plugin_states
                        .write()
                        .await
                        .insert(plugin_name.clone(), PluginState::Loaded);
                    state_notify.notify_waiters();

                    info!("Loaded {} ({})", metadata.name, metadata.version);

                    if !metadata.permissions.is_empty() {
                        warn!(
                            "Plugin \"{}\" uses the following permissions: {:?}",
                            metadata.name, metadata.permissions
                        );
                    }
                }
                Err(e) => {
                    // Handle initialization failure
                    let error_msg = format!("Initialization failed: {e}");
                    let _ = instance.on_unload(context).await;

                    // Get the loader data before removing the plugin
                    let loader_data: Option<Box<dyn Any + Send + Sync>> = {
                        let mut plugins = self_ref_clone
                            .plugins
                            .write()
                            .unwrap_or_else(std::sync::PoisonError::into_inner);
                        if let Some(plugin) = plugins.get_mut(plugin_index) {
                            plugin.loader_data.take()
                        } else {
                            None
                        }
                    };

                    // Try to unload the plugin data
                    if let Some(data) = loader_data {
                        loader_clone.unload(data).await.ok();
                    }

                    {
                        let mut plugins = self_ref_clone
                            .plugins
                            .write()
                            .unwrap_or_else(std::sync::PoisonError::into_inner);
                        if plugin_index < plugins.len() {
                            plugins.remove(plugin_index);
                        }
                    };
                    self_ref_clone
                        .plugin_states
                        .write()
                        .await
                        .insert(plugin_name.clone(), PluginState::Failed(error_msg.clone()));
                    state_notify.notify_waiters();

                    error!("Failed to initialize plugin {plugin_name}: {error_msg}",);
                }
            }
        });

        Ok(task)
    }

    /// Load all plugins from the plugin directory
    #[allow(clippy::too_many_lines)]
    pub async fn load_plugins(
        self: &Arc<Self>,
        server: &Arc<Server>,
    ) -> Result<std::time::Duration, ManagerError> {
        let path = Path::new(PLUGIN_DIR);

        if !path.exists() {
            std::fs::create_dir(path)?;
            return Ok(std::time::Duration::ZERO);
        }

        let cache_path = path.join("permission_cache.json");
        let mut cache = cache::PermissionCache::load(&cache_path).await;

        let mut prepared_plugins = Vec::new();
        let loaders = self.loaders.read().await.clone();

        for entry in std::fs::read_dir(path)? {
            let entry = entry?;
            let path = entry.path();

            if path.is_dir() {
                continue;
            }

            if path
                .extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("deactivated"))
            {
                continue;
            }

            // Find a loader that can handle this file
            let mut loader_found = false;
            for loader in &loaders {
                if loader.can_load(&path) {
                    match loader.load(&path).await {
                        Ok((instance, metadata, loader_data)) => {
                            let plugin_override =
                                server.advanced_config.plugins.overrides.get(&metadata.name);

                            if plugin_override.is_some_and(|o| !o.enabled) {
                                info!(
                                    "Plugin \"{}\" is disabled in configuration, skipping.",
                                    metadata.name
                                );
                                loader_found = true;
                                break;
                            }

                            let allow_unsigned = plugin_override
                                .and_then(|o| o.allow_unsigned)
                                .unwrap_or(server.advanced_config.plugins.allow_unsigned);

                            if !allow_unsigned && !loader.is_signed(&path) {
                                error!(
                                    "Plugin \"{}\" ({:?}) is unsigned or invalid and allow_unsigned is disabled in configuration, skipping.",
                                    metadata.name, path
                                );
                                loader_found = true;
                                break;
                            }

                            prepared_plugins.push((
                                instance,
                                metadata,
                                loader_data,
                                loader.clone(),
                                path.clone(),
                            ));
                            loader_found = true;
                        }
                        Err(err) => error!("Failed to load plugin from {:?}: {}", path, err),
                    }
                    break;
                }
            }

            if !loader_found {
                self.unloaded_files.write().await.insert(path.clone());
            }
        }

        // Resolve dependencies
        let metadata_list: Vec<(String, Vec<String>)> = prepared_plugins
            .iter()
            .map(|(_, m, _, _, _)| (m.name.clone(), m.dependencies.clone()))
            .collect();

        let sorted_names =
            Self::topological_sort(&metadata_list).map_err(ManagerError::DependencyError)?;

        // Map names back to prepared plugins
        #[expect(clippy::type_complexity)]
        let mut plugins_map: HashMap<
            String,
            (
                Arc<dyn Plugin>,
                PluginMetadata,
                Box<dyn Any + Send + Sync>,
                Arc<dyn PluginLoader>,
                PathBuf,
            ),
        > = prepared_plugins
            .into_iter()
            .map(|(i, m, d, l, p)| (m.name.clone(), (i, m, d, l, p)))
            .collect();

        let mut total_wait_time = std::time::Duration::ZERO;

        for name in sorted_names {
            if let Some((instance, metadata, loader_data, loader, path)) = plugins_map.remove(&name)
            {
                let (allowed, wait_time) = self
                    .clone()
                    .check_permissions_cached(&path, &metadata, &mut cache, &cache_path, server)
                    .await;

                total_wait_time += wait_time;

                if !allowed {
                    warn!(
                        "Permission denied for plugin \"{}\", skipping loading.",
                        metadata.name
                    );
                    continue;
                }

                match self
                    .spawn_plugin_initialization(
                        server.clone(),
                        instance,
                        metadata,
                        loader_data,
                        loader,
                        path,
                    )
                    .await
                {
                    Ok(task) => {
                        // We must await each initialization to ensure dependencies are ready
                        if let Err(err) = task.await {
                            error!("Plugin initialization task panicked: {}", err);
                        }
                    }
                    Err(err) => error!("{}", err),
                }
            }
        }

        Ok(total_wait_time)
    }

    async fn check_permissions_cached(
        &self,
        path: &Path,
        metadata: &PluginMetadata,
        cache: &mut cache::PermissionCache,
        cache_path: &Path,
        server: &Arc<Server>,
    ) -> (bool, std::time::Duration) {
        let plugin_config = &server.advanced_config.plugins;
        let plugin_override = plugin_config.overrides.get(&metadata.name);

        let is_blocked = |p: &str| {
            plugin_config.blocked_permissions.iter().any(|b| b == p)
                || plugin_override.is_some_and(|o| o.blocked_permissions.iter().any(|b| b == p))
        };

        let is_pre_allowed = |p: &str| {
            plugin_config.allowed_permissions.iter().any(|a| a == p)
                || plugin_override.is_some_and(|o| o.allowed_permissions.iter().any(|a| a == p))
        };

        let effective_permissions: Vec<String> = metadata
            .permissions
            .iter()
            .filter(|p| !is_blocked(p))
            .cloned()
            .collect();

        // If all requested permissions are pre-allowed, grant without prompting
        if !effective_permissions.iter().any(|p| !is_pre_allowed(p)) {
            return (true, std::time::Duration::ZERO);
        }

        let hash = cache::calculate_hash(path).await.unwrap_or_default();

        if let Some(entry) = cache.entries.get(&hash)
            && entry.permissions_requested == metadata.permissions
        {
            info!(
                "Found cached permission decision for plugin \"{}\" (approved: {})",
                metadata.name, entry.approved
            );
            return (entry.approved, std::time::Duration::ZERO);
        }

        if !plugin_config.ask_permission_confirmation {
            info!(
                "Auto-approving permissions for plugin \"{}\" (ask_permission_confirmation is disabled)",
                metadata.name
            );
            cache.entries.insert(
                hash,
                cache::PermissionCacheEntry {
                    permissions_requested: metadata.permissions.clone(),
                    approved: true,
                },
            );
            let _ = cache.save(cache_path).await;
            return (true, std::time::Duration::ZERO);
        }

        let (allowed, wait_time, cacheable) = self.ask_permission_confirmation(metadata);
        if cacheable {
            cache.entries.insert(
                hash,
                cache::PermissionCacheEntry {
                    permissions_requested: metadata.permissions.clone(),
                    approved: allowed,
                },
            );
            let _ = cache.save(cache_path).await;
        }
        (allowed, wait_time)
    }

    /// Start loading a plugin asynchronously
    async fn start_loading_plugin(
        self: &Arc<Self>,
        server: &Arc<Server>,
        path: &Path,
    ) -> Result<tokio::task::JoinHandle<()>, ManagerError> {
        if !server.advanced_config.plugins.enabled {
            return Err(ManagerError::LoaderError(LoaderError::RuntimeError(
                "Plugin system is disabled in configuration".to_string(),
            )));
        }

        let loaders = self.loaders.read().await.clone();
        for loader in &loaders {
            if loader.can_load(path) {
                let (instance, metadata, loader_data) = loader.load(path).await?;

                let plugin_override = server.advanced_config.plugins.overrides.get(&metadata.name);

                if plugin_override.is_some_and(|o| !o.enabled) {
                    return Err(ManagerError::LoaderError(LoaderError::RuntimeError(
                        format!("Plugin \"{}\" is disabled in configuration", metadata.name),
                    )));
                }

                let allow_unsigned = plugin_override
                    .and_then(|o| o.allow_unsigned)
                    .unwrap_or(server.advanced_config.plugins.allow_unsigned);

                if !allow_unsigned && !loader.is_signed(path) {
                    return Err(ManagerError::LoaderError(LoaderError::RuntimeError(
                        format!(
                            "Plugin \"{}\" is unsigned or invalid and allow_unsigned is disabled",
                            metadata.name
                        ),
                    )));
                }

                let cache_path = Path::new(PLUGIN_DIR).join("permission_cache.json");
                let mut cache = cache::PermissionCache::load(&cache_path).await;

                let (allowed, _) = self
                    .check_permissions_cached(path, &metadata, &mut cache, &cache_path, server)
                    .await;

                if !allowed {
                    warn!(
                        "Permission denied for plugin \"{}\", skipping loading.",
                        metadata.name
                    );
                    return Err(ManagerError::LoaderError(LoaderError::RuntimeError(
                        "Permission denied".to_string(),
                    )));
                }

                return self
                    .spawn_plugin_initialization(
                        server.clone(),
                        instance,
                        metadata,
                        loader_data,
                        loader.clone(),
                        path.to_path_buf(),
                    )
                    .await;
            }
        }

        // No loader could handle this file, track it for future attempts
        self.unloaded_files.write().await.insert(path.to_path_buf());

        Err(ManagerError::PluginNotFound(
            path.to_string_lossy().to_string(),
        ))
    }

    /// Attempt to load a single plugin file
    pub async fn try_load_plugin(
        self: &Arc<Self>,
        server: &Arc<Server>,
        path: &Path,
    ) -> Result<(), ManagerError> {
        self.start_loading_plugin(server, path)
            .await?
            .await
            .map_err(|e| {
                ManagerError::LoaderError(LoaderError::InitializationFailed(format!(
                    "Task join error: {e}"
                )))
            })
    }

    /// Wait for a plugin to finish loading
    pub async fn wait_for_plugin(&self, plugin_name: &str) -> Result<(), ManagerError> {
        loop {
            let state = self.plugin_states.read().await.get(plugin_name).cloned();
            if let Some(state) = state {
                match state {
                    PluginState::Loaded => return Ok(()),
                    PluginState::Failed(error) => {
                        return Err(ManagerError::LoaderError(
                            LoaderError::InitializationFailed(error),
                        ));
                    }
                    PluginState::Loading => {
                        // Wait for state change notification
                        self.state_notify.notified().await;
                        continue;
                    }
                }
            }
            return Err(ManagerError::PluginNotFound(plugin_name.to_string()));
        }
    }

    /// Get the current state of a plugin
    pub async fn get_plugin_state(&self, plugin_name: &str) -> Option<PluginState> {
        self.plugin_states.read().await.get(plugin_name).cloned()
    }

    /// Checks if plugin active
    #[must_use]
    pub fn is_plugin_active(&self, name: &str) -> bool {
        let plugins = self
            .plugins
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        plugins
            .iter()
            .any(|p| p.metadata.name == name && p.is_active && p.instance.is_some())
    }

    /// Get list of active plugins
    #[must_use]
    pub fn active_plugins(&self) -> Vec<PluginMetadata> {
        let plugins = self
            .plugins
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        plugins
            .iter()
            .filter(|p| p.is_active && p.instance.is_some())
            .map(|p| p.metadata.clone())
            .collect()
    }

    /// Checks if plugin loaded
    #[must_use]
    pub fn is_plugin_loaded(&self, name: &str) -> bool {
        let plugins = self
            .plugins
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        plugins.iter().any(|p| p.metadata.name == name)
    }

    /// Get list of loaded plugins
    #[must_use]
    pub fn loaded_plugins(&self) -> Vec<PluginMetadata> {
        let plugins = self
            .plugins
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        plugins.iter().map(|p| p.metadata.clone()).collect()
    }

    /// Unload a plugin by name
    pub async fn unload_plugin(&self, name: &str) -> Result<(), ManagerError> {
        let mut plugin = {
            let mut plugins = self
                .plugins
                .write()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let index = plugins
                .iter()
                .position(|p| p.metadata.name == name)
                .ok_or_else(|| ManagerError::PluginNotFound(name.to_string()))?;
            plugins.remove(index)
        };

        self.unregister_handlers(name);
        plugin.context.unregister_commands();

        if let Some(instance) = plugin.instance.take() {
            // A plugin's cleanup must not take the server down with it: report the failure
            // and carry on unloading.
            if let Err(error) = instance.on_unload(plugin.context.clone()).await {
                error!("Plugin {name} failed to unload: {error}");
            }
        }

        // A plugin that is gone must not keep chunks loaded or collision boxes in the world.
        crate::plugin_chunks::release_owner(name);
        crate::plugin_collision::release_owner(name);

        if plugin.loader.can_unload() {
            if let Some(data) = plugin.loader_data {
                plugin.loader.unload(data).await?;
            }
        } else {
            plugin.is_active = false;
            self.plugins
                .write()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(plugin);
        }

        // Remove from plugin states
        self.plugin_states.write().await.remove(name);

        Ok(())
    }

    fn unregister_handlers(&self, source: &str) {
        self.handlers.rcu(|handlers| {
            let mut new_handlers = (**handlers).clone();
            new_handlers.retain(|_, handlers| {
                handlers.retain(|handler| handler.source() != Some(source));
                !handlers.is_empty()
            });
            Arc::new(new_handlers)
        });
    }

    /// Get all plugins that are currently loading
    pub async fn get_loading_plugins(&self) -> Vec<String> {
        let plugin_states = self.plugin_states.read().await;
        plugin_states
            .iter()
            .filter(|(_, state)| matches!(state, PluginState::Loading))
            .map(|(name, _)| name.clone())
            .collect()
    }

    /// Get all plugins that failed to load
    pub async fn get_failed_plugins(&self) -> Vec<(String, String)> {
        let plugin_states = self.plugin_states.read().await;
        plugin_states
            .iter()
            .filter_map(|(name, state)| {
                if let PluginState::Failed(error) = state {
                    Some((name.clone(), error.clone()))
                } else {
                    None
                }
            })
            .collect()
    }

    /// Check if all plugins have finished loading (either succeeded or failed)
    pub async fn all_plugins_loaded(&self) -> bool {
        let plugin_states = self.plugin_states.read().await;
        !plugin_states
            .values()
            .any(|state| matches!(state, PluginState::Loading))
    }

    /// Wait for all plugins to finish loading
    pub async fn wait_for_all_plugins(&self) {
        while !self.all_plugins_loaded().await {
            self.state_notify.notified().await;
        }
    }

    /// Register an event handler
    pub fn register<E, H>(&self, handler: Arc<H>, priority: EventPriority, blocking: bool)
    where
        E: Payload + Send + Sync + 'static,
        H: EventHandler<E> + 'static,
    {
        let typed_handler = Arc::new(TypedEventHandler {
            handler,
            priority,
            blocking,
            source: None,
            _phantom: std::marker::PhantomData,
        });

        self.handlers.rcu(|handlers| {
            let mut new_handlers = (**handlers).clone();
            new_handlers
                .entry(E::get_name_static())
                .or_default()
                .push(typed_handler.clone());
            Arc::new(new_handlers)
        });
    }

    #[must_use]
    pub fn has_handlers<E: Payload + 'static>(&self) -> bool {
        self.handlers
            .load()
            .get(E::get_name_static())
            .is_some_and(|handlers| !handlers.is_empty())
    }

    /// Fire an event to all registered handlers
    pub async fn fire<E: Payload + Send + Sync + 'static>(
        &self,
        server: &Arc<Server>,
        event: &mut E,
    ) {
        self.fire_dyn(server, E::get_name_static(), event).await;
    }

    async fn fire_dyn(
        &self,
        server: &Arc<Server>,
        event_name: &'static str,
        event: &mut (dyn Payload + Send + Sync),
    ) {
        let handlers_map = self.handlers.load();
        if handlers_map.is_empty() {
            return;
        }

        let Some(handlers) = handlers_map.get(event_name) else {
            return;
        };

        if handlers.is_empty() {
            return;
        }

        // Process blocking handlers first
        for handler in handlers {
            if handler.is_blocking() {
                handler.handle_blocking_dyn(server, event).await;
            }
        }

        // Process non-blocking handlers
        for handler in handlers {
            if !handler.is_blocking() {
                handler.handle_dyn(server, event).await;
            }
        }
    }

    /// Fire an event to all registered handlers synchronously (blocking if handlers exist).
    /// If no handlers are registered for this event, returns immediately without runtime overhead.
    pub fn fire_blocking<E: Payload + Send + Sync + 'static>(
        &self,
        server: &Arc<Server>,
        event: &mut E,
    ) {
        self.fire_blocking_dyn(server, E::get_name_static(), event);
    }

    fn fire_blocking_dyn(
        &self,
        server: &Arc<Server>,
        event_name: &'static str,
        event: &mut (dyn Payload + Send + Sync),
    ) {
        if !in_guest_call() {
            dispatch_deferred_events();
        }

        let handlers_map = self.handlers.load();
        if handlers_map.is_empty() {
            return;
        }

        let Some(handlers) = handlers_map.get(event_name) else {
            return;
        };

        if handlers.is_empty() {
            return;
        }

        if in_guest_call() {
            defer_event(server, event_name, event);
            return;
        }

        if tokio::runtime::Handle::try_current().is_ok() {
            tokio::task::block_in_place(|| {
                server
                    .runtime
                    .block_on(self.fire_dyn(server, event_name, event));
            });
        } else {
            server
                .runtime
                .block_on(self.fire_dyn(server, event_name, event));
        }
    }

    #[expect(clippy::result_unit_err)]
    pub async fn send_message(
        &self,
        sender: &str,
        recipient: &str,
        message: &[u8],
    ) -> Result<Result<Vec<u8>, String>, ()> {
        if sender == recipient {
            return Err(());
        }

        let instance = {
            let plugins = self
                .plugins
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let target_plugin = plugins
                .iter()
                .find(|p| p.metadata.name == recipient)
                .ok_or(())?;
            target_plugin.instance.clone()
        };
        if let Some(instance) = instance {
            Ok(instance.on_ipc_message(sender, message).await)
        } else {
            Err(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plugin(name: &str, deps: &[&str]) -> (String, Vec<String>) {
        (
            name.to_string(),
            deps.iter().map(ToString::to_string).collect(),
        )
    }

    #[tokio::test]
    async fn topological_sort() {
        let plugins = vec![
            plugin("A", &["B"]),
            plugin("B", &["C"]),
            plugin("C", &[]),
        ];
        let sorted = PluginManager::topological_sort(&plugins).unwrap();
        assert_eq!(sorted, vec!["C", "B", "A"]);

        let plugins_complex = vec![
            plugin("A", &["B", "C"]),
            plugin("B", &["D"]),
            plugin("C", &["D"]),
            plugin("D", &[]),
        ];
        let sorted = PluginManager::topological_sort(&plugins_complex).unwrap();
        // D must be before B and C, and B, C before A. Ties are broken by name, so the sort is
        // the same on every start: B (whose dependency D is visited first) then C.
        assert_eq!(sorted, vec!["D", "B", "C", "A"]);

        let plugins_circular = vec![plugin("A", &["B"]), plugin("B", &["A"])];
        assert!(PluginManager::topological_sort(&plugins_circular).is_err());

        let plugins_missing = vec![plugin("A", &["B"])];
        assert!(PluginManager::topological_sort(&plugins_missing).is_err());
    }

    /// The directory order `read_dir` returns must not reach the load order: the ids the host
    /// gives registry entries are `vanilla count + registration index`, so an unstable order
    /// would number the same world differently after a restart.
    #[test]
    fn topological_sort_is_independent_of_the_input_order() {
        let plugins = vec![
            plugin("create", &[]),
            plugin("offroad", &[]),
            plugin("sable", &["create"]),
            plugin("aeronautics", &["create", "sable"]),
            plugin("simulated", &["create"]),
        ];
        let expected = PluginManager::topological_sort(&plugins).unwrap();
        assert_eq!(
            expected,
            vec!["create", "sable", "aeronautics", "offroad", "simulated"]
        );

        let mut reversed = plugins.clone();
        reversed.reverse();
        assert_eq!(PluginManager::topological_sort(&reversed).unwrap(), expected);

        let mut rotated = plugins.clone();
        rotated.rotate_left(3);
        assert_eq!(PluginManager::topological_sort(&rotated).unwrap(), expected);

        // The declared order of a plugin's dependencies is not part of the result either.
        let mut reordered_deps = plugins.clone();
        reordered_deps[3].1.reverse();
        assert_eq!(
            PluginManager::topological_sort(&reordered_deps).unwrap(),
            expected
        );
    }

    /// Two loads of the same plugin set must give every registry entry the same id.
    #[test]
    fn registry_ids_are_identical_for_two_loads_of_the_same_plugins() {
        const VANILLA_ITEMS: u32 = 1_500;
        let plugins = vec![
            plugin("offroad", &[]),
            plugin("create", &[]),
            plugin("sable", &[]),
        ];
        let items = |name: &str| match name {
            "create" => 3,
            "offroad" => 2,
            _ => 1,
        };
        // The host numbers entries `vanilla count + registration index`, in load order.
        let ids = |order: &[String]| {
            let mut next = VANILLA_ITEMS;
            let mut out = Vec::new();
            for name in order {
                for _ in 0..items(name) {
                    out.push((name.clone(), next));
                    next += 1;
                }
            }
            out
        };

        let first_order = PluginManager::topological_sort(&plugins).unwrap();
        let mut shuffled = plugins.clone();
        shuffled.reverse();
        let second_order = PluginManager::topological_sort(&shuffled).unwrap();

        assert_eq!(first_order, second_order);
        assert_eq!(ids(&first_order), ids(&second_order));
        // Pinned: the first plugin by name takes the first ids, whatever the directory says.
        assert_eq!(
            ids(&first_order),
            vec![
                ("create".to_string(), VANILLA_ITEMS),
                ("create".to_string(), VANILLA_ITEMS + 1),
                ("create".to_string(), VANILLA_ITEMS + 2),
                ("offroad".to_string(), VANILLA_ITEMS + 3),
                ("offroad".to_string(), VANILLA_ITEMS + 4),
                ("sable".to_string(), VANILLA_ITEMS + 5),
            ]
        );
    }

    /// Two plugin files with the same name would make both the load order and the ids depend on
    /// the directory order, so they are rejected instead.
    #[test]
    fn duplicate_plugin_names_are_rejected() {
        let plugins = vec![plugin("create", &[]), plugin("create", &[])];
        let error = PluginManager::topological_sort(&plugins).unwrap_err();
        assert!(error.contains("Duplicate plugin name: create"), "{error}");
    }
}
