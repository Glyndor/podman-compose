//! The identity palette: which colour a service is drawn in.
//!
//! Colour here is identity, never status. Red, green and yellow are reserved for
//! state everywhere in podup, so no identity colour may sit near one.

/// Identity colours, as xterm-256 indices.
///
/// Chosen so each clears 3:1 contrast against both white and black, sits at
/// least deltaE 40 from the semantic red/green/yellow, and is at least deltaE 22
/// from every other entry. `palette_tests.rs` recomputes all three from the sRGB
/// values, so an edit that adds a pretty but unreadable colour fails the build
/// rather than shipping.
///
/// Twenty is what survives those three filters, not a round number: of the 256
/// colours only 80 clear 3:1 against both backgrounds at all, and 61 of those
/// are far enough from the status colours. At the stricter 4.5:1 text bar only
/// six qualify anywhere in the 256-colour space, which is why the wide palette
/// targets the 3:1 interface bar instead.
pub(crate) const WIDE_PALETTE: [u8; 20] = [
	6, 13, 32, 33, 59, 61, 62, 65, 67, 93, 99, 128, 133, 138, 139, 163, 168, 170, 197, 198,
];

/// The palette entry at `index`, wrapping when a project has more services than
/// the palette has colours. Repeating past the twentieth is better than running
/// out of colour.
pub(crate) fn wide_colour(index: usize) -> anstyle::Color {
	anstyle::Ansi256Color(WIDE_PALETTE[index % WIDE_PALETTE.len()]).into()
}

/// Whether the terminal can render the wide palette, from the values that
/// describe it.
///
/// Pure so all four tiers are tested without a terminal. First match wins:
/// - `COLORTERM` announcing truecolor or 24bit
/// - `TERM` carrying the conventional `256color` marker
/// - on Windows, unconditionally: the console there takes 256 colours in every
///   modern host, and Windows commonly sets neither variable, so without this
///   tier every Windows user would fall back
/// - anything else falls back to the six ANSI basics
///
/// Guessing wider paints escape codes into the output of a terminal that cannot
/// decode them.
pub(crate) fn supports_wide_palette(
	colorterm: Option<&str>,
	term: Option<&str>,
	windows_vt: bool,
) -> bool {
	if matches!(colorterm, Some("truecolor" | "24bit")) {
		return true;
	}
	if term.is_some_and(|t| t.contains("256color")) {
		return true;
	}
	windows_vt
}

/// [`supports_wide_palette`] against this process's environment.
///
/// Read once and cached: the environment does not change under a running
/// command, and every styled cell would otherwise pay two `var_os` calls.
///
/// The Windows argument is `cfg!(windows)` — the target, not a runtime probe of
/// virtual-terminal processing. That is sufficient because this is only ever
/// consulted after the colour gate has decided to emit colour at all, which
/// already required a terminal and a permitting `--ansi`/`NO_COLOR`; and on a
/// console that genuinely lacks VT, anstream's wincon path converts the escapes
/// rather than printing them raw.
///
/// Cached for the process, so tests must exercise [`supports_wide_palette`]
/// rather than this: two tests varying the environment around this function
/// would see whichever answer was cached first and pass or fail on test
/// scheduling.
pub(crate) fn wide_palette_available() -> bool {
	use std::sync::OnceLock;
	static AVAILABLE: OnceLock<bool> = OnceLock::new();
	*AVAILABLE.get_or_init(|| {
		let colorterm = std::env::var("COLORTERM").ok();
		let term = std::env::var("TERM").ok();
		supports_wide_palette(colorterm.as_deref(), term.as_deref(), cfg!(windows))
	})
}

use std::collections::HashMap;

/// Give each name its own palette slot, walking them in sorted order.
///
/// Sequential rather than hashed: a hash does not know how many services exist,
/// so it collides well before the palette is full — measured on a four-service
/// project, two came out the same colour, and over twenty colours five services
/// still collide about 42% of the time.
///
/// Sorting is what keeps it deterministic. Assigning in order of appearance
/// would repaint everything when a service moved in the compose file; sorted,
/// the same file always renders the same and adding a service only shifts those
/// that sort after it. Nothing is written to disk.
pub(crate) fn assign(names: &[String]) -> HashMap<String, usize> {
	let mut sorted: Vec<&String> = names.iter().collect();
	sorted.sort();
	sorted.dedup();
	sorted
		.into_iter()
		.enumerate()
		.map(|(i, name)| (name.clone(), i % WIDE_PALETTE.len()))
		.collect()
}

/// The palette slot for `label`, falling back to a hash for a label that was
/// never registered.
///
/// The fallback matters for anything podup renders but did not resolve from the
/// compose file — an orphan container, a project listed by `ls`. A stable wrong
/// colour reads better than no colour at all, and the hash is what guarantees
/// the same label always lands on the same slot.
pub(crate) fn colour_for(label: &str, map: &HashMap<String, usize>) -> usize {
	if let Some(&index) = map.get(label) {
		return index;
	}
	let mut h: u64 = 0xcbf2_9ce4_8422_2325;
	for b in label.bytes() {
		h ^= u64::from(b);
		h = h.wrapping_mul(0x0100_0000_01b3);
	}
	(h % WIDE_PALETTE.len() as u64) as usize
}

#[cfg(test)]
#[path = "palette_tests.rs"]
mod tests;
