use std::{
    any::Any,
    collections::HashMap,
    future::{Future, poll_fn},
    ops::Deref,
    panic::{AssertUnwindSafe, catch_unwind},
    pin::Pin,
    sync::{
        Arc, Mutex as StdMutex, OnceLock,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
};

use futures::{StreamExt, stream::FuturesUnordered};
use tokio::sync::{Mutex, mpsc, oneshot};
use wasmtime::{
    AsContextMut, Store, StoreContextMut,
    component::{Accessor, AccessorTask, ComponentNamedList, Lift, Lower, TypedFunc},
};

use crate::{
    RuntimeSpawner,
    chain::{AdmissionClass, ReentryContext, RootAdmissionGuard, scope, sync_scope},
    lifecycle::{DriverError, DriverJoin, DriverState, Lifecycle},
    policy::{LegacySyncReentry, StorePolicy},
};

const STORE_QUEUE_CAPACITY: usize = 64;
const REENTRY_QUEUE_CAPACITY: usize = 64;

pub type StoreFuture<'a, R> = Pin<Box<dyn Future<Output = wasmtime::Result<R>> + Send + 'a>>;

trait StoreJob<T>: Send {
    fn run(self: Box<Self>, accessor: &Accessor<T>) -> StoreFuture<'_, ()>;
}

trait GuestStoreJob<T>: Send {
    fn run_concurrent(self: Box<Self>, accessor: &Accessor<T>) -> StoreFuture<'_, ()>;

    fn run_reentrant(self: Box<Self>, store: StoreContextMut<'_, T>) -> StoreFuture<'_, ()>;
}

struct StoreCall<F, R> {
    call: F,
    result: oneshot::Sender<wasmtime::Result<R>>,
    context: Option<ReentryContext>,
}

impl<T, F, R> StoreJob<T> for StoreCall<F, R>
where
    T: Send + 'static,
    F: for<'a> FnOnce(&'a Accessor<T>) -> StoreFuture<'a, R> + Send + 'static,
    R: Send + 'static,
{
    fn run(self: Box<Self>, accessor: &Accessor<T>) -> StoreFuture<'_, ()> {
        let Self {
            call,
            result,
            context,
        } = *self;
        let future = call(accessor);
        Box::pin(async move {
            let output = if let Some(context) = context {
                scope(context, future).await
            } else {
                future.await
            };
            let _ = result.send(output);
            Ok(())
        })
    }
}

struct ShutdownStoreCall<F, R> {
    call: F,
    result: oneshot::Sender<wasmtime::Result<R>>,
    context: ReentryContext,
}

impl<T, F, R> StoreJob<T> for ShutdownStoreCall<F, R>
where
    T: Send + 'static,
    F: for<'a> FnOnce(&'a Accessor<T>) -> StoreFuture<'a, R> + Send + 'static,
    R: Send + 'static,
{
    fn run(self: Box<Self>, accessor: &Accessor<T>) -> StoreFuture<'_, ()> {
        let Self {
            call,
            result,
            context,
        } = *self;
        let future = call(accessor);
        Box::pin(async move {
            match scope(context, future).await {
                Ok(value) => {
                    let _ = result.send(Ok(value));
                    Ok(())
                }
                Err(error) => {
                    let message = error.to_string();
                    let _ = result.send(Err(error));
                    Err(wasmtime::Error::msg(message))
                }
            }
        })
    }
}

struct GuestStoreCall<T, F, R> {
    call: F,
    result: oneshot::Sender<wasmtime::Result<R>>,
    context: ReentryContext,
    reentry: Arc<ReentryState<T>>,
    guest_call_failure: Arc<GuestCallFailure>,
    root_admission: Option<RootAdmissionGuard>,
}

#[derive(Default)]
struct GuestCallFailure {
    message: OnceLock<String>,
}

impl GuestCallFailure {
    fn record(&self, error: &wasmtime::Error) {
        let _ = self.message.set(format!("{error:#}"));
    }

    fn message(&self) -> Option<String> {
        self.message.get().cloned()
    }
}

impl<T, F, R> GuestStoreJob<T> for GuestStoreCall<T, F, R>
where
    T: Send + 'static,
    F: for<'a> FnOnce(LegacyGuestScope<'a, T>) -> StoreFuture<'a, R> + Send + 'static,
    R: Send + 'static,
{
    fn run_concurrent(self: Box<Self>, accessor: &Accessor<T>) -> StoreFuture<'_, ()> {
        let Self {
            call,
            result,
            context,
            reentry,
            guest_call_failure,
            root_admission,
        } = *self;
        Box::pin(async move {
            let failure_for_scope = Arc::clone(&guest_call_failure);
            let output = scope(context, async move {
                let _active_context = reentry.enter_active_context(context);
                call(LegacyGuestScope::concurrent(accessor, failure_for_scope)).await
            })
            .await;
            let guest_call_failure = guest_call_failure.message();
            drop(root_admission);
            if let Some(message) = guest_call_failure {
                let output = match output {
                    Ok(_) => Err(wasmtime::Error::msg(message.clone())),
                    Err(error) => Err(error),
                };
                let _ = result.send(output);
                Err(wasmtime::Error::msg(message))
            } else {
                let _ = result.send(output);
                Ok(())
            }
        })
    }

    fn run_reentrant(self: Box<Self>, store: StoreContextMut<'_, T>) -> StoreFuture<'_, ()> {
        let Self {
            call,
            result,
            context,
            reentry,
            guest_call_failure,
            root_admission,
        } = *self;
        Box::pin(async move {
            let failure_for_scope = Arc::clone(&guest_call_failure);
            let output = scope(context, async move {
                let _active_context = reentry.enter_active_context(context);
                call(LegacyGuestScope::reentrant(store, failure_for_scope)).await
            })
            .await;
            let guest_call_failure = guest_call_failure.message();
            drop(root_admission);
            if let Some(message) = guest_call_failure {
                let output = match output {
                    Ok(_) => Err(wasmtime::Error::msg(message.clone())),
                    Err(error) => Err(error),
                };
                let _ = result.send(output);
                Err(wasmtime::Error::msg(message))
            } else {
                let _ = result.send(output);
                Ok(())
            }
        })
    }
}

