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
//! the right base depends on what the reader is comparing against — podman and
//! docker print decimal, while `free` and `htop` are binary.

// `stats` is the only caller so far, and it wants one shape: binary units at
// one decimal. The decimal ladder, the composite shape and every duration are
// exercised by this module's own tests and by nothing else yet, which is what
// the allow is for — the surfaces that need them are `ps`, `images` and
// `volumes`, and #1298 is where they arrive. Take this line out with that
// change; it is scoped to the module so a genuinely dead item elsewhere still
// warns. `unused_imports` rides along because the re-exports below are what a
// caller will reach for, and an unused one is the same fact reported twice.
#![allow(dead_code, unused_imports)]

mod bytes;
mod duration;

pub(crate) use bytes::{format_bytes, SizeBase, SizeFormat, SizeShape};
pub(crate) use duration::{format_duration, DurationFormat};
