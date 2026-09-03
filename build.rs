//! Links the aarch64 musl target as a static PIE.
//!
//! rustc links `x86_64-unknown-linux-musl` as a static PIE by itself. For
//! `aarch64-unknown-linux-musl` its target description lacks that flag (as of
//! 1.98), so the same build there is a static executable at a fixed address:
//! `podup-linux-arm64` and the arm64 `.deb` shipped without ASLR through
//! 5.7.0. Asking the linker for `-pie` on its own is not enough, since rustc
//! still starts the binary with `crt1.o`, which does not relocate itself, and
//! the result faults before `main` (measured on the arm64 runner, podup#1645).
//!
//! So for that target `.cargo/config.toml` hands the link to `rust-lld`
//! without rustc's own CRT objects, and this script supplies the static-PIE
//! set rustc ships beside the target: `rcrt1.o`, which relocates the image
//! at start, the `crti`/`crtn` pair, the PIC `crtbegin`/`crtend`, the
//! search path for `libc.a` and `libunwind.a`, and the flags rustc itself
//! passes for a static PIE where it supports one. The release reads the
//! outcome off the binary before signing it (`check-hardening.sh`), and the
//! `linux-hardening` job runs the binary on an arm64 runner on every change
//! to the build configuration.
use std::path::Path;
use std::process::Command;

const STATIC_PIE_TARGET: &str = "aarch64-unknown-linux-musl";

fn main() {
	println!("cargo:rerun-if-changed=build.rs");
	let target = std::env::var("TARGET").expect("cargo sets TARGET");
	if target != STATIC_PIE_TARGET {
		return;
	}
	let rustc = std::env::var("RUSTC").unwrap_or_else(|_| "rustc".to_string());
	let out = Command::new(rustc)
		.args(["--print", "sysroot"])
		.output()
		.expect("rustc --print sysroot");
	let sysroot = String::from_utf8(out.stdout).expect("sysroot is UTF-8");
	let dir = format!("{}/lib/rustlib/{target}/lib/self-contained", sysroot.trim());
	for object in [
		"rcrt1.o",
		"crti.o",
		"crtbeginS.o",
		"crtendS.o",
		"crtn.o",
		"libc.a",
	] {
		assert!(
			Path::new(&dir).join(object).is_file(),
			"{dir}/{object} is missing: the {target} target is not installed for this toolchain"
		);
	}
	for arg in ["-static", "-pie", "--no-dynamic-linker", "-z", "text"] {
		println!("cargo:rustc-link-arg={arg}");
	}
	for object in ["rcrt1.o", "crti.o", "crtbeginS.o", "crtendS.o", "crtn.o"] {
		println!("cargo:rustc-link-arg={dir}/{object}");
	}
	println!("cargo:rustc-link-arg=-L{dir}");
}
