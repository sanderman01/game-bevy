//! Input injection: an agent's key presses, pointer moves, clicks and scrolls, written as the same
//! messages `bevy_winit` writes for real input, so `bevy_input`, egui and picking all see them.
//!
//! Nothing here writes `Window`: a changed cursor position would warp the user's real pointer and
//! `focused = true` would raise the window. The cost is that code reading
//! `Window::cursor_position()` (Bevy's transform gizmo, `bevy_ui` focus) does not see the
//! injected pointer; the gizmo is switched off while a script runs.
//!
//! A script is one job. It runs one operation per frame in `First`, finishes in `Last` of the
//! frame its last message was written (after picking and egui consumed it), and always releases
//! the keys and buttons it pressed, whether it ends, fails, times out or is cancelled.

mod geometry;
mod keymap;
mod script;
#[cfg(test)]
mod tests;

use std::{
    collections::{HashMap, HashSet, VecDeque},
    time::{Duration, Instant},
};

use bevy::{
    ecs::message::MessageUpdateSystems,
    input::{
        ButtonState,
        keyboard::{Key, KeyboardFocusLost, KeyboardInput, NativeKey, NativeKeyCode},
        mouse::{MouseButtonInput, MouseMotion, MouseScrollUnit, MouseWheel},
        touch::TouchPhase,
    },
    picking::{PickingSystems, pointer::PointerId, pointer::PointerLocation},
    prelude::*,
    window::WindowEvent,
};
use bevy_egui::{EguiContext, EguiInputSet, PrimaryEguiContext, input::EguiInputEvent};
use ename_engine::command::{CommandError, JobCancelRequested, JobHandle, JobWork, Jobs};
use schemars::JsonSchema;
use serde::Serialize;

pub(crate) use geometry::{GeometryError, Space, geometry, primary_window};
pub(crate) use keymap::{Modifier, close_key_names, modifier_key, parse_key};
pub(crate) use script::{Action, Point};
use script::{Level, Op, expand};

/// Where injected input is written: in `First`, after messages are swapped and before picking
/// turns window events into pointer input, so picking, `bevy_input` and egui all see it in the
/// frame it is written, as they see real input.
#[derive(SystemSet, Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct InputInjectSystems;

/// The injector: its state, the systems that write and track input, and the gizmo gate.
pub(crate) struct InputInjectPlugin;

impl Plugin for InputInjectPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<InputInjector>()
            .init_resource::<InputClock>()
            .configure_sets(
                First,
                InputInjectSystems
                    .after(MessageUpdateSystems)
                    .before(PickingSystems::Input),
            )
            .add_systems(
                First,
                (track_real_input, inject)
                    .chain()
                    .in_set(InputInjectSystems),
            )
            // Between egui's reading of `CursorMoved` and its writing of events, so the
            // `PointerMoved` it makes from a `leave` step's move does not undo `PointerGone`.
            .add_systems(
                PreUpdate,
                write_pointer_gone
                    .after(EguiInputSet::ReadBevyMessages)
                    .before(EguiInputSet::WriteEguiEvents),
            )
            .add_systems(Last, finish_script);
    }
}

/// Longest wait, and longest script timeout, in seconds. Keeps `Instant` arithmetic in range.
pub(crate) const MAX_SECONDS: f64 = 3600.0;

/// Most frames one `wait`, `drag` or `motion` step may take.
pub(crate) const MAX_FRAMES: u32 = 100_000;

/// The wall clock scripts wait and time out on. Tests move `offset` instead of sleeping.
/// `Time<Real>` is not used: it can take a render-world timestamp from before a stall.
#[derive(Resource, Default)]
pub(crate) struct InputClock {
    pub(crate) offset: Duration,
}

impl InputClock {
    pub(crate) fn now(&self) -> Instant {
        Instant::now() + self.offset
    }
}

/// The running script, if any, and what the injector knows about real input.
#[derive(Resource, Default)]
pub(crate) struct InputInjector {
    script: Option<Script>,
    /// Keys and buttons held by the user, from real messages. `ButtonInput` in `First` is still
    /// last frame's, so it cannot tell a press that arrived this frame.
    real_keys: HashMap<KeyCode, Key>,
    real_buttons: HashSet<MouseButton>,
    own: OwnIds,
    focus_lost: bool,
    pointer_gone: bool,
    /// Logical keys the script pressed whose physical key the user has since taken over.
    orphaned_logical: Vec<Key>,
}