enum LegacyGuestAccess<'a, T: 'static> {
    Concurrent(&'a Accessor<T>),
    Reentrant(StoreContextMut<'a, T>),
}

/// Capability available while one synchronous legacy guest call is active.
///
/// Store data may only be borrowed by a synchronous closure. The underlying
/// Wasmtime Store context is never exposed outside this crate.
pub struct LegacyGuestScope<'a, T: 'static> {
    access: LegacyGuestAccess<'a, T>,
    guest_call_failure: Arc<GuestCallFailure>,
}

impl<'a, T> LegacyGuestScope<'a, T>
where
    T: Send + 'static,
{
    const fn concurrent(
        accessor: &'a Accessor<T>,
        guest_call_failure: Arc<GuestCallFailure>,
    ) -> Self {
        Self {
            access: LegacyGuestAccess::Concurrent(accessor),
            guest_call_failure,
        }
    }

    const fn reentrant(
        store: StoreContextMut<'a, T>,
        guest_call_failure: Arc<GuestCallFailure>,
    ) -> Self {
        Self {
            access: LegacyGuestAccess::Reentrant(store),
            guest_call_failure,
        }
    }

    #[cfg(test)]
    pub(crate) const fn is_reentrant(&self) -> bool {
        matches!(self.access, LegacyGuestAccess::Reentrant(_))
    }

    pub fn with<R>(&mut self, call: impl for<'store> FnOnce(StoreDataMut<'store, T>) -> R) -> R {
        match &mut self.access {
            LegacyGuestAccess::Concurrent(accessor) => accessor.with(|mut access| {
                call(StoreDataMut {
                    data: access.data_mut(),
                })
            }),
            LegacyGuestAccess::Reentrant(store) => call(StoreDataMut {
                data: store.data_mut(),
            }),
        }
    }

    pub async fn call<Params, Return>(
        &mut self,
        function: TypedFunc<Params, Return>,
        params: Params,
    ) -> wasmtime::Result<Return>
    where
        Params: ComponentNamedList + Lower + 'static,
        Return: ComponentNamedList + Lift + 'static,
    {
        if let Some(message) = self.guest_call_failure.message() {
            return Err(wasmtime::Error::msg(message));
        }
        let result = match &mut self.access {
            LegacyGuestAccess::Concurrent(accessor) => {
                function.call_concurrent(&**accessor, params).await
            }
            LegacyGuestAccess::Reentrant(store) => {
                function.call_async(store.as_context_mut(), params).await
            }
        };
        if let Err(error) = &result {
            self.guest_call_failure.record(error);
        }
        result
    }
}

pub struct StoreDataMut<'a, T> {
    data: &'a mut T,
}

impl<T> StoreDataMut<'_, T> {
    pub const fn data_mut(&mut self) -> &mut T {
        self.data
    }
}

struct ConcurrentTask<T>(Box<dyn StoreJob<T>>);

impl<T> AccessorTask<T> for ConcurrentTask<T>
where
    T: Send + 'static,
{
    fn run(self, accessor: &Accessor<T>) -> impl Future<Output = wasmtime::Result<()>> + Send {
        self.0.run(accessor)
    }
}

struct ConcurrentGuestTask<T>(Box<dyn GuestStoreJob<T>>);

impl<T> AccessorTask<T> for ConcurrentGuestTask<T>
where
    T: Send + 'static,
{
    fn run(self, accessor: &Accessor<T>) -> impl Future<Output = wasmtime::Result<()>> + Send {
        self.0.run_concurrent(accessor)
    }
}

enum StoreMessage<T> {
    Call(Box<dyn StoreJob<T>>),
    GuestCall(Box<dyn GuestStoreJob<T>>),
    Shutdown {
        job: Box<dyn StoreJob<T>>,
        root_admission: Option<RootAdmissionGuard>,
    },
}

enum ControlMessage {
    Dropped,
    Discarded,
}

type ReentrySender<T> = mpsc::Sender<Box<dyn GuestStoreJob<T>>>;

std::thread_local! {
    /// The admission of the Store whose driver is polling its guest calls on this thread right now.
    ///
    /// Wasmtime's event loop polls host imports outside the guest call's task-local scope (see
    /// [`StoreHandle::in_active_context`]), so a callback into a plugin Store cannot read the
    /// chain from there. This marks the thread for the duration of one poll of the driver only, so
    /// such a callback is told apart from a fresh root call that a worker polls afterwards: taking
    /// a root admission for the callback would wait for the semaphore its own call holds.
    static DRIVING_ADMISSION: std::cell::Cell<Option<u64>> = const { std::cell::Cell::new(None) };
}

/// The admission of the driver polling on this thread, if any.
fn driving_admission() -> Option<u64> {
    DRIVING_ADMISSION.with(std::cell::Cell::get)
}

/// Whether a host-initiated guest call is running on the current thread right now.
///
/// The driver that would serve a nested guest call is on this thread's stack for as long as that
/// call runs, so a plugin callback fired from here can never be admitted: it would wait for the
/// driver that is waiting for the caller. Host code that can re-enter a plugin therefore has to
/// check this before it fires an event.
#[must_use]
pub fn is_running_guest_call() -> bool {
    driving_admission().is_some()
}

/// The hook the plugin host installs to flush the events a guest call had to defer.
///
/// A host write fired inside a guest call cannot dispatch the event it fires — the nested call
/// would wait for the driver that is waiting for the write — so the host queues it instead. The
/// queue is only dispatched once no guest call is active, and the earliest such point is the
/// outermost guest call of a chain returning: the plugin code has unwound and the driver is free
/// again. Without the hook the queue waits for the next host dispatch outside a guest call, which
/// in practice is the next server tick, so every deferred callback costs a tick.
static GUEST_CALL_RETURN_HOOK: OnceLock<fn()> = OnceLock::new();

/// Registers the hook the executor calls when the outermost guest call of a chain returns.
///
/// See [`GUEST_CALL_RETURN_HOOK`]; the plugin host installs it once at startup, like
/// `pumpkin_core::plugin::install_guest_call_probe` installs the probe in the other direction.
pub fn install_guest_call_return_hook(hook: fn()) {
    let _ = GUEST_CALL_RETURN_HOOK.set(hook);
}

/// Tells the host that the outermost guest call of a chain returned.
fn notify_guest_call_return() {
    if let Some(hook) = GUEST_CALL_RETURN_HOOK.get() {
        hook();
    }
}

/// Sets [`DRIVING_ADMISSION`] while alive and restores the previous value on drop.
struct DrivingAdmissionGuard(Option<u64>);

