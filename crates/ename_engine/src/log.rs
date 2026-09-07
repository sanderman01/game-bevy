//! A bounded ring buffer of recent `tracing` events.
//!
//! It lives at the bottom of the layer graph because two crates above it read the same buffer:
//! the editor's console panel and `ename_remote`'s `game.logs.get`. `LogPlugin::custom_layer` is
//! one `fn` slot and a subscriber cannot gain layers after it is installed, so they cannot each
//! capture for themselves.
//!
//! Entries are structured rather than formatted. Level, target, message and timestamp stay
//! separate fields, so a reader filters on them instead of pattern-matching rendered text.
//!
//! Three filters decide what lands here, because an `EnvFilter` added to a subscriber gates every
//! layer on it whatever the order. `EnginePlugins` opens the subscriber-wide one to
//! [`MAX_LEVEL`], then each consumer carries its own: [`terminal_layer`] pins stderr at
//! [`DEFAULT_LEVEL`], and the buffer's is reloadable, so [`CaptureLevel`] can raise it while the
//! game runs.
//!
//! Reloadable rather than fixed at trace, because a callsite no filter wants is never even
//! formatted. Left at trace the physics solver alone allocates a few hundred strings a second and
//! fills the ring in about fifteen seconds. Off, those callsites cost a filter check.

use std::{
    collections::{HashMap, VecDeque},
    sync::{Arc, Mutex, MutexGuard},
    time::Instant,
};

use bevy::{
    log::{
        BoxedFmtLayer, BoxedLayer, DEFAULT_FILTER, Level,
        tracing::{
            Event, Subscriber,
            field::{Field, Visit},
        },
        tracing_subscriber::{
            EnvFilter, Layer, fmt,
            layer::Context,
            registry::Registry,
            reload::{self, Handle},
        },
    },
    prelude::*,
};

/// Entries kept before the oldest is dropped, unless a target overrides it with
/// [`crate::EnginePlugins::with_log_capacity`].
pub const DEFAULT_CAPACITY: usize = 3000;

/// The ceiling on [`CaptureLevel`], and so the level `EnginePlugins` opens the subscriber to.
///
/// Nothing above this can ever be captured, whatever the console asks for: the subscriber-wide
/// filter is built once and `LogPlugin` does not make it reloadable.
pub const MAX_LEVEL: Level = Level::TRACE;

/// What stderr prints, and where [`CaptureLevel`] starts.
///
/// It is `LogPlugin::level`'s own default, kept here because the buffer took that field over.
pub const DEFAULT_LEVEL: Level = Level::INFO;

/// An interned emitting module path, such as `bevy_render::renderer`.
///
/// Targets repeat: thousands of events come from a handful of modules. Interning stores the text
/// once, makes equality an integer compare, and turns a category filter into set membership.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Target(u32);

/// The flyweight store behind [`Target`]. It only grows; the set of emitting modules in a process
/// is bounded by the code that is linked into it.
#[derive(Default)]
struct Interner {
    names: Vec<Arc<str>>,
    ids: HashMap<Arc<str>, Target>,
}

impl Interner {
    fn intern(&mut self, name: &str) -> Target {
        if let Some(&target) = self.ids.get(name) {
            return target;
        }
        let name: Arc<str> = Arc::from(name);
        let target = Target(self.names.len() as u32);
        self.names.push(Arc::clone(&name));
        self.ids.insert(name, target);
        target
    }

    fn resolve(&self, target: Target) -> &str {
        self.names.get(target.0 as usize).map_or("", Arc::as_ref)
    }
}

/// One captured event, as it is stored.
struct LogEntry {
    sequence: u64,
    timestamp: f64,
    level: Level,
    target: Target,
    message: Box<str>,
}

/// One captured event, as a reader sees it, with the target already resolved.
#[derive(Clone, Copy)]
pub struct LogEntryRef<'a> {
    /// Monotonic and contiguous. It keeps counting past a drop or a clear, so a reader polling
    /// with "everything after what I already read" is never sent backwards.
    pub sequence: u64,
    /// Seconds since the process started.
    pub timestamp: f64,
    pub level: Level,
    pub target: Target,
    pub target_name: &'a str,
    /// The message field only. Structured fields around it are not captured.
    pub message: &'a str,
}

/// The buffer itself.
///
/// Shared with a `tracing` layer that is installed before the `App` exists, so it is an `Arc`
/// rather than plain resource data. A `Mutex` and not a channel, because a reader wants the last
/// N entries rather than the ones that arrived since it last looked.
#[derive(Resource, Clone)]
pub struct LogBuffer(Arc<Mutex<Ring>>);

