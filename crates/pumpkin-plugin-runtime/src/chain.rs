use std::{
    collections::VecDeque,
    future::Future,
    sync::{
        Arc, Mutex, MutexGuard, PoisonError,
        atomic::{AtomicU64, Ordering},
    },
    time::Instant,
};

use tokio::sync::watch;

pub const MAX_SYNC_REENTRY_DEPTH: usize = 64;

/// The classes a guest call root can be admitted in.
///
/// A Store serves one guest call at a time, so what a caller waits for is the queue in front of it.
/// A [`Normal`](Self::Normal) request is served before a [`Background`](Self::Background) one,
/// whatever order they arrived in: world generation only wants throughput from the plugin, while
/// the server's own timeline (a tick, a player action) has to keep running. Background work must
/// not be able to put itself in front of that, which is the whole point: a slow plugin bounds a
/// background call's wait, not the server's.
///
/// The trade-off is deliberate and one-directional: background work waits while a normal request
/// is queued, so a server that fires normal events continuously delays generation instead of the
/// other way round. Generation has no deadline, the tick does.
///
/// The bound this buys is exact and narrow: it takes the *backlog* out of a normal caller's wait,
/// so N queued background requests cost the same as one. On the fresh-world forceload burst
/// (225 chunks, 16 generation workers) that moved the tick from P95 1886ms/P99 3337ms to
/// P95 495ms/P99 763ms, and the plugin's world-tick dispatch stopped stalling behind generation
/// entirely. It does not remove the one guest call that is already running: a guest slowed
/// *uniformly per call* still makes a normal caller pay that call's remaining time on every
/// dispatch (where the old FIFO sometimes found the queue drained), so a uniformly slow guest is
/// not what this fixes. A guest that is slow in bursts — a vein draw, a compile, a save — is.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AdmissionClass {
    Normal,
    Background,
}

impl AdmissionClass {
    const COUNT: usize = 2;

    const fn slot(self) -> usize {
        match self {
            Self::Normal => 0,
            Self::Background => 1,
        }
    }
}

static NEXT_ROOT_ADMISSION_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Debug)]
pub struct RootAdmission {
    state: Arc<RootAdmissionState>,
}

#[derive(Debug)]
struct RootAdmissionState {
    id: u64,
    next_chain_id: AtomicU64,
    /// Who holds the admission, and the requests waiting for it.
    gate: Mutex<AdmissionGate>,
    /// Bumped whenever the admission changes hands. A queued request looks at the gate again on
    /// every change; the version of a watch channel also carries a change made after a request
    /// last looked, so a handover between the look and the wait is not missed.
    handovers: watch::Sender<u64>,
}

#[derive(Debug)]
struct AdmissionGate {
    /// Whether a guest call holds the admission.
    held: bool,
    /// Number the next request is queued under.
    next_number: u64,
    /// Queued request numbers per class, oldest first.
    queued: [VecDeque<u64>; AdmissionClass::COUNT],
}

impl AdmissionGate {
    /// The slot whose oldest request the gate serves next: the most important class with a queued
    /// request, or `None` while a guest call holds the admission.
    fn serviceable(&self) -> Option<usize> {
        if self.held {
            return None;
        }
        (0..AdmissionClass::COUNT).find(|slot| !self.queued[*slot].is_empty())
    }
}

/// One request for the admission, waiting in the queue until it is served.
///
/// Dropping it withdraws the request, or gives the admission back when it had already been served,
/// which is how a caller that goes away without ever holding the admission releases it.
struct AdmissionRequest {
    state: Arc<RootAdmissionState>,
    class: AdmissionClass,
    number: u64,
}

impl AdmissionRequest {
    /// Takes the admission when this request is the one the gate serves next.
    fn serve(&mut self, gate: &mut AdmissionGate) -> bool {
        let slot = self.class.slot();
        if gate.serviceable() != Some(slot) {
            return false;
        }
        if gate.queued[slot].front() != Some(&self.number) {
            return false;
        }
        gate.queued[slot].pop_front();
        gate.held = true;
        true
    }
}

impl Drop for AdmissionRequest {
    fn drop(&mut self) {
        {
            let mut gate = self.state.lock_gate();
            let queue = &mut gate.queued[self.class.slot()];
            if let Some(position) = queue.iter().position(|number| *number == self.number) {
                queue.remove(position);
            } else {
                gate.held = false;
            }
        }
        self.state.announce_handover();
    }
}

pub struct RootAdmissionGuard {
    context: ReentryContext,
    acquired_at: Instant,
    /// Holds the admission; dropping it is the whole point of the field.
    _request: AdmissionRequest,
}

