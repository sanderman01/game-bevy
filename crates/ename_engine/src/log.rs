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
//! The layer sits below `LogPlugin`'s `EnvFilter`, so this buffer never holds anything the
//! terminal did not also print.

use std::{
    collections::{HashMap, VecDeque},
    sync::{Arc, Mutex, MutexGuard},
    time::Instant,
};

use bevy::{
    log::{
        BoxedLayer, Level,
        tracing::{
            Event, Subscriber,
            field::{Field, Visit},
        },
        tracing_subscriber::{Layer, layer::Context},
    },
    prelude::*,
};

/// Entries kept before the oldest is dropped, unless a target overrides it with
/// [`crate::EnginePlugins::with_log_capacity`].
pub const DEFAULT_CAPACITY: usize = 3000;

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

/// The layer handed to `LogPlugin::custom_layer`.
///
/// It also inserts the buffer as a resource, which is why it takes the `App`: the layer and every
/// reader have to share one buffer, and this is the only point where both are reachable.
pub fn capture_layer(app: &mut App) -> Option<BoxedLayer> {
    let buffer = LogBuffer::default();
    app.insert_resource(buffer.clone());
    Some(Box::new(CaptureLayer {
        buffer,
        start: Instant::now(),
    }))
}

struct CaptureLayer {
    buffer: LogBuffer,
    start: Instant,
}

impl<S: Subscriber> Layer<S> for CaptureLayer {
    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        let mut message = MessageVisitor(String::new());
        event.record(&mut message);
        self.buffer.push(
            self.start.elapsed().as_secs_f64(),
            *event.metadata().level(),
            event.metadata().target(),
            message.0,
        );
    }
}

/// Pulls the `message` field out of an event, ignoring the structured fields around it.
struct MessageVisitor(String);

impl Visit for MessageVisitor {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            self.0 = format!("{value:?}");
        }
    }

    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "message" {
            self.0 = value.to_owned();
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
    use super::{DEFAULT_CAPACITY, LogBuffer};
    use bevy::log::Level;

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
