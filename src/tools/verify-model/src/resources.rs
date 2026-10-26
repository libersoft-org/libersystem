// WHAT A GATE USES BESIDE ITS OWN PRIVATE FILES, declared, and checked against what its scripts call.
//
// Nothing overlapped because nothing said what it touched: two host gates run side by side broke each other's
// diagnostics, and a test runner refused six profile rows for a collision that was not one. So every gate
// `check.sh` runs declares here the shared things it reads or writes - an architecture's staging, in which
// profile; its bootable volume, in which DMA mode; the boot media; the USB gadget; a fixed host port; the persistent
// development instance; the build's step marks; the injection traces - and a scheduler overlaps two steps only where
// their declarations do not conflict.
//
// A DECLARATION NOBODY CHECKS IS A GUESS, so two halves of it are held to the gate's own scripts, in both
// directions: whether it boots a guest, and whether it writes a staging. `verify-model check` follows each
// gate's command in `check.sh` to the scripts it runs and refuses a gate with no declaration, one declared as
// a host gate whose scripts call a runner, and one declared as booting whose scripts call none.
//
// The declaration language, one gate a line:
//   guest | host                  first, always
//   stage:<arch>:<r|w>            the architecture's staging, read or written
//   profile:<shipping|development> the profile those staging uses are in
//   volume:<arch>:<mode>:<r|w>    the bootable volume, needed in or written for a DMA mode
//   media                         the boot media templates `qemu-run.sh` keeps in `.build/boot`
//   gadget                        the host's USB gadget
//   port:<n>                      a fixed host port
//   lab:<name>                    a lab name with one holder, `dev-instance`: a person's persistent development
//                                 instance - the lab's and the cold runs' other names are each run's own
//   marks:<r|w>                   the build's step marks in `.build/state`
//   traces                        the injection traces in the temporary directory

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Resource {
	Staging(String),
	Volume(String),
	Media,
	Gadget,
	Port(u16),
	Lab(String),
	Marks,
	Traces,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Use {
	pub resource: Resource,
	pub write: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Declaration {
	pub guest: bool,
	pub uses: Vec<Use>,
	// The profile its staging uses need, and the DMA mode its volume uses need.
	pub profile: Option<String>,
	pub volume_mode: Option<String>,
}

impl Declaration {
	pub fn writes_staging(&self) -> bool {
		self.uses.iter().any(|item| item.write && matches!(item.resource, Resource::Staging(_)))
	}

	// Whether two steps may NOT run at once: a resource both use, written by either, keeps them apart, and so does
	// one staging needed in two profiles or one volume in two modes - it is in one state at a time. The gadget, a
	// port and a lab name have one holder at a time, so any two uses of one of them conflict.
	pub fn conflicts_with(&self, other: &Declaration) -> bool {
		self.uses.iter().any(|mine| other.uses.iter().any(|theirs| mine.resource == theirs.resource && (mine.write || theirs.write || matches!(mine.resource, Resource::Gadget | Resource::Port(_) | Resource::Lab(_)) || (matches!(mine.resource, Resource::Staging(_)) && self.profile != other.profile) || (matches!(mine.resource, Resource::Volume(_)) && self.volume_mode != other.volume_mode))))
	}

	// THE DECLARATION AS THE RUNNER READS IT, one token a resource: `RESOURCE=ACCESS:STATE`. ACCESS is `r`, `w`, or `x`
	// for a resource with one holder at a time; STATE is the profile a staging is needed in, `PROFILE/MODE` for a
	// bootable volume, and `-` for the rest. Two tokens on one resource conflict when either is `w` or `x` or their
	// states differ - `conflicts_with` in a form the shell compares - and a read token's state is the one the runner
	// brings that resource to before the step starts.
	pub fn tokens(&self) -> Vec<String> {
		let profile = self.profile.as_deref().unwrap_or("-");
		self.uses
			.iter()
			.map(|item| {
				let (name, state) = match &item.resource {
					Resource::Staging(arch) => (format!("stage:{arch}"), profile.to_string()),
					Resource::Volume(arch) => (format!("volume:{arch}"), format!("{profile}/{}", self.volume_mode.as_deref().unwrap_or("-"))),
					Resource::Media => (String::from("media"), String::from("-")),
					Resource::Gadget => (String::from("gadget"), String::from("-")),
					Resource::Port(port) => (format!("port:{port}"), String::from("-")),
					Resource::Lab(name) => (format!("lab:{name}"), String::from("-")),
					Resource::Marks => (String::from("marks"), String::from("-")),
					Resource::Traces => (String::from("traces"), String::from("-")),
				};
				let access = if matches!(item.resource, Resource::Gadget | Resource::Port(_) | Resource::Lab(_)) {
					"x"
				} else if item.write {
					"w"
				} else {
					"r"
				};
				format!("{name}={access}:{state}")
			})
			.collect()
	}

	// TWO DECLARATIONS AS ONE, for a step that runs several gates: everything either uses, in the one profile and the
	// one volume mode they agree on - `None` when they need two, which no one step can be given.
	pub fn union(&self, other: &Declaration) -> Option<Declaration> {
		let agree = |left: &Option<String>, right: &Option<String>| match (left, right) {
			(Some(one), Some(two)) if one != two => None,
			_ => Some(left.clone().or_else(|| right.clone())),
		};
		Some(Declaration { guest: self.guest || other.guest, uses: self.uses.iter().chain(&other.uses).cloned().collect(), profile: agree(&self.profile, &other.profile)?, volume_mode: agree(&self.volume_mode, &other.volume_mode)? })
	}

	// WHERE IN A RUN THIS STEP BELONGS, by the state it needs: the steps that need one state of a staging run
	// together, the shipping profile's first - every build of the run leaves the staging there - then the
	// development profile's, and within each the bootable volume's modes, `enforcing-required` first. A step that
	// needs no staging is at the start.
	pub fn state_rank(&self) -> usize {
		if !self.uses.iter().any(|item| matches!(item.resource, Resource::Staging(_) | Resource::Volume(_))) {
			return 0;
		}
		let profile = usize::from(self.profile.as_deref() == Some("development"));
		let mode = match self.volume_mode.as_deref() {
			None | Some("enforcing-required") => 0,
			Some("harness") => 1,
			Some(_) => 2,
		};
		profile * 3 + mode
	}
}

const ARCHITECTURES: [&str; 3] = ["x86_64", "aarch64", "riscv64"];
const MODES: [&str; 3] = ["harness", "enforcing-required", "no-iommu"];

fn access(text: &str, token: &str) -> Result<bool, String> {
	match text {
		"r" => Ok(false),
		"w" => Ok(true),
		_ => Err(format!("`{token}` names neither r nor w")),
	}
}

fn architecture(text: &str, token: &str) -> Result<String, String> {
	if ARCHITECTURES.contains(&text) { Ok(text.to_string()) } else { Err(format!("`{token}` names no architecture")) }
}

pub fn parse(text: &str) -> Result<Declaration, String> {
	let mut tokens = text.split_whitespace();
	let guest = match tokens.next() {
		Some("guest") => true,
		Some("host") => false,
		_ => return Err(String::from("a declaration starts with `guest` or `host`")),
	};
	let mut declaration = Declaration { guest, ..Declaration::default() };
	for token in tokens {
		let parts: Vec<&str> = token.split(':').collect();
		let item = match parts.as_slice() {
			["stage", arch, rw] => Use { resource: Resource::Staging(architecture(arch, token)?), write: access(rw, token)? },
			["volume", arch, mode, rw] if MODES.contains(mode) => {
				if declaration.volume_mode.replace(mode.to_string()).is_some_and(|before| before != *mode) {
					return Err(String::from("a declaration needs one DMA mode"));
				}
				Use { resource: Resource::Volume(architecture(arch, token)?), write: access(rw, token)? }
			}
			["profile", profile @ ("shipping" | "development")] => {
				declaration.profile = Some(profile.to_string());
				continue;
			}
			["media"] => Use { resource: Resource::Media, write: false },
			["gadget"] => Use { resource: Resource::Gadget, write: true },
			["port", number] => Use { resource: Resource::Port(number.parse().map_err(|_| format!("`{token}` names no port"))?), write: true },
			["lab", name] if !name.is_empty() => Use { resource: Resource::Lab(name.to_string()), write: true },
			["marks", rw] => Use { resource: Resource::Marks, write: access(rw, token)? },
			["traces"] => Use { resource: Resource::Traces, write: true },
			_ => return Err(format!("`{token}` is not a resource")),
		};
		declaration.uses.push(item);
	}
	let stages = declaration.uses.iter().any(|item| matches!(item.resource, Resource::Staging(_) | Resource::Volume(_)));
	if stages != declaration.profile.is_some() {
		return Err(String::from("a staging or volume use needs a profile, and a profile needs one of them"));
	}
	Ok(declaration)
}

pub const GATE_RESOURCES: [(&str, &str); 198] = [
	("acpi", "guest stage:x86_64:r profile:development media"),
	("aml-emitter", "host"),
	("arch-profile-aarch64-gicv2-1", "guest stage:aarch64:r profile:shipping media"),
	("arch-profile-aarch64-gicv2-4", "guest stage:aarch64:r profile:shipping media"),
	("arch-profile-aarch64-gicv3-1", "guest stage:aarch64:r profile:shipping media"),
	("arch-profile-aarch64-gicv3-4", "guest stage:aarch64:r profile:shipping media"),
	("arch-profile-aarch64-gicv3-its-1", "guest stage:aarch64:r profile:shipping media"),
	("arch-profile-aarch64-gicv3-its-4", "guest stage:aarch64:r profile:shipping media"),
	("arch-profile-aarch64-gicv3-its-device-4", "guest stage:aarch64:r profile:shipping media"),
	("arch-profile-aarch64-no-dt-1", "guest stage:aarch64:r profile:shipping media"),
	("arch-profile-aarch64-no-dt-absent-1", "guest stage:aarch64:r profile:shipping media"),
	("arch-profile-aarch64-uefi-1", "guest stage:aarch64:r profile:shipping media"),
	("arch-profile-riscv64-aia-1", "guest stage:riscv64:r profile:shipping media"),
	("arch-profile-riscv64-aia-4", "guest stage:riscv64:r profile:shipping media"),
	("arch-profile-riscv64-no-dt-1", "guest stage:riscv64:r profile:shipping media"),
	("arch-profile-riscv64-no-dt-absent-1", "guest stage:riscv64:r profile:shipping media"),
	("arch-profile-riscv64-uefi-1", "guest stage:riscv64:r profile:shipping media"),
	("arch-surface", "host"),
	("artifact-metadata", "host stage:x86_64:r profile:shipping"),
	("audio-routing", "guest stage:x86_64:r profile:development media"),
	("balloon", "guest stage:x86_64:r profile:development media"),
	("bluetooth-audio", "guest stage:x86_64:r profile:development media volume:x86_64:harness:r"),
	("bluetooth-classic", "guest stage:x86_64:r profile:development media volume:x86_64:harness:r"),
	("bluetooth-codecs", "host"),
	("bluetooth-independent", "guest stage:x86_64:w profile:development media"),
	("bluetooth-input", "guest stage:x86_64:r profile:development media volume:x86_64:harness:r"),
	("bluetooth-le", "guest stage:x86_64:r profile:development media volume:x86_64:harness:r"),
	("bluetooth-le-audio", "guest stage:x86_64:r profile:development media volume:x86_64:harness:r"),
	("bluetooth-service", "guest stage:x86_64:r profile:development media volume:x86_64:harness:r"),
	("bluetooth-transfer", "guest stage:x86_64:r profile:development media volume:x86_64:harness:r"),
	("boot-harness", "host"),
	("boot-manifest", "host stage:x86_64:r profile:shipping"),
	("boot-volume-controllers", "guest stage:x86_64:r profile:development media volume:x86_64:harness:r"),
	("boot-volume-controllers-aarch64", "guest stage:aarch64:r profile:development media"),
	("boot-volume-controllers-riscv64", "guest stage:riscv64:r profile:development media"),
	("bootstrap-plan", "host"),
	("brightness-acpi", "guest stage:x86_64:r profile:development media gadget"),
	("brightness-acpi-no-dos", "guest stage:x86_64:r profile:development media gadget"),
	("brightness-usb", "guest stage:x86_64:r profile:development media volume:x86_64:harness:r gadget"),
	("build-order", "host stage:x86_64:w profile:shipping"),
	("camera-usb", "guest stage:x86_64:r profile:development media volume:x86_64:harness:r"),
	("camera-usb-aarch64", "guest stage:aarch64:r profile:development media"),
	("camera-usb-riscv64", "guest stage:riscv64:r profile:development media"),
	("capability-model", "host"),
	("capability-trace", "host"),
	("component-oracles", "host stage:x86_64:r profile:shipping"),
	("concurrent-selection", "guest stage:x86_64:r profile:shipping media"),
	("declared-interfaces", "host"),
	("dependency-policy", "host"),
	("development-build", "host"),
	("development-gate", "host stage:x86_64:r profile:shipping"),
	("development-lifecycle", "guest stage:x86_64:r profile:development media"),
	("dma-mode-aarch64", "guest stage:aarch64:r profile:shipping media"),
	("dma-mode-carrier", "host"),
	("dma-mode-ports", "guest stage:aarch64:r stage:riscv64:r profile:shipping media"),
	("dma-mode-riscv64", "guest stage:riscv64:r profile:shipping media"),
	("dma-mode-x86_64", "guest stage:x86_64:w profile:shipping media volume:x86_64:enforcing-required:w"),
	("driver-event-dispatch", "host"),
	("driver-mutations", "host"),
	("driver-protocol-note", "host stage:x86_64:r profile:shipping"),
	("duplicate-edge", "host stage:x86_64:r profile:shipping traces"),
	("dynamic-report", "host stage:x86_64:r profile:shipping"),
	("dynamic-report-regressions", "host stage:x86_64:r profile:shipping"),
	("firmware-fixtures", "host"),
	("firmware-tree-aarch64", "guest stage:aarch64:r profile:development media"),
	("firmware-tree-riscv64", "guest stage:riscv64:r profile:development media"),
	("font-declarations", "host stage:x86_64:r profile:shipping"),
	("foreign-audit-artifact", "host stage:x86_64:r profile:shipping"),
	("foreign-audit-link", "host stage:x86_64:r profile:shipping"),
	("foreign-cxx-abi", "host stage:x86_64:r profile:shipping"),
	("foreign-facilities", "host"),
	("foreign-facilities-guest", "guest stage:x86_64:r profile:development media volume:x86_64:harness:r"),
	("foreign-identity", "host stage:x86_64:r profile:shipping"),
	("foreign-loader-guest", "guest stage:x86_64:r profile:development media volume:x86_64:harness:r"),
	("foreign-pin", "host stage:x86_64:r profile:shipping"),
	("forwarded-abi", "host"),
	("frame-retirement", "host"),
	("gate-oracles", "host"),
	("gate-result-logs", "host"),
	("grant-vocabulary", "host"),
	("graphics-profile", "host"),
	("guest-verdict", "host"),
	("hibernate", "guest stage:x86_64:w profile:development media"),
	("hibernate-ports", "guest stage:aarch64:r stage:riscv64:r profile:development media"),
	("hibernate-ports-aarch64", "guest stage:aarch64:r profile:development media"),
	("hibernate-ports-riscv64", "guest stage:riscv64:r profile:development media"),
	("host-share", "guest stage:x86_64:r profile:development media"),
	("host-tests", "host"),
	("i2c-backend", "host"),
	("i2c-bus", "guest stage:x86_64:r profile:shipping media"),
	("i2c-hid", "guest stage:x86_64:r profile:development media volume:x86_64:harness:r"),
	("icd-selection", "guest stage:x86_64:r profile:development media volume:x86_64:harness:r"),
	("identity-note", "host stage:x86_64:r profile:shipping traces"),
	("implementation-mutations", "guest stage:x86_64:r profile:shipping media"),
	("iommu-aarch64-direct-gicv2", "guest stage:aarch64:r profile:shipping media"),
	("iommu-aarch64-direct-gicv2-hostile", "guest stage:aarch64:r profile:shipping media"),
	("iommu-aarch64-direct-gicv2-ordinary", "guest stage:aarch64:r profile:shipping media"),
	("iommu-aarch64-direct-gicv3-its", "guest stage:aarch64:r profile:shipping media"),
	("iommu-aarch64-direct-gicv3-its-ordinary", "guest stage:aarch64:r profile:shipping media"),
	("iommu-aarch64-direct-gicv3-its-transition", "guest stage:aarch64:r profile:shipping media"),
	("iommu-aarch64-uefi-gicv2", "guest stage:aarch64:r profile:shipping media"),
	("iommu-aarch64-uefi-gicv2-display", "guest stage:aarch64:r profile:shipping media"),
	("iommu-aarch64-uefi-gicv2-ordinary", "guest stage:aarch64:r profile:shipping media"),
	("iommu-aarch64-uefi-gicv2-transition", "guest stage:aarch64:r profile:shipping media"),
	("iommu-ports", "guest stage:aarch64:r stage:riscv64:r profile:shipping media"),
	("iommu-riscv64-direct-aia", "guest stage:riscv64:r profile:shipping media"),
	("iommu-riscv64-direct-aia-hostile", "guest stage:riscv64:r profile:shipping media"),
	("iommu-riscv64-direct-aia-ordinary", "guest stage:riscv64:r profile:shipping media"),
	("iommu-riscv64-uefi-aia", "guest stage:riscv64:r profile:shipping media"),
	("iommu-riscv64-uefi-aia-display", "guest stage:riscv64:r profile:shipping media"),
	("iommu-riscv64-uefi-aia-ordinary", "guest stage:riscv64:r profile:shipping media"),
	("iommu-riscv64-uefi-aia-transition", "guest stage:riscv64:r profile:shipping media"),
	("ipmi", "guest stage:x86_64:r profile:development media"),
	("ipmi-harness", "host"),
	("ipv6-peer", "guest stage:x86_64:w profile:shipping media volume:x86_64:enforcing-required:w"),
	("kernel-allocations", "host"),
	("lifecycle", "guest stage:x86_64:r profile:development media volume:x86_64:harness:r"),
	("malformed-dynamic", "host stage:x86_64:r profile:shipping traces"),
	("malformed-symbol-relocation", "host stage:x86_64:r profile:shipping traces"),
	("media-import-service", "guest stage:x86_64:r profile:shipping media"),
	("memory-hotplug", "guest stage:x86_64:r profile:development media"),
	("milestone-index", "host"),
	("model-mutations", "host"),
	("no-fixed-provider-slots", "host"),
	("no-suppression", "host"),
	("numa-profile-aarch64", "guest stage:aarch64:r profile:shipping media"),
	("numa-profile-riscv64", "guest stage:riscv64:r profile:shipping media"),
	("numa-profile-x86_64", "guest stage:x86_64:r profile:shipping media"),
	("one-wait", "host"),
	("opentype-profile", "host"),
	("perf-anchor", "guest stage:x86_64:r profile:shipping media volume:x86_64:enforcing-required:r"),
	("power-service", "guest stage:x86_64:r profile:development media volume:x86_64:harness:r"),
	("power-ups", "guest stage:x86_64:r profile:development media volume:x86_64:harness:r gadget"),
	("processor-power", "guest stage:x86_64:r profile:development media"),
	("profile-sysroot", "host stage:x86_64:r profile:shipping"),
	("provider-media-order", "guest stage:x86_64:r profile:shipping media"),
	("provider-routing", "host"),
	("qemu-2d-account", "guest stage:x86_64:r profile:development media volume:x86_64:enforcing-required:r"),
	("qemu-2d-demo", "guest stage:x86_64:r profile:development media"),
	("qemu-3d-demo", "guest stage:x86_64:w profile:development media"),
	("qemu-3d-extended", "guest stage:x86_64:w profile:development media"),
	("qemu-admin-path", "guest stage:x86_64:r profile:development media volume:x86_64:enforcing-required:r"),
	("qemu-arch-profiles", "guest stage:x86_64:r stage:aarch64:r stage:riscv64:r profile:shipping media"),
	("qemu-camera-service", "guest stage:x86_64:r profile:development media volume:x86_64:harness:r"),
	("qemu-dfu-tool", "guest stage:x86_64:r profile:development media volume:x86_64:enforcing-required:r"),
	("qemu-gamepad-tool", "guest stage:x86_64:r profile:development media volume:x86_64:harness:r"),
	("qemu-ipmi-admin", "guest stage:x86_64:r profile:development media volume:x86_64:enforcing-required:r"),
	("qemu-midi-service", "guest stage:x86_64:r profile:development media volume:x86_64:harness:r"),
	("qemu-modem-service", "guest stage:x86_64:r profile:development media volume:x86_64:harness:r"),
	("qemu-numa", "guest stage:x86_64:r stage:aarch64:r stage:riscv64:r profile:shipping media"),
	("qemu-pcie-aer", "guest stage:x86_64:r profile:development media"),
	("qemu-pcie-hotplug", "guest stage:x86_64:r profile:development media"),
	("qemu-tpm-tool", "guest stage:x86_64:r profile:development media volume:x86_64:enforcing-required:r"),
	("qemu-virtio-iommu-x86_64", "guest stage:x86_64:w profile:shipping media volume:x86_64:enforcing-required:w"),
	("rollback-floor-x86_64", "guest stage:x86_64:w profile:shipping media volume:x86_64:enforcing-required:w"),
	("run-verdict", "host stage:x86_64:w profile:shipping marks:r"),
	("secure-boot", "guest stage:x86_64:r profile:shipping media volume:x86_64:enforcing-required:r"),
	("serial-handoff", "guest stage:x86_64:r profile:development media volume:x86_64:enforcing-required:r"),
	("shell-large-output", "guest stage:x86_64:r profile:development media volume:x86_64:enforcing-required:r"),
	("signed-boot", "guest stage:x86_64:r profile:shipping media volume:x86_64:enforcing-required:r"),
	("single-cap-receive", "host"),
	("sleep", "guest stage:x86_64:r profile:development media"),
	("smartcard-service", "guest stage:x86_64:r profile:development media volume:x86_64:harness:r"),
	("smp-core-cap", "guest stage:x86_64:r profile:shipping media"),
	("soft2d-performance", "host"),
	("source-history-hygiene", "host stage:x86_64:r profile:shipping"),
	("source-hygiene", "host stage:x86_64:r profile:shipping"),
	("spool-service", "guest stage:x86_64:r profile:shipping media"),
	("staged-consistency", "host stage:x86_64:r profile:shipping"),
	("static-image", "host stage:x86_64:r profile:shipping traces"),
	("supervisor-waits", "host"),
	("system-suspend-riscv64", "guest stage:riscv64:r profile:development media"),
	("targeted-cache", "host stage:x86_64:w profile:shipping"),
	("test-tags", "host"),
	("text-corpus", "host"),
	("text-limits", "host"),
	("tickless-idle", "guest stage:x86_64:r stage:aarch64:r stage:riscv64:r profile:development media volume:x86_64:enforcing-required:r"),
	("trust-profile", "host"),
	("typec-tcpci", "guest stage:x86_64:r profile:development media"),
	("typec-tcpci-aarch64", "guest stage:aarch64:r profile:development media"),
	("typec-tcpci-riscv64", "guest stage:riscv64:r profile:development media"),
	("typec-ucsi", "guest stage:x86_64:r profile:development media"),
	("ucsi-ppm", "host"),
	("undeclared-edge", "host stage:x86_64:r profile:shipping traces"),
	("unicode-segmentation", "host"),
	("usb-legacy", "guest stage:x86_64:r profile:development media volume:x86_64:harness:r"),
	("usb-release", "guest stage:x86_64:r profile:development media volume:x86_64:harness:r"),
	("verify-evidence", "guest stage:x86_64:r profile:shipping media"),
	("verify-model", "host"),
	("verify-model-tests", "host"),
	("verify-scheduler", "host"),
	("virtio-gpu-edid", "guest stage:x86_64:r profile:development media volume:x86_64:harness:r"),
	("virtio-gpu-edid-aarch64", "guest stage:aarch64:r profile:development media"),
	("virtio-gpu-edid-riscv64", "guest stage:riscv64:r profile:development media"),
	("virtio-iommu-protocol", "host"),
	("virtio-multiport", "guest stage:x86_64:r profile:shipping media"),
	("volume-layout", "host stage:x86_64:r profile:shipping"),
	("watchdog", "guest stage:x86_64:r profile:development media volume:x86_64:enforcing-required:r"),
];

// WHAT A STEP OF A PLAN USES, from what the step is: a gate's own declaration, or the union of its gates'; a build
// writes its architecture's staging in the shipping profile; a kernel suite reads it and boots the test media; an image
// producer writes the staging and the bootable volume it signs; a host suite and a conformance suite share nothing; the
// development lifecycle is a gate; and a development check uses the persistent instance. `None` for a step a gate of
// which declares nothing - `verify-model check` refuses that - and the runner keeps such a step apart from everything.
pub fn step_declaration(step: &crate::commands::Step) -> Option<Declaration> {
	let (kind, rest) = step.id.split_once(':')?;
	let arch = rest.split(':').next()?;
	match kind {
		"build" => parse(&format!("host stage:{arch}:w profile:shipping")).ok(),
		"host" | "conformance" => parse("host").ok(),
		"guest" => parse(&format!("guest stage:{arch}:r profile:shipping media")).ok(),
		"producer" => {
			let mode = step.command.split_whitespace().skip_while(|word| *word != "--dma-mode").nth(1)?;
			parse(&format!("host stage:{arch}:w profile:shipping volume:{arch}:{mode}:w media")).ok()
		}
		"dev" if rest == "lifecycle" => declaration(crate::catalog::DEV_LIFECYCLE_GATE),
		"dev" => parse("guest lab:dev-instance").ok(),
		// The gates it runs are in its command, which a tier keeps when it strips the keys it already answered.
		"gate" | "gate-guest" | "gate-profile" | "gate-concurrent" | "gate-after-guest" => {
			let gates = step.command.strip_prefix("./check.sh --gate ")?;
			let mut merged: Option<Declaration> = None;
			for gate in gates.split(',') {
				let one = declaration(gate)?;
				merged = Some(match merged {
					None => one,
					Some(before) => before.union(&one)?,
				});
			}
			merged
		}
		_ => None,
	}
}

// THE GATES WHOSE SCRIPTS NAME A RUNNER OR A BUILD THEY DO NOT RUN, and why: each is held to its declaration
// on the other half only. `(gate, "guest" | "staging" | "both", reason)`.
pub const NAMES_WHAT_IT_DOES_NOT_RUN: [(&str, &str, &str); 7] = [
	("boot-harness", "both", "host tests of lab.py and the scenario runner, against fakes and copies in temporary trees"),
	("guest-verdict", "both", "host tests of the guest runners, with stand-in QEMU, runners and builds in temporary trees"),
	("run-verdict", "guest", "copies of the entry scripts in private trees, with stub runners; its one real build is declared"),
	("test-tags", "guest", "asks test.sh for the tags it accepts, which boots nothing"),
	("dma-mode-carrier", "both", "reads the runners' sources for the run-mode carrier, and runs none of them"),
	("gate-result-logs", "both", "reads the gate scripts' sources for how they find their logs, and runs none of them"),
	("verify-scheduler", "staging", "drives the runner over prepared plans in a private tree, with a `build.sh` stand-in it writes there"),
];

// WHAT A GATE'S SCRIPTS WERE SEEN TO CALL: a runner, which boots a guest, and a build, which writes a staging.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Reach {
	pub guest: bool,
	pub writes_staging: bool,
	pub scripts: BTreeSet<String>,
}

// What boots a guest, and what writes a staging.
//
// A RUNNER IS NOT A BUILD, though `guest_gate_run`, the development instance and the lab each assemble the image they
// boot: under the runner the staging is brought to the state a step declares before the step starts, so the build
// inside a runner finds it current and changes nothing. A gate that calls `build.sh` or `image.sh` itself does change
// it - a no-IOMMU volume, a mutated build, a private image - and that is the staging write it declares.
const RUNNERS: [&str; 8] = ["run.sh", "qemu-run.sh", "test.sh", "test-kernel.sh", "guest-verdict.py", "lab.sh", "lab.py", "dev.sh"];
const BUILDS: [&str; 2] = ["build.sh", "image.sh"];
// Libraries and dispatchers are not followed: a library is judged by the functions a gate calls from it, and a
// dispatcher would lead to every gate.
const NOT_FOLLOWED: [&str; 9] = ["lib.sh", "guest-gate.sh", "evidence.sh", "result-logs.sh", "staging-state.sh", "volume-pairing.sh", "check.sh", "verify.sh", "gen.sh"];
const MESSAGES: [&str; 10] = ["echo", "printf", "note", "die", "fail", "complain", "guest_gate_fail", "info", "warn", "say"];

fn script_name_end(byte: u8) -> bool {
	!(byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
}

// Every `NAME.sh` or `NAME.py` in `text`, with the bytes before and after it.
fn script_names(text: &str) -> Vec<(Option<u8>, String, Option<u8>)> {
	let bytes = text.as_bytes();
	let mut found = Vec::new();
	for extension in [".sh", ".py"] {
		let mut from = 0;
		while let Some(at) = text[from..].find(extension) {
			let end = from + at + extension.len();
			from = end;
			if end < bytes.len() && !script_name_end(bytes[end]) {
				continue;
			}
			let mut start = end - extension.len();
			while start > 0 && !script_name_end(bytes[start - 1]) {
				start -= 1;
			}
			if start + extension.len() == end {
				continue;
			}
			found.push(((start > 0).then(|| bytes[start - 1]), text[start..end].to_string(), bytes.get(end).copied()));
		}
	}
	found
}

// A message call and its quoted text, taken out of a line: a message names a command for a person to run, and
// running it is not what the line does.
fn without_messages(line: &str) -> String {
	let mut out = String::new();
	let mut rest = line;
	'scan: while !rest.is_empty() {
		for word in MESSAGES {
			if let Some(after) = rest.strip_prefix(word)
				&& (out.is_empty() || out.ends_with(|c: char| !c.is_alphanumeric() && c != '_' && c != '-'))
				&& after.starts_with(' ')
			{
				let mut quoted = after.trim_start();
				while let Some(flag) = quoted.strip_prefix('-') {
					quoted = flag.split_once(' ').map_or("", |(_, tail)| tail.trim_start());
				}
				if let Some(quote) = quoted.chars().next().filter(|c| *c == '"' || *c == '\'') {
					let body = &quoted[1..];
					let mut escaped = false;
					for (at, c) in body.char_indices() {
						if quote == '"' && c == '\\' && !escaped {
							escaped = true;
							continue;
						}
						if c == quote && !escaped {
							rest = &body[at + 1..];
							continue 'scan;
						}
						escaped = false;
					}
					return out;
				}
			}
		}
		let next = rest.chars().next().map_or(1, char::len_utf8);
		out.push_str(&rest[..next]);
		rest = &rest[next..];
	}
	out
}

// The lines of a script that run something: no comments, no heredoc bodies, no messages.
fn code_lines(text: &str, shell: bool) -> Vec<String> {
	let mut out = Vec::new();
	let mut heredoc: Option<String> = None;
	for line in text.lines() {
		if let Some(end) = &heredoc {
			if line.trim() == end {
				heredoc = None;
			}
			continue;
		}
		let trimmed = line.trim();
		if trimmed.is_empty() || trimmed.starts_with('#') {
			continue;
		}
		// A line that IS a message names commands for a person and runs none of them.
		if MESSAGES.iter().any(|word| trimmed.strip_prefix(word).is_some_and(|after| after.is_empty() || after.starts_with([' ', '\t']))) {
			continue;
		}
		if shell && let Some(word) = heredoc_word(line) {
			heredoc = Some(word);
		}
		out.push(without_messages(line));
	}
	out
}

// The word a heredoc opened on this line ends with: `<<EOF`, `<<-'EOF'`, `<< "EOF"` - never `<<<`, and never an
// arithmetic shift like `$((1 << 30))`.
fn heredoc_word(line: &str) -> Option<String> {
	let mut from = 0;
	while let Some(at) = line[from..].find("<<") {
		let start = from + at;
		from = start + 2;
		if line[..start].ends_with('<') || line[start + 2..].starts_with('<') {
			continue;
		}
		let rest = line[start + 2..].trim_start_matches('-').trim_start().trim_start_matches(['\'', '"']);
		let word: String = rest.chars().take_while(|c| c.is_ascii_alphanumeric() || *c == '_').collect();
		if word.chars().next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_') {
			return Some(word);
		}
	}
	None
}