struct Ring {
    entries: VecDeque<LogEntry>,
    interner: Interner,
    capacity: usize,
    next_sequence: u64,
}

impl Default for LogBuffer {
    fn default() -> Self {
        Self(Arc::new(Mutex::new(Ring {
            entries: VecDeque::with_capacity(DEFAULT_CAPACITY),
            interner: Interner::default(),
            capacity: DEFAULT_CAPACITY,
            next_sequence: 0,
        })))
    }
}

impl LogBuffer {
    /// Runs `f` over a borrowed view of the entries. Nothing is cloned.
    ///
    /// The lock is held for the whole closure, so a thread emitting an event waits on it. Keep
    /// the closure to reading and laying out; do not call back into the buffer from inside it,
    /// which deadlocks.
    pub fn read<R>(&self, f: impl FnOnce(LogView<'_>) -> R) -> R {
        let ring = self.lock();
        f(LogView(&ring))
    }

    /// Drops every entry. Sequence numbers keep counting, so a reader polling by sequence sees a
    /// gap rather than a repeat.
    pub fn clear(&self) {
        self.lock().entries.clear();
    }

    fn push(&self, timestamp: f64, level: Level, target: &str, message: String) {
        let mut ring = self.lock();
        if ring.capacity == 0 {
            return;
        }
        let sequence = ring.next_sequence;
        ring.next_sequence += 1;
        let target = ring.interner.intern(target);
        while ring.entries.len() >= ring.capacity {
            ring.entries.pop_front();
        }
        ring.entries.push_back(LogEntry {
            sequence,
            timestamp,
            level,
            target,
            message: message.into_boxed_str(),
        });
    }

    fn set_capacity(&self, capacity: usize) {
        let mut ring = self.lock();
        ring.capacity = capacity;
        while ring.entries.len() > capacity {
            ring.entries.pop_front();
        }
    }

    /// A poisoned lock means a writer panicked between two `VecDeque` calls. No entry can be
    /// half-written, so the buffer is still readable and dropping every future event is the worse
    /// outcome.
    fn lock(&self) -> MutexGuard<'_, Ring> {
        self.0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// A borrowed view of the buffer, valid for the body of [`LogBuffer::read`].
///
/// Indices are positions in the buffer and shift as it wraps. Anything that outlives a frame
/// should hold a sequence number and come back through [`LogView::by_sequence`].
pub struct LogView<'a>(&'a Ring);

impl<'a> LogView<'a> {
    pub fn len(&self) -> usize {
        self.0.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.entries.is_empty()
    }

    pub fn get(&self, index: usize) -> Option<LogEntryRef<'a>> {
        self.0.entries.get(index).map(|entry| self.entry_ref(entry))
    }

    /// Sequence numbers are contiguous, so this is a subtraction rather than a search. Returns
    /// `None` for an entry that has aged out.
    pub fn by_sequence(&self, sequence: u64) -> Option<LogEntryRef<'a>> {
        let oldest = self.0.entries.front()?.sequence;
        self.get(usize::try_from(sequence.checked_sub(oldest)?).ok()?)
    }

    /// The oldest and newest sequence numbers held, or `None` when empty.
    pub fn sequence_range(&self) -> Option<(u64, u64)> {
        Some((
            self.0.entries.front()?.sequence,
            self.0.entries.back()?.sequence,
        ))
    }

    pub fn resolve(&self, target: Target) -> &'a str {
        self.0.interner.resolve(target)
    }

    pub fn iter(&self) -> impl Iterator<Item = LogEntryRef<'a>> {
        self.0.entries.iter().map(|entry| self.entry_ref(entry))
    }

    fn entry_ref(&self, entry: &'a LogEntry) -> LogEntryRef<'a> {
        LogEntryRef {
            sequence: entry.sequence,
            timestamp: entry.timestamp,
            level: entry.level,
            target: entry.target,
            target_name: self.0.interner.resolve(entry.target),
            message: &entry.message,
        }
    }
}

/// The stderr layer, carrying its own [`DEFAULT_LEVEL`] filter rather than borrowing the
/// subscriber's.
///
/// `LogPlugin` builds exactly this layer when `fmt_layer` returns `None`. The filter is the only
/// addition, and it has to be here because the subscriber-wide one is open to [`MAX_LEVEL`];
/// without it, everything the console asks the buffer for would be printed as well.
pub fn terminal_layer(_app: &mut App) -> Option<BoxedFmtLayer> {
    Some(Box::new(
        // `fmt::Layer::default` is what reads `NO_COLOR`.
        fmt::Layer::default()
            .with_writer(std::io::stderr)
            .with_filter(env_filter(DEFAULT_LEVEL)),
    ))
}