impl DrivingAdmissionGuard {
    fn enter(admission_id: u64) -> Self {
        Self(DRIVING_ADMISSION.with(|current| current.replace(Some(admission_id))))
    }
}

impl Drop for DrivingAdmissionGuard {
    fn drop(&mut self) {
        DRIVING_ADMISSION.with(|current| current.set(self.0));
    }
}

struct ReentryScope<T> {
    id: u64,
    sender: ReentrySender<T>,
}

type ReentryScopes<T> = HashMap<(u64, u64), Vec<ReentryScope<T>>>;

struct ReentryState<T> {
    next_scope_id: AtomicU64,
    scopes: StdMutex<ReentryScopes<T>>,
    active_contexts: StdMutex<Vec<(u64, ReentryContext)>>,
    /// Guest calls served by an active host frame instead of the driver.
    routed_jobs: AtomicU64,
}

impl<T> ReentryState<T> {
    fn new() -> Self {
        Self {
            next_scope_id: AtomicU64::new(0),
            scopes: StdMutex::new(HashMap::new()),
            active_contexts: StdMutex::new(Vec::new()),
            routed_jobs: AtomicU64::new(0),
        }
    }

    fn enter_active_context(
        self: &Arc<Self>,
        context: ReentryContext,
    ) -> ActiveContextRegistration<T> {
        let id = self.next_scope_id.fetch_add(1, Ordering::Relaxed);
        self.active_contexts
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push((id, context));
        ActiveContextRegistration {
            state: Arc::clone(self),
            id,
        }
    }

    fn current_context(&self) -> Option<ReentryContext> {
        self.active_contexts
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .last()
            .map(|(_, context)| *context)
    }

    fn has_active_context(&self, context: ReentryContext) -> bool {
        self.active_contexts
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .any(|(_, active)| {
                active.admission_id == context.admission_id && active.chain_id == context.chain_id
            })
    }

    fn remove_active_context(&self, id: u64) {
        let mut contexts = self
            .active_contexts
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(index) = contexts
            .iter()
            .rposition(|(context_id, _)| *context_id == id)
        {
            contexts.remove(index);
        }
    }

    fn register(
        self: &Arc<Self>,
        context: ReentryContext,
    ) -> (
        ReentryRegistration<T>,
        mpsc::Receiver<Box<dyn GuestStoreJob<T>>>,
    ) {
        let scope_id = self.next_scope_id.fetch_add(1, Ordering::Relaxed);
        let (sender, receiver) = mpsc::channel(REENTRY_QUEUE_CAPACITY);
        let mut scopes = self
            .scopes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        scopes
            .entry((context.admission_id, context.chain_id))
            .or_default()
            .push(ReentryScope {
                id: scope_id,
                sender,
            });
        (
            ReentryRegistration {
                state: Arc::clone(self),
                admission_id: context.admission_id,
                chain_id: context.chain_id,
                scope_id,
                active: true,
            },
            receiver,
        )
    }

    fn sender_for(&self, context: ReentryContext) -> Option<ReentrySender<T>> {
        let scopes = self
            .scopes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        scopes
            .get(&(context.admission_id, context.chain_id))
            .and_then(|scopes| scopes.last())
            .map(|scope| scope.sender.clone())
    }

    fn remove(&self, admission_id: u64, chain_id: u64, scope_id: u64) {
        let mut scopes = self
            .scopes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let chain = (admission_id, chain_id);
        let remove_chain = scopes.get_mut(&chain).is_some_and(|chain_scopes| {
            if let Some(index) = chain_scopes.iter().rposition(|scope| scope.id == scope_id) {
                chain_scopes.remove(index);
            }
            chain_scopes.is_empty()
        });
        if remove_chain {
            scopes.remove(&chain);
        }
    }
}

struct ActiveContextRegistration<T> {
    state: Arc<ReentryState<T>>,
    id: u64,
}

impl<T> Drop for ActiveContextRegistration<T> {
    fn drop(&mut self) {
        self.state.remove_active_context(self.id);
    }
}

struct ReentryRegistration<T> {
    state: Arc<ReentryState<T>>,
    admission_id: u64,
    chain_id: u64,
    scope_id: u64,
    active: bool,
}

impl<T> ReentryRegistration<T> {
    fn close(&mut self) {
        if self.active {
            self.state
                .remove(self.admission_id, self.chain_id, self.scope_id);
            self.active = false;
        }
    }
}

impl<T> Drop for ReentryRegistration<T> {
    fn drop(&mut self) {
        self.close();
    }
}

struct StoreShared<T, P> {
    sender: mpsc::Sender<StoreMessage<T>>,
    accepting: Arc<AtomicBool>,
    guest_call_failure: Arc<GuestCallFailure>,
    send_gate: Mutex<()>,
    lifecycle: Arc<Lifecycle>,
    reentry: Arc<ReentryState<T>>,
    policy: P,
    spawner: Arc<dyn RuntimeSpawner>,
}

/// Cloneable submission and observation capability for one Wasmtime Store.
pub struct StoreHandle<T, P> {
    shared: Arc<StoreShared<T, P>>,
}

impl<T, P> Clone for StoreHandle<T, P> {
    fn clone(&self) -> Self {
        Self {
            shared: Arc::clone(&self.shared),
        }
    }
}