impl InputInjector {
    pub(crate) fn is_busy(&self) -> bool {
        self.script.is_some()
    }
}

/// Ids of the messages the injector wrote, so tracking can tell them from real ones.
#[derive(Default)]
struct OwnIds {
    keyboard: HashSet<usize>,
    buttons: HashSet<usize>,
    cursor: HashSet<usize>,
    wheel: HashSet<usize>,
    motion: HashSet<usize>,
}

/// What a finished script reports.
#[derive(Serialize, JsonSchema, Debug, Clone, PartialEq)]
pub(crate) struct InputSent {
    /// Frames the script ran.
    frames: u32,
    /// The mouse pointer afterwards, physical window pixels; `null` if picking has none.
    pointer: Option<[f32; 2]>,
    /// Real pointer and keyboard messages seen while the script ran: the user's mouse passing
    /// over the window, say. Non-zero means the run may have been disturbed.
    real_input: RealInput,
    /// Keys the script was asked to press that were already held, so nothing fired for them.
    already_held: Vec<String>,
    /// egui's interest after the last step; `null` without egui.
    egui: Option<EguiWants>,
}

#[derive(Serialize, JsonSchema, Debug, Clone, Copy, Default, PartialEq)]
pub(crate) struct RealInput {
    pointer: u32,
    keyboard: u32,
}

#[derive(Serialize, JsonSchema, Debug, Clone, Copy, PartialEq)]
pub(crate) struct EguiWants {
    pub(crate) wants_pointer: bool,
    pub(crate) wants_keyboard: bool,
}

enum Outcome {
    Done,
    Failed(CommandError),
    Cancelled,
}

struct Script {
    actions: Vec<Action>,
    /// Index of the next action to expand, and of the one being run.
    next: usize,
    current: usize,
    ops: VecDeque<Op>,
    wait_until: Option<Instant>,
    deadline: Instant,
    work: Entity,
    /// The primary window when the script started.
    window: Entity,
    job: JobHandle<InputSent>,
    /// Each held key with the logical key its press carried.
    keys: HashMap<KeyCode, (Level, Key)>,
    buttons: HashMap<MouseButton, Level>,
    /// The last point the script moved to, re-asserted before every button and wheel frame.
    pointer: Option<Point>,
    frames: u32,
    real: RealInput,
    already_held: Vec<String>,
    cancel: bool,
    /// The op just run was a `leave`, so egui gets `PointerGone`.
    left: bool,
    outcome: Option<Outcome>,
}

/// Starts `actions` as the job `job` on `work`. The caller has checked the injector is idle and
/// started the job; `timeout` is the wall-clock limit, at most [`MAX_SECONDS`], as is every wait.
pub(crate) fn start(
    world: &mut World,
    actions: Vec<Action>,
    window: Entity,
    work: Entity,
    job: JobHandle<InputSent>,
    timeout: Duration,
) {
    let deadline = world.resource::<InputClock>().now() + timeout;
    world.entity_mut(work).observe(
        |cancel: On<JobCancelRequested>, mut injector: ResMut<InputInjector>| {
            if let Some(script) = injector.script.as_mut()
                && script.work == cancel.entity
            {
                script.cancel = true;
            }
        },
    );
    world.resource_mut::<InputInjector>().script = Some(Script {
        actions,
        next: 0,
        current: 0,
        ops: VecDeque::new(),
        wait_until: None,
        deadline,
        work,
        window,
        job,
        keys: HashMap::new(),
        buttons: HashMap::new(),
        pointer: None,
        frames: 0,
        real: RealInput::default(),
        already_held: Vec::new(),
        cancel: false,
        left: false,
        outcome: None,
    });
}

