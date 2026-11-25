//! Host pipelines are made by workers, not by the title thread that first
//! records a draw with them. Translating a shader and the host driver's
//! compile take from milliseconds to seconds each (structurisation of the
//! biggest compute programs ~0.4 s, some driver compiles 3.4 s), and
//! the title's first world frame wants over 1,200 new pipelines: made
//! on its recording threads, that frame took more than 30 s and the
//! title's GPU watchdog ended the run (decision 0007, "Pipelines made off
//! the title's threads").
//!
//! A first use queues the pipeline and waits for it within a budget per
//! frame, so the cheap ones (menus, one-off passes such as a colour-grading
//! bake) are there at once as before; past the budget a draw or dispatch
//! whose pipeline is not ready is dropped, as an untranslated one is.
//!
//! The default budget is half a second per title frame. A pass the title
//! runs only once and whose pipeline takes longer is lost.
//!
//! Passes over the screen (triangles with no vertex input and no depth or
//! stencil test, [`GraphicsState::screen_pass`]) go before every other
//! pipeline a worker takes, and have a budget of their own ([`screen_wait`],
//! 20 s a frame): the title fills its lighting tables with such draws once,
//! at the world's first frame, and behind a world's worth of pipelines to
//! make they waited past their budget and were dropped for good; that run
//! had no diffuse light at all (Belobug's fork, 18e57db8, which gives them
//! workers of their own). With a cold cache our runs make a world's worth
//! too: 1,320 pipelines over four minutes in a gameplay run of a new
//! translator.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};

use super::Shader;
use super::host::{GraphicsState, HostPipeline};

/// What a host pipeline is made from, and the host pipeline once made.
pub(crate) struct Pending {
    shaders: Vec<Arc<Shader>>,
    pub(crate) graphics: Option<Box<GraphicsState>>,
    queue_state: AtomicU8,
    made: OnceLock<Option<HostPipeline>>,
    /// What identifies it across runs (shader containers and state), and
    /// whether a draw or dispatch has requested it yet.
    key: String,
    used: AtomicBool,
    /// A pass over the screen ([`GraphicsState::screen_pass`]).
    screen: bool,
}

// SAFETY: D3D12 objects are free-threaded; the graphics state (whose input
// elements point into its own names) is only read after creation.
unsafe impl Send for Pending {}
// SAFETY: as above.
unsafe impl Sync for Pending {}

impl Pending {
    pub(crate) fn new(shaders: Vec<Arc<Shader>>, graphics: Option<Box<GraphicsState>>) -> Pending {
        let mut key = blake3::Hasher::new();
        for s in &shaders {
            key.update(s.stage.as_bytes());
            key.update(s.hash.as_bytes());
        }
        key.update(graphics.as_ref().map(|g| g.describe()).unwrap_or_default().as_bytes());
        // Mesh pipelines have no vertex input either: never a pass over the
        // screen (the world's 8,000 mesh programs are the bulk to make).
        let mesh = shaders.iter().any(|s| matches!(s.stage, "mesh" | "amplification"));
        Pending {
            screen: !shaders.is_empty() && !mesh && graphics.as_ref().is_some_and(|g| g.screen_pass()),
            shaders,
            graphics,
            queue_state: AtomicU8::new(NOT_QUEUED),
            made: OnceLock::new(),
            key: key.finalize().to_hex()[..32].to_owned(),
            used: AtomicBool::new(false),
        }
    }

    fn make(&self) {
        self.made
            .get_or_init(|| HostPipeline::new(&self.shaders, self.graphics.as_deref()));
        let (lock, done) = &DONE;
        let _held = lock.lock().unwrap_or_else(|e| e.into_inner());
        done.notify_all();
    }
}

/// Why a pipeline has no host pipeline (diagnostics): its shaders that do
/// not translate, else "refused" or "being made"; `None` if it has one.
pub(crate) fn missing(p: &Pending) -> Option<String> {
    match p.made.get() {
        Some(Some(_)) => None,
        None => Some("being made".into()),
        Some(None) => {
            let failed: Vec<String> = p
                .shaders
                .iter()
                .filter(|s| matches!(s.translated.get(), Some(None)))
                .map(|s| format!("{} {}", s.stage, s.hash))
                .collect();
            Some(if failed.is_empty() {
                format!("refused {:?}", p.shaders)
            } else {
                failed.join(" + ")
            })
        }
    }
}