impl<T, P> StoreHandle<T, P>
where
    T: Send + 'static,
    P: StorePolicy,
{
    #[must_use]
    pub fn state(&self) -> DriverState {
        self.shared.lifecycle.state()
    }

    #[must_use]
    pub fn driver_join(&self) -> DriverJoin {
        self.shared.lifecycle.subscribe()
    }

    /// Number of guest calls this Store served from an active host frame rather
    /// than by queueing them on the driver.
    #[cfg(test)]
    pub(crate) fn routed_jobs(&self) -> u64 {
        self.shared.reentry.routed_jobs.load(Ordering::Relaxed)
    }

    /// Queues an owned Store operation. Once accepted, the operation runs even
    /// if the caller stops waiting for its result.
    pub async fn call<R, F>(&self, call: F) -> wasmtime::Result<R>
    where
        F: for<'a> FnOnce(&'a Accessor<T>) -> StoreFuture<'a, R> + Send + 'static,
        R: Send + 'static,
    {
        let (result, receiver) = oneshot::channel();
        let send_guard = self.shared.send_gate.lock().await;
        self.ensure_accepting("Wasm plugin store is shutting down")?;
        let permit = self
            .shared
            .sender
            .reserve()
            .await
            .map_err(|_| self.terminal_error("Wasm plugin store driver is not running"))?;
        permit.send(StoreMessage::Call(Box::new(StoreCall {
            call,
            result,
            context: None,
        })));
        drop(send_guard);

        self.receive_result(receiver, "Wasm plugin store call did not complete")
            .await
    }

    fn ensure_accepting(&self, message: &'static str) -> wasmtime::Result<()> {
        match self.state() {
            DriverState::Failed(error) => Err(wasmtime::Error::msg(format!(
                "Wasm plugin store driver failed: {error:#}"
            ))),
            DriverState::Stopped => Err(wasmtime::Error::msg(message)),
            _ if self.shared.guest_call_failure.message().is_some() => Err(wasmtime::Error::msg(
                "Wasm plugin store failed during a guest call",
            )),
            _ if self.shared.accepting.load(Ordering::Acquire) => Ok(()),
            _ => Err(wasmtime::Error::msg(message)),
        }
    }

    fn terminal_error(&self, fallback: &'static str) -> wasmtime::Error {
        match self.state() {
            DriverState::Failed(error) => {
                wasmtime::Error::msg(format!("Wasm plugin store driver failed: {error:#}"))
            }
            DriverState::Stopped => wasmtime::Error::msg("Wasm plugin store driver has stopped"),
            _ => wasmtime::Error::msg(fallback),
        }
    }

    async fn receive_result<R>(
        &self,
        receiver: oneshot::Receiver<wasmtime::Result<R>>,
        fallback: &'static str,
    ) -> wasmtime::Result<R> {
        if let Ok(result) = receiver.await {
            result
        } else {
            let mut join = self.driver_join();
            match join.wait().await {
                Ok(()) => Err(wasmtime::Error::msg(fallback)),
                Err(error) => Err(wasmtime::Error::msg(format!(
                    "Wasm plugin store driver failed: {error:#}"
                ))),
            }
        }
    }
}

