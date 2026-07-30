//! The live board: what a lifecycle command is working through, and where it
//! has got to.
//!
//! One model, two renderers. Which one runs is decided from the terminal and the
//! colour choice, never from the command: `up` on a tty repaints a tail region,
//! `up` in CI emits the same events as plain append-only lines. **Animation in a
//! CI log is a defect**, and so is a CI log that says less than the terminal
//! did — both renderers see every event.

mod board;

pub use board::{Board, Kind, Row, State};
