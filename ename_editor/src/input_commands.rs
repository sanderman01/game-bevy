//! `input.send` and `input.state`: drive the editor window with injected input, and read what the
//! pointer, held keys, egui and picking make of it.

use std::time::Duration;

use bevy::{
    gizmos::transform_gizmo::TransformGizmoState,
    picking::{hover::HoverMap, pointer::PointerId},
    prelude::*,
    window::PrimaryWindow,
};
use ename_engine::command::{
    Call, CommandAppExt, CommandError, CommandSpec, JobHandle, NoParams, OnCancel,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::input_inject::{
    self, Action, InputInjector, InputSent, MAX_FRAMES, MAX_SECONDS, Modifier, Point, Space,
    close_key_names, geometry, geometry_error, modifier_key, parse_key, primary_window,
};

/// Seconds a script gets beyond its own waits before it times out.
const TIMEOUT_MARGIN: f64 = 10.0;

/// The frame rate the default timeout assumes for frame-counted steps: slow, so a loaded editor
/// does not time out.
const SLOW_FPS: f64 = 20.0;

/// Registers `input.send` and `input.state`.
pub(crate) struct InputCommandsPlugin;

impl Plugin for InputCommandsPlugin {
    fn build(&self, app: &mut App) {
        app.register_job_command(
            CommandSpec::new(
                "input.send",
                "Send keys, text, pointer moves, clicks, drags and scrolls to the game window as \
                 real input, without focus or an OS injector. Coordinates are pixels of a \
                 screenshot_capture PNG.",
            ),
            send,
        )
        .register_command(
            CommandSpec::new(
                "input.state",
                "Read the window, the pointer, held keys and buttons, egui's interest and what \
                 picking hovers.",
            )
            .read_only(),
            state,
        );
    }
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct SendParams {
    /// Run in order, one frame per key, button or pointer event.
    steps: Vec<Step>,
    /// Wall-clock limit; past it the script releases what it holds and fails `script_timeout`.
    /// Defaults to the script's own waits plus 10 s.
    #[serde(default)]
    timeout_seconds: Option<f64>,
}

/// One step. Keys are Bevy `KeyCode` names (`F10`, `KeyW`, `ShiftLeft`, `Space`). `x`/`y` are
/// pixels of the `in` source's full-size screenshot, origin top-left: `window` (default,
/// physical window pixels), `editor_camera` (the Scene View) or `game_camera` (the Game View).
#[derive(Deserialize, JsonSchema)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
enum Step {
    /// Modifiers down, key down, key up, modifiers up: one frame each.
    Key {
        key: String,
        #[serde(default)]
        modifiers: Vec<Modifier>,
    },
    /// Held until a `key_up` or the end of the script.
    KeyDown {
        key: String,
    },
    KeyUp {
        key: String,
    },
    /// One frame per character, typed on a US layout.
    Text {
        text: String,
    },
    Move {
        x: f32,
        y: f32,
        #[serde(default, rename = "in")]
        space: Space,
    },
    /// A move, then down and up per `count` (2 is a double click).
    Click {
        x: f32,
        y: f32,
        #[serde(default, rename = "in")]
        space: Space,
        #[serde(default)]
        button: Button,
        #[serde(default)]
        count: Option<u8>,
    },
    /// At the pointer's current point; held until `button_up` or the end of the script.
    ButtonDown {
        #[serde(default)]
        button: Button,
    },
    ButtonUp {
        #[serde(default)]
        button: Button,
    },
    /// Move to `from`, press, `frames` moves to `to` (default 10), release.
    Drag {
        from: [f32; 2],
        to: [f32; 2],
        #[serde(default, rename = "in")]
        space: Space,
        #[serde(default)]
        button: Button,
        #[serde(default)]
        frames: Option<u32>,
    },
    /// Wheel lines; positive scrolls up. At `x`/`y` when given, else where the pointer is.
    Scroll {
        lines: f32,
        #[serde(default)]
        x: Option<f32>,
        #[serde(default)]
        y: Option<f32>,
        #[serde(default, rename = "in")]
        space: Space,
    },
    /// Raw mouse motion (what mouse-look reads), split over `frames` (default 1).
    Motion {
        dx: f32,
        dy: f32,
        #[serde(default)]
        frames: Option<u32>,
    },
    /// Moves the pointer out of the window, clearing egui and picking hover. Use before a
    /// screenshot.
    Leave {},
    /// `frames` frames, or `seconds` of wall-clock time. Exactly one.
    Wait {
        #[serde(default)]
        frames: Option<u32>,
        #[serde(default)]
        seconds: Option<f64>,
    },
}

#[derive(Deserialize, JsonSchema, Clone, Copy, Default)]
#[serde(rename_all = "snake_case")]
enum Button {
    #[default]
    Left,
    Right,
    Middle,
    Back,
    Forward,
}

impl From<Button> for MouseButton {
    fn from(button: Button) -> Self {
        match button {
            Button::Left => Self::Left,
            Button::Right => Self::Right,
            Button::Middle => Self::Middle,
            Button::Back => Self::Back,
            Button::Forward => Self::Forward,
        }
    }
}

fn send(
    In(call): In<Call<SendParams>>,
    world: &mut World,
) -> Result<JobHandle<InputSent>, CommandError> {
    if call.params.steps.is_empty() {
        return Err(CommandError::invalid("no_steps", "`steps` is empty"));
    }
    if world.resource::<InputInjector>().is_busy() {
        return Err(busy("another `input.send` script is running"));
    }
    if world
        .get_resource::<TransformGizmoState>()
        .is_some_and(|gizmo| gizmo.active)
    {
        return Err(busy("the transform gizmo is being dragged by the user"));
    }
    let (window, ..) = primary_window(world).ok_or_else(input_inject::no_window)?;
    let actions = call
        .params
        .steps
        .iter()
        .enumerate()
        .map(|(index, step)| validate(world, index, step))
        .collect::<Result<Vec<_>, _>>()?;
    let waits: f64 = actions.iter().map(Action::wait_seconds).sum();
    let frames: f64 = actions
        .iter()
        .map(|action| f64::from(action.frames()))
        .sum();
    let expected = waits + frames / SLOW_FPS;
    if expected > MAX_SECONDS {
        return Err(CommandError::invalid(
            "invalid_params",
            format!(
                "the script would take about {expected:.0} s, more than the {MAX_SECONDS} s a \
                 script may run"
            ),
        )
        .with_hint("Split it into several `input.send` calls."));
    }
    let timeout = call
        .params
        .timeout_seconds
        .unwrap_or((expected + TIMEOUT_MARGIN).min(MAX_SECONDS));
    if !(timeout.is_finite() && timeout > 0.0 && timeout <= MAX_SECONDS) {
        return Err(CommandError::invalid(
            "invalid_params",
            format!("`timeout_seconds` must be above 0 and at most {MAX_SECONDS}"),
        ));
    }

    let work = world.spawn(Name::new("input.send")).id();
    let job = match call.start_job(world, work, OnCancel::Notify) {
        Ok(job) => job,
        Err(err) => {
            world.despawn(work);
            return Err(err);
        }
    };
    input_inject::start(
        world,
        actions,
        window,
        work,
        job.clone(),
        Duration::from_secs_f64(timeout),
    );
    Ok(job)
}

fn busy(message: &str) -> CommandError {
    CommandError::failed("input_busy", message.to_owned())
        .with_hint("Wait for it with `job.wait`, or end it with `job.cancel`.")
}

fn validate(world: &World, index: usize, step: &Step) -> Result<Action, CommandError> {
    let key = |name: &str| key(name, index);
    let point = |x: f32, y: f32, space: Space| point(world, index, Vec2::new(x, y), space);
    Ok(match step {
        Step::Key {
            key: name,
            modifiers,
        } => Action::Key {
            key: key(name)?,
            modifiers: modifiers.iter().copied().map(modifier_key).collect(),
        },
        Step::KeyDown { key: name } => Action::KeyDown(key(name)?),
        Step::KeyUp { key: name } => Action::KeyUp(key(name)?),
        Step::Text { text } => {
            let chars: Vec<char> = text.chars().collect();
            frame_count(index, u32::try_from(chars.len()).unwrap_or(u32::MAX))?;
            Action::Text(chars)
        }
        Step::Move { x, y, space } => Action::Move(point(*x, *y, *space)?),
        Step::Click {
            x,
            y,
            space,
            button,
            count,
        } => Action::Click {
            point: point(*x, *y, *space)?,
            button: (*button).into(),
            count: count.unwrap_or(1).max(1),
        },
        Step::ButtonDown { button } => Action::ButtonDown((*button).into()),
        Step::ButtonUp { button } => Action::ButtonUp((*button).into()),
        Step::Drag {
            from,
            to,
            space,
            button,
            frames,
        } => Action::Drag {
            from: point(from[0], from[1], *space)?,
            to: point(to[0], to[1], *space)?,
            button: (*button).into(),
            frames: frame_count(index, frames.unwrap_or(10))?,
        },
        Step::Scroll { lines, x, y, space } => Action::Scroll {
            lines: *lines,
            at: match (x, y) {
                (Some(x), Some(y)) => Some(point(*x, *y, *space)?),
                (None, None) => None,
                _ => {
                    return Err(step_failed(
                        index,
                        "`scroll` needs both `x` and `y`, or neither",
                    ));
                }
            },
        },
        Step::Motion { dx, dy, frames } => Action::Motion {
            delta: Vec2::new(*dx, *dy),
            frames: frame_count(index, frames.unwrap_or(1))?,
        },
        Step::Leave {} => Action::Leave,
        Step::Wait { frames, seconds } => match (frames, seconds) {
            (Some(frames), None) => Action::WaitFrames(frame_count(index, *frames)?),
            (None, Some(seconds)) if (0.0..=MAX_SECONDS).contains(seconds) => {
                Action::WaitSeconds(*seconds)
            }
            _ => {
                return Err(step_failed(
                    index,
                    "`wait` needs exactly one of `frames` or `seconds` (0 to 3600)",
                ));
            }
        },
    })
}

fn key(name: &str, index: usize) -> Result<KeyCode, CommandError> {
    parse_key(name).ok_or_else(|| {
        CommandError::invalid(
            "unknown_key",
            format!("step {index}: `{name}` is not a key name"),
        )
        .with_hint(format!(
            "Keys are Bevy `KeyCode` names. Close: {}.",
            close_key_names(name).join(", ")
        ))
    })
}

fn point(world: &World, index: usize, at: Vec2, space: Space) -> Result<Point, CommandError> {
    let geometry = geometry(world, space).map_err(|err| geometry_error(err, index))?;
    if !geometry.contains(at) {
        let size = geometry.image();
        return Err(CommandError::invalid(
            "out_of_source",
            format!(
                "step {index}: ({}, {}) is outside the {} source, which is {}x{} pixels",
                at.x,
                at.y,
                space.name(),
                size.x,
                size.y
            ),
        ));
    }
    Ok(Point {
        at,
        space,
        geometry,
    })
}

/// A step's frame count, at least 1 and at most [`MAX_FRAMES`].
fn frame_count(index: usize, frames: u32) -> Result<u32, CommandError> {
    if frames > MAX_FRAMES {
        return Err(step_failed(
            index,
            &format!("{frames} frames is more than the {MAX_FRAMES} one step may take"),
        ));
    }
    Ok(frames.max(1))
}

fn step_failed(index: usize, message: &str) -> CommandError {
    CommandError::invalid("step_failed", format!("step {index}: {message}"))
}

#[derive(Serialize, JsonSchema)]
struct InputState {
    window: WindowState,
    pointer: PointerState,
    held: Held,
    /// `null` without egui.
    egui: Option<EguiState>,
    /// What picking hovers, for every pointer.
    picking: Vec<Hover>,
}

#[derive(Serialize, JsonSchema)]
struct WindowState {
    /// Physical pixels.
    size: [u32; 2],
    scale_factor: f32,
    focused: bool,
}

#[derive(Serialize, JsonSchema)]
struct PointerState {
    /// The mouse pointer in physical window pixels, real or injected; `null` if picking has none.
    position: Option<[f32; 2]>,
    /// "scene_view", "game_view", "egui" (any other part of the window) or "outside".
    over: &'static str,
}

#[derive(Serialize, JsonSchema)]
struct Held {
    keys: Vec<String>,
    buttons: Vec<String>,
}

#[derive(Serialize, JsonSchema)]
struct EguiState {
    wants_pointer: bool,
    wants_keyboard: bool,
    popup_open: bool,
}

#[derive(Serialize, JsonSchema)]
struct Hover {
    /// "mouse", "touch:<n>" or "custom:<uuid>".
    pointer: String,
    entity: u64,
    name: Option<String>,
    /// Distance from the camera; hit positions are left out, being floating-origin relative.
    depth: f32,
}

fn state(In(_): In<Call<NoParams>>, world: &mut World) -> Result<InputState, CommandError> {
    let window = world
        .try_query_filtered::<&Window, With<PrimaryWindow>>()
        .and_then(|mut query| query.single(world).ok())
        .ok_or_else(input_inject::no_window)?;
    let window_state = WindowState {
        size: window.physical_size().to_array(),
        scale_factor: window.scale_factor(),
        focused: window.focused,
    };
    let position = input_inject::mouse_pointer(world);
    let over = match position {
        None => "outside",
        Some(p)
            if !(p.cmpge(Vec2::ZERO).all()
                && p.cmplt(UVec2::from(window_state.size).as_vec2()).all()) =>
        {
            "outside"
        }
        Some(p) if covered(world, Space::EditorCamera, p) => "scene_view",
        Some(p) if covered(world, Space::GameCamera, p) => "game_view",
        Some(_) => "egui",
    };
    let held = Held {
        keys: pressed_names(world.get_resource::<ButtonInput<KeyCode>>()),
        buttons: pressed_names(world.get_resource::<ButtonInput<MouseButton>>()),
    };
    let egui = world
        .get_resource::<bevy_egui::input::EguiWantsInput>()
        .map(|wants| EguiState {
            wants_pointer: wants.wants_any_pointer_input(),
            wants_keyboard: wants.wants_any_keyboard_input(),
            popup_open: wants.is_popup_open(),
        });
    let mut picking = Vec::new();
    if let Some(hover) = world.get_resource::<HoverMap>() {
        for (pointer, hits) in hover.iter() {
            for (entity, hit) in hits {
                picking.push(Hover {
                    pointer: pointer_name(pointer),
                    entity: entity.to_bits(),
                    name: world.get::<Name>(*entity).map(|name| name.to_string()),
                    depth: hit.depth,
                });
            }
        }
    }
    picking.sort_by(|a, b| a.pointer.cmp(&b.pointer).then(a.depth.total_cmp(&b.depth)));
    Ok(InputState {
        window: window_state,
        pointer: PointerState {
            position: position.map(|p| [p.x, p.y]),
            over,
        },
        held,
        egui,
        picking,
    })
}

fn covered(world: &World, space: Space, point: Vec2) -> bool {
    geometry(world, space).is_ok_and(|geometry| geometry.covers(point))
}

fn pressed_names<T: Copy + Eq + std::hash::Hash + Send + Sync + std::fmt::Debug + 'static>(
    input: Option<&ButtonInput<T>>,
) -> Vec<String> {
    let mut names: Vec<String> = input
        .into_iter()
        .flat_map(|input| input.get_pressed().map(|key| format!("{key:?}")))
        .collect();
    names.sort();
    names
}

fn pointer_name(pointer: &PointerId) -> String {
    match pointer {
        PointerId::Mouse => "mouse".to_owned(),
        PointerId::Touch(id) => format!("touch:{id}"),
        PointerId::Custom(id) => format!("custom:{id}"),
    }
}