impl RootAdmissionState {
    fn lock_gate(&self) -> MutexGuard<'_, AdmissionGate> {
        self.gate.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Lets every queued request look at the gate again.
    fn announce_handover(&self) {
        self.handovers
            .send_modify(|version| *version = version.wrapping_add(1));
    }
}

impl RootAdmission {
    pub fn new() -> Self {
        Self {
            state: Arc::new(RootAdmissionState {
                id: NEXT_ROOT_ADMISSION_ID.fetch_add(1, Ordering::Relaxed),
                next_chain_id: AtomicU64::new(1),
                gate: Mutex::new(AdmissionGate {
                    held: false,
                    next_number: 0,
                    queued: Default::default(),
                }),
                handovers: watch::channel(0).0,
            }),
        }
    }

    pub fn root_context(&self) -> ReentryContext {
        ReentryContext {
            admission_id: self.state.id,
            chain_id: self.state.next_chain_id.fetch_add(1, Ordering::Relaxed),
            depth: 0,
        }
    }

    /// The id of this admission authority, shared by every context it hands out.
    #[must_use]
    pub fn id(&self) -> u64 {
        self.state.id
    }

    pub fn admits(&self, context: ReentryContext) -> bool {
        context.admission_id == self.state.id
    }

    /// Waits for the admission in `class`, and returns the guard that holds it until dropped.
    pub async fn acquire_root(
        &self,
        class: AdmissionClass,
    ) -> wasmtime::Result<RootAdmissionGuard> {
        let context = self.root_context();
        let waiting_since = Instant::now();
        tracing::trace!(
            wasm_plugin_admission_id = context.admission_id,
            wasm_plugin_chain_id = context.chain_id,
            wasm_plugin_admission_class = ?class,
            "Waiting for legacy Wasm plugin root admission"
        );

        // Subscribed before the gate is looked at, so a handover in between is not missed.
        let mut handovers = self.state.handovers.subscribe();
        let mut request = self.queue(class);
        loop {
            {
                let mut gate = self.state.lock_gate();
                if request.serve(&mut gate) {
                    break;
                }
            }
            if handovers.changed().await.is_err() {
                return Err(wasmtime::Error::msg("Wasm plugin root admission is gone"));
            }
        }

        let acquired_at = Instant::now();
        tracing::trace!(
            wasm_plugin_admission_id = context.admission_id,
            wasm_plugin_chain_id = context.chain_id,
            wasm_plugin_admission_wait_micros =
                acquired_at.duration_since(waiting_since).as_micros(),
            "Acquired legacy Wasm plugin root admission"
        );

        Ok(RootAdmissionGuard {
            context,
            acquired_at,
            _request: request,
        })
    }

    /// Queues a request, in request order within its class.
    fn queue(&self, class: AdmissionClass) -> AdmissionRequest {
        let mut gate = self.state.lock_gate();
        let number = gate.next_number;
        gate.next_number = gate.next_number.wrapping_add(1);
        gate.queued[class.slot()].push_back(number);
        drop(gate);
        AdmissionRequest {
            state: Arc::clone(&self.state),
            class,
            number,
        }
    }
}

impl RootAdmissionGuard {
    pub const fn context(&self) -> ReentryContext {
        self.context
    }
}

impl Drop for RootAdmissionGuard {
    fn drop(&mut self) {
        tracing::trace!(
            wasm_plugin_admission_id = self.context.admission_id,
            wasm_plugin_chain_id = self.context.chain_id,
            wasm_plugin_admission_held_micros = self.acquired_at.elapsed().as_micros(),
            "Releasing legacy Wasm plugin root admission"
        );
        // `request` gives the admission back when it drops.
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct ReentryContext {
    pub admission_id: u64,
    pub chain_id: u64,
    pub depth: usize,
}

impl ReentryContext {
    pub fn current() -> Option<Self> {
        REENTRY_CONTEXT.try_with(|context| *context).ok()
    }

    pub fn child(self) -> wasmtime::Result<Self> {
        if self.depth >= MAX_SYNC_REENTRY_DEPTH {
            return Err(wasmtime::Error::msg(format!(
                "Wasm plugin synchronous reentry exceeded the maximum depth of {MAX_SYNC_REENTRY_DEPTH}"
            )));
        }

        Ok(Self {
            admission_id: self.admission_id,
            chain_id: self.chain_id,
            depth: self.depth + 1,
        })
    }
}

tokio::task_local! {
    static REENTRY_CONTEXT: ReentryContext;
}

pub async fn scope<T>(context: ReentryContext, future: impl Future<Output = T>) -> T {
    REENTRY_CONTEXT.scope(context, future).await
}

pub fn sync_scope<T>(context: ReentryContext, operation: impl FnOnce() -> T) -> T {
    REENTRY_CONTEXT.sync_scope(context, operation)
}