impl<T> StoreHandle<T, LegacySyncReentry>
where
    T: Send + 'static,
{
    /// The context of a guest call the driver polls on the current thread, if any.
    ///
    /// `None` outside the driver's poll, so a root call from another task never looks nested. The
    /// driver may be polling another Store of the same admission authority; the callback still
    /// belongs to that chain, so the Store takes a context of the same policy instead of a root
    /// admission that the chain already holds.
    fn polled_call_context(&self) -> Option<ReentryContext> {
        if driving_admission() != Some(self.shared.policy.admission_id()) {
            return None;
        }
        Some(
            self.shared
                .reentry
                .current_context()
                .unwrap_or_else(|| self.shared.policy.root_context()),
        )
    }

    /// Queues a synchronous guest call, or routes it through the active host
    /// frame when this Store already appears in the current causal chain.
    pub async fn call_guest<R, F>(&self, call: F) -> wasmtime::Result<R>
    where
        F: for<'a> FnOnce(LegacyGuestScope<'a, T>) -> StoreFuture<'a, R> + Send + 'static,
        R: Send + 'static,
    {
        self.call_guest_in(AdmissionClass::Normal, call).await
    }

    /// Like [`Self::call_guest`], for a root call that must not delay a synchronous one: the class
    /// only decides who the admission is handed to next, see [`AdmissionClass`].
    pub async fn call_guest_in<R, F>(&self, class: AdmissionClass, call: F) -> wasmtime::Result<R>
    where
        F: for<'a> FnOnce(LegacyGuestScope<'a, T>) -> StoreFuture<'a, R> + Send + 'static,
        R: Send + 'static,
    {
        if let Some(message) = self.shared.guest_call_failure.message() {
            return Err(wasmtime::Error::msg(format!(
                "Wasm plugin store failed during a guest call: {message}"
            )));
        }
        let (result, receiver) = oneshot::channel();
        // TEMPORARY DRAINDBG.
        let guest_call_started = std::time::Instant::now();
        let inherited_context = self.shared.policy.inherited_context();
        let (context, root_admission) = if let Some(context) = inherited_context {
            tracing::trace!(
                wasm_plugin_admission_id = context.admission_id,
                wasm_plugin_chain_id = context.chain_id,
                wasm_plugin_reentry_depth = context.depth,
                "Inherited legacy Wasm plugin root admission"
            );
            (context, None)
        } else if let Some(context) = self.polled_call_context() {
            // This is a callback of a guest call the driver is running on this thread, not a
            // fresh root call: Wasmtime's event loop polls host imports outside the guest call's
            // task-local scope, so the chain is only visible here. Acquiring a root admission
            // would wait for the semaphore that call is holding while it waits for this callback.
            let context = context.child()?;
            tracing::trace!(
                wasm_plugin_admission_id = context.admission_id,
                wasm_plugin_chain_id = context.chain_id,
                wasm_plugin_reentry_depth = context.depth,
                "Recovered the legacy Wasm plugin chain of the polled guest call"
            );
            (context, None)
        } else {
            self.ensure_accepting("Wasm plugin store is shutting down")?;
            let admission = self.shared.policy.acquire_root(class).await?;
            (admission.context(), Some(admission))
        };
        // A call that had no chain to inherit or route through took a fresh root admission: nothing
        // else of that chain is on this task's stack, so this is its outermost call and the point
        // where the events a host write inside it had to defer can be dispatched.
        let outermost = root_admission.is_some();
        let mut job: Box<dyn GuestStoreJob<T>> = Box::new(GuestStoreCall {
            call,
            result,
            context,
            reentry: Arc::clone(&self.shared.reentry),
            guest_call_failure: Arc::clone(&self.shared.guest_call_failure),
            root_admission,
        });

        if let Some(context) = inherited_context
            && let Some(sender) = self.shared.reentry.sender_for(context)
        {
            match sender.send(job).await {
                Ok(()) => {
                    self.shared
                        .reentry
                        .routed_jobs
                        .fetch_add(1, Ordering::Relaxed);
                    return self
                        .receive_result(receiver, "Wasm plugin guest call did not complete")
                        .await;
                }
                Err(error) => job = error.0,
            }
        }

        let send_guard = self.shared.send_gate.lock().await;
        self.ensure_accepting("Wasm plugin store is shutting down")?;
        let permit = self
            .shared
            .sender
            .reserve()
            .await
            .map_err(|_| self.terminal_error("Wasm plugin store driver is not running"))?;
        permit.send(StoreMessage::GuestCall(job));
        drop(send_guard);

        let result = self
            .receive_result(receiver, "Wasm plugin guest call did not complete")
            .await;
        // TEMPORARY DRAINDBG: the guest-call round trip a dispatch waits for.
        let micros = guest_call_started.elapsed().as_micros();
        if micros > 5000 {
            tracing::warn!(
                DRAINDBG = "call_guest_in",
                outermost,
                micros,
                tid = ?std::thread::current().id(),
                "DRAINDBG slow guest call"
            );
        }
        if outermost && !is_running_guest_call() {
            notify_guest_call_return();
        }
        result
    }

    /// Awaits host work in the active guest call's chain with the reentry depth incremented
    ///
    /// Host imports are polled by Wasmtime's own event loop, outside the guest call's task-local
    /// scope, so the context has to come from the Store's active-context registry
    /// Without an active guest call, the future runs without adding a reentry context
    ///
    /// # Errors
    ///
    /// Returns an error before polling the future if the reentry depth limit is reached
    pub async fn in_active_context<R>(
        &self,
        future: impl Future<Output = R>,
    ) -> wasmtime::Result<R> {
        let context = self
            .shared
            .reentry
            .current_context()
            .map(ReentryContext::child)
            .transpose()?;
        Ok(match context {
            Some(context) => scope(context, future).await,
            None => future.await,
        })
    }

    /// Runs synchronous host work on Tokio's blocking pool in the active guest call's chain
    ///
    /// Increments the reentry depth before spawning so callbacks inherit the same chain and
    /// root admission on the blocking thread
    /// Without an active guest call, the operation runs without adding a reentry context
    ///
    /// # Errors
    ///
    /// Returns an error if the reentry depth limit is reached or the blocking task fails
    /// The operation is not spawned when the depth check fails
    pub async fn spawn_blocking_in_active_context<R, F>(&self, operation: F) -> wasmtime::Result<R>
    where
        R: Send + 'static,
        F: FnOnce() -> R + Send + 'static,
    {
        let context = self
            .shared
            .reentry
            .current_context()
            .map(ReentryContext::child)
            .transpose()?;
        tokio::task::spawn_blocking(move || match context {
            Some(context) => sync_scope(context, operation),
            None => operation(),
        })
        .await
        .map_err(|error| {
            wasmtime::Error::msg(format!(
                "Synchronous Wasm plugin host operation failed: {error}"
            ))
        })
    }

    /// Runs synchronous host work on the injected blocking spawner while servicing
    /// callbacks routed back into this Store from the active chain.
    ///
    /// Unlike [`Self::spawn_blocking_in_active_context`], the blocking operation is
    /// registered as the active reentry target before it starts. Callbacks it fires
    /// synchronously into the same Store are run on the active host frame, so a host
    /// call that triggers a callback into its own plugin instance completes instead of
    /// waiting for the suspended outer guest call.
    ///
    /// # Errors
    ///
    /// Returns an error if the reentry depth limit is reached, the blocking task fails,
    /// or a reentrant callback fails. The operation is not spawned when the depth check
    /// fails.
    pub async fn pump_blocking_accessor<R, F>(
        &self,
        accessor: &Accessor<T>,
        operation: F,
    ) -> wasmtime::Result<R>
    where
        R: Send + 'static,
        F: FnOnce() -> R + Send + 'static,
    {
        let Some(context) = self.shared.reentry.current_context() else {
            return self.spawn_blocking_in_active_context(operation).await;
        };
        let context = context.child()?;
        // Register before the operation starts: it can fire a callback into this Store before
        // `spawn_blocking` returns, and a callback that finds no scope is queued on the driver,
        // which cannot run it while the outer guest call is suspended here.
        let (mut registration, mut reentry) = self.shared.reentry.register(context);
        // TEMPORARY DRAINDBG.
        let pump_started = std::time::Instant::now();
        let (result, receiver) = oneshot::channel();
        if let Err(error) = self.shared.spawner.spawn_blocking(Box::new(move || {
            let output = catch_unwind(AssertUnwindSafe(|| sync_scope(context, operation)))
                .map_err(|payload| panic_message(payload.as_ref()));
            let _ = result.send(output);
        })) {
            registration.close();
            return Err(wasmtime::Error::msg(format!(
                "Failed to spawn synchronous Wasm plugin host operation: {error}"
            )));
        }

        let output = pump_reentrant_jobs(accessor, &mut reentry, receiver).await;
        // TEMPORARY DRAINDBG: how long the host operation plus the callbacks it fired took.
        let micros = pump_started.elapsed().as_micros();
        if micros > 5000 {
            tracing::warn!(
                DRAINDBG = "pump_blocking_accessor",
                micros,
                tid = ?std::thread::current().id(),
                "DRAINDBG slow blocking operation"
            );
        }
        registration.close();
        reentry.close();
        while let Ok(job) = reentry.try_recv() {
            job.run_concurrent(accessor).await?;
        }

        output?.map_err(|message| {
            wasmtime::Error::msg(format!(
                "Synchronous Wasm plugin host operation panicked: {message}"
            ))
        })
    }

    /// Waits for an outbound host operation while servicing callbacks routed
    /// back into this Store from the active synchronous chain.
    pub async fn pump_reentry<R, S, F>(&self, store: &mut S, future: F) -> wasmtime::Result<R>
    where
        S: AsContextMut<Data = T> + Send,
        F: Future<Output = R> + Send,
    {
        let context = self.next_reentry_context()?;
        self.pump_reentry_with_context(store, context, future).await
    }

    fn next_reentry_context(&self) -> wasmtime::Result<ReentryContext> {
        self.shared
            .reentry
            .current_context()
            .or_else(|| self.shared.policy.inherited_context())
            .ok_or_else(|| {
                wasmtime::Error::msg("Wasm plugin reentry requires an active admitted causal chain")
            })?
            .child()
    }

    async fn pump_reentry_with_context<R, S, F>(
        &self,
        store: &mut S,
        context: ReentryContext,
        future: F,
    ) -> wasmtime::Result<R>
    where
        S: AsContextMut<Data = T> + Send,
        F: Future<Output = R> + Send,
    {
        let (registration, receiver) = self.shared.reentry.register(context);
        self.pump_registered(store, registration, receiver, context, future)
            .await
    }

    /// Services the callbacks routed to an already registered scope until `future` completes.
    async fn pump_registered<R, S, F>(
        &self,
        store: &mut S,
        mut registration: ReentryRegistration<T>,
        mut receiver: mpsc::Receiver<Box<dyn GuestStoreJob<T>>>,
        context: ReentryContext,
        future: F,
    ) -> wasmtime::Result<R>
    where
        S: AsContextMut<Data = T> + Send,
        F: Future<Output = R> + Send,
    {
        let scoped_future = scope(context, future);
        tokio::pin!(scoped_future);

        let output = loop {
            tokio::select! {
                Some(job) = receiver.recv() => {
                    job.run_reentrant(store.as_context_mut()).await?;
                }
                output = &mut scoped_future => break output,
            }
        };

        registration.close();
        receiver.close();
        while let Ok(job) = receiver.try_recv() {
            job.run_reentrant(store.as_context_mut()).await?;
        }
        Ok(output)
    }

    /// Runs synchronous host work through the injected blocking spawner while
    /// the active Store continues servicing reentrant callbacks.
    pub async fn pump_blocking<R, S, F>(&self, store: &mut S, operation: F) -> wasmtime::Result<R>
    where
        R: Send + 'static,
        S: AsContextMut<Data = T> + Send,
        F: FnOnce() -> R + Send + 'static,
    {
        let context = self.next_reentry_context()?;
        // Register before the operation starts: it can fire a callback into this Store before
        // `spawn_blocking` returns, and a callback that finds no scope is queued on the driver,
        // which cannot run it while the outer guest call is suspended here.
        let (registration, reentry) = self.shared.reentry.register(context);
        let (result, receiver) = oneshot::channel();
        if let Err(error) = self.shared.spawner.spawn_blocking(Box::new(move || {
            let output = catch_unwind(AssertUnwindSafe(|| sync_scope(context, operation)))
                .map_err(|payload| panic_message(payload.as_ref()));
            let _ = result.send(output);
        })) {
            drop(registration);
            return Err(wasmtime::Error::msg(format!(
                "Failed to spawn synchronous Wasm plugin host operation: {error}"
            )));
        }

        self.pump_registered(store, registration, reentry, context, receiver)
            .await?
            .map_err(|_| {
                wasmtime::Error::msg("Synchronous Wasm plugin host operation did not complete")
            })?
            .map_err(|message| {
                wasmtime::Error::msg(format!(
                    "Synchronous Wasm plugin host operation panicked: {message}"
                ))
            })
    }
}