/// How verbose the buffer is, changeable while the game runs.
///
/// The console's level checkboxes drive it, so a level nobody has asked for costs a filter check
/// at its callsite instead of a formatted message and a push.
#[derive(Resource, Clone)]
pub struct CaptureLevel {
    handle: Handle<EnvFilter, Registry>,
    current: Arc<Mutex<Level>>,
}

impl CaptureLevel {
    pub fn get(&self) -> Level {
        *self.lock()
    }

    /// Raises or lowers what the buffer captures, up to [`MAX_LEVEL`].
    ///
    /// Cheap to call every frame with a level that has not changed, which is how the console
    /// calls it. Changing it is not cheap: `tracing` rebuilds its callsite interest cache.
    pub fn set(&self, level: Level) {
        // `tracing::Level` orders by verbosity, so `min` is the less verbose of the two.
        let level = level.min(MAX_LEVEL);
        let mut current = self.lock();
        if *current == level {
            return;
        }
        if self.handle.reload(env_filter(level)).is_ok() {
            *current = level;
        }
    }

    fn lock(&self) -> MutexGuard<'_, Level> {
        self.current
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// Builds a filter the way `LogPlugin` builds its own: its default directives at `level`, then
/// `RUST_LOG` appended so that anything it names wins.
///
/// Strings on both ends of this are not a choice. `DEFAULT_FILTER` is a `&str` constant,
/// `RUST_LOG` is an environment variable, and `EnvFilter` has no constructor for a directive that
/// does not go through `FromStr`.
fn env_filter(level: Level) -> EnvFilter {
    // `DEFAULT_FILTER` ends in a comma, and `parse_lossy` skips the empty directive an unset
    // `RUST_LOG` leaves behind.
    let environment = std::env::var(EnvFilter::DEFAULT_ENV).unwrap_or_default();
    EnvFilter::builder().parse_lossy(format!("{level},{DEFAULT_FILTER}{environment}"))
}

/// The layer handed to `LogPlugin::custom_layer`.
///
/// It also inserts the buffer and [`CaptureLevel`] as resources, which is why it takes the `App`:
/// the layer and every reader have to share one buffer, and this is the only point where both are
/// reachable.
pub fn capture_layer(app: &mut App) -> Option<BoxedLayer> {
    let buffer = LogBuffer::default();
    app.insert_resource(buffer.clone());

    let (filter, handle) = reload::Layer::new(env_filter(DEFAULT_LEVEL));
    app.insert_resource(CaptureLevel {
        handle,
        current: Arc::new(Mutex::new(DEFAULT_LEVEL)),
    });

    Some(Box::new(
        CaptureLayer {
            buffer,
            start: Instant::now(),
        }
        // A per-layer filter, not a subscriber-wide one: it decides what this layer sees without
        // touching what stderr prints, and it takes part in callsite interest, so a level it
        // rejects is never formatted.
        .with_filter(filter),
    ))
}

struct CaptureLayer {
    buffer: LogBuffer,
    start: Instant,
}

impl<S: Subscriber> Layer<S> for CaptureLayer {
    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        let mut fields = EventFields::default();
        event.record(&mut fields);
        let target = fields
            .target
            .as_deref()
            .unwrap_or_else(|| event.metadata().target());
        self.buffer.push(
            self.start.elapsed().as_secs_f64(),
            *event.metadata().level(),
            target,
            fields.message,
        );
    }
}

/// Pulls the two fields the buffer keeps out of an event, ignoring the structured fields around
/// them.
///
/// `log.target` is how `tracing-log` carries the real target of a record that came through the
/// `log` crate, which `bevy_app` and the other `no_std`-friendly crates use: the event's own
/// metadata says only `log`. The terminal formatter makes the same swap, so both readers name the
/// same module.
#[derive(Default)]
struct EventFields {
    message: String,
    target: Option<String>,
}

impl Visit for EventFields {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            self.message = format!("{value:?}");
        }
    }

    fn record_str(&mut self, field: &Field, value: &str) {
        match field.name() {
            "message" => self.message = value.to_owned(),
            "log.target" => self.target = Some(value.to_owned()),
            _ => {}
        }
    }
}