// What one script calls: in shell, every script named with a path in front of it, and `guest_gate_run`; in
// Python, every script named in a string, and the lab when it is imported.
fn calls(text: &str, python: bool) -> BTreeSet<String> {
	let mut found = BTreeSet::new();
	for line in code_lines(text, !python) {
		for (before, name, after) in script_names(&line) {
			// A path in front of the name and a word boundary behind it, in shell; a string around it, in Python.
			let named = if python { matches!(before, Some(b'"' | b'\'' | b'/')) && matches!(after, Some(b'"' | b'\'')) } else { before == Some(b'/') && matches!(after, None | Some(b'"' | b'\'' | b' ' | b'\t' | b';' | b')' | b'|' | b'&' | b'`')) };
			if named {
				found.insert(name);
			}
		}
		let trimmed = line.trim_start();
		if python && (trimmed.starts_with("import lab") || trimmed.starts_with("from lab import")) {
			found.insert(String::from("lab.py"));
		}
		if !python && line.split(|c: char| !c.is_alphanumeric() && c != '_').any(|word| word == "guest_gate_run") {
			found.insert(String::from("guest_gate_run"));
		}
	}
	found
}

fn script_path(repo_root: &Path, name: &str) -> Option<std::path::PathBuf> {
	["", "src/tools", "src/harness", "src"].iter().map(|directory| repo_root.join(directory).join(name)).find(|path| path.is_file())
}

