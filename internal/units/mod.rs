//! Human-readable sizes and durations, shared by every surface that renders
//! one.
//!
//! Both formatters are total over their input type. That is the point of the
//! module rather than a nicety: a formatter with a ceiling has a silent cliff,
//! and the cliff is only ever found by whoever is big enough to hit it. `stats`
//! saturated at `TiB` for years, so a petabyte rendered `1024.0TiB`.
//!
//! Both are configurable, and the defaults belong to the caller rather than to
//! this module: a table cell has a width budget a summary line does not, and
//! the right base depends on what the reader is comparing against: podman and
//! docker print decimal, while `free` and `htop` are binary.

mod bytes;
mod duration;

pub(crate) use bytes::{format_bytes, SizeFormat};
pub(crate) use duration::{format_duration, DurationFormat};