/// Services guest calls routed back into the active host frame until the
/// outbound operation completes.
async fn pump_reentrant_jobs<T, R>(
    accessor: &Accessor<T>,
    jobs: &mut mpsc::Receiver<Box<dyn GuestStoreJob<T>>>,
    result: oneshot::Receiver<Result<R, String>>,
) -> wasmtime::Result<Result<R, String>>
where
    T: Send + 'static,
{
    let mut result = std::pin::pin!(result);
    let mut jobs_open = true;
    loop {
        if jobs_open {
            tokio::select! {
                job = jobs.recv() => match job {
                    Some(job) => {
                        // TEMPORARY DRAINDBG.
                        let started = std::time::Instant::now();
                        job.run_concurrent(accessor).await?;
                        let micros = started.elapsed().as_micros();
                        if micros > 5000 {
                            tracing::warn!(
                                DRAINDBG = "pump_reentrant_jobs",
                                micros,
                                tid = ?std::thread::current().id(),
                                "DRAINDBG slow reentrant job"
                            );
                        }
                    }
                    None => jobs_open = false,
                },
                output = &mut result => {
                    return output.map_err(|_| {
                        wasmtime::Error::msg(
                            "Synchronous Wasm plugin host operation did not complete",
                        )
                    });
                }
            }
        } else {
            return (&mut result).await.map_err(|_| {
                wasmtime::Error::msg("Synchronous Wasm plugin host operation did not complete")
            });
        }
    }
}

/// Non-cloneable owner of one Store driver's lifecycle capability.
pub struct StoreExecutor<T: 'static, P>
where
    P: StorePolicy,
{
    handle: StoreHandle<T, P>,
    control: mpsc::UnboundedSender<ControlMessage>,
    terminal_admitted: AtomicBool,
}

impl<T, P> StoreExecutor<T, P>
where
    T: Send + 'static,
    P: StorePolicy,
{
    pub async fn start(
        store: Store<T>,
        policy: P,
        spawner: Arc<dyn RuntimeSpawner>,
    ) -> wasmtime::Result<Self> {
        let (sender, receiver) = mpsc::channel(STORE_QUEUE_CAPACITY);
        let (control, control_receiver) = mpsc::unbounded_channel();
        let lifecycle = Lifecycle::new();
        let accepting = Arc::new(AtomicBool::new(true));
        let guest_call_failure = Arc::new(GuestCallFailure::default());
        let shared = Arc::new(StoreShared {
            sender,
            accepting: Arc::clone(&accepting),
            guest_call_failure,
            send_gate: Mutex::new(()),
            lifecycle: Arc::clone(&lifecycle),
            reentry: Arc::new(ReentryState::new()),
            policy,
            spawner: Arc::clone(&spawner),
        });
        let (ready_sender, ready) = oneshot::channel();
        let policy_name = P::NAME;
        let admission_id = shared.policy.admission_id();
        let task_guard = DriverTaskGuard::new(Arc::clone(&lifecycle), accepting);

        tracing::debug!(
            wasm_plugin_policy = policy_name,
            "Starting Wasm plugin store driver"
        );

        spawner
            .spawn(Box::pin(async move {
                let mut task_guard = task_guard;
                run_driver(
                    store,
                    receiver,
                    control_receiver,
                    Arc::clone(&task_guard.lifecycle),
                    ready_sender,
                    policy_name,
                    admission_id,
                )
                .await;
                task_guard.disarm();
            }))
            .map_err(|error| {
                let error = Arc::new(DriverError::new(format!(
                    "Failed to spawn Wasm plugin store driver: {error}"
                )));
                lifecycle.transition(DriverState::Failed(Arc::clone(&error)));
                wasmtime::Error::msg(error.to_string())
            })?;

        if ready.await.is_err() {
            let mut join = lifecycle.subscribe();
            return match join.wait().await {
                Ok(()) => Err(wasmtime::Error::msg(
                    "Wasm plugin store driver stopped before becoming ready",
                )),
                Err(error) => Err(wasmtime::Error::msg(format!(
                    "Wasm plugin store driver failed before becoming ready: {error:#}"
                ))),
            };
        }

        Ok(Self {
            handle: StoreHandle { shared },
            control,
            terminal_admitted: AtomicBool::new(false),
        })
    }

    #[must_use]
    pub fn handle(&self) -> StoreHandle<T, P> {
        self.handle.clone()
    }

    #[must_use]
    pub fn driver_join(&self) -> DriverJoin {
        self.handle.driver_join()
    }

    fn claim_terminal(&self) -> bool {
        self.terminal_admitted
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }

    /// Stops admission and drains accepted work without running a final Store
    /// operation.
    ///
    /// This is intended for a Store whose owner was prepared but never admitted
    /// to its higher-level lifecycle. Dropping an executor without either
    /// calling this method or completing [`Self::shutdown`] remains an error.
    pub fn discard(&self) {
        if !self.claim_terminal() {
            return;
        }

        self.handle.shared.accepting.store(false, Ordering::Release);

        let _ = self.control.send(ControlMessage::Discarded);
    }
}