/// Resizes the buffer [`capture_layer`] already built.
///
/// `LogPlugin::custom_layer` is a bare `fn` pointer and cannot capture a chosen capacity, so the
/// layer starts at [`DEFAULT_CAPACITY`] and this plugin applies the target's choice afterwards.
/// It has to build after `LogPlugin`, which it does by being added later in `EnginePlugins`.
pub struct LogBufferPlugin {
    pub capacity: usize,
}

impl Plugin for LogBufferPlugin {
    fn build(&self, app: &mut App) {
        if let Some(buffer) = app.world().get_resource::<LogBuffer>() {
            buffer.set_capacity(self.capacity);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        CaptureLevel, DEFAULT_CAPACITY, DEFAULT_LEVEL, LogBuffer, MAX_LEVEL, capture_layer,
    };
    use bevy::{
        app::App,
        log::{
            Level,
            tracing_subscriber::{layer::SubscriberExt as _, registry::Registry},
        },
    };

    /// The whole point of the reloadable filter: a level nobody asked for is not captured, and
    /// asking for it starts capture without rebuilding the subscriber.
    #[test]
    fn the_capture_level_decides_what_reaches_the_buffer() {
        let mut app = App::new();
        let layer = capture_layer(&mut app).expect("capture layer");
        let buffer = app.world().resource::<LogBuffer>().clone();
        let level = app.world().resource::<CaptureLevel>().clone();
        let count = || buffer.read(|view| view.len());

        bevy::log::tracing::subscriber::with_default(Registry::default().with(layer), || {
            bevy::log::debug!("before");
            assert_eq!(count(), 0, "debug below the capture level");

            level.set(Level::DEBUG);
            bevy::log::debug!("after");
            assert_eq!(count(), 1, "debug once the console asks for it");

            level.set(DEFAULT_LEVEL);
            bevy::log::debug!("later");
            assert_eq!(count(), 1, "debug again once it is unticked");
        });
    }

    #[test]
    fn the_buffer_can_be_raised_but_not_past_the_subscriber() {
        // `tracing::Level` orders by verbosity, so the ceiling is the *greatest* level.
        assert!(DEFAULT_LEVEL < MAX_LEVEL);
        assert_eq!(MAX_LEVEL.min(Level::TRACE), MAX_LEVEL);
    }

    fn fill(buffer: &LogBuffer, count: usize) {
        for i in 0..count {
            buffer.push(i as f64, Level::INFO, "test", i.to_string());
        }
    }

    #[test]
    fn interning_is_stable_and_resolves_back() {
        let buffer = LogBuffer::default();
        buffer.push(0., Level::INFO, "one", "a".to_owned());
        buffer.push(1., Level::WARN, "two", "b".to_owned());
        buffer.push(2., Level::INFO, "one", "c".to_owned());

        buffer.read(|view| {
            let first = view.get(0).unwrap();
            let second = view.get(1).unwrap();
            let third = view.get(2).unwrap();
            assert_eq!(first.target, third.target);
            assert_ne!(first.target, second.target);
            assert_eq!(view.resolve(first.target), "one");
            assert_eq!(view.resolve(second.target), "two");
        });
    }

    #[test]
    fn drops_the_oldest_entry_once_full() {
        let buffer = LogBuffer::default();
        fill(&buffer, DEFAULT_CAPACITY + 10);

        buffer.read(|view| {
            assert_eq!(view.len(), DEFAULT_CAPACITY);
            // Sequence numbers keep counting past the drop, so a reader polling with
            // `after_sequence` is not sent backwards by the buffer wrapping.
            assert_eq!(
                view.sequence_range(),
                Some((10, (DEFAULT_CAPACITY + 9) as u64))
            );
            assert!(view.by_sequence(9).is_none());
            assert_eq!(view.by_sequence(10).unwrap().message, "10");
        });
    }

    #[test]
    fn shrinking_the_capacity_drops_from_the_front() {
        let buffer = LogBuffer::default();
        fill(&buffer, 100);
        buffer.set_capacity(10);

        buffer.read(|view| {
            assert_eq!(view.len(), 10);
            assert_eq!(view.sequence_range(), Some((90, 99)));
        });
    }

    #[test]
    fn clearing_keeps_counting() {
        let buffer = LogBuffer::default();
        fill(&buffer, 5);
        buffer.clear();
        fill(&buffer, 1);

        buffer.read(|view| assert_eq!(view.sequence_range(), Some((5, 5))));
    }
}
