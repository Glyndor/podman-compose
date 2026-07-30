use super::{supports_wide_palette, wide_colour, WIDE_PALETTE};

/// The xterm-256 index to its sRGB triple. The first 16 are the ANSI basics,
/// then a 6x6x6 cube, then a 24-step grey ramp.
fn srgb(index: u8) -> (u8, u8, u8) {
	const BASE: [(u8, u8, u8); 16] = [
		(0, 0, 0),
		(128, 0, 0),
		(0, 128, 0),
		(128, 128, 0),
		(0, 0, 128),
		(128, 0, 128),
		(0, 128, 128),
		(192, 192, 192),
		(128, 128, 128),
		(255, 0, 0),
		(0, 255, 0),
		(255, 255, 0),
		(0, 0, 255),
		(255, 0, 255),
		(0, 255, 255),
		(255, 255, 255),
	];
	const STEP: [u8; 6] = [0, 95, 135, 175, 215, 255];
	match index {
		0..=15 => BASE[index as usize],
		16..=231 => {
			let i = index - 16;
			(
				STEP[(i / 36) as usize],
				STEP[((i / 6) % 6) as usize],
				STEP[(i % 6) as usize],
			)
		}
		_ => {
			let v = 8 + (index - 232) * 10;
			(v, v, v)
		}
	}
}

/// Relative luminance, WCAG 2.1.
fn luminance((r, g, b): (u8, u8, u8)) -> f64 {
	fn channel(c: u8) -> f64 {
		let c = f64::from(c) / 255.0;
		if c <= 0.03928 {
			c / 12.92
		} else {
			((c + 0.055) / 1.055).powf(2.4)
		}
	}
	0.2126 * channel(r) + 0.7152 * channel(g) + 0.0722 * channel(b)
}

/// WCAG contrast ratio between two colours.
fn contrast(a: (u8, u8, u8), b: (u8, u8, u8)) -> f64 {
	let (la, lb) = (luminance(a), luminance(b));
	let (hi, lo) = if la > lb { (la, lb) } else { (lb, la) };
	(hi + 0.05) / (lo + 0.05)
}

/// CIE L*a*b*, D65, for perceptual distance.
fn lab(c: (u8, u8, u8)) -> (f64, f64, f64) {
	fn linear(c: u8) -> f64 {
		let c = f64::from(c) / 255.0;
		if c <= 0.03928 {
			c / 12.92
		} else {
			((c + 0.055) / 1.055).powf(2.4)
		}
	}
	let (r, g, b) = (linear(c.0), linear(c.1), linear(c.2));
	let x = (r * 0.4124 + g * 0.3576 + b * 0.1805) / 0.95047;
	let y = r * 0.2126 + g * 0.7152 + b * 0.0722;
	let z = (r * 0.0193 + g * 0.1192 + b * 0.9505) / 1.08883;
	fn f(t: f64) -> f64 {
		if t > 0.008856 {
			t.cbrt()
		} else {
			7.787 * t + 16.0 / 116.0
		}
	}
	let (fx, fy, fz) = (f(x), f(y), f(z));
	(116.0 * fy - 16.0, 500.0 * (fx - fy), 200.0 * (fy - fz))
}

/// Euclidean distance in Lab (CIE76).
fn delta_e(a: (u8, u8, u8), b: (u8, u8, u8)) -> f64 {
	let (la, aa, ba) = lab(a);
	let (lb, ab, bb) = lab(b);
	((la - lb).powi(2) + (aa - ab).powi(2) + (ba - bb).powi(2)).sqrt()
}

const WHITE: (u8, u8, u8) = (255, 255, 255);
const BLACK: (u8, u8, u8) = (0, 0, 0);

/// Every identity colour must be readable on a light terminal and a dark one.
///
/// 3:1 is the WCAG AA bar for interface components. The stricter 4.5:1 text bar
/// is unreachable here: only six of the 256 colours clear it against both
/// backgrounds, which is why this wide palette targets the 3:1 bar instead.
#[test]
fn every_palette_colour_reads_on_both_backgrounds() {
	for &index in &WIDE_PALETTE {
		let c = srgb(index);
		let on_white = contrast(c, WHITE);
		let on_black = contrast(c, BLACK);
		assert!(
			on_white >= 3.0,
			"colour {index} scores {on_white:.2} against white, below the 3:1 bar"
		);
		assert!(
			on_black >= 3.0,
			"colour {index} scores {on_black:.2} against black, below the 3:1 bar"
		);
	}
}

/// Two services must never be given colours a reader cannot tell apart.
#[test]
fn palette_colours_are_distinguishable_from_each_other() {
	for (i, &a) in WIDE_PALETTE.iter().enumerate() {
		for &b in &WIDE_PALETTE[i + 1..] {
			let d = delta_e(srgb(a), srgb(b));
			assert!(
				d >= 22.0,
				"colours {a} and {b} are only deltaE {d:.1} apart; a reader sees one colour"
			);
		}
	}
}

/// Red, green and yellow carry status meaning everywhere in podup. An identity
/// colour close to one of them would read as a state.
#[test]
fn palette_avoids_the_semantic_colours() {
	const SEMANTIC: [(u8, u8, u8); 6] = [
		(255, 0, 0),
		(128, 0, 0),
		(0, 255, 0),
		(0, 128, 0),
		(255, 255, 0),
		(128, 128, 0),
	];
	for &index in &WIDE_PALETTE {
		for sem in SEMANTIC {
			let d = delta_e(srgb(index), sem);
			assert!(
				d > 40.0,
				"colour {index} is deltaE {d:.1} from a status colour; it would read as a state"
			);
		}
	}
}

/// `wide_colour` wraps rather than panicking past the palette size, and returns
/// the palette entry rather than the raw index.
#[test]
fn wide_colour_wraps_at_the_palette_size() {
	assert_eq!(wide_colour(0), wide_colour(WIDE_PALETTE.len()));
	assert_eq!(
		wide_colour(3),
		anstyle::Ansi256Color(WIDE_PALETTE[3]).into(),
		"the returned colour must be the palette entry, not the index"
	);
}

/// A terminal announcing truecolor can certainly render 256 colours.
#[test]
fn colorterm_truecolor_gets_the_wide_palette() {
	assert!(supports_wide_palette(Some("truecolor"), None, false));
	assert!(supports_wide_palette(Some("24bit"), None, false));
}

/// The conventional TERM marker.
#[test]
fn term_256color_gets_the_wide_palette() {
	assert!(supports_wide_palette(None, Some("xterm-256color"), false));
	assert!(supports_wide_palette(None, Some("screen-256color"), false));
}

/// Windows commonly sets neither variable while rendering 256 colours fine.
/// Without this tier every Windows user would fall back to six.
#[test]
fn windows_with_vt_enabled_gets_the_wide_palette() {
	assert!(supports_wide_palette(None, None, true));
}

/// A terminal that announces nothing gets the six ANSI basics, which render
/// everywhere. Guessing wider would paint escape codes into its output.
#[test]
fn an_unannounced_terminal_falls_back() {
	assert!(!supports_wide_palette(None, None, false));
	assert!(!supports_wide_palette(None, Some("vt100"), false));
	assert!(!supports_wide_palette(Some(""), Some("dumb"), false));
}
