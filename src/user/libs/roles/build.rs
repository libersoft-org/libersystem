use std::collections::BTreeSet;
use std::env;
use std::fs;
use std::path::PathBuf;
use system_manifest::{Manifest, Presence, RoleKind};

fn main() {
	let crate_root = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR not set"));
	let workspace = crate_root.join("../../..");
	println!("cargo:rerun-if-changed={}", workspace.join("user/services/manifest.toml").display());
	let manifest = Manifest::load_workspace(&workspace).unwrap_or_else(|error| panic!("{error}"));

	let mut tags: BTreeSet<&str> = BTreeSet::new();
	for service in manifest.services.values() {
		for role in &service.roles {
			tags.insert(role.tag.as_str());
		}
	}

	let mut out = String::from("// @generated from services/manifest.toml by roles/build.rs - do not edit.\n\n");
	for tag in &tags {
		out.push_str(&format!("pub const {tag}: &[u8] = b\"{tag}\";\n"));
	}
	out.push_str("\n// Every managed service's roles, in the order the bootstrap sends them.\n");
	out.push_str(&format!("pub const SERVICES: [(&[u8], &[Role]); {}] = [\n", manifest.services.len()));
	for service in manifest.services.values() {
		let roles = service
			.roles
			.iter()
			.map(|role| {
				let kind = match role.kind {
					RoleKind::ServeRoot => "Kind::ServeRoot",
					RoleKind::Client => "Kind::Client",
					RoleKind::Factory => "Kind::Factory",
					RoleKind::Privilege => "Kind::Privilege",
					RoleKind::Power => "Kind::Power",
					RoleKind::Package => "Kind::Package",
					RoleKind::Device => "Kind::Device",
					RoleKind::Payload => "Kind::Payload",
				};
				format!("Role {{ tag: {}, kind: {kind}, provider: b\"{}\", required: {} }}", role.tag, role.provider, role.presence == Presence::Required)
			})
			.collect::<Vec<_>>()
			.join(", ");
		out.push_str(&format!("\t(b\"{}\", &[{roles}]),\n", service.name));
	}
	out.push_str("];\n");

	let dir = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR not set"));
	fs::write(dir.join("roles.rs"), out).expect("write generated roles");
}
