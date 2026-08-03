//! Byte counts rendered against either unit ladder.

/// Which ladder of units a byte count is rendered against.
///
/// Neither is universally right, so the caller picks per surface: podman and
/// docker print decimal (`8.71MB`), while `stats` is read next to `free` and
/// `htop`, which are binary.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SizeBase {
	/// Powers of 1024, suffixed `B`, `KiB`, `MiB`, `GiB`, `TiB`, `PiB`, `EiB`.
	Binary,
	/// Powers of 1000, suffixed `B`, `KB`, `MB`, `GB`, `TB`, `PB`, `EB`.
	Decimal,
}

/// Binary units, largest exponent last. `EiB` is 1024^6; `u64::MAX` is just
/// under 16 of them, so the ladder covers the whole input type.
const BINARY_UNITS: [&str; 7] = ["B", "KiB", "MiB", "GiB", "TiB", "PiB", "EiB"];

/// Decimal units, largest exponent last. `EB` is 1000^6; `u64::MAX` is a little
/// over 18 of them, so this ladder covers the whole input type too.
///
/// `kB` with a lowercase k, which is the SI prefix for kilo — `K` is kelvin.
/// This is not pedantry about the standard: it is what the tools this table is
/// compared against print. `podman images` rendered `805 kB` on Podman 5.7.0,
/// and every other prefix from mega up is uppercase in the same output.
const DECIMAL_UNITS: [&str; 7] = ["B", "kB", "MB", "GB", "TB", "PB", "EB"];

impl SizeBase {
	/// The factor between two neighbouring units on this ladder.
	const fn step(self) -> u64 {
		match self {
			Self::Binary => 1024,
			Self::Decimal => 1000,
		}
	}

	/// The unit names, index 0 being plain bytes.
	const fn units(self) -> &'static [&'static str; 7] {
		match self {
			Self::Binary => &BINARY_UNITS,
			Self::Decimal => &DECIMAL_UNITS,
		}
	}
}

/// How many numbers a rendered size is made of.
///
/// The two shapes take different settings, and neither setting means anything
/// under the other: decimals on a composite value would read `1GB 512.00MB`,
/// and a component count on a single value is always one. Splitting them keeps
/// a caller from asking for a combination that has no rendering.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SizeShape {
	/// One unit carrying `decimals` decimal places: `8.71MB`. What a table cell
	/// wants, since a column has a width budget.
	Single { decimals: usize },
	/// One unit carrying `digits` significant digits: `98.2MB`, `8.71MB`,
	/// `805kB`, `1.01GB`.
	///
	/// This is what podman and docker print, measured on Podman 5.7.0 and
	/// docker compose v5.1.3 rather than assumed — `podman images` rendered
	/// `1.01 GB`, `101 MB` and `103 MB`, and `docker compose images` rendered
	/// `98.2MB`, all three digits wide. A fixed decimal count cannot express it:
	/// `98.23MB` and `8.71MB` disagree about how many decimals the reference
	/// uses because the reference is not counting decimals at all.
	///
	/// The digit count is also what keeps the column from breathing — every
	/// value is three digits plus its unit, whatever its magnitude.
	Significant { digits: usize },
	/// Up to `parts` whole components, largest first, zeros skipped:
	/// `1TB 1GB`, `1GB 512MB`. For the places where the number is the point.
	Composite { parts: usize },
}

/// How to render a byte count.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct SizeFormat {
	/// The unit ladder.
	pub base: SizeBase,
	/// How many numbers the result carries.
	pub shape: SizeShape,
}

impl SizeFormat {
	/// Two decimals against the binary ladder: `8.31MiB`.
	pub(crate) const fn binary() -> Self {
		Self {
			base: SizeBase::Binary,
			shape: SizeShape::Single { decimals: 2 },
		}
	}

	/// Two decimals against the decimal ladder: `8.71MB`. This is what podman
	/// and docker print, so it is the base to compare a table against theirs.
	pub(crate) const fn decimal() -> Self {
		Self {
			base: SizeBase::Decimal,
			shape: SizeShape::Single { decimals: 2 },
		}
	}

	/// Same ladder, `decimals` decimal places on a single component.
	pub(crate) const fn with_decimals(self, decimals: usize) -> Self {
		Self {
			shape: SizeShape::Single { decimals },
			..self
		}
	}

	/// Same ladder, up to `parts` whole components.
	pub(crate) const fn with_parts(self, parts: usize) -> Self {
		Self {
			shape: SizeShape::Composite { parts },
			..self
		}
	}

	/// Same ladder, `digits` significant digits on a single component.
	pub(crate) const fn with_significant(self, digits: usize) -> Self {
		Self {
			shape: SizeShape::Significant { digits },
			..self
		}
	}
}

