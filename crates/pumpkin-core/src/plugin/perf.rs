//! Where a plugin's cost goes: host→guest calls per tick, by kind, and how long the host waited.
//!
//! Set `PUMPKIN_PLUGIN_PERF=<ticks>` in the server's environment to turn it on; `<ticks>` is the
//! report interval (a summary is logged every that many server ticks). Unset or `0` and every
//! instrumented call site pays one atomic load and a compare — nothing else.
//!
//! Why the *host* side: a plugin's idle cost is not only the work the guest does, it is also the
//! number of times the host crosses into the guest and waits for the answer. A tick with nothing
//! built still fires one event per registered listener per tick (`server-tick-start`,
//! `server-tick-end`, `world-tick-event`, and one `block-entity-tick-event` per plugin block
//! entity type), and every one of those is a separate component call the ticker thread blocks on.
//! Counting them by kind, next to the guest's own per-tick report (the plugin's `perf.dart`),
//! separates "the guest is doing work" from "the host is queueing calls".
//!
//! The report is a delta over the interval, so `per_tick` is the number to read:
//!
//! ```text
//! [perf-host] ticks=100 guest_calls=740 (7.40/tick) waited_total_us=7400 (74.00/tick)
//! [perf-host]   kind=event.world-tick-event calls=100 per_tick=1.00 total_us=3200 avg_us=32 max_us=90
//! ```

use std::{
    collections::HashMap,
    sync::{
        LazyLock, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

/// What one instrumented kind cost: how many calls, and the time the host waited for them.
#[derive(Default, Clone, Copy)]
struct Kind {
    calls: u64,
    total: Duration,
    max: Duration,
}

/// Counts since the previous report, plus the running totals for the whole process.
#[derive(Default)]
struct Report {
    kinds: HashMap<&'static str, Kind>,
    calls: u64,
    total: Duration,
}

/// The report interval in ticks from `PUMPKIN_PLUGIN_PERF`, `0` when off.
///
/// Read once, at the first instrumented call: the variable is part of the server's environment,
/// which does not change while it runs.
static INTERVAL: LazyLock<u64> = LazyLock::new(|| {
    std::env::var("PUMPKIN_PLUGIN_PERF")
        .ok()
        .and_then(|value| value.trim().parse().ok())
        .unwrap_or(0)
});
static STATE: LazyLock<Mutex<Report>> = LazyLock::new(|| Mutex::new(Report::default()));
static TICKS: AtomicU64 = AtomicU64::new(0);

/// Whether the host-side attribution is on.
pub fn enabled() -> bool {
    *INTERVAL != 0
}

/// Starts timing one host→guest call, or `None` while the attribution is off.
///
/// Pair it with [`record`] on the same kind, after the call returned:
///
/// ```ignore
/// let started = perf::start();
/// let result = store.call_guest_in(class, call).await;
/// perf::record("event.world-tick-event", started);
/// ```
#[inline]
pub fn start() -> Option<Instant> {
    enabled().then(Instant::now)
}

/// Adds one completed host→guest call of `kind` to the current interval.
///
/// Does nothing when `started` is `None`, so a call site disabled by
/// [`enabled`] is free.
pub fn record(kind: &'static str, started: Option<Instant>) {
    let Some(started) = started else {
        return;
    };
    let elapsed = started.elapsed();
    let mut report = STATE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let entry = report.kinds.entry(kind).or_default();
    entry.calls += 1;
    entry.total += elapsed;
    entry.max = entry.max.max(elapsed);
    report.calls += 1;
    report.total += elapsed;
}

/// Counts a server tick, and logs the attribution of the interval it closes.
///
/// Called from the server ticker (`server/ticker.rs`) once per tick, whatever the plugin does.
pub fn on_tick() {
    let every = *INTERVAL;
    if every == 0 {
        return;
    }
    let ticks = TICKS.fetch_add(1, Ordering::Relaxed) + 1;
    if ticks % every != 0 {
        return;
    }
    let mut report = STATE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let taken = std::mem::take(&mut *report);
    drop(report);

    let per_tick = |value: u64| value as f64 / every as f64;
    tracing::info!(
        "[perf-host] ticks={every} guest_calls={} ({:.2}/tick) waited_total_us={} ({:.2}/tick)",
        taken.calls,
        per_tick(taken.calls),
        taken.total.as_micros(),
        per_tick(taken.total.as_micros() as u64),
    );
    let mut kinds: Vec<_> = taken.kinds.into_iter().collect();
    // Slowest total first: the interesting kinds are at the top of the log.
    kinds.sort_unstable_by_key(|(_, kind)| std::cmp::Reverse(kind.total));
    for (name, kind) in kinds {
        tracing::info!(
            "[perf-host]   kind={name} calls={} per_tick={:.2} total_us={} avg_us={:.1} max_us={}",
            kind.calls,
            per_tick(kind.calls),
            kind.total.as_micros(),
            kind.total.as_micros() as f64 / kind.calls as f64,
            kind.max.as_micros(),
        );
    }
}