// Follow a gate's command to every script it runs, and say what they were seen to call.
pub fn reach(repo_root: &Path, command: &str) -> Reach {
	let mut out = Reach::default();
	let mut pending: Vec<String> = script_names(command).into_iter().map(|(_, name, _)| name).collect();
	while let Some(name) = pending.pop() {
		if !out.scripts.insert(name.clone()) {
			continue;
		}
		out.guest |= RUNNERS.contains(&name.as_str()) || name == "guest_gate_run";
		out.writes_staging |= BUILDS.contains(&name.as_str());
		if RUNNERS.contains(&name.as_str()) || BUILDS.contains(&name.as_str()) || NOT_FOLLOWED.contains(&name.as_str()) || name == "guest_gate_run" {
			continue;
		}
		let Some(path) = script_path(repo_root, &name) else { continue };
		let Ok(text) = std::fs::read_to_string(&path) else { continue };
		pending.extend(calls(&text, name.ends_with(".py")).into_iter().filter(|called| !out.scripts.contains(called)));
	}
	out
}

// Every gate `check.sh` runs, with its command.
pub fn gate_commands_in_check_sh(repo_root: &Path) -> Result<BTreeMap<String, String>, String> {
	let path = repo_root.join("check.sh");
	let text = std::fs::read_to_string(&path).map_err(|error| format!("{}: {error}", path.display()))?;
	let start = text.find("declare -A GATES=(").ok_or_else(|| format!("{}: no GATES array", path.display()))?;
	let end = text[start..].find("\n)").ok_or_else(|| format!("{}: unterminated GATES array", path.display()))? + start;
	let mut gates = BTreeMap::new();
	for line in text[start..end].lines().skip(1) {
		let Some(rest) = line.trim().strip_prefix("[\"") else { continue };
		let Some((name, command)) = rest.split_once("\"]=\"") else { continue };
		gates.insert(name.to_string(), command.trim_end_matches('"').to_string());
	}
	if gates.is_empty() {
		return Err(format!("{}: parsed no gates, so the check would pass vacuously", path.display()));
	}
	Ok(gates)
}