/// Signalled whenever a worker has made a pipeline.
static DONE: (Mutex<()>, Condvar) = (Mutex::new(()), Condvar::new());

/// Waiting allowed per title frame, over all threads.
fn wait_budget() -> Duration {
    static BUDGET: OnceLock<Duration> = OnceLock::new();
    *BUDGET.get_or_init(|| {
        let millis = std::env::var("VICTORY_PIPELINE_WAIT_MS")
            .ok()
            .and_then(|value| value.trim().parse::<u64>().ok())
            .unwrap_or(500);
        tracing::info!(millis, "host pipeline wait budget per frame");
        Duration::from_millis(millis)
    })
}

/// Waiting allowed per title frame for passes over the screen, over all
/// threads ([`GraphicsState::screen_pass`]): `VICTORY_SCREEN_PASS_WAIT_MS`,
/// 20,000 by default (below the title's 30 s watchdog); 0 makes them
/// pipelines like the others, as before.
fn screen_wait() -> Duration {
    static BUDGET: OnceLock<Duration> = OnceLock::new();
    *BUDGET.get_or_init(|| {
        let millis = std::env::var("VICTORY_SCREEN_PASS_WAIT_MS")
            .ok()
            .and_then(|value| value.trim().parse::<u64>().ok())
            .unwrap_or(20_000);
        tracing::info!(millis, "passes over the screen: made first, wait budget per frame");
        Duration::from_millis(millis)
    })
}

/// Whether `p` is made before the others and waited for within
/// [`screen_wait`].
fn first(p: &Pending) -> bool {
    p.screen && !screen_wait().is_zero()
}

/// Pipelines queued and not made yet.
static WAITING: AtomicU64 = AtomicU64::new(0);

/// Pipelines queued and not made yet (for the frame line).
pub(crate) fn waiting() -> u64 {
    WAITING.load(Ordering::Relaxed)
}

/// The host pipeline if it is made (or is made within the frame's budget);
/// queues it on first use.
pub(crate) fn ready(p: &Arc<Pending>) -> Option<&HostPipeline> {
    // A still-pending pipeline was requested by the title too. Prewarm it
    // on the next launch, even if this run never got to use it.
    if !p.used.load(Ordering::Relaxed) && !p.used.swap(true, Ordering::Relaxed) {
        super::library::note_used(&p.key);
    }
    if p.made.get().is_none() {
        let first = first(p);
        start(p, if first { SCREEN } else { REQUESTED });
        wait(p, crate::frame::current(), first);
    }
    p.made.get()?.as_ref()
}

/// Makes `p` on this thread if it is not made yet (waiting for a worker
/// that is making it); whether it has a host pipeline.
pub(crate) fn now(p: &Arc<Pending>) -> bool {
    p.make();
    p.made.get().is_some_and(|made| made.is_some())
}

/// At the title's creation of a pipeline: starts it at once if an earlier
/// run drew with it (during loading, so the world's first frames find it).
pub(crate) fn early(p: &Arc<Pending>) {
    // Turning off speculative starts keeps first-use scheduling and the disk caches available.
    static PREWARM: OnceLock<bool> = OnceLock::new();
    let enabled = *PREWARM.get_or_init(|| std::env::var_os("VICTORY_EARLY_PIPELINES").is_none_or(|v| v != "0"));
    if enabled && !p.shaders.is_empty() && super::library::wanted_early(&p.key) {
        super::library::EARLY.fetch_add(1, Ordering::Relaxed);
        start(p, EARLY);
    }
}

/// Whether the frame's budget is one for all threads together: a thread
/// that waits takes what is left of it and gives back what it did not
/// use. Counted only when a wait had ended, the budget was one per thread:
/// each of the title's five recording threads began its first wait of a
/// frame with nothing counted yet and waited the whole half second, 2.5 s
/// of waiting in every frame in which pipelines were being made (runs
/// 34-49: "2504 ms waiting for pipelines" in each such frame).
/// On by default; `VICTORY_PIPELINE_WAIT_SHARED=0` counts as before.
fn shared_budget() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("VICTORY_PIPELINE_WAIT_SHARED").is_none_or(|v| v != "0"))
}