impl<T> StoreExecutor<T, LegacySyncReentry>
where
    T: Send + 'static,
{
    /// Stops admission, drains accepted work, runs one final Store operation,
    /// drains Wasmtime tasks, and drops the Store.
    pub async fn shutdown<R, F>(&self, call: F) -> wasmtime::Result<R>
    where
        F: for<'a> FnOnce(&'a Accessor<T>) -> StoreFuture<'a, R> + Send + 'static,
        R: Send + 'static,
    {
        let (result, receiver) = oneshot::channel();
        self.handle
            .ensure_accepting("Wasm plugin store is already shutting down")?;
        let inherited_context = self.handle.shared.policy.inherited_context();
        let (context, root_admission) = if let Some(context) = inherited_context {
            if self.handle.shared.reentry.has_active_context(context) {
                return Err(wasmtime::Error::msg(
                    "Cannot shut down a Wasm plugin store while it is active in the current causal chain",
                ));
            }
            tracing::trace!(
                wasm_plugin_admission_id = context.admission_id,
                wasm_plugin_chain_id = context.chain_id,
                wasm_plugin_reentry_depth = context.depth,
                "Inherited legacy Wasm plugin root admission"
            );
            (context, None)
        } else {
            let admission = self.handle.shared.policy.acquire_root(AdmissionClass::Normal).await?;
            (admission.context(), Some(admission))
        };
        let send_guard = self.handle.shared.send_gate.lock().await;
        self.handle
            .ensure_accepting("Wasm plugin store is already shutting down")?;
        let permit = self.handle.shared.sender.reserve().await.map_err(|_| {
            self.handle
                .terminal_error("Wasm plugin store driver is not running")
        })?;

        if !self.claim_terminal() {
            drop(permit);
            return Err(wasmtime::Error::msg(
                "Already shutting down wasm plugin store",
            ));
        }

        self.handle.shared.accepting.store(false, Ordering::Release);

        permit.send(StoreMessage::Shutdown {
            job: Box::new(ShutdownStoreCall {
                call,
                result,
                context,
            }),
            root_admission,
        });
        drop(send_guard);

        let result = receiver.await;
        let mut join = self.driver_join();
        let terminal = join.wait().await;
        reconcile_shutdown_result(result, terminal)
    }
}

pub fn reconcile_shutdown_result<R>(
    result: Result<wasmtime::Result<R>, oneshot::error::RecvError>,
    terminal: Result<(), Arc<DriverError>>,
) -> wasmtime::Result<R> {
    match (result, terminal) {
        (Ok(Err(error)), _) => Err(error),
        (Ok(Ok(value)), Ok(())) => Ok(value),
        (Ok(Ok(_)) | Err(_), Err(error)) => Err(wasmtime::Error::msg(format!(
            "Wasm plugin store driver failed: {error:#}"
        ))),
        (Err(_), Ok(())) => Err(wasmtime::Error::msg(
            "Wasm plugin store shutdown did not complete",
        )),
    }
}

impl<T, P> Deref for StoreExecutor<T, P>
where
    P: StorePolicy,
{
    type Target = StoreHandle<T, P>;

    fn deref(&self) -> &Self::Target {
        &self.handle
    }
}

impl<T, P> Drop for StoreExecutor<T, P>
where
    P: StorePolicy,
{
    fn drop(&mut self) {
        if self.terminal_admitted.load(Ordering::Acquire)
            || !self.handle.shared.accepting.swap(false, Ordering::AcqRel)
        {
            return;
        }

        let _ = self.control.send(ControlMessage::Dropped);
    }
}

pub type LegacyStore<T> = StoreExecutor<T, LegacySyncReentry>;

fn control_result(message: Option<&ControlMessage>) -> wasmtime::Result<()> {
    match message {
        Some(ControlMessage::Dropped) => Err(wasmtime::Error::msg(
            "Wasm plugin store lifecycle control was dropped before shutdown",
        )),
        Some(ControlMessage::Discarded) => Ok(()),
        None => Err(wasmtime::Error::msg(
            "Wasm plugin store submission channel closed before shutdown",
        )),
    }
}

/// Drives `driver` until it completes, marking every poll with [`DRIVING_ADMISSION`].
///
/// The mark may not span suspension points: another task polled on the same thread afterwards
/// would still look like it runs inside a guest call.
async fn drive_marked<R>(driver: impl Future<Output = R>, admission_id: u64) -> R {
    let mut driver = std::pin::pin!(driver);
    // TEMPORARY DRAINDBG: how long the driver goes unpolled, i.e. whether a queued job waits for
    // the driver to be scheduled or for the guest call itself to execute.
    let mut last_poll = std::time::Instant::now();
    poll_fn(|cx| {
        let gap = last_poll.elapsed();
        if gap.as_millis() > 5 {
            tracing::warn!(
                DRAINDBG = "drive_marked",
                gap_micros = gap.as_micros(),
                tid = ?std::thread::current().id(),
                "DRAINDBG driver not polled for a while"
            );
        }
        let _driving = DrivingAdmissionGuard::enter(admission_id);
        let output = driver.as_mut().poll(cx);
        last_poll = std::time::Instant::now();
        output
    })
    .await
}