/// Reads every input message since the last frame, keeps real ones apart from the injector's own,
/// and drops ownership of anything the user pressed or released themselves.
#[allow(clippy::too_many_arguments)]
fn track_real_input(
    mut injector: ResMut<InputInjector>,
    mut keyboard: MessageReader<KeyboardInput>,
    mut buttons: MessageReader<MouseButtonInput>,
    mut cursor: MessageReader<CursorMoved>,
    mut wheel: MessageReader<MouseWheel>,
    mut motion: MessageReader<MouseMotion>,
    mut focus_lost: MessageReader<KeyboardFocusLost>,
) {
    let injector = &mut *injector;
    let mut real = RealInput::default();
    for (message, id) in keyboard.read_with_id() {
        if injector.own.keyboard.remove(&id.id) {
            continue;
        }
        real.keyboard += 1;
        match message.state {
            ButtonState::Pressed => {
                injector
                    .real_keys
                    .insert(message.key_code, message.logical_key.clone());
            }
            ButtonState::Released => {
                injector.real_keys.remove(&message.key_code);
            }
        }
        // The user took the key over. If their press means something else (Shift+W over a
        // scripted W), the script's logical key would stay down: release it on its own.
        if let Some(script) = injector.script.as_mut()
            && let Some((_, logical)) = script.keys.remove(&message.key_code)
            && logical != message.logical_key
            && !matches!(logical, Key::Unidentified(_))
        {
            injector.orphaned_logical.push(logical);
        }
    }
    for (message, id) in buttons.read_with_id() {
        if injector.own.buttons.remove(&id.id) {
            continue;
        }
        real.pointer += 1;
        match message.state {
            ButtonState::Pressed => injector.real_buttons.insert(message.button),
            ButtonState::Released => injector.real_buttons.remove(&message.button),
        };
        if let Some(script) = injector.script.as_mut() {
            script.buttons.remove(&message.button);
        }
    }
    for (_, id) in cursor.read_with_id() {
        if !injector.own.cursor.remove(&id.id) {
            real.pointer += 1;
        }
    }
    for (_, id) in wheel.read_with_id() {
        if !injector.own.wheel.remove(&id.id) {
            real.pointer += 1;
        }
    }
    for (_, id) in motion.read_with_id() {
        if !injector.own.motion.remove(&id.id) {
            real.pointer += 1;
        }
    }
    if focus_lost.read().count() > 0 {
        // `bevy_input` releases `ButtonInput<KeyCode>` only; what the script holds is released by
        // `inject` with real messages, so logical keys and buttons are released too. Mouse
        // buttons are left alone: focus loss releases none, and their releases still arrive.
        injector.real_keys.clear();
        injector.focus_lost = true;
    }
    if let Some(script) = injector.script.as_mut()
        && script.outcome.is_none()
    {
        script.real.pointer += real.pointer;
        script.real.keyboard += real.keyboard;
    }
}

/// Runs the script's operations for this frame.
fn inject(world: &mut World) {
    world.resource_scope(|world, mut injector: Mut<InputInjector>| {
        let injector = &mut *injector;
        // An unidentified physical key, so the user's physical key stays down.
        for logical_key in std::mem::take(&mut injector.orphaned_logical) {
            // `bevy_input` ignores the window, so a gone one still releases the key.
            let window = primary_window(world)
                .map(|(window, ..)| window)
                .or(injector.script.as_ref().map(|script| script.window))
                .unwrap_or(Entity::PLACEHOLDER);
            if let Some(id) = world.write_message(KeyboardInput {
                key_code: KeyCode::Unidentified(NativeKeyCode::Unidentified),
                logical_key,
                state: ButtonState::Released,
                text: None,
                repeat: false,
                window,
            }) {
                injector.own.keyboard.insert(id.id);
            }
        }
        let Some(mut script) = injector.script.take() else {
            injector.focus_lost = false;
            return;
        };
        if script.outcome.is_none() {
            run(world, injector, &mut script);
        }
        injector.script = Some(script);
    });
}

