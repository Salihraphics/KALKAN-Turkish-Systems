//! KOR — Kernel Operations Runtime: the KALKAN background engine.
//!
//! KOR is ordinary **user-mode** code: threads inside the KALKAN process.
//! "Kernel" is part of its name only; it does not run in Windows kernel
//! mode, use or install a driver, or form part of the Windows kernel.
//! (Previously named GoodBoyDPI.)
//!
//! **Scope:** lifecycle management, real process metrics and read-only
//! network counters. No DPI circumvention, packet capture or packet
//! modification exists.
//!
//! # Threads
//!
//! ```text
//!  UI thread                 controller "kalkan-engine"          worker "kor-core"
//!  ─────────                 ──────────────────────────          ────────────────────────
//!  send(Command) ──Msg::Cmd──► recv() (blocks, no polling)
//!                              owns Lifecycle                     spawned on Start
//!                              spawn worker ─────────────────────► init (probe) ─┐
//!                              ◄──────────Msg::Worker(Ready)──────────────────────┘
//!  waker() ◄── emit(Event) ─── Starting → Running
//!  try_recv()                                                     recv_timeout(interval)
//!                              ◄──────────Msg::Worker(Sample)──── sample CPU/memory + network
//!  send(Stop) ──────────────►  Running → Stopping ─WorkerCtl::Stop─► loop exits
//!                              ◄──────────Msg::Worker(Exited)──── (drop guard, always)
//!                              join() → Stopping → Stopped
//! ```
//!
//! * Only the controller changes state or emits events, so event order is
//!   deterministic and every `StateChanged` is a legal transition.
//! * The controller sleeps in `recv()`. The worker sleeps in
//!   `recv_timeout(sample_interval)`. Neither ever busy-waits.
//! * Worker messages carry a generation number. Messages from an earlier
//!   worker are ignored.
//! * The worker's drop guard sends `Exited` even if it panics, and the
//!   controller joins it only after that. A join therefore never waits on a
//!   live loop, and a panic is reported as an error.

use std::any::Any;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crate::lifecycle::{EngineState, Lifecycle, Transition, TransitionError};
use crate::link::{EngineLink, Waker};
use crate::protocol::{Command, DiagLevel, EngineStatus, Event};
use kalkan_net::{InterfaceChange, NetTracker, NetworkMonitor, NetworkStatus, Snapshot};

/// Engine name (KOR — Kernel Operations Runtime; previously GoodBoyDPI).
/// A user-mode engine: "Kernel" does not mean Windows kernel mode.
pub const ENGINE_NAME: &str = "KOR";
/// Display identity.
pub const ENGINE_TITLE: &str = "KOR — Kernel Operations Runtime";
/// What the engine does today, shown next to its identity.
pub const ENGINE_SCOPE: &str =
    "Read-only monitoring only. No DPI circumvention or packet processing.";
/// Name of the worker thread (it appears in log lines). Log files written
/// before the rename contain the previous name, `goodboydpi-core`; the log
/// viewer reads both, because it does not depend on thread names.
pub const WORKER_THREAD_NAME: &str = "kor-core";

/// Accepted range for [`Command::SetSampleInterval`] (matches the
/// configuration's `monitor.sample_interval_ms` bounds).
pub const MIN_SAMPLE_INTERVAL: Duration = Duration::from_millis(250);
pub const MAX_SAMPLE_INTERVAL: Duration = Duration::from_secs(60);

/// How long `Shutdown` waits for the worker to exit before giving up on it.
pub const WORKER_STOP_TIMEOUT: Duration = Duration::from_secs(5);

/// Process-wide count of live engine threads (controllers + workers).
static LIVE_ENGINE_THREADS: AtomicUsize = AtomicUsize::new(0);

/// Number of engine threads currently alive in this process.
pub fn live_engine_threads() -> usize {
    LIVE_ENGINE_THREADS.load(Ordering::SeqCst)
}

// ── Process metrics source ───────────────────────────────────────────────────