pub fn declaration(gate: &str) -> Option<Declaration> {
	GATE_RESOURCES.iter().find(|(name, _)| *name == gate).and_then(|(_, text)| parse(text).ok())
}

// THE CHECK: every gate declared, every declaration well-formed, its booting and its staging writes what its
// scripts call - and its booting what the catalog schedules it as.
pub fn declaration_failures(repo_root: &Path, table: &[(&str, &str)], exemptions: &[(&str, &str, &str)], catalogued_guest: &dyn Fn(&str) -> bool) -> Vec<String> {
	let mut failures = Vec::new();
	let gates = match gate_commands_in_check_sh(repo_root) {
		Ok(gates) => gates,
		Err(error) => return vec![error],
	};
	for (name, _) in table {
		if !gates.contains_key(*name) {
			failures.push(format!("the resource table declares gate '{name}', which check.sh does not run"));
		}
	}
	for (gate, command) in &gates {
		let Some((_, text)) = table.iter().find(|(name, _)| name == gate) else {
			failures.push(format!("gate '{gate}' declares no resources - say what it reads and writes, or that it is a host gate that shares nothing"));
			continue;
		};
		let declared = match parse(text) {
			Ok(declared) => declared,
			Err(error) => {
				failures.push(format!("gate '{gate}': {error}"));
				continue;
			}
		};
		let seen = reach(repo_root, command);
		let exempt = |half: &str| exemptions.iter().any(|(name, what, _)| name == gate && (*what == half || *what == "both"));
		if seen.guest != declared.guest && !exempt("guest") {
			let called: Vec<&str> = seen.scripts.iter().map(String::as_str).filter(|name| RUNNERS.contains(name) || *name == "guest_gate_run").collect();
			if declared.guest {
				failures.push(format!("gate '{gate}' is declared to boot a guest, and none of its scripts calls a runner"));
			} else {
				failures.push(format!("gate '{gate}' is declared as a host gate, and its scripts call {}", called.join(", ")));
			}
		}
		if seen.writes_staging != declared.writes_staging() && !exempt("staging") {
			if declared.writes_staging() {
				failures.push(format!("gate '{gate}' is declared to write a staging, and none of its scripts builds"));
			} else {
				failures.push(format!("gate '{gate}' is declared to write no staging, and its scripts build"));
			}
		}
		if declared.guest != catalogued_guest(gate) {
			failures.push(format!("gate '{gate}' is declared {} and catalogued as {} - the scheduler would give it {} guest slot", if declared.guest { "to boot a guest" } else { "as a host gate" }, if declared.guest { "a host gate" } else { "booting one" }, if declared.guest { "no" } else { "a" }));
		}
	}
	failures
}