/// Claims remaining wait budget for `frame`. Lagging threads from an older
/// frame get `Duration::ZERO` without resetting the newer frame's budget.
fn take_budget(
    spent: &mut (u64, Duration),
    frame: u64,
    budget: Duration,
    shared: bool,
) -> Duration {
    if spent.0 != u64::MAX && frame < spent.0 {
        return Duration::ZERO;
    }
    if spent.0 == u64::MAX || frame > spent.0 {
        *spent = (frame, Duration::ZERO);
    }
    let left = budget.saturating_sub(spent.1);
    if shared {
        spent.1 += left;
    }
    left
}

/// Waits for `p` while frame `frame` has budget left (of the budget for
/// passes over the screen if `screen`, [`screen_wait`]).
fn wait(p: &Pending, frame: u64, screen: bool) {
    let budget = if screen { screen_wait() } else { wait_budget() };
    if budget.is_zero() {
        return;
    }
    // The frame each budget belongs to, and what was waited of it (or is
    // taken by a thread waiting now).
    static SPENT: [Mutex<(u64, Duration)>; 2] = [const { Mutex::new((u64::MAX, Duration::ZERO)) }; 2];
    let shared = shared_budget();
    let spent_of = &SPENT[usize::from(screen)];
    let left = {
        let mut spent = spent_of.lock().unwrap_or_else(|e| e.into_inner());
        take_budget(&mut spent, frame, budget, shared)
    };
    if left.is_zero() {
        return;
    }
    let started = Instant::now();
    let (lock, done) = &DONE;
    let mut held = lock.lock().unwrap_or_else(|e| e.into_inner());
    while p.made.get().is_none() {
        let rest = left.saturating_sub(started.elapsed());
        if rest.is_zero() {
            break;
        }
        held = done.wait_timeout(held, rest).unwrap_or_else(|e| e.into_inner()).0;
    }
    drop(held);
    let waited = started.elapsed().min(left);
    {
        let mut spent = spent_of.lock().unwrap_or_else(|e| e.into_inner());
        // A frame that began meanwhile has its own budget.
        if spent.0 == frame {
            if shared {
                spent.1 = spent.1.saturating_sub(left - waited);
            } else {
                spent.1 += waited;
            }
        }
    }
    crate::framelog::add(crate::framelog::T::PipelineWaitUs, started.elapsed().as_micros() as u64);
    #[cfg(feature = "diagnostics")]
    crate::copystat::add(
        crate::copystat::C::PipelineWaitUs,
        started.elapsed().as_micros() as u64,
    );
}

const NOT_QUEUED: u8 = 0;
const EARLY: u8 = 1;
const REQUESTED: u8 = 2;
/// Requested, a pass over the screen ([`first`]): made before the others.
const SCREEN: u8 = 3;
const MAKING: u8 = 4;

struct Queue {
    early: VecDeque<Arc<Pending>>,
    requested: VecDeque<Arc<Pending>>,
    screen: VecDeque<Arc<Pending>>,
}

impl Queue {
    const fn new() -> Self {
        Self {
            early: VecDeque::new(),
            requested: VecDeque::new(),
            screen: VecDeque::new(),
        }
    }

    /// Queues an unrequested pipeline or promotes one queued at creation.
    /// Returns whether it is new to the queue, or `None` if nothing changed.
    fn push(&mut self, p: &Arc<Pending>, priority: u8) -> Option<bool> {
        let prev = p.queue_state.load(Ordering::Relaxed);
        if prev >= priority {
            return None;
        }
        p.queue_state.store(priority, Ordering::Relaxed);
        match priority {
            SCREEN => self.screen.push_back(p.clone()),
            REQUESTED => self.requested.push_back(p.clone()),
            _ => self.early.push_back(p.clone()),
        }
        Some(prev == NOT_QUEUED)
    }