/// One reading of the KALKAN process's own resource usage.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProcessSample {
    /// Total CPU time (user + kernel) consumed so far.
    pub cpu_time: Duration,
    /// Current working set in bytes.
    pub memory_bytes: u64,
}

/// A source of [`ProcessSample`]s. Implemented with Win32 calls by the app.
pub trait ProcessProbe: Send {
    fn sample(&mut self) -> Result<ProcessSample, String>;
}

/// Creates the probe during worker initialisation. An `Err` is a start-up
/// failure that is reported to the user.
pub type ProbeFactory = Arc<dyn Fn() -> Result<Box<dyn ProcessProbe>, String> + Send + Sync>;

/// Creates the read-only network monitor during worker initialisation. An
/// `Err` is **not** fatal: the engine runs and reports why network data is
/// unavailable.
pub type NetworkFactory = Arc<dyn Fn() -> Result<Box<dyn NetworkMonitor>, String> + Send + Sync>;

/// Engine configuration.
#[derive(Clone)]
pub struct EngineSpec {
    /// Interval between samples (process metrics and network) while Running.
    pub sample_interval: Duration,
    pub probe: ProbeFactory,
    /// `None` = network monitoring not configured.
    pub network: Option<NetworkFactory>,
}

impl std::fmt::Debug for EngineSpec {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EngineSpec")
            .field("sample_interval", &self.sample_interval)
            .finish_non_exhaustive()
    }
}

/// CPU usage in percent of one core between two samples.
pub fn cpu_percent(prev_cpu: Duration, cur_cpu: Duration, wall: Duration) -> Option<f32> {
    if wall.is_zero() {
        return None;
    }
    let used = cur_cpu.checked_sub(prev_cpu)?;
    Some((used.as_secs_f64() / wall.as_secs_f64() * 100.0) as f32)
}

// ── Messages ─────────────────────────────────────────────────────────────────

enum Msg {
    Cmd(Command),
    SetWaker(Waker),
    Worker { gen: u64, msg: WorkerMsg },
}

#[derive(Debug)]
enum WorkerMsg {
    Ready,
    InitFailed(String),
    /// One tick: process metrics (or why they failed) and network data.
    Sample {
        process: Result<(ProcessSample, Option<f32>), String>,
        net: Option<NetUpdate>,
    },
    Exited,
}

#[derive(Debug)]
struct NetUpdate {
    status: Result<NetworkStatus, String>,
    changes: Vec<InterfaceChange>,
}

/// The worker's network side: monitor + tracker + a reused snapshot buffer.
struct NetSampler {
    monitor: Box<dyn NetworkMonitor>,
    tracker: NetTracker,
    snap: Snapshot,
}

impl NetSampler {
    fn sample(&mut self) -> NetUpdate {
        match self.monitor.sample(&mut self.snap) {
            Ok(()) => {
                let (status, changes) = self.tracker.update(&self.snap);
                NetUpdate {
                    status: Ok(status),
                    changes,
                }
            }
            Err(e) => NetUpdate {
                status: Err(e.to_string()),
                changes: Vec::new(),
            },
        }
    }
}

enum WorkerCtl {
    Stop,
}

/// Counts a live engine thread; decrements on drop (also during unwinding).
struct ThreadCount(Arc<AtomicUsize>);

impl ThreadCount {
    fn new(per_handle: &Arc<AtomicUsize>) -> Self {
        per_handle.fetch_add(1, Ordering::SeqCst);
        LIVE_ENGINE_THREADS.fetch_add(1, Ordering::SeqCst);
        ThreadCount(per_handle.clone())
    }
}

impl Drop for ThreadCount {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
        LIVE_ENGINE_THREADS.fetch_sub(1, Ordering::SeqCst);
    }
}

/// Sends `Exited` when the worker body ends, however it ends.
struct ExitGuard {
    tx: Sender<Msg>,
    gen: u64,
}

impl Drop for ExitGuard {
    fn drop(&mut self) {
        let _ = self.tx.send(Msg::Worker {
            gen: self.gen,
            msg: WorkerMsg::Exited,
        });
    }
}

