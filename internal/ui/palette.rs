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

#[cfg(test)]
#[path = "palette_tests.rs"]
mod tests;
