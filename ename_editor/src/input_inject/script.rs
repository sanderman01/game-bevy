//! A validated script and the per-frame operations each of its steps expands to.

use std::{collections::VecDeque, time::Duration};

use bevy::prelude::*;

use super::geometry::{Geometry, Space};

/// A point in a space, with that space's geometry as it was when the script was validated.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Point {
    pub(crate) at: Vec2,
    pub(crate) space: Space,
    pub(crate) geometry: Geometry,
}

/// One validated step.
#[derive(Clone, Debug)]
pub(crate) enum Action {
    Key {
        key: KeyCode,
        modifiers: Vec<KeyCode>,
    },
    KeyDown(KeyCode),
    KeyUp(KeyCode),
    Text(Vec<char>),
    Move(Point),
    Click {
        point: Point,
        button: MouseButton,
        count: u8,
    },
    ButtonDown(MouseButton),
    ButtonUp(MouseButton),
    Drag {
        from: Point,
        to: Point,
        button: MouseButton,
        frames: u32,
    },
    Scroll {
        lines: f32,
        at: Option<Point>,
    },
    Motion {
        delta: Vec2,
        frames: u32,
    },
    Leave,
    WaitFrames(u32),
    WaitSeconds(f64),
}

impl Action {
    /// Real seconds this step waits on its own, for the default deadline.
    pub(crate) fn wait_seconds(&self) -> f64 {
        match self {
            Self::WaitSeconds(seconds) => *seconds,
            _ => 0.0,
        }
    }

    /// Frames this step takes at least, for the default deadline.
    pub(crate) fn frames(&self) -> u32 {
        match self {
            Self::Key { modifiers, .. } if !modifiers.is_empty() => 4,
            Self::Key { .. } => 2,
            Self::Text(chars) => u32::try_from(chars.len()).unwrap_or(u32::MAX),
            Self::Click { count, .. } => 1 + 2 * u32::from(*count),
            Self::Drag { frames, .. } => frames + 3,
            Self::Scroll { at: Some(_), .. } => 2,
            Self::Motion { frames, .. } | Self::WaitFrames(frames) => *frames,
            _ => 1,
        }
    }
}

/// Who holds an injected press: one step (a `key` step's modifiers, a click's button) or the
/// whole script (`key_down`, `button_down`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Level {
    Step,
    Script,
}

/// One frame's worth of a step.
#[derive(Clone, Debug)]
pub(crate) enum Op {
    Press(Vec<KeyCode>, Level),
    Release(Vec<KeyCode>, Level),
    /// Press and release in one frame, typing `char`.
    Char(char),
    /// Moves the pointer to `point` (or re-asserts the last injected point when `None`), then
    /// optionally presses or releases a button there.
    Pointer {
        point: Option<Point>,
        button: Option<(MouseButton, bool, Level)>,
    },
    Wheel {
        point: Option<Point>,
        lines: f32,
    },
    Motion(Vec2),
    Leave,
    /// Does nothing for this many frames.
    Idle(u32),
    /// Holds until the clock passes `Duration` since the step started.
    WaitFor(Duration),
}

/// The operations of `action`, one per frame.
pub(crate) fn expand(action: &Action) -> VecDeque<Op> {
    let mut ops = VecDeque::new();
    match action {
        Action::Key { key, modifiers } => {
            if !modifiers.is_empty() {
                ops.push_back(Op::Press(modifiers.clone(), Level::Step));
            }
            ops.push_back(Op::Press(vec![*key], Level::Step));
            ops.push_back(Op::Release(vec![*key], Level::Step));
            if !modifiers.is_empty() {
                ops.push_back(Op::Release(modifiers.clone(), Level::Step));
            }
        }
        Action::KeyDown(key) => ops.push_back(Op::Press(vec![*key], Level::Script)),
        Action::KeyUp(key) => ops.push_back(Op::Release(vec![*key], Level::Script)),
        Action::Text(chars) => ops.extend(chars.iter().copied().map(Op::Char)),
        Action::Move(point) => ops.push_back(Op::Pointer {
            point: Some(*point),
            button: None,
        }),
        Action::Click {
            point,
            button,
            count,
        } => {
            ops.push_back(Op::Pointer {
                point: Some(*point),
                button: None,
            });
            for _ in 0..*count {
                for pressed in [true, false] {
                    ops.push_back(Op::Pointer {
                        point: Some(*point),
                        button: Some((*button, pressed, Level::Step)),
                    });
                }
            }
        }
        Action::ButtonDown(button) => ops.push_back(Op::Pointer {
            point: None,
            button: Some((*button, true, Level::Script)),
        }),
        Action::ButtonUp(button) => ops.push_back(Op::Pointer {
            point: None,
            button: Some((*button, false, Level::Script)),
        }),
        Action::Drag {
            from,
            to,
            button,
            frames,
        } => {
            ops.push_back(Op::Pointer {
                point: Some(*from),
                button: None,
            });
            ops.push_back(Op::Pointer {
                point: Some(*from),
                button: Some((*button, true, Level::Step)),
            });
            for i in 1..=*frames {
                let t = i as f32 / *frames as f32;
                ops.push_back(Op::Pointer {
                    point: Some(Point {
                        at: from.at.lerp(to.at, t),
                        ..*to
                    }),
                    button: None,
                });
            }
            ops.push_back(Op::Pointer {
                point: Some(*to),
                button: Some((*button, false, Level::Step)),
            });
        }
        Action::Scroll { lines, at } => {
            if at.is_some() {
                ops.push_back(Op::Pointer {
                    point: *at,
                    button: None,
                });
            }
            ops.push_back(Op::Wheel {
                point: *at,
                lines: *lines,
            });
        }
        Action::Motion { delta, frames } => {
            let frames = (*frames).max(1);
            ops.extend((0..frames).map(|_| Op::Motion(*delta / frames as f32)));
        }
        Action::Leave => ops.push_back(Op::Leave),
        Action::WaitFrames(frames) if *frames > 0 => ops.push_back(Op::Idle(*frames)),
        Action::WaitFrames(_) => {}
        Action::WaitSeconds(seconds) => {
            ops.push_back(Op::WaitFor(Duration::from_secs_f64(seconds.max(0.0))));
        }
    }
    ops
}