fn panic_message(p: &(dyn Any + Send)) -> String {
    if let Some(s) = p.downcast_ref::<&str>() {
        (*s).to_owned()
    } else if let Some(s) = p.downcast_ref::<String>() {
        s.clone()
    } else {
        "unknown panic payload".to_owned()
    }
}

// ── Worker ───────────────────────────────────────────────────────────────────

fn worker_main(gen: u64, spec: EngineSpec, ctl: Receiver<WorkerCtl>, tx: Sender<Msg>) {
    let interval = spec.sample_interval;
    // Declared first so it is dropped last: `Exited` is the final message.
    let _exit = ExitGuard {
        tx: tx.clone(),
        gen,
    };
    let send = |msg| {
        let _ = tx.send(Msg::Worker { gen, msg });
    };

    tracing::info!(gen, "engine worker initialising");
    let mut probe = match (spec.probe)() {
        Ok(p) => p,
        Err(e) => return send(WorkerMsg::InitFailed(e)),
    };
    let first = match probe.sample() {
        Ok(s) => s,
        Err(e) => {
            return send(WorkerMsg::InitFailed(format!(
                "process metrics unavailable: {e}"
            )))
        }
    };

    // Network monitoring is optional: a failure here is reported, not fatal.
    let mut net_init_error = None;
    let mut net = match spec.network.as_ref().map(|f| f()) {
        Some(Ok(monitor)) => {
            tracing::info!(monitor = monitor.name(), "network monitor initialised");
            Some(NetSampler {
                monitor,
                tracker: NetTracker::new(),
                snap: Snapshot::empty(),
            })
        }
        Some(Err(e)) => {
            tracing::warn!(error = %e, "network monitor unavailable");
            net_init_error = Some(e);
            None
        }
        None => None,
    };
    let net_update = |net: &mut Option<NetSampler>, init_err: &Option<String>| match net {
        Some(n) => Some(n.sample()),
        None => init_err.as_ref().map(|e| NetUpdate {
            status: Err(e.clone()),
            changes: Vec::new(),
        }),
    };

    // Initialisation succeeded. Only now may the engine report Running.
    send(WorkerMsg::Ready);
    send(WorkerMsg::Sample {
        process: Ok((first, None)),
        net: net_update(&mut net, &net_init_error),
    });

    let mut prev = (Instant::now(), first);
    let mut next = prev.0 + interval;
    loop {
        let wait = next.saturating_duration_since(Instant::now());
        match ctl.recv_timeout(wait) {
            Ok(WorkerCtl::Stop) | Err(RecvTimeoutError::Disconnected) => break,
            Err(RecvTimeoutError::Timeout) => {
                let now = Instant::now();
                // Fixed-rate schedule; skip missed ticks instead of bursting.
                next += interval;
                if next <= now {
                    next = now + interval;
                }
                let process = match probe.sample() {
                    Ok(s) => {
                        let cpu = cpu_percent(prev.1.cpu_time, s.cpu_time, now - prev.0);
                        prev = (now, s);
                        Ok((s, cpu))
                    }
                    Err(e) => Err(e),
                };
                send(WorkerMsg::Sample {
                    process,
                    net: net_update(&mut net, &net_init_error),
                });
            }
        }
    }
    // Release resources before announcing exit.
    drop(net);
    drop(probe);
    tracing::info!(gen, "engine worker stopped");
}

struct Worker {
    gen: u64,
    ctl: Sender<WorkerCtl>,
    handle: JoinHandle<()>,
}

// ── Controller ───────────────────────────────────────────────────────────────

struct Controller {
    rx: Receiver<Msg>,
    /// Cloned into workers so they can report back.
    tx: Sender<Msg>,
    events: Sender<Event>,
    waker: Option<Waker>,
    lc: Lifecycle,
    spec: EngineSpec,
    worker: Option<Worker>,
    gen: u64,
    /// Start-up error reported by the current worker, applied on `Exited`.
    init_error: Option<String>,
    sample_error_reported: bool,
    net_error_reported: bool,
    /// When the current run entered Running (for `EngineStatus::running_for`).
    running_since: Option<Instant>,
    status: EngineStatus,
    alive: Arc<AtomicUsize>,
}