fn run(world: &mut World, injector: &mut InputInjector, script: &mut Script) {
    script.frames += 1;
    let window = primary_window(world);
    let mut out = Writer {
        world,
        own: &mut injector.own,
        real_keys: &injector.real_keys,
        real_buttons: &injector.real_buttons,
        // Releases still go to the window the script started on, should it be gone.
        window: window.map_or(script.window, |(entity, ..)| entity),
        scale_factor: window.map_or(1.0, |(_, _, scale_factor)| scale_factor),
    };
    if window.is_none() {
        script.pointer = None;
        abort(&mut out, script, Outcome::Failed(no_window()));
        return;
    }
    // A press written this frame would be cleared with the rest, and later steps would run
    // against a window the user just left: stop rather than continue half done.
    if std::mem::take(&mut injector.focus_lost) {
        abort(
            &mut out,
            script,
            Outcome::Failed(
                CommandError::failed(
                    "focus_lost",
                    format!(
                        "the window lost keyboard focus at step {}; what the script held was \
                         released",
                        script.current
                    ),
                )
                .with_hint("Send the script again; injection itself needs no focus."),
            ),
        );
        return;
    }
    // Removing the work entity or its `JobWork` already marked the job cancelled.
    let removed = out.world.get::<JobWork>(script.work).is_none();
    if removed || script.cancel {
        abort(&mut out, script, Outcome::Cancelled);
        return;
    }
    let now = out.world.resource::<InputClock>().now();
    if now > script.deadline {
        abort(
            &mut out,
            script,
            Outcome::Failed(
                CommandError::failed(
                    "script_timeout",
                    format!(
                        "the script passed its deadline at step {}; what it held was released",
                        script.current
                    ),
                )
                .with_hint(
                    "Raise `timeout_seconds`, or check the window is not minimized: some \
                 platforms stop frames then.",
                ),
            ),
        );
        return;
    }
    loop {
        if !advance(script) {
            end(&mut out, script);
            return;
        }
        match script.ops.front_mut() {
            Some(Op::WaitFor(duration)) => {
                let until = *script.wait_until.get_or_insert(now + *duration);
                if now < until {
                    return;
                }
                script.wait_until = None;
                script.ops.pop_front();
                continue;
            }
            // A frame-count wait is one counter, not one queued op per frame.
            Some(Op::Idle(frames)) => {
                *frames = frames.saturating_sub(1);
                if *frames == 0 {
                    script.ops.pop_front();
                }
            }
            _ => {
                let Some(op) = script.ops.pop_front() else {
                    continue;
                };
                if let Err(err) = execute(&mut out, script, op) {
                    abort(&mut out, script, Outcome::Failed(err));
                    return;
                }
                if std::mem::take(&mut script.left) {
                    injector.pointer_gone = true;
                }
            }
        }
        if !advance(script) && script.keys.is_empty() && script.buttons.is_empty() {
            script.outcome = Some(Outcome::Done);
        }
        return;
    }
}

/// Every step has run. What the script still holds is released a frame after its last step, so
/// a script ending in `key_down` still shows the key pressed for a frame.
fn end(out: &mut Writer, script: &mut Script) {
    release_owned(out, script);
    script.outcome = Some(Outcome::Done);
}

/// Ends the script early: releases what it holds and drops the steps left.
fn abort(out: &mut Writer, script: &mut Script, outcome: Outcome) {
    release_owned(out, script);
    script.ops.clear();
    script.wait_until = None;
    script.outcome = Some(outcome);
}

/// Makes sure `ops` holds the next operation. False when the script has none left.
fn advance(script: &mut Script) -> bool {
    while script.ops.is_empty() {
        let Some(action) = script.actions.get(script.next) else {
            return false;
        };
        script.ops = expand(action);
        script.current = script.next;
        script.next += 1;
    }
    true
}

/// Writes input messages with their `WindowEvent` twins, as `bevy_winit` does, and remembers
/// their ids.
struct Writer<'w> {
    world: &'w mut World,
    own: &'w mut OwnIds,
    real_keys: &'w HashMap<KeyCode, Key>,
    real_buttons: &'w HashSet<MouseButton>,
    window: Entity,
    scale_factor: f32,
}

impl Writer<'_> {
    /// Whether `key`, or another key with the same logical meaning (the other Shift, a held key
    /// that types the same character), is held by the user or the script.
    fn key_held(&self, script: &Script, key: KeyCode, logical: &Key) -> bool {
        let identified = !matches!(logical, Key::Unidentified(_));
        let same = |code: &KeyCode, held: &Key| *code == key || (identified && held == logical);
        self.real_keys.iter().any(|(code, held)| same(code, held))
            || script.keys.iter().any(|(code, (_, held))| same(code, held))
    }

    /// Releases a key the script pressed. While the user holds a key with the same logical
    /// meaning, the release carries no logical key, so the user's logical state survives.
    fn release_key(&mut self, key: KeyCode, logical: Key) {
        let shared = !matches!(logical, Key::Unidentified(_))
            && self.real_keys.values().any(|held| *held == logical);
        let logical = if shared {
            Key::Unidentified(NativeKey::Unidentified)
        } else {
            logical
        };
        self.key(key, logical, ButtonState::Released);
    }

    fn key(&mut self, key_code: KeyCode, logical_key: Key, state: ButtonState) {
        let text = match (&logical_key, state) {
            (Key::Character(text), ButtonState::Pressed) => Some(text.clone()),
            (Key::Space, ButtonState::Pressed) => Some(" ".into()),
            _ => None,
        };
        let message = KeyboardInput {
            key_code,
            logical_key,
            state,
            text,
            repeat: false,
            window: self.window,
        };
        self.world
            .write_message(WindowEvent::KeyboardInput(message.clone()));
        if let Some(id) = self.world.write_message(message) {
            self.own.keyboard.insert(id.id);
        }
    }

    fn button(&mut self, button: MouseButton, state: ButtonState) {
        let message = MouseButtonInput {
            button,
            state,
            window: self.window,
        };
        self.world
            .write_message(WindowEvent::MouseButtonInput(message));
        if let Some(id) = self.world.write_message(message) {
            self.own.buttons.insert(id.id);
        }
    }

    /// `physical` is in physical window pixels; `CursorMoved` carries logical ones.
    fn cursor(&mut self, physical: Vec2) {
        let message = CursorMoved {
            window: self.window,
            position: physical / self.scale_factor,
            delta: None,
        };
        self.world
            .write_message(WindowEvent::CursorMoved(message.clone()));
        if let Some(id) = self.world.write_message(message) {
            self.own.cursor.insert(id.id);
        }
    }

    fn wheel(&mut self, lines: f32) {
        let message = MouseWheel {
            unit: MouseScrollUnit::Line,
            x: 0.0,
            y: lines,
            window: self.window,
            phase: TouchPhase::Moved,
        };
        self.world.write_message(WindowEvent::MouseWheel(message));
        if let Some(id) = self.world.write_message(message) {
            self.own.wheel.insert(id.id);
        }
    }

    fn motion(&mut self, delta: Vec2) {
        let message = MouseMotion { delta };
        self.world.write_message(WindowEvent::MouseMotion(message));
        if let Some(id) = self.world.write_message(message) {
            self.own.motion.insert(id.id);
        }
    }
}