#[allow(clippy::too_many_lines)]
async fn run_driver<T>(
    mut store: Store<T>,
    mut receiver: mpsc::Receiver<StoreMessage<T>>,
    mut control: mpsc::UnboundedReceiver<ControlMessage>,
    lifecycle: Arc<Lifecycle>,
    ready: oneshot::Sender<()>,
    policy_name: &'static str,
    admission_id: u64,
) where
    T: Send + 'static,
{
    let loop_lifecycle = Arc::clone(&lifecycle);
    let result = drive_marked(
        store.run_concurrent(async move |accessor| -> wasmtime::Result<()> {
            loop_lifecycle.transition(DriverState::Accepting);
            let _ = ready.send(());
            let mut active_calls = FuturesUnordered::new();

            loop {
                let message = tokio::select! {
                    biased;
                    Some(()) = active_calls.next(), if !active_calls.is_empty() => {
                        continue;
                    }
                    Some(control_message) = control.recv() => {
                        loop_lifecycle.transition(DriverState::Draining);
                        receiver.close();
                        loop {
                            let message = tokio::select! {
                                Some(()) = active_calls.next(), if !active_calls.is_empty() => {
                                    continue;
                                }
                                message = receiver.recv() => message,
                            };
                            let Some(message) = message else {
                                break;
                            };
                            match message {
                                StoreMessage::Call(job) => {
                                    active_calls.push(accessor.spawn(ConcurrentTask(job))?);
                                }
                                StoreMessage::GuestCall(job) => {
                                    active_calls.push(accessor.spawn(ConcurrentGuestTask(job))?);
                                }
                                StoreMessage::Shutdown {
                                    job,
                                    root_admission,
                                } => {
                                    while active_calls.next().await.is_some() {}
                                    loop_lifecycle.transition(DriverState::Stopping);
                                    let result = job.run(accessor).await;
                                    poll_fn(|cx| accessor.poll_no_interesting_tasks(cx)).await;
                                    drop(root_admission);
                                    return result;
                                }
                            }
                        }
                        while active_calls.next().await.is_some() {}
                        loop_lifecycle.transition(DriverState::Stopping);
                        poll_fn(|cx| accessor.poll_no_interesting_tasks(cx)).await;
                        return control_result(Some(&control_message));
                    }
                    message = receiver.recv() => message,
                };
                let Some(message) = message else {
                    loop_lifecycle.transition(DriverState::Draining);
                    while active_calls.next().await.is_some() {}
                    // The owner may have closed the submission channel while its control signal is still pending
                    let control_message = control.recv().await;
                    loop_lifecycle.transition(DriverState::Stopping);
                    poll_fn(|cx| accessor.poll_no_interesting_tasks(cx)).await;
                    return control_result(control_message.as_ref());
                };

                match message {
                    StoreMessage::Call(job) => {
                        active_calls.push(accessor.spawn(ConcurrentTask(job))?);
                        tokio::task::yield_now().await;
                    }
                    StoreMessage::GuestCall(job) => {
                        active_calls.push(accessor.spawn(ConcurrentGuestTask(job))?);
                    }
                    StoreMessage::Shutdown {
                        job,
                        root_admission,
                    } => {
                        loop_lifecycle.transition(DriverState::Draining);
                        receiver.close();
                        while active_calls.next().await.is_some() {}
                        loop_lifecycle.transition(DriverState::Stopping);
                        let result = job.run(accessor).await;
                        poll_fn(|cx| accessor.poll_no_interesting_tasks(cx)).await;
                        drop(root_admission);
                        return result;
                    }
                }
            }
        }),
        admission_id,
    )
    .await;
    drop(store);

    match result {
        Ok(Ok(())) => lifecycle.transition(DriverState::Stopped),
        Ok(Err(error)) | Err(error) => {
            let error = Arc::new(DriverError::new(error.to_string()));
            tracing::error!(
                %error,
                wasm_plugin_policy = policy_name,
                "Wasm plugin store driver stopped"
            );
            lifecycle.transition(DriverState::Failed(error));
        }
    }
}

struct DriverTaskGuard {
    lifecycle: Arc<Lifecycle>,
    accepting: Arc<AtomicBool>,
    armed: bool,
}

impl DriverTaskGuard {
    const fn new(lifecycle: Arc<Lifecycle>, accepting: Arc<AtomicBool>) -> Self {
        Self {
            lifecycle,
            accepting,
            armed: true,
        }
    }

    fn disarm(&mut self) {
        self.accepting.store(false, Ordering::Release);
        self.armed = false;
    }
}

impl Drop for DriverTaskGuard {
    fn drop(&mut self) {
        if self.armed {
            self.accepting.store(false, Ordering::Release);
            self.lifecycle
                .transition(DriverState::Failed(Arc::new(DriverError::new(
                    "Wasm plugin store driver task ended without reporting a terminal state",
                ))));
        }
    }
}

fn panic_message(payload: &(dyn Any + Send)) -> String {
    payload.downcast_ref::<&str>().map_or_else(
        || {
            payload
                .downcast_ref::<String>()
                .cloned()
                .unwrap_or_else(|| "unknown panic payload".to_owned())
        },
        |message| (*message).to_owned(),
    )
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use std::sync::Arc;

    use tokio::{
        sync::{mpsc, oneshot},
        time::{Duration, timeout},
    };
    use wasmtime::{Config, Engine, Store};

    use super::{ControlMessage, DriverState, Lifecycle, run_driver};

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn submission_close_waits_for_owner_control() {
        let mut config = Config::new();
        config.wasm_component_model(true);
        config.wasm_component_model_async(true);
        config.concurrency_support(true);
        let engine = Engine::new(&config).expect("test engine");
        let store = Store::new(&engine, ());
        let (sender, receiver) = mpsc::channel(1);
        let (control_sender, control_receiver) = mpsc::unbounded_channel();
        let lifecycle = Lifecycle::new();
        let (ready_sender, ready) = oneshot::channel();
        let driver = tokio::spawn(run_driver(
            store,
            receiver,
            control_receiver,
            Arc::clone(&lifecycle),
            ready_sender,
            "test",
            1,
        ));

        timeout(Duration::from_secs(10), async {
            ready.await.expect("driver should become ready");
            drop(sender);
            while lifecycle.state() != DriverState::Draining {
                tokio::task::yield_now().await;
            }
            assert!(control_sender.send(ControlMessage::Dropped).is_ok());
            driver.await.expect("driver task should finish");
        })
        .await
        .expect("owner control should stop the driver");

        let mut join = lifecycle.subscribe();
        let error = join
            .wait()
            .await
            .expect_err("owner drop should fail the driver");
        assert!(error.to_string().contains("control was dropped"));
    }
}