enum Flow {
    Continue,
    Exit,
}

impl Controller {
    fn emit(&self, ev: Event) {
        tracing::debug!(?ev, "engine event");
        // The receiver is gone only if the handle was dropped; nothing to do.
        let _ = self.events.send(ev);
        self.wake();
    }

    fn wake(&self) {
        if let Some(w) = &self.waker {
            w();
        }
    }

    fn transition(&mut self, t: Transition) -> Result<(), TransitionError> {
        let (from, to) = self.lc.apply(t)?;
        tracing::info!(%from, %to, "engine state changed");
        self.status.state = Some(to);
        if to == EngineState::Running {
            self.running_since = Some(Instant::now());
        } else {
            // Metrics describe a running engine only.
            self.running_since = None;
            self.status.cpu_percent = None;
            self.status.memory_bytes = None;
            self.status.network = None;
            self.status.network_error = None;
            self.status.running_for = None;
        }
        self.emit(Event::StateChanged { from, to });
        Ok(())
    }

    fn reject(&self, cmd: Command, err: TransitionError) {
        tracing::warn!(?cmd, state = %err.from, "invalid state transition; command rejected");
        let state = err.from.label().to_lowercase();
        let message = match (cmd, err.from) {
            (Command::Start, EngineState::Running) => {
                "Start ignored: the engine is already running.".to_owned()
            }
            (Command::Stop, EngineState::Stopped) => {
                "Stop ignored: the engine is not running.".to_owned()
            }
            (c, _) => format!("{c:?} ignored: the engine is {state}."),
        };
        self.emit(Event::DiagnosticMessage {
            level: DiagLevel::Warning,
            message,
        });
    }

    fn fail(&mut self, message: String) {
        tracing::error!(%message, "engine failed");
        match self.transition(Transition::Fail) {
            Ok(()) => self.emit(Event::EngineError { message }),
            Err(e) => {
                // Should be unreachable; still never swallow the error.
                tracing::error!(%e, "could not enter Failed state");
                self.emit(Event::EngineError { message });
            }
        }
    }

    fn spawn_worker(&mut self) {
        self.gen += 1;
        self.init_error = None;
        self.sample_error_reported = false;
        self.net_error_reported = false;
        let (ctl_tx, ctl_rx) = mpsc::channel();
        let (gen, spec, tx) = (self.gen, self.spec.clone(), self.tx.clone());
        let alive = self.alive.clone();
        let spawned = thread::Builder::new()
            .name(WORKER_THREAD_NAME.into())
            .spawn(move || {
                let _count = ThreadCount::new(&alive);
                worker_main(gen, spec, ctl_rx, tx);
            });
        match spawned {
            Ok(handle) => {
                self.worker = Some(Worker {
                    gen,
                    ctl: ctl_tx,
                    handle,
                })
            }
            Err(e) => self.fail(format!("could not start the engine thread: {e}")),
        }
    }

    fn on_command(&mut self, cmd: Command) -> Flow {
        tracing::debug!(?cmd, state = %self.lc.state(), "command received");
        match cmd {
            Command::Start => match self.transition(Transition::Start) {
                Ok(()) => {
                    tracing::info!("engine initialisation requested");
                    self.spawn_worker();
                }
                Err(e) => self.reject(cmd, e),
            },
            Command::Stop => match self.transition(Transition::Stop) {
                Ok(()) => {
                    tracing::info!("engine shutdown requested");
                    self.signal_worker_stop();
                }
                Err(e) => self.reject(cmd, e),
            },
            Command::QueryStatus => self.emit_status(),
            Command::SetSampleInterval(d) => {
                if !(MIN_SAMPLE_INTERVAL..=MAX_SAMPLE_INTERVAL).contains(&d) {
                    tracing::warn!(?d, "sample interval rejected");
                    self.emit(Event::DiagnosticMessage {
                        level: DiagLevel::Warning,
                        message: format!(
                            "Sample interval {} ms ignored: allowed {}–{} ms.",
                            d.as_millis(),
                            MIN_SAMPLE_INTERVAL.as_millis(),
                            MAX_SAMPLE_INTERVAL.as_millis()
                        ),
                    });
                } else if d != self.spec.sample_interval {
                    tracing::info!(ms = d.as_millis() as u64, "sample interval changed");
                    self.spec.sample_interval = d;
                    if self.lc.state() != EngineState::Stopped
                        && self.lc.state() != EngineState::Failed
                    {
                        self.emit(Event::DiagnosticMessage {
                            level: DiagLevel::Info,
                            message: format!(
                                "Sample interval set to {} ms; it applies the next time the engine starts.",
                                d.as_millis()
                            ),
                        });
                    }
                }
            }
            Command::Shutdown => {
                self.shutdown();
                return Flow::Exit;
            }
        }
        Flow::Continue
    }

