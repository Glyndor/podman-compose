use crate::quadlet::{QuadletOutput, QuadletUnit};

mod fields;
mod fields_logging;
mod fields_resources;
mod health;
mod network_volume;
mod units;

fn unit_named<'a>(out: &'a QuadletOutput, filename: &str) -> &'a QuadletUnit {
	out.units
		.iter()
		.find(|u| u.filename == filename)
		.unwrap_or_else(|| panic!("no unit named {filename}"))
}