    fn pop(&mut self) -> Option<Arc<Pending>> {
        // An entry can have been promoted to a queue served sooner and
        // claimed already: each queue keeps only those still in its state.
        let take = |queue: &mut VecDeque<Arc<Pending>>, state: u8| {
            std::iter::from_fn(|| queue.pop_front()).find(|p| p.queue_state.load(Ordering::Relaxed) == state)
        };
        let p = take(&mut self.screen, SCREEN)
            .or_else(|| take(&mut self.requested, REQUESTED))
            .or_else(|| take(&mut self.early, EARLY))?;
        p.queue_state.store(MAKING, Ordering::Relaxed);
        Some(p)
    }
}

static QUEUE: Mutex<Queue> = Mutex::new(Queue::new());
static AVAILABLE: Condvar = Condvar::new();

/// Start or promote work for a draw without waiting behind the entire early
/// list. Queue changes and worker claims happen under the same lock.
fn start(p: &Arc<Pending>, priority: u8) {
    if p.queue_state.load(Ordering::Relaxed) >= priority {
        return;
    }
    if !workers() {
        if p.queue_state.swap(MAKING, Ordering::Relaxed) == NOT_QUEUED {
            p.make();
        }
        return;
    }
    let mut queue = QUEUE.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(new) = queue.push(p, priority) {
        if new {
            PEAK.fetch_max(WAITING.fetch_add(1, Ordering::Relaxed) + 1, Ordering::Relaxed);
        }
        AVAILABLE.notify_one();
    }
}

fn next() -> Arc<Pending> {
    let mut queue = QUEUE.lock().unwrap_or_else(|e| e.into_inner());
    loop {
        if let Some(p) = queue.pop() {
            return p;
        }
        queue = AVAILABLE.wait(queue).unwrap_or_else(|e| e.into_inner());
    }
}

/// Starts the workers (three quarters of the logical processors, 2 to 24)
/// on first use. They were half, at most 8: on a 32-thread processor the
/// first run of a new translator had 1,117 pipelines queued as the opening
/// cutscene began and took four minutes to make them, with thousands of
/// draws a frame dropped meanwhile and 24 threads idle. The workers run
/// below the title's threads, so these only take time the title leaves.
fn workers() -> bool {
    static RUNNING: OnceLock<bool> = OnceLock::new();
    *RUNNING.get_or_init(|| {
        let mut running = false;
        let workers = std::thread::available_parallelism().map_or(2, |n| (n.get() * 3 / 4).clamp(2, 24));
        for i in 0..workers {
            let spawned = std::thread::Builder::new()
                .name(format!("victory-pipelines-{i}"))
                .spawn(|| {
                    // Below the title's own threads (loading, recording).
                    // SAFETY: a plain call on our own thread.
                    unsafe {
                        use windows::Win32::System::Threading::*;
                        let _ = SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_BELOW_NORMAL);
                    }
                    loop {
                        next().make();
                        if WAITING.fetch_sub(1, Ordering::Relaxed) == 1 {
                            drained();
                        } else {
                            super::library::save(false);
                        }
                    }
                });
            if let Err(e) = spawned {
                tracing::warn!(error = %e, "no pipeline worker");
            } else {
                running = true;
            }
        }
        running
    })
}

/// The most pipelines waiting at once since the workers last ran out.
static PEAK: AtomicU64 = AtomicU64::new(0);