    fn signal_worker_stop(&self) {
        if let Some(w) = &self.worker {
            // Err = worker already gone; its `Exited` is on the way.
            let _ = w.ctl.send(WorkerCtl::Stop);
        }
    }

    fn on_worker(&mut self, gen: u64, msg: WorkerMsg) {
        if self.worker.as_ref().map(|w| w.gen) != Some(gen) {
            tracing::debug!(
                gen,
                current = self.gen,
                ?msg,
                "ignoring message from a previous worker"
            );
            return;
        }
        let state = self.lc.state();
        match msg {
            WorkerMsg::Ready => match state {
                EngineState::Starting => {
                    if self.transition(Transition::Started).is_ok() {
                        tracing::info!("engine started");
                        self.emit(Event::EngineStarted);
                    }
                }
                // Stop was requested during initialisation: never report Running.
                _ => {
                    tracing::info!(%state, "initialisation finished after stop request; not entering Running")
                }
            },
            WorkerMsg::InitFailed(e) => {
                tracing::error!(error = %e, "engine initialisation failed");
                self.init_error = Some(e);
            }
            WorkerMsg::Sample { process, net } => {
                if state != EngineState::Running {
                    return; // e.g. a tick that raced with Stop
                }
                match process {
                    Ok((s, cpu)) => {
                        self.status.memory_bytes = Some(s.memory_bytes);
                        if cpu.is_some() {
                            self.status.cpu_percent = cpu;
                        }
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "process metrics sample failed");
                        if !self.sample_error_reported {
                            self.sample_error_reported = true;
                            self.emit(Event::DiagnosticMessage {
                                level: DiagLevel::Warning,
                                message: format!("Process metrics could not be read: {e}"),
                            });
                        }
                    }
                }
                if let Some(n) = net {
                    self.on_net(n);
                }
                self.emit_status();
            }
            WorkerMsg::Exited => self.on_worker_exited(),
        }
    }

    fn on_net(&mut self, n: NetUpdate) {
        for c in n.changes {
            let message = c.message();
            tracing::info!(%message, "network adapter change");
            self.emit(Event::DiagnosticMessage {
                level: DiagLevel::Info,
                message,
            });
        }
        match n.status {
            Ok(status) => {
                if self.status.network_error.take().is_some() {
                    tracing::info!("network monitor recovered");
                }
                self.net_error_reported = false;
                self.status.network = Some(status);
            }
            Err(e) => {
                // Keep the reason visible; do not show stale numbers.
                self.status.network = None;
                if !self.net_error_reported {
                    self.net_error_reported = true;
                    tracing::warn!(error = %e, "network monitoring unavailable");
                    self.emit(Event::DiagnosticMessage {
                        level: DiagLevel::Warning,
                        message: format!("Network data unavailable: {e}"),
                    });
                }
                self.status.network_error = Some(e);
            }
        }
    }

    fn emit_status(&mut self) {
        self.status.state = Some(self.lc.state());
        self.status.running_for = self.running_since.map(|t| t.elapsed());
        self.emit(Event::Status(self.status.clone()));
    }

    fn on_worker_exited(&mut self) {
        let Some(w) = self.worker.take() else { return };
        // `Exited` is the worker's last action, so this join returns promptly.
        let joined = w.handle.join();
        tracing::debug!(gen = w.gen, "engine worker thread joined");
        let state = self.lc.state();
        match (joined, self.init_error.take()) {
            (Err(p), _) => self.fail(format!("The engine crashed: {}", panic_message(&*p))),
            (Ok(()), Some(e)) => self.fail(format!("The engine failed to start: {e}")),
            (Ok(()), None) if state == EngineState::Stopping => {
                if self.transition(Transition::Stopped).is_ok() {
                    tracing::info!("engine shutdown complete");
                    self.emit(Event::EngineStopped);
                }
            }
            (Ok(()), None) => self.fail(format!("The engine stopped unexpectedly while {state}.")),
        }
    }

    /// Stop the worker (if any) and wait for it, bounded by
    /// [`WORKER_STOP_TIMEOUT`]. Commands that arrive meanwhile are ignored.
    fn shutdown(&mut self) {
        tracing::info!(state = %self.lc.state(), "engine shutdown requested (application exit)");
        if self.lc.state().can_stop() {
            let _ = self.transition(Transition::Stop);
        }
        if self.worker.is_some() {
            self.signal_worker_stop();
            let deadline = Instant::now() + WORKER_STOP_TIMEOUT;
            while self.worker.is_some() {
                let left = deadline.saturating_duration_since(Instant::now());
                match self.rx.recv_timeout(left) {
                    Ok(Msg::Worker { gen, msg }) => self.on_worker(gen, msg),
                    Ok(Msg::SetWaker(w)) => self.waker = Some(w),
                    Ok(Msg::Cmd(c)) => tracing::debug!(?c, "command ignored during shutdown"),
                    Err(_) => {
                        let message = format!(
                            "The engine worker did not stop within {} s; it was abandoned.",
                            WORKER_STOP_TIMEOUT.as_secs()
                        );
                        tracing::error!("{message}");
                        self.emit(Event::EngineError { message });
                        self.worker = None; // detach: cannot kill a thread safely
                        break;
                    }
                }
            }
        }
        tracing::info!("engine controller exiting");
        self.emit(Event::ShutdownComplete);
    }

    fn run(mut self) {
        tracing::info!(
            engine = ENGINE_NAME,
            interval_ms = self.spec.sample_interval.as_millis() as u64,
            "engine controller started"
        );
        // `self.tx` keeps the channel open, so recv() only returns messages.
        while let Ok(msg) = self.rx.recv() {
            match msg {
                Msg::Cmd(cmd) => {
                    if let Flow::Exit = self.on_command(cmd) {
                        break;
                    }
                }
                Msg::SetWaker(w) => {
                    self.waker = Some(w);
                    self.wake(); // flush events queued before the waker existed
                }
                Msg::Worker { gen, msg } => self.on_worker(gen, msg),
            }
        }
    }
}