fn execute(out: &mut Writer, script: &mut Script, op: Op) -> Result<(), CommandError> {
    let step = script.current;
    match op {
        Op::Press(keys, level) => {
            for key in keys {
                let logical = keymap::logical_key(key);
                if out.key_held(script, key, &logical) {
                    script.already_held.push(format!("{key:?}"));
                    continue;
                }
                out.key(key, logical.clone(), ButtonState::Pressed);
                script.keys.insert(key, (level, logical));
            }
        }
        Op::Release(keys, level) => {
            for key in keys {
                if script
                    .keys
                    .get(&key)
                    .is_some_and(|(held, _)| *held == level)
                    && let Some((_, logical)) = script.keys.remove(&key)
                {
                    out.release_key(key, logical);
                }
            }
        }
        Op::Char(c) => {
            let key = keymap::key_for_char(c);
            let logical = keymap::logical_char(c);
            // Its release would also release the held key's state.
            if out.key_held(script, key, &logical) {
                script.already_held.push(format!("{key:?}"));
                return Ok(());
            }
            out.key(key, logical.clone(), ButtonState::Pressed);
            out.key(key, logical, ButtonState::Released);
        }
        Op::Pointer { point, button } => {
            if point.is_none() && script.pointer.is_none() && button.is_some() {
                // A press with no point of its own: pin where the pointer is now, so its release
                // lands there even if the user's mouse moves meanwhile.
                script.pointer = current_pointer(out.world);
            }
            move_pointer(out, script, point, step)?;
            if let Some((button, pressed, level)) = button {
                if pressed {
                    if out.real_buttons.contains(&button) || script.buttons.contains_key(&button) {
                        script.already_held.push(format!("{button:?}"));
                    } else {
                        out.button(button, ButtonState::Pressed);
                        script.buttons.insert(button, level);
                    }
                } else if script.buttons.get(&button) == Some(&level) {
                    out.button(button, ButtonState::Released);
                    script.buttons.remove(&button);
                }
            }
        }
        Op::Wheel { point, lines } => {
            move_pointer(out, script, point, step)?;
            out.wheel(lines);
        }
        Op::Motion(delta) => out.motion(delta),
        Op::Leave => {
            // Outside the window, so picking hits nothing; egui also gets `PointerGone`.
            out.cursor(Vec2::splat(-1.0) * out.scale_factor);
            script.pointer = None;
            script.left = true;
        }
        Op::Idle(_) | Op::WaitFor(_) => {}
    }
    Ok(())
}

/// Moves to `point`, or re-asserts the last injected point, after checking its space still has
/// the geometry it was validated against.
fn move_pointer(
    out: &mut Writer,
    script: &mut Script,
    point: Option<Point>,
    step: usize,
) -> Result<(), CommandError> {
    let Some(point) = point.or(script.pointer) else {
        return Ok(());
    };
    let now = geometry(out.world, point.space).map_err(|err| geometry_error(err, step))?;
    if now != point.geometry {
        return Err(CommandError::failed(
            "layout_changed",
            format!(
                "step {step}: the {} view moved or resized since the script was checked",
                point.space.name()
            ),
        )
        .with_hint("Take a new screenshot, pick the point again, and send a new script."));
    }
    out.cursor(now.to_window(point.at));
    script.pointer = Some(point);
    Ok(())
}