/// The workers ran out of work: logged after a batch of 10 or more, and
/// the cache saved (at most every 20 s).
fn drained() {
    use super::library::{EARLY, LOADED, MADE};
    if PEAK.swap(0, Ordering::Relaxed) >= 10 {
        tracing::info!(
            made = MADE.load(Ordering::Relaxed),
            loaded = LOADED.load(Ordering::Relaxed),
            started_early = EARLY.load(Ordering::Relaxed),
            "host pipelines all made"
        );
    }
    super::library::save(false);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A first use queues the pipeline and, within the budget, gets the
    /// worker's answer (here: no host pipeline for no shaders).
    #[test]
    fn first_use_is_made_by_a_worker() {
        let p = Arc::new(Pending::new(Vec::new(), None));
        assert!(ready(&p).is_none());
        assert_ne!(p.queue_state.load(Ordering::Relaxed), NOT_QUEUED);
        let started = Instant::now();
        while p.made.get().is_none() && started.elapsed() < Duration::from_secs(10) {
            std::thread::yield_now();
        }
        assert!(matches!(p.made.get(), Some(None)));
    }

    #[test]
    fn pending_pipeline_is_recorded_for_the_next_launch() {
        let mut p = Pending::new(Vec::new(), None);
        p.key = "pending-pipeline-test".to_owned();
        // Simulate a worker that has taken the job but cannot finish it yet.
        p.queue_state.store(MAKING, Ordering::Relaxed);
        let p = Arc::new(p);

        assert!(!super::super::library::wanted_early(&p.key));
        assert!(ready(&p).is_none());
        assert!(p.made.get().is_none());
        assert!(super::super::library::wanted_early(&p.key));
    }

    #[test]
    fn draws_promote_early_pipelines_without_claiming_them_twice() {
        let earlier = Arc::new(Pending::new(Vec::new(), None));
        let promoted = Arc::new(Pending::new(Vec::new(), None));
        let requested = Arc::new(Pending::new(Vec::new(), None));
        let mut queue = Queue::new();

        assert_eq!(queue.push(&earlier, EARLY), Some(true));
        assert_eq!(queue.push(&promoted, EARLY), Some(true));
        assert_eq!(queue.push(&requested, REQUESTED), Some(true));
        assert_eq!(queue.push(&promoted, REQUESTED), Some(false));
        assert_eq!(queue.push(&promoted, REQUESTED), None);

        assert!(Arc::ptr_eq(&queue.pop().unwrap(), &requested));
        assert!(Arc::ptr_eq(&queue.pop().unwrap(), &promoted));
        assert_eq!(queue.push(&promoted, REQUESTED), None);
        assert!(Arc::ptr_eq(&queue.pop().unwrap(), &earlier));
        assert!(queue.pop().is_none());
    }

    /// A pass over the screen asked for after others is made before them,
    /// and one started early and then asked for is taken once.
    #[test]
    fn passes_over_the_screen_go_first() {
        let requested = Arc::new(Pending::new(Vec::new(), None));
        let screen = Arc::new(Pending::new(Vec::new(), None));
        let early_screen = Arc::new(Pending::new(Vec::new(), None));
        let mut queue = Queue::new();

        assert_eq!(queue.push(&early_screen, EARLY), Some(true));
        assert_eq!(queue.push(&requested, REQUESTED), Some(true));
        assert_eq!(queue.push(&screen, SCREEN), Some(true));
        assert_eq!(queue.push(&early_screen, SCREEN), Some(false));
        // Asked for again as an ordinary one: nothing changes.
        assert_eq!(queue.push(&screen, REQUESTED), None);

        assert!(Arc::ptr_eq(&queue.pop().unwrap(), &screen));
        assert!(Arc::ptr_eq(&queue.pop().unwrap(), &early_screen));
        assert!(Arc::ptr_eq(&queue.pop().unwrap(), &requested));
        assert!(queue.pop().is_none());
    }

    #[test]
    fn lagging_frame_does_not_rewind_or_reset_budget() {
        let budget = Duration::from_millis(500);
        let mut spent = (u64::MAX, Duration::ZERO);

        // First frame (even frame 0) initializes the budget.
        assert_eq!(take_budget(&mut spent, 0, budget, true), budget);
        assert_eq!(spent, (0, budget));

        // Advancing to frame 10 resets the budget once.
        assert_eq!(take_budget(&mut spent, 10, budget, true), budget);
        assert_eq!(spent, (10, budget));

        // A lagging thread from frame 9 gets zero budget and must not rewind
        // `spent.0` (which would otherwise grant frame 10 a second full budget).
        assert_eq!(take_budget(&mut spent, 9, budget, true), Duration::ZERO);
        assert_eq!(spent, (10, budget));
        assert_eq!(take_budget(&mut spent, 10, budget, true), Duration::ZERO);

        // Advancing to frame 11 grants a fresh budget.
        assert_eq!(take_budget(&mut spent, 11, budget, false), budget);
        assert_eq!(spent, (11, Duration::ZERO));
    }
}