// ── Handle (owned by the front end) ──────────────────────────────────────────

/// Why [`EngineHandle::shutdown`] could not confirm termination.
#[derive(Debug, thiserror::Error)]
pub enum ShutdownError {
    #[error("the engine did not confirm shutdown within {0:?}")]
    Timeout(Duration),
    #[error("the engine controller thread panicked: {0}")]
    Panicked(String),
}

/// Owner of the engine threads. Dropping it shuts the engine down and joins
/// the controller thread.
pub struct EngineHandle {
    tx: Sender<Msg>,
    events: Receiver<Event>,
    /// Signalled by the controller thread as its very last action.
    done: Receiver<()>,
    thread: Option<JoinHandle<()>>,
    alive: Arc<AtomicUsize>,
    shutdown_sent: bool,
    /// Errors generated on the caller's side (engine already gone).
    local: std::collections::VecDeque<Event>,
}

impl std::fmt::Debug for EngineHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EngineHandle")
            .field("threads_alive", &self.threads_alive())
            .field("shutdown_sent", &self.shutdown_sent)
            .finish_non_exhaustive()
    }
}

impl EngineHandle {
    /// Spawn the controller thread. The engine starts in `Stopped`. Nothing
    /// runs until [`Command::Start`].
    pub fn spawn(spec: EngineSpec) -> std::io::Result<Self> {
        let (tx, rx) = mpsc::channel();
        let (ev_tx, events) = mpsc::channel();
        let (done_tx, done) = mpsc::channel::<()>();
        let alive = Arc::new(AtomicUsize::new(0));
        let controller = Controller {
            rx,
            tx: tx.clone(),
            events: ev_tx,
            waker: None,
            lc: Lifecycle::new(),
            spec,
            worker: None,
            gen: 0,
            init_error: None,
            sample_error_reported: false,
            net_error_reported: false,
            running_since: None,
            status: EngineStatus {
                state: Some(EngineState::Stopped),
                ..EngineStatus::default()
            },
            alive: alive.clone(),
        };
        let count_for_thread = alive.clone();
        let thread = thread::Builder::new()
            .name("kalkan-engine".into())
            .spawn(move || {
                let _count = ThreadCount::new(&count_for_thread);
                controller.run();
                let _ = done_tx.send(());
            })?;
        Ok(Self {
            tx,
            events,
            done,
            thread: Some(thread),
            alive,
            shutdown_sent: false,
            local: Default::default(),
        })
    }