/// The mouse pointer's current window point, as a point the script can return to.
fn current_pointer(world: &World) -> Option<Point> {
    let at = mouse_pointer(world)?;
    let geometry = geometry(world, Space::Window).ok()?;
    geometry.contains(at).then_some(Point {
        at,
        space: Space::Window,
        geometry,
    })
}

/// Releases everything the script and its current step hold. Buttons are released where the
/// script last put the pointer, not wherever the user's mouse has gone since.
fn release_owned(out: &mut Writer, script: &mut Script) {
    for (key, (_, logical)) in script.keys.drain() {
        out.release_key(key, logical);
    }
    if script.buttons.is_empty() {
        return;
    }
    let step = script.current;
    if move_pointer(out, script, None, step).is_err() {
        // The view is gone or moved: release outside the window rather than on something else.
        out.cursor(Vec2::splat(-1.0) * out.scale_factor);
    }
    for (button, _) in script.buttons.drain() {
        out.button(button, ButtonState::Released);
    }
}

/// Tells egui the pointer is gone after a `leave` step. Skipped without egui.
fn write_pointer_gone(world: &mut World) {
    let Some(mut injector) = world.get_resource_mut::<InputInjector>() else {
        return;
    };
    if !std::mem::take(&mut injector.pointer_gone) {
        return;
    }
    let Some(context) = world
        .try_query_filtered::<Entity, (With<EguiContext>, With<PrimaryEguiContext>)>()
        .and_then(|mut query| query.single(world).ok())
    else {
        return;
    };
    world.write_message(EguiInputEvent {
        context,
        event: egui::Event::PointerGone,
    });
}

/// Reports the outcome of a script whose last messages were written this frame.
fn finish_script(world: &mut World) {
    let finished = world
        .resource::<InputInjector>()
        .script
        .as_ref()
        .is_some_and(|script| script.outcome.is_some());
    if !finished {
        return;
    }
    let Some(script) = world.resource_mut::<InputInjector>().script.take() else {
        return;
    };
    let result = InputSent {
        frames: script.frames,
        pointer: mouse_pointer(world).map(|p| [p.x, p.y]),
        real_input: script.real,
        already_held: script.already_held,
        egui: egui_wants(world),
    };
    let mut jobs = world.resource_mut::<Jobs>();
    match script.outcome {
        Some(Outcome::Done) => jobs.finish(&script.job, Ok(result)),
        Some(Outcome::Failed(err)) => jobs.finish(&script.job, Err(err)),
        Some(Outcome::Cancelled) | None => jobs.cancelled(script.job.id()),
    }
    if let Ok(work) = world.get_entity_mut(script.work) {
        work.despawn();
    }
}

/// The mouse pointer, in physical window pixels.
pub(crate) fn mouse_pointer(world: &World) -> Option<Vec2> {
    let (_, _, scale_factor) = primary_window(world)?;
    let mut pointers = world.try_query::<(&PointerId, &PointerLocation)>()?;
    pointers
        .iter(world)
        .find(|(id, _)| **id == PointerId::Mouse)
        .and_then(|(_, location)| location.location.as_ref())
        .map(|location| location.position * scale_factor)
}

pub(crate) fn egui_wants(world: &World) -> Option<EguiWants> {
    let wants = world.get_resource::<bevy_egui::input::EguiWantsInput>()?;
    Some(EguiWants {
        wants_pointer: wants.wants_any_pointer_input(),
        wants_keyboard: wants.wants_any_keyboard_input(),
    })
}

pub(crate) fn no_window() -> CommandError {
    CommandError::failed(
        "no_window",
        "the game has no primary window to send input to",
    )
}

pub(crate) fn geometry_error(err: GeometryError, step: usize) -> CommandError {
    match err {
        GeometryError::NoWindow => no_window(),
        GeometryError::Inactive(space) => CommandError::failed(
            "camera_inactive",
            format!("step {step}: the {} view is not shown", space.name()),
        )
        .with_hint(match space {
            Space::GameCamera => {
                "Show the Game View with `editor.set_view` first; the stage also needs a \
                 MainCamera."
            }
            _ => "Show the Scene View with `editor.set_view` first.",
        }),
    }
}