/// How many decimal places a single-unit value shows.
///
/// Two ways of asking, because a table that has to line up against podman's own
/// output is asking a different question from one that just needs a readable
/// number.
#[derive(Clone, Copy)]
enum Precision {
	/// Always this many decimals, whatever the magnitude.
	Fixed(usize),
	/// As many decimals as leave this many digits in total.
	Significant(usize),
}

impl Precision {
	/// Decimal places for a value already reduced onto its unit.
	fn decimals_for(self, value: f64) -> usize {
		match self {
			Self::Fixed(decimals) => decimals,
			Self::Significant(digits) => {
				let whole = value.trunc().abs();
				let before_the_point = if whole < 1.0 {
					1
				} else {
					whole.log10().floor() as usize + 1
				};
				digits.max(1).saturating_sub(before_the_point)
			}
		}
	}
}

/// Render `bytes` as a human-readable size.
///
/// Total over `u64`: both ladders reach an exponent that `u64::MAX` cannot
/// exceed, so nothing saturates into a wrong unit the way a table stopping at
/// `TiB` renders a petabyte as `1024.0TiB`.
///
/// Zero renders as `0B` under either shape. A composite value is computed with
/// integer division, so `1TB 1GB` is exact rather than a float rounded twice.
pub(crate) fn format_bytes(bytes: u64, fmt: &SizeFormat) -> String {
	match fmt.shape {
		SizeShape::Single { decimals } => single(bytes, fmt.base, Precision::Fixed(decimals)),
		SizeShape::Significant { digits } => {
			single(bytes, fmt.base, Precision::Significant(digits))
		}
		SizeShape::Composite { parts } => composite(bytes, fmt.base, parts),
	}
}

/// One number and one unit: the largest unit whose value is at least one.
fn single(bytes: u64, base: SizeBase, precision: Precision) -> String {
	let units = base.units();
	let step = base.step();

	let mut index = 0;
	let mut divisor = 1u64;
	while index + 1 < units.len() && bytes / divisor >= step {
		divisor *= step;
		index += 1;
	}

	// Whole bytes never carry decimals: `512B`, not `512.00B`. There is no
	// fraction of a byte to report, and the padding costs column width.
	if index == 0 {
		return format!("{bytes}B");
	}

	let mut value = bytes as f64 / divisor as f64;
	let mut decimals = precision.decimals_for(value);
	// Rounding can carry the value onto the next rung: 1048575 bytes is
	// 1023.999 KiB, which prints as `1024.00KiB` — right arithmetic, and the
	// same unit-boundary artefact the ladder exists to avoid. Promote once,
	// which is enough: the value was below `step` before rounding, so it cannot
	// land more than one rung high. At the top rung there is nowhere to go, so
	// `u64::MAX` reads `16.00EiB` rather than inventing an eighth unit.
	let rounds_onto_the_next_unit = format!("{value:.decimals$}")
		.parse::<f64>()
		.is_ok_and(|rounded| rounded >= step as f64);
	if rounds_onto_the_next_unit && index + 1 < units.len() {
		divisor *= step;
		index += 1;
		value = bytes as f64 / divisor as f64;
		// The new value has a different magnitude, so a significant-digit
		// request wants a different number of decimals for it. Recomputing is
		// the whole reason the promotion happens before the format rather than
		// as a retry after one.
		decimals = precision.decimals_for(value);
	}
	format!("{value:.decimals$}{}", units[index])
}

/// Up to `parts` whole components, largest first, zeros skipped.
///
/// Skipping zeros rather than stopping at one is what makes `1y 1d 1h 4s 5ms`
/// possible for durations and `1TB 5MB` here: the components that survive are
/// the ones that carry value, not the ones that happen to be adjacent.
fn composite(bytes: u64, base: SizeBase, parts: usize) -> String {
	let units = base.units();
	let step = base.step();
	let wanted = parts.max(1);

	// Largest divisor first: step^(units.len() - 1).
	let mut divisor = 1u64;
	for _ in 1..units.len() {
		divisor *= step;
	}

	let mut remainder = bytes;
	let mut out: Vec<String> = Vec::with_capacity(wanted);
	for index in (0..units.len()).rev() {
		if out.len() == wanted {
			break;
		}
		let count = remainder / divisor;
		if count > 0 {
			out.push(format!("{count}{}", units[index]));
			remainder %= divisor;
		}
		if index > 0 {
			divisor /= step;
		}
	}

	if out.is_empty() {
		return "0B".to_string();
	}
	out.join(" ")
}

#[cfg(test)]
#[path = "bytes_tests.rs"]
mod tests;