    /// Engine threads (controller + worker) currently alive for this handle.
    pub fn threads_alive(&self) -> usize {
        self.alive.load(Ordering::SeqCst)
    }

    /// Blocking receive with timeout. For tests and headless use only; a GUI
    /// must use [`EngineLink::try_recv`].
    pub fn recv_timeout(&mut self, timeout: Duration) -> Option<Event> {
        if let Some(ev) = self.local.pop_front() {
            return Some(ev);
        }
        self.events.recv_timeout(timeout).ok()
    }

    /// Request shutdown (if not already requested) and wait until the
    /// controller thread has exited and been joined.
    pub fn shutdown(&mut self, timeout: Duration) -> Result<(), ShutdownError> {
        let Some(handle) = self.thread.take() else {
            return Ok(());
        };
        if !self.shutdown_sent {
            self.shutdown_sent = true;
            let _ = self.tx.send(Msg::Cmd(Command::Shutdown));
        }
        match self.done.recv_timeout(timeout) {
            // Disconnected = the thread ended without signalling (panic).
            Ok(()) | Err(RecvTimeoutError::Disconnected) => {}
            Err(RecvTimeoutError::Timeout) => {
                tracing::error!(?timeout, "engine controller did not exit; detaching it");
                return Err(ShutdownError::Timeout(timeout));
            }
        }
        match handle.join() {
            Ok(()) => {
                tracing::info!("engine threads terminated");
                Ok(())
            }
            Err(p) => {
                let m = panic_message(&*p);
                tracing::error!(%m, "engine controller panicked");
                Err(ShutdownError::Panicked(m))
            }
        }
    }
}

impl EngineLink for EngineHandle {
    fn send(&mut self, cmd: Command) {
        if cmd == Command::Shutdown {
            self.shutdown_sent = true;
        }
        if self.tx.send(Msg::Cmd(cmd)).is_err() {
            tracing::error!(?cmd, "command sent after the engine exited");
            self.local.push_back(Event::EngineError {
                message: format!("{cmd:?} failed: the engine has shut down."),
            });
        }
    }

    fn try_recv(&mut self) -> Option<Event> {
        self.local
            .pop_front()
            .or_else(|| self.events.try_recv().ok())
    }

    fn set_waker(&mut self, waker: Waker) {
        let _ = self.tx.send(Msg::SetWaker(waker));
    }
}

impl Drop for EngineHandle {
    fn drop(&mut self) {
        if let Err(e) = self.shutdown(WORKER_STOP_TIMEOUT + Duration::from_secs(1)) {
            tracing::error!(%e, "engine shutdown on drop failed");
        }
    }
}

#[cfg(test)]
#[path = "engine_tests.rs"]
mod tests;
