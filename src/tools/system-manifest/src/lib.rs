use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fs;
use std::path::{Component, Path, PathBuf};

pub const SCHEMA_VERSION: u32 = 1;
pub const MAX_PROVIDER_DEPTH: usize = 16;

// How many modules ONE PROGRAM may pull in. This is a real limit and not a sanity bound: the kernel
// gives a dynamic process sixty-four 16 MiB module slots from `DYNAMIC_MODULE_BASE`, so a closure
// larger than this is a program that cannot be loaded. Kept in step with
// `bootproto::elf::MAX_DYNAMIC_MODULES` by name rather than by dependency - this is a host tool and
// that is a bare-metal crate.
pub const MAX_PROVIDER_MODULES: usize = 64;

// How many shared libraries the SYSTEM may have, which is a different question and was answered
// with the same number for as long as the two happened to agree.
//
// Nothing loads every library at once - that is what the per-program bound above is for - so this
// is a bound on the manifest rather than on any process: a graph that grows past it is one nobody
// decided to grow, and the check is here to make that a decision. Adding the sixty-fifth library
// was a legitimate change that the shared constant reported as a program that could not be loaded.
pub const MAX_LIBRARIES: usize = 256;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct RawManifest {
	schema: u32,
	#[serde(default)]
	sources: Vec<RawSource>,
	#[serde(default)]
	programs: Vec<RawProgram>,
	#[serde(default)]
	factory_files: Vec<RawFactoryFile>,
	#[serde(default)]
	runtime_paths: Vec<RawRuntimePath>,
	#[serde(default)]
	services: Vec<RawService>,
	#[serde(default)]
	libraries: Vec<RawLibrary>,
	#[serde(default)]
	boot_artifacts: Vec<RawBootArtifact>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct RawSource {
	owner: String,
	path: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct RawProgram {
	name: String,
	owner: String,
	role: ProgramRole,
	linkage: Linkage,
	stage: Stage,
	destination: String,
	#[serde(default)]
	providers: Vec<String>,
	// A development-only program: built and staged only when the development feature is on,
	// and absent from a shipped image rather than present and refusing to work. Declaring it
	// here keeps the manifest the single place that says what the system is made of, in both
	// configurations.
	#[serde(default)]
	development: bool,
	// The binding rules, for `role = "driver"` and for nothing else.
	//
	// REQUIRED on a driver and refused on anything else, because both halves are errors that would
	// otherwise be discovered at runtime as silence: a driver with no rules is a driver DeviceManager
	// can never select, and rules on a service are rules nothing will ever consult.
	#[serde(default)]
	driver: Option<RawDriver>,
}

// The driver registry's per-entry declaration, as it is written in the manifest.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct RawDriver {
	lifecycle: DriverLifecycle,
	#[serde(rename = "match")]
	rules: Vec<RawMatchRule>,
	#[serde(default)]
	priority: MatchPriority,
	// WHAT THIS ENTRY NEEDS BEFORE IT CAN BIND, as provider kinds. A driver that needs a bus its
	// controller has not published yet is not a driver that failed - it is one that was tried too
	// early, and today it is launched anyway and fails.
	#[serde(default)]
	requires: Vec<ProviderKindName>,
	// WHICH KINDS IT MAY PUBLISH, AND HOW MANY. A bound, so a driver cannot publish a kind it never
	// declared - a compromised one advertising itself as a disk is the case this closes.
	#[serde(default)]
	provides: Vec<RawProvides>,
}

// The provider kinds a manifest may name, spelled once. A closed set for the reason the match rules
// are closed: a kind this system does not have is a manifest that fails to parse, not a requirement
// nothing ever satisfies.
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "kebab-case")]
pub enum ProviderKindName {
	Block,
	Net,
	Display,
	Audio,
	Input,
	UsbBus,
	Pointer,
	ConsoleBytes,
}

impl ProviderKindName {
	// The wire number `driver_protocol::provider` gives this kind. Written here because this crate
	// is a build-time tool and does not link the driver protocol; the generated registry carries the
	// numbers, so a disagreement would be a driver whose declaration nothing matches.
	pub fn wire(self) -> u16 {
		match self {
			ProviderKindName::Block => 1,
			ProviderKindName::Net => 2,
			ProviderKindName::Display => 3,
			ProviderKindName::Audio => 4,
			ProviderKindName::Input => 5,
			ProviderKindName::UsbBus => 6,
			ProviderKindName::Pointer => 7,
			ProviderKindName::ConsoleBytes => 8,
		}
	}
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
struct RawProvides {
	kind: ProviderKindName,
	// At most this many of that kind. `most = 0` would be a declaration that publishes nothing,
	// which is what leaving the row out already says.
	most: u16,
}

// A match rule, as a CLOSED tagged set rather than free-form key/value pairs.
//
// The point of the closure is that a rule discovery cannot answer fails when the registry is built
// instead of never matching when the machine boots. `deny_unknown_fields` plus one variant per rule
// is what enforces it: a rule naming a field this system does not discover is not a rule that binds
// nothing, it is a manifest that does not parse.
// ONE RULE IS A CONJUNCTION OF PREDICATES; a driver's `match` list is a disjunction of rules.
//
// It started as a tagged enum with one variant per question, and that was wrong in a way the tree
// showed immediately: the development console's old selection was `device_type == VIRTIO_TYPE_CONSOLE
// AND the pinned PCI address`, and an enum of single questions can only OR them. A rule where every
// declared field must hold expresses both - `{ pci-address = {...}, virtio-type = 3 }` is the AND,
// two entries in `match` are the OR - and it stays closed, because `deny_unknown_fields` means a
// field this system does not discover fails to parse rather than never matching.
//
// Every field is optional and at least one must be present; an empty rule would select everything.
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq, Default)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
struct RawMatchRule {
	// THE TRANSPORT AND THE VIRTIO TYPE, together or not at all.
	//
	// `device-type = N` used to be the whole vocabulary, and seven of the eight rows using it were
	// right by accident: for a virtio function `device_type` IS the virtio specification's own
	// device type. The eighth said `device-type = 256`, a LiberSystem constant invented for the
	// table, standing in for the PCI class triple the scan had already resolved. One name for two
	// number spaces cannot be reasoned about - and no row pinned the transport, so the rule said
	// "device type 1" where it meant "a virtio-pci function whose virtio type is 1".
	//
	// The pair is what makes it a standards identity: the virtio specification defines the number,
	// and the transport says which specification is being cited.
	#[serde(default)]
	transport: Option<RawTransport>,
	#[serde(default)]
	virtio_type: Option<u32>,
	// The standards path: how a driver claims a FAMILY of hardware rather than one vendor-defined
	// model number. `DeviceInfo` carries class, subclass and programming interface since
	// 2026-08-12; the kernel had resolved all three since the first PCI scan and kept them for
	// `lspci` alone.
	#[serde(default)]
	pci_class: Option<u8>,
	#[serde(default)]
	pci_subclass: Option<u8>,
	#[serde(default)]
	pci_interface: Option<u8>,
	// THE PART, not the kind. A vendor number says who made a device; it never says what it is, so
	// it may only NARROW a rule that already names a standard identity. That is what a quirk is: a
	// rule for a particular part, which says which part it is a quirk for.
	#[serde(default)]
	pci_vendor: Option<u16>,
	#[serde(default)]
	pci_product: Option<u16>,
	// The one predicate that names a LOCATION rather than a kind, for a second device of a type
	// that already has a driver.
	#[serde(default)]
	pci_address: Option<RawPciAddress>,
}

// THE TRANSPORTS THIS SYSTEM DISCOVERS, as a closed set rather than a string.
//
// An enum is what makes `transport = "nvme"` a manifest that does not parse instead of a rule that
// silently never matches - the same argument `deny_unknown_fields` makes for the keys, applied to a
// value. One variant today, because one is what the scan can answer.
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
enum RawTransport {
	VirtioPci,
	// A function that speaks no virtio transport, so its class triple is its whole identity. NOT
	// the absence of the key: omitting `transport` means "do not ask", and a rule that does not ask
	// can match a virtio function AND a plain one - which is what made every virtio row overlap the
	// xHCI row the first time this vocabulary was checked. Naming it is what separates them.
	PlainPci,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct RawPciAddress {
	bus: u8,
	dev: u8,
	func: u8,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct RawFactoryFile {
	name: String,
	kind: FactoryFileKind,
	#[serde(default)]
	source: Option<String>,
	// Which component BUILDS this payload, for the kinds that have no source path.
	//
	// An `sdk-component` is a compiled artifact rather than a checked-in file, so it has no
	// `source` - and dropping the question entirely meant the model had no way to learn that
	// `src/sdk` reaches the system volume. The volume's staleness digest is derived from the
	// components staged into it, so an unowned payload was a directory whose edits could not
	// invalidate a built volume: change the SDK, rebuild nothing, and test against the old
	// `app.wasm` with a digest that agreed.
	#[serde(default)]
	owner: Option<String>,
	destination: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct RawRuntimePath {
	name: String,
	owner: String,
	destination: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct RawService {
	name: String,
	program: String,
	restart: Restart,
	#[serde(default)]
	dependencies: Vec<String>,
	#[serde(default)]
	roles: Vec<RawRole>,
	state_class: StateClass,
	state_scope: StateScope,
	#[serde(default)]
	state_storage: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct RawRole {
	tag: String,
	kind: RoleKind,
	provider: String,
	#[serde(default)]
	presence: Presence,
	#[serde(default)]
	interface: String,
	#[serde(default)]
	source: String,
	#[serde(default)]
	exclusive: bool,
	#[serde(default)]
	handed_on: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct RawLibrary {
	name: String,
	owner: String,
	destination: String,
	#[serde(default)]
	features: Vec<String>,
	#[serde(default)]
	providers: Vec<String>,
}

// The pieces of the boot chain: the kernel and the UEFI loader. They are not userspace
// programs - nothing stages them onto a volume and no service supervises them - but the
// manifest is the final assembly of the whole system, so what goes into an ISO or IMG has to
// be named here rather than hard-coded in the image builder.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct RawBootArtifact {
	name: String,
	owner: String,
	kind: BootArtifactKind,
	destination: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum BootArtifactKind {
	// The kernel ELF the loader hands control to.
	Kernel,
	// The UEFI application the firmware starts, staged as BOOTX64.EFI (or its per-target name).
	Loader,
	// The init package: the pinned userspace set, handed to the kernel as a boot module rather
	// than linked into it. Named here because packaging must fail when it is missing, and
	// because the kernel binary genuinely does not contain it.
	InitPackage,
	// The volume package: everything staged onto the system volume rather than pinned into the
	// boot module - the programs this manifest marks `stage = "volume"`.
	VolumePackage,
}

#[derive(Clone, Debug, Serialize)]
pub struct BootArtifact {
	pub name: Name,
	pub owner: Name,
	pub kind: BootArtifactKind,
	pub destination: RelativePath,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum ProgramRole {
	Launcher,
	Service,
	Probe,
	Driver,
	Tool,
	Helper,
}

// What kind of thing a driver binds to, which decides how it is supervised and when it is started.
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum DriverLifecycle {
	// Needed to mount the system volume, so it is staged in `init.pkg` and started before there is
	// a volume to load anything from.
	BootCritical,
	// Owns a bus or controller: it discovers children and stays online with none.
	Controller,
	// One PCI function or standalone platform device.
	Function,
	// One mediated interface of a composite device - a USB class driver on its own interface.
	Interface,
}

// How specific a match is, as a KIND rather than a number.
//
// The milestone's arbitration rule is about kind: an exact standardised match beats a broader class
// match, and an explicit tested quirk beats the generic path only where it matches at all. A numeric
// priority invites tie-breaks nobody can justify, and the registry refuses ties anyway.
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "kebab-case")]
pub enum MatchPriority {
	// A broader class match: correct for a whole family, and the one anything else outranks.
	#[default]
	Generic,
	// An exact standardised compatible/class revision.
	Exact,
	// An explicit, tested compliance quirk for one device. Outranks both, and is the only place a
	// vendor/product pair may appear - never as the primary binding mechanism.
	Quirk,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum Linkage {
	Static,
	Dynamic,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum Stage {
	Pinned,
	Volume,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum FactoryFileKind {
	Source,
	SdkComponent,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum Restart {
	Transparent,
	Escalate,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(transparent)]
pub struct Name(String);

impl Name {
	pub fn as_str(&self) -> &str {
		&self.0
	}
}

impl fmt::Display for Name {
	fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
		formatter.write_str(&self.0)
	}
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(transparent)]
pub struct RelativePath(String);

impl RelativePath {
	pub fn as_str(&self) -> &str {
		&self.0
	}
}

#[derive(Clone, Debug, Serialize)]
pub struct Source {
	pub owner: Name,
	pub path: RelativePath,
}

#[derive(Clone, Debug, Serialize)]
pub struct Program {
	pub name: Name,
	pub owner: Name,
	pub role: ProgramRole,
	pub linkage: Linkage,
	pub stage: Stage,
	pub destination: RelativePath,
	pub providers: Vec<Name>,
	// Built and staged only in the development configuration; see RawProgram.
	pub development: bool,
	// The binding rules, present exactly on drivers. `Some` for every `role = "driver"` entry that
	// validated, `None` for every other role - which the shape check enforces both ways.
	pub driver: Option<Driver>,
}

// One driver registry entry: what it binds to, how it is supervised, and how specific its claim is.
#[derive(Clone, Debug, Serialize)]
pub struct Driver {
	pub lifecycle: DriverLifecycle,
	pub rules: Vec<MatchRule>,
	pub priority: MatchPriority,
	pub requires: Vec<ProviderKindName>,
	pub provides: Vec<Provides>,
}

#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
pub struct Provides {
	pub kind: ProviderKindName,
	pub most: u16,
}

// A match rule the generated registry can evaluate against a discovered node: every predicate that
// is present must hold. `None` is "do not ask", not "must be absent".
#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq, Default)]
pub struct MatchRule {
	// `Some(TRANSPORT_VIRTIO_PCI)` for a virtio-pci rule; `None` is "do not ask".
	pub transport: Option<u8>,
	pub virtio_type: Option<u32>,
	pub pci_class: Option<u8>,
	pub pci_subclass: Option<u8>,
	pub pci_interface: Option<u8>,
	pub pci_vendor: Option<u16>,
	pub pci_product: Option<u16>,
	pub pci_address: Option<PciAddress>,
}

// What `transport = "virtio-pci"` becomes. The same number `abi::TRANSPORT_VIRTIO_PCI` is, written
// here because this crate is a build-time tool and does not link the kernel ABI - and the two are
// kept in step by `check-declared-interfaces`, which reads both.
pub const TRANSPORT_VIRTIO_PCI: u8 = 1;
pub const TRANSPORT_PLAIN_PCI: u8 = 0;

#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
pub struct PciAddress {
	pub bus: u8,
	pub dev: u8,
	pub func: u8,
}

impl MatchRule {
	pub fn is_empty(self) -> bool {
		self == MatchRule::default()
	}

	// Whether two rules can both match one node - that is, whether a node satisfying both could
	// exist.
	//
	// Field by field: two predicates conflict only when both are present and differ. Everything
	// else leaves a node that satisfies both, so the rules overlap. Decidable because the predicate
	// set is closed, which is what lets `validate_references` refuse an ambiguous registry when it
	// is BUILT rather than discover it on a machine that happens to have the device.
	pub fn overlaps(self, other: MatchRule) -> bool {
		optional_overlaps32(self.virtio_type, other.virtio_type)
			&& optional_overlaps(self.transport, other.transport)
			&& optional_overlaps(self.pci_class, other.pci_class)
			&& optional_overlaps(self.pci_subclass, other.pci_subclass)
			&& optional_overlaps(self.pci_interface, other.pci_interface)
			&& optional_overlaps16(self.pci_vendor, other.pci_vendor)
			&& optional_overlaps16(self.pci_product, other.pci_product)
			&& match (self.pci_address, other.pci_address) {
				(Some(left), Some(right)) => left == right,
				_ => true,
			}
	}
}

fn optional_overlaps16(left: Option<u16>, right: Option<u16>) -> bool {
	match (left, right) {
		(Some(l), Some(r)) => l == r,
		_ => true,
	}
}

fn optional_overlaps32(left: Option<u32>, right: Option<u32>) -> bool {
	match (left, right) {
		(Some(l), Some(r)) => l == r,
		_ => true,
	}
}

fn optional_overlaps(left: Option<u8>, right: Option<u8>) -> bool {
	match (left, right) {
		(Some(l), Some(r)) => l == r,
		_ => true,
	}
}

#[derive(Clone, Debug, Serialize)]
pub struct Service {
	pub name: Name,
	pub program: Name,
	pub restart: Restart,
	pub dependencies: Vec<Name>,
	/// THE CAPABILITIES THIS SERVICE MUST BE HANDED BEFORE IT CAN RUN, in the order they are sent.
	///
	/// Order is part of the contract, not presentation: the bootstrap is read positionally - a
	/// receiver checks the tag of the NEXT message rather than searching for one - so a role
	/// inserted in the middle shifts every read after it. `bootstrap.rs` says exactly that beside
	/// DeviceManager's branch, having learned it the hard way.
	pub roles: Vec<Role>,
	/// WHAT SURVIVES, which is not the same question as who it belongs to.
	pub state_class: StateClass,
	/// WHO IT BELONGS TO. Kept apart from the class because one service can hold durable files and
	/// ephemeral sessions at the same time, and a single enum mixing the two cannot say so.
	pub state_scope: StateScope,
	/// Where durable state lives. Empty for every other class - a path on a service that keeps
	/// nothing is a claim nobody checks.
	pub state_storage: String,
}

/// What a restart of the process, and a reboot of the guest, do to a service's state.
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum StateClass {
	/// Gone on a process restart. Nothing is written down and nothing is rebuilt: what the service
	/// held is lost, and its clients are told so rather than handed something that looks live.
	Ephemeral,
	/// Rebuilt after a restart from a source outside the service - the manifest, the devices
	/// present, the kernel's own tables. Not preserved; DERIVED again.
	Reconstructible,
	/// Written to a medium and read back. Survives a process restart and a guest reboot, and is
	/// the only class that does.
	Durable,
}

/// Who a service's state belongs to.
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum StateScope {
	/// The service's own, for as long as it runs.
	Service,
	/// A session's: it belongs to whoever is logged in on a terminal, and ends when they do.
	Session,
	/// Not the system's at all - the medium, the device, the far end of a network connection.
	External,
}

/// One capability a service is handed at bootstrap: what it is called, what shape it is, and who
/// supplies it.
///
/// This is ONE relation and not three. A service requiring an interface, that interface having a
/// provider, and the provider arriving under a tag are the same fact seen from three sides; written
/// as three fields they can disagree, and the disagreement is silent.
#[derive(Clone, Debug, Serialize)]
pub struct Role {
	pub tag: Name,
	pub kind: RoleKind,
	/// The service that supplies it, `self` for a channel the supervisor creates for this service
	/// to serve on, or `kernel` for a capability the kernel handed the boot chain.
	pub provider: Name,
	pub presence: Presence,
	/// The LSIDL interface a channel role speaks, empty for the kinds that carry no protocol. A
	/// REFERENCE, never a copy: what the interface means is LSIDL's to say.
	pub interface: String,
	/// WHICH OF THE PROVIDER'S SERVE ROOTS THIS IS A CLIENT OF, for the kinds that are clients of
	/// something. Empty means `SERVE`, which is what nearly every one of them is.
	///
	/// It has to be said because it cannot be guessed: PermissionManager's `STORAGE_ADMIN` is a
	/// client of StorageService's `ADMIN` root and not of its `SERVE` root, and the two grant
	/// different authority over the same volume. Until now the difference lived only in which local
	/// variable a hand-written bootstrap function happened to pass.
	pub source: Name,
	/// WHETHER THE RECEIVER IS THE ONLY HOLDER, which decides whether its death is observable.
	///
	/// An ordinary client role is a DUPLICATE: the supervisor keeps the end it copied, so several
	/// services can hold one provider's channel and none of them closing it means anything. An
	/// exclusive one is HANDED OVER: the supervisor keeps nothing, so when the receiver ends, the
	/// provider's end really does see its peer close.
	///
	/// The shell's console channels are why this exists. ConsoleService reloads the shell on a VT
	/// when that VT's channel closes - a logout returning a fresh login prompt - and it can only
	/// see the close if nobody else is holding the other copy. Delivered as an ordinary duplicate,
	/// the shell exits and the console waits forever for a peer that is still open in the
	/// supervisor's handle table.
	pub exclusive: bool,
	/// WHETHER THE HOLDER PASSES THIS CHANNEL ON, and therefore whether it may duplicate it.
	///
	/// A client role arrives narrowed to send, receive, wait and transfer - the ceiling a receiver
	/// can check against, and enough for a service that talks to another service. It is NOT enough
	/// for a holder whose job is to hand the same channel to somebody else, because that needs
	/// `duplicate`, and `duplicate` needs the right by that name.
	///
	/// The shell's console is why this exists. A shell hands its terminal to the foreground job it
	/// starts - that is what a foreground job IS - and with the ceiling alone every interactive
	/// tool failed to launch: `duplicate` was refused, `run_tool_interactive` returned false, and
	/// the shell printed nothing because a launch that never happened has nothing to report. Said
	/// at the role, so the widening is a decision somebody made about one channel rather than a
	/// right handed to every client in the system.
	pub handed_on: bool,
}

/// How a role is delivered - which is also what decides whether it can be delivered AGAIN, and
/// therefore whether the service holding it can be restarted.
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum RoleKind {
	/// One end of a channel pair the supervisor made; the service serves its clients on it and the
	/// supervisor keeps the other end. Re-creatable: the supervisor simply makes another pair.
	ServeRoot,
	/// Another service's client end: a duplicate by default, or the end itself when the role is
	/// `exclusive`. A duplicate is re-creatable while that service lives AND the supervisor still
	/// holds the end it copies from; an exclusive one is not re-creatable at all, because the
	/// supervisor gave away the only end it had.
	Client,
	/// A fresh connection minted from another service's serve root. Re-creatable while it lives.
	Factory,
	/// A privileged handle the kernel gave the boot chain, duplicated on. Re-creatable while the
	/// supervisor holds its own copy.
	Privilege,
	/// The root-Domain handle. Whoever holds it can stop the machine and kill every process on it;
	/// The migration removes every one of these in favour of a narrow SystemPower service, and
	/// this kind exists so the ones that remain can be counted.
	Power,
	/// The init package as a read-only memory object.
	Package,
	/// A handle produced by a driver and passed through DeviceManager. NOT re-creatable on its
	/// own: it exists only while its driver does.
	Device,
	/// A tagged message carrying bytes and no capability at all.
	Payload,
}

/// Whether a role must arrive carrying a capability.
///
/// `Optional` is not "may be omitted": the TAG IS ALWAYS SENT, and it may carry a zero handle. A
/// boot with no second disk still sends `FATBLOCK`, empty, because the read is positional and a
/// missing message would shift every one after it.
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum Presence {
	#[default]
	Required,
	Optional,
}

#[derive(Clone, Debug, Serialize)]
pub struct Library {
	pub name: Name,
	pub owner: Name,
	pub destination: RelativePath,
	pub features: Vec<Name>,
	pub providers: Vec<Name>,
}

#[derive(Clone, Debug, Serialize)]
pub struct FactoryFile {
	pub name: Name,
	pub kind: FactoryFileKind,
	pub source: Option<RelativePath>,
	// The component that builds a payload with no source path. Required for `sdk-component`,
	// refused for `source` (whose provenance is the path itself).
	pub owner: Option<Name>,
	pub destination: RelativePath,
}

#[derive(Clone, Debug, Serialize)]
pub struct RuntimePath {
	pub name: Name,
	pub owner: Name,
	pub destination: RelativePath,
}

#[derive(Clone, Debug, Serialize)]
pub struct Manifest {
	pub schema: u32,
	pub sources: BTreeMap<Name, Source>,
	pub programs: BTreeMap<Name, Program>,
	pub factory_files: BTreeMap<Name, FactoryFile>,
	pub runtime_paths: BTreeMap<Name, RuntimePath>,
	pub services: BTreeMap<Name, Service>,
	pub libraries: BTreeMap<Name, Library>,
	pub boot_artifacts: BTreeMap<Name, BootArtifact>,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct ValidationError {
	pub location: String,
	pub message: String,
}

impl fmt::Display for ValidationError {
	fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
		write!(formatter, "manifest: {}: {}", self.location, self.message)
	}
}

#[derive(Debug)]
pub enum LoadError {
	Io { path: PathBuf, error: std::io::Error },
	Toml(toml::de::Error),
	Validation(Vec<ValidationError>),
}

impl fmt::Display for LoadError {
	fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
		match self {
			Self::Io { path, error } => write!(formatter, "cannot read {}: {error}", path.display()),
			Self::Toml(error) => write!(formatter, "manifest TOML: {error}"),
			Self::Validation(errors) => {
				for (index, error) in errors.iter().enumerate() {
					if index != 0 {
						formatter.write_str("\n")?;
					}
					write!(formatter, "{error}")?;
				}
				Ok(())
			}
		}
	}
}

impl std::error::Error for LoadError {}

impl Manifest {
	pub fn load_workspace(workspace_root: &Path) -> Result<Self, LoadError> {
		Self::load(&workspace_root.join("user/services/manifest.toml"), workspace_root)
	}

	pub fn load(path: &Path, workspace_root: &Path) -> Result<Self, LoadError> {
		let text = fs::read_to_string(path).map_err(|error| LoadError::Io { path: path.to_path_buf(), error })?;
		Self::parse(&text, workspace_root).map_err(|error| match error {
			LoadError::Toml(mut source) => {
				source.set_input(Some(&text));
				LoadError::Toml(source)
			}
			other => other,
		})
	}

	pub fn parse(text: &str, workspace_root: &Path) -> Result<Self, LoadError> {
		let raw: RawManifest = toml::from_str(text).map_err(LoadError::Toml)?;
		let mut errors = Vec::new();
		if raw.schema != SCHEMA_VERSION {
			push_error(&mut errors, "schema", format!("unsupported version {}, expected {SCHEMA_VERSION}", raw.schema));
		}

		let mut source_paths = BTreeSet::new();
		let mut sources = BTreeMap::new();
		for raw_source in raw.sources {
			let location = format!("sources.{}", raw_source.owner);
			let Some(owner) = validate_name(&raw_source.owner, &format!("{location}.owner"), &mut errors) else { continue };
			let Some(path) = validate_relative_path(&raw_source.path, &format!("{location}.path"), &mut errors) else { continue };
			if !workspace_root.join(path.as_str()).join("Cargo.toml").is_file() {
				push_error(&mut errors, format!("{location}.path"), format!("no Cargo.toml at {}", path.as_str()));
			}
			if !source_paths.insert(path.clone()) {
				push_error(&mut errors, format!("{location}.path"), format!("duplicate source path {}", path.as_str()));
			}
			if sources.insert(owner.clone(), Source { owner, path }).is_some() {
				push_error(&mut errors, format!("{location}.owner"), "duplicate source owner");
			}
		}

		let mut destinations = BTreeSet::new();
		let mut libraries = BTreeMap::new();
		for raw_library in raw.libraries {
			let location = format!("libraries.{}", raw_library.name);
			let Some(name) = validate_name(&raw_library.name, &format!("{location}.name"), &mut errors) else { continue };
			let Some(owner) = validate_name(&raw_library.owner, &format!("{location}.owner"), &mut errors) else { continue };
			let Some(destination) = validate_relative_path(&raw_library.destination, &format!("{location}.destination"), &mut errors) else { continue };
			let features = validate_name_list(raw_library.features, &format!("{location}.features"), &mut errors);
			let providers = validate_name_list(raw_library.providers, &format!("{location}.providers"), &mut errors);
			if !sources.contains_key(&owner) {
				push_error(&mut errors, format!("{location}.owner"), format!("unknown source owner {owner}"));
			}
			let expected = sources.get(&owner).and_then(|source| library_category(name.as_str(), owner.as_str(), source.path.as_str())).map(|category| format!("lib/{category}/{name}.lslib"));
			match expected {
				Some(expected) if destination.as_str() != expected => push_error(&mut errors, format!("{location}.destination"), format!("expected {expected}")),
				None if sources.contains_key(&owner) => push_error(&mut errors, format!("{location}.destination"), "source has no library ownership category"),
				_ => {}
			}
			if !destinations.insert(destination.clone()) {
				push_error(&mut errors, format!("{location}.destination"), "duplicate staged destination");
			}
			if libraries.insert(name.clone(), Library { name, owner, destination, features, providers }).is_some() {
				push_error(&mut errors, format!("{location}.name"), "duplicate library name");
			}
		}

		let mut programs = BTreeMap::new();
		for raw_program in raw.programs {
			let location = format!("programs.{}", raw_program.name);
			let Some(name) = validate_name(&raw_program.name, &format!("{location}.name"), &mut errors) else { continue };
			let Some(owner) = validate_name(&raw_program.owner, &format!("{location}.owner"), &mut errors) else { continue };
			let destination = validate_relative_path(&raw_program.destination, &format!("{location}.destination"), &mut errors).unwrap_or_else(|| RelativePath(raw_program.destination.clone()));
			validate_program_shape(&raw_program, &name, &destination, &location, &mut errors);
			let providers = validate_name_list(raw_program.providers, &format!("{location}.providers"), &mut errors);
			if !sources.contains_key(&owner) {
				push_error(&mut errors, format!("{location}.owner"), format!("unknown source owner {owner}"));
			}
			if !destinations.insert(destination.clone()) {
				push_error(&mut errors, format!("{location}.destination"), "duplicate staged destination");
			}
			let driver = raw_program.driver.as_ref().map(|raw| Driver {
				lifecycle: raw.lifecycle,
				priority: raw.priority,
				requires: raw.requires.clone(),
				provides: raw.provides.iter().map(|entry| Provides { kind: entry.kind, most: entry.most }).collect(),
				rules: raw
					.rules
					.iter()
					.map(|rule| MatchRule {
						transport: rule.transport.map(|transport| match transport {
							RawTransport::VirtioPci => TRANSPORT_VIRTIO_PCI,
							RawTransport::PlainPci => TRANSPORT_PLAIN_PCI,
						}),
						virtio_type: rule.virtio_type,
						pci_class: rule.pci_class,
						pci_subclass: rule.pci_subclass,
						pci_interface: rule.pci_interface,
						pci_vendor: rule.pci_vendor,
						pci_product: rule.pci_product,
						pci_address: rule.pci_address.map(|address| PciAddress { bus: address.bus, dev: address.dev, func: address.func }),
					})
					.collect(),
			});
			if programs.insert(name.clone(), Program { name, owner, role: raw_program.role, linkage: raw_program.linkage, stage: raw_program.stage, destination, providers, development: raw_program.development, driver }).is_some() {
				push_error(&mut errors, format!("{location}.name"), "duplicate program name");
			}
		}

		let mut factory_files = BTreeMap::new();
		let mut factory_sources = BTreeSet::new();
		for raw_factory_file in raw.factory_files {
			let location = format!("factory_files.{}", raw_factory_file.name);
			let Some(name) = validate_name(&raw_factory_file.name, &format!("{location}.name"), &mut errors) else { continue };
			let Some(destination) = validate_relative_path(&raw_factory_file.destination, &format!("{location}.destination"), &mut errors) else { continue };
			let source = match raw_factory_file.kind {
				FactoryFileKind::Source => {
					let Some(raw_source) = raw_factory_file.source else {
						push_error(&mut errors, format!("{location}.source"), "source factory files require a source path");
						continue;
					};
					let Some(source) = validate_relative_path(&raw_source, &format!("{location}.source"), &mut errors) else { continue };
					if !source.as_str().starts_with("volume/") {
						push_error(&mut errors, format!("{location}.source"), "source factory files must live below volume/");
					}
					if !workspace_root.join(source.as_str()).is_file() {
						push_error(&mut errors, format!("{location}.source"), format!("no factory file at {}", source.as_str()));
					}
					if !factory_sources.insert(source.clone()) {
						push_error(&mut errors, format!("{location}.source"), format!("duplicate factory source {}", source.as_str()));
					}
					Some(source)
				}
				FactoryFileKind::SdkComponent => {
					if raw_factory_file.source.is_some() {
						push_error(&mut errors, format!("{location}.source"), "SDK component payloads do not accept a source path");
					}
					None
				}
			};
			// The owner: required exactly where `source` is absent, so every staged byte can be
			// traced back to something that builds it. Without this the SDK payload was the one
			// thing in the volume with no provenance at all, and the volume's freshness model is
			// derived from provenance.
			let owner = match (raw_factory_file.kind, raw_factory_file.owner.as_deref()) {
				(FactoryFileKind::SdkComponent, None) => {
					push_error(&mut errors, format!("{location}.owner"), "SDK component payloads require the component that builds them");
					None
				}
				(FactoryFileKind::SdkComponent, Some(raw_owner)) => validate_name(raw_owner, &format!("{location}.owner"), &mut errors),
				(FactoryFileKind::Source, Some(_)) => {
					push_error(&mut errors, format!("{location}.owner"), "source factory files take their provenance from their source path");
					None
				}
				(FactoryFileKind::Source, None) => None,
			};
			validate_factory_file_shape(raw_factory_file.kind, source.as_ref(), &destination, &location, &mut errors);
			if !destinations.insert(destination.clone()) {
				push_error(&mut errors, format!("{location}.destination"), "duplicate staged destination");
			}
			if factory_files.insert(name.clone(), FactoryFile { name, kind: raw_factory_file.kind, source, owner, destination }).is_some() {
				push_error(&mut errors, format!("{location}.name"), "duplicate factory file name");
			}
		}

		let mut runtime_paths = BTreeMap::new();
		for raw_runtime_path in raw.runtime_paths {
			let location = format!("runtime_paths.{}", raw_runtime_path.name);
			let Some(name) = validate_name(&raw_runtime_path.name, &format!("{location}.name"), &mut errors) else { continue };
			let Some(owner) = validate_name(&raw_runtime_path.owner, &format!("{location}.owner"), &mut errors) else { continue };
			let Some(destination) = validate_relative_path(&raw_runtime_path.destination, &format!("{location}.destination"), &mut errors) else { continue };
			if !programs.contains_key(&owner) {
				push_error(&mut errors, format!("{location}.owner"), format!("unknown program owner {owner}"));
			}
			validate_runtime_path_shape(&name, &owner, &destination, &location, &mut errors);
			if !destinations.insert(destination.clone()) {
				push_error(&mut errors, format!("{location}.destination"), "duplicate staged or runtime destination");
			}
			if runtime_paths.insert(name.clone(), RuntimePath { name, owner, destination }).is_some() {
				push_error(&mut errors, format!("{location}.name"), "duplicate runtime path name");
			}
		}

		let mut services = BTreeMap::new();
		for raw_service in raw.services {
			let location = format!("services.{}", raw_service.name);
			let Some(name) = validate_name(&raw_service.name, &format!("{location}.name"), &mut errors) else { continue };
			let Some(program) = validate_name(&raw_service.program, &format!("{location}.program"), &mut errors) else { continue };
			let dependencies = validate_name_list(raw_service.dependencies, &format!("{location}.dependencies"), &mut errors);
			let mut roles: Vec<Role> = Vec::new();
			let mut seen_tags: BTreeSet<String> = BTreeSet::new();
			for raw_role in raw_service.roles {
				let where_role = format!("{location}.roles.{}", raw_role.tag);
				let Some(tag) = validate_tag(&raw_role.tag, &format!("{where_role}.tag"), &mut errors) else { continue };
				let Some(provider) = validate_name(&raw_role.provider, &format!("{where_role}.provider"), &mut errors) else { continue };
				if !seen_tags.insert(tag.to_string()) {
					push_error(&mut errors, format!("{where_role}.tag"), "duplicate role tag within one service");
				}
				// A `serve-root` is made by the supervisor FOR this service, so its provider is
				// itself; anything else naming itself is a service that cannot be started.
				if raw_role.kind == RoleKind::ServeRoot {
					if provider.as_str() != "self" {
						push_error(&mut errors, format!("{where_role}.provider"), "a serve-root role is created for the service itself, so its provider must be `self`");
					}
				} else if provider.as_str() == "self" {
					push_error(&mut errors, format!("{where_role}.provider"), "only a serve-root role may name `self` as its provider");
				}
				// An interface is what a CHANNEL speaks. On a memory object, a Domain handle or a
				// message of bytes there is nothing for it to describe, and a field that describes
				// nothing is one that goes stale unnoticed.
				if !raw_role.interface.is_empty() && matches!(raw_role.kind, RoleKind::Package | RoleKind::Payload | RoleKind::Power) {
					push_error(&mut errors, format!("{where_role}.interface"), "this role carries no channel, so it speaks no interface");
				}
				let source = if raw_role.source.is_empty() {
					Name(String::from("SERVE"))
				} else {
					match validate_tag(&raw_role.source, &format!("{where_role}.source"), &mut errors) {
						Some(tag) => tag,
						None => continue,
					}
				};
				// EXCLUSIVITY IS A PROPERTY OF A DUPLICATE, so it can only be said where duplication
				// is what happens. A serve root is made for this service alone, a factory mints a
				// fresh connection per caller, and the rest carry no client end to hand over -
				// marking any of them exclusive would be a word with nothing behind it.
				// `handed_on` widens a delivery that is a NARROWED DUPLICATE, and two kinds are:
				// a client role, and a serve root - `serve_root` makes the pair and hands the
				// service a duplicate narrowed to the same ceiling, which is why a service holding
				// one cannot pass it on either. A factory connection is minted per caller by the
				// provider, so there is nothing for this to widen.
				//
				// THE SERVE ROOT HAD TO BE ALLOWED BECAUSE ONE HOLDER'S JOB IS TO HAND IT ON:
				// ConsoleService spawns every shell, including the replacement a logout puts on the
				// primary VT, and that shell needs the supervisor channel the first one was given
				// by the supervisor itself. Without the right to duplicate, a reloaded shell had no
				// way to stop the machine at all.
				if raw_role.handed_on && !matches!(raw_role.kind, RoleKind::Client | RoleKind::ServeRoot) {
					push_error(&mut errors, format!("{where_role}.handed_on"), "only a role delivered as a duplicate can be given the right to duplicate");
				}
				if raw_role.exclusive && raw_role.kind != RoleKind::Client {
					push_error(&mut errors, format!("{where_role}.exclusive"), "only a client role is delivered as a duplicate, so only a client role can be handed over instead");
				}
				roles.push(Role { tag, kind: raw_role.kind, provider, presence: raw_role.presence, interface: raw_role.interface, source, exclusive: raw_role.exclusive, handed_on: raw_role.handed_on });
			}
			// A CLASS AND A PLACE MUST AGREE. Durable means written down, so it has to say where;
			// anything else means not written down, so a path would be a claim about a file that
			// does not exist and nothing would ever notice.
			if raw_service.state_class == StateClass::Durable && raw_service.state_storage.is_empty() {
				push_error(&mut errors, format!("{location}.state_storage"), "durable state has to say where it lives");
			}
			if raw_service.state_class != StateClass::Durable && !raw_service.state_storage.is_empty() {
				push_error(&mut errors, format!("{location}.state_storage"), "only durable state has a place - this service keeps nothing across a restart");
			}
			if services.insert(name.clone(), Service { name, program, restart: raw_service.restart, dependencies, roles, state_class: raw_service.state_class, state_scope: raw_service.state_scope, state_storage: raw_service.state_storage }).is_some() {
				push_error(&mut errors, format!("{location}.name"), "duplicate service name");
			}
		}
		// WHO MAY HOLD THE ROOT DOMAIN, COUNTED FROM THE DECLARATION RATHER THAN FOUND BY READING.
		//
		// The root-Domain handle carries `MANAGE`, and the kernel's own comment beside
		// `sys_system_power` says what that means: whoever holds it can already `sys_domain_kill`
		// the whole system. It reaches DeviceManager so the Power key works, and DeviceManager
		// passes it on to the two keyboard drivers. That is the authority leak the migration exists to
		// close, and until M11 replaces it with a narrow SystemPower service this list is what
		// stops a THIRD holder appearing unnoticed.
		//
		// M11 LANDED AND THE LIST IS EMPTY. The root Domain stays in SystemManager, which is not a
		// managed service and has no row here; everything that used to hold it - this supervisor,
		// DeviceManager, and one instance each of `virtio_input` and `xhci` - now holds a
		// SystemPower connection instead, which can ask for a reboot and can do nothing else.
		const MAY_HOLD_ROOT_DOMAIN: [&str; 0] = [];
		for service in services.values() {
			for role in &service.roles {
				if role.kind == RoleKind::Power && !MAY_HOLD_ROOT_DOMAIN.contains(&service.name.as_str()) {
					push_error(&mut errors, format!("services.{}.roles.{}", service.name, role.tag), "this service is not on the short list allowed to hold the root Domain - whoever holds it can kill the whole system");
				}
			}
		}

		// A provider has to exist and has to be something that can supply a capability: another
		// declared service, or `kernel` for what the boot chain was handed, or `service_manager`
		// for what the supervisor itself holds. Checked after the loop because a service may name
		// a provider that appears later in the file - the manifest's order is not the boot order.
		for service in services.values() {
			for role in &service.roles {
				let provider = role.provider.as_str();
				if provider == "self" || provider == "kernel" || provider == "service_manager" {
					continue;
				}
				if !services.contains_key(&role.provider) {
					push_error(&mut errors, format!("services.{}.roles.{}.provider", service.name, role.tag), format!("unknown role provider {provider}"));
					continue;
				}
				// A CAPABILITY FROM A SERVICE IS A DEPENDENCY ON IT, and saying so is what makes the
				// start order a consequence of the declaration rather than of the resolver's habits.
				// Thirteen roles were being handed over without one when this check was written -
				// the manifest already carried a comment about the last time that happened, where
				// the session service came up first on one machine and second on another and the
				// difference was a boot with no shell.
				// A CLIENT IS A CLIENT OF SOMETHING THAT EXISTS. The source names one of the
				// provider's serve roots; naming one it does not offer is a wiring that cannot be
				// carried out, and until now nothing could say so because the wiring was a variable
				// name in a hand-written function.
				if matches!(role.kind, RoleKind::Client | RoleKind::Factory) {
					if let Some(provider_service) = services.get(&role.provider) {
						let offered = provider_service.roles.iter().any(|candidate| candidate.kind == RoleKind::ServeRoot && candidate.tag == role.source);
						if !offered {
							push_error(&mut errors, format!("services.{}.roles.{}.source", service.name, role.tag), format!("{} offers no serve root called {}", role.provider, role.source));
						}
					}
				}
				if !service.dependencies.contains(&role.provider) {
					push_error(&mut errors, format!("services.{}.roles.{}.provider", service.name, role.tag), format!("{} supplies this role but is not a declared dependency, so nothing orders it first", provider));
				}
			}
		}

		// AN EXCLUSIVE CLIENT IS THE ONLY CLIENT. The supervisor has one end of each serve root and
		// hands it over whole for an exclusive role, so anyone else declaring a client of the same
		// root would be promised a handle that is already gone - and, worse, would break the
		// exclusivity the first one depends on. Order does not save it: the second delivery would
		// succeed or fail depending on which service the resolver happened to start first, which is
		// the class of bug this whole milestone exists to remove.
		for service in services.values() {
			for role in &service.roles {
				if !role.exclusive {
					continue;
				}
				for other in services.values() {
					for candidate in &other.roles {
						let same_root = candidate.provider == role.provider && candidate.source == role.source;
						let same_role = other.name == service.name && candidate.tag == role.tag;
						if same_root && !same_role && matches!(candidate.kind, RoleKind::Client) {
							push_error(&mut errors, format!("services.{}.roles.{}", other.name, candidate.tag), format!("{}.{} is handed this serve root exclusively, so nothing else can be a client of it", service.name, role.tag));
						}
					}
				}
			}
		}

		// The boot chain. Each kind may appear exactly once: two kernels or two loaders in one
		// image is not a configuration, it is a mistake, and the image builder would silently
		// pick whichever it saw last.
		let mut boot_artifacts = BTreeMap::new();
		let mut boot_kinds: BTreeMap<String, Name> = BTreeMap::new();
		for raw_artifact in raw.boot_artifacts {
			let location = format!("boot_artifacts.{}", raw_artifact.name);
			let Some(name) = validate_name(&raw_artifact.name, &format!("{location}.name"), &mut errors) else { continue };
			let Some(owner) = validate_name(&raw_artifact.owner, &format!("{location}.owner"), &mut errors) else { continue };
			let Some(destination) = validate_relative_path(&raw_artifact.destination, &format!("{location}.destination"), &mut errors) else { continue };
			if !sources.contains_key(&owner) {
				push_error(&mut errors, format!("{location}.owner"), format!("unknown source owner {owner}"));
			}
			let kind_label = format!("{:?}", raw_artifact.kind);
			if let Some(previous) = boot_kinds.insert(kind_label.clone(), name.clone()) {
				push_error(&mut errors, format!("{location}.kind"), format!("{kind_label} is already provided by {previous}"));
			}
			if !destinations.insert(destination.clone()) {
				push_error(&mut errors, format!("{location}.destination"), "duplicate staged destination");
			}
			if boot_artifacts.insert(name.clone(), BootArtifact { name, owner, kind: raw_artifact.kind, destination }).is_some() {
				push_error(&mut errors, format!("{location}.name"), "duplicate boot artifact name");
			}
		}
		// An image without a kernel or without a loader does not boot, so their absence is an
		// error here rather than a discovery at packaging time.
		for required in ["Kernel", "Loader", "InitPackage", "VolumePackage"] {
			if !boot_kinds.contains_key(required) {
				push_error(&mut errors, "boot_artifacts", format!("no artifact provides {required}"));
			}
		}
		validate_references(&libraries, &programs, &services, &mut errors);
		validate_graph("libraries", &libraries, |library| &library.providers, MAX_PROVIDER_DEPTH, MAX_LIBRARIES, &mut errors);
		validate_graph("services", &services, |service| &service.dependencies, usize::MAX, usize::MAX, &mut errors);
		validate_program_closures(&programs, &libraries, &mut errors);
		validate_user_source_coverage(workspace_root, &sources, &mut errors);
		validate_factory_source_coverage(workspace_root, &factory_files, &mut errors);
		if !factory_files.values().any(|file| file.kind == FactoryFileKind::SdkComponent) {
			push_error(&mut errors, "factory_files", "no SDK component payload is declared");
		}
		validate_executable_aliases(&programs, &mut errors);

		errors.sort();
		errors.dedup();
		if !errors.is_empty() {
			return Err(LoadError::Validation(errors));
		}
		Ok(Self { schema: raw.schema, sources, programs, factory_files, runtime_paths, services, libraries, boot_artifacts })
	}

	pub fn source_path(&self, owner: &str) -> Option<&str> {
		self.sources.iter().find(|(name, _)| name.as_str() == owner).map(|(_, source)| source.path.as_str())
	}

	pub fn canonical_json(&self) -> Result<String, serde_json::Error> {
		serde_json::to_string_pretty(self).map(|mut json| {
			json.push('\n');
			json
		})
	}

	// Every destination the system volume is expected to carry. `development` selects the
	// configuration: false is the shipping volume, which omits the development-only programs
	// entirely, and the two answers must not be conflated - a build that stages one set and
	// checks against the other is exactly the mistake this returns a parameter to prevent.
	pub fn volume_destinations(&self, development: bool) -> BTreeSet<String> {
		self.libraries.values().map(|library| library.destination.as_str().to_string()).chain(self.programs.values().filter(|program| program.stage == Stage::Volume && (development || !program.development)).map(|program| program.destination.as_str().to_string())).chain(self.factory_files.values().map(|file| file.destination.as_str().to_string())).collect()
	}
}

fn validate_name(value: &str, location: &str, errors: &mut Vec<ValidationError>) -> Option<Name> {
	let valid = !value.is_empty() && value.len() <= 64 && value.bytes().all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'));
	if !valid {
		push_error(errors, location, format!("invalid logical name {value:?}"));
		return None;
	}
	Some(Name(value.to_string()))
}

// A bootstrap role's tag, which travels on the wire as the first bytes of its message and is
// generated into a constant both ends read. Upper case with underscores, because that is what the
// twenty-three hand-written branches already use and because a tag that differs from its constant
// only by case is a defect nobody would see in a diff.
fn validate_tag(value: &str, location: &str, errors: &mut Vec<ValidationError>) -> Option<Name> {
	let valid = !value.is_empty() && value.len() <= 32 && value.bytes().all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'_');
	if !valid {
		push_error(errors, location, format!("invalid role tag {value:?} - upper case, digits and underscores, at most 32 bytes"));
		return None;
	}
	Some(Name(value.to_string()))
}

fn validate_relative_path(value: &str, location: &str, errors: &mut Vec<ValidationError>) -> Option<RelativePath> {
	let path = Path::new(value);
	let valid = !value.is_empty() && !path.is_absolute() && path.components().all(|component| matches!(component, Component::Normal(_))) && !value.contains('\\');
	if !valid {
		push_error(errors, location, format!("invalid normalized relative path {value:?}"));
		return None;
	}
	Some(RelativePath(value.to_string()))
}

fn validate_name_list(values: Vec<String>, location: &str, errors: &mut Vec<ValidationError>) -> Vec<Name> {
	let mut names = Vec::new();
	let mut seen = BTreeSet::new();
	for value in values {
		let Some(name) = validate_name(&value, location, errors) else { continue };
		if !seen.insert(name.clone()) {
			push_error(errors, location, format!("duplicate {name}"));
		}
		names.push(name);
	}
	names.sort();
	names
}

fn library_category<'a>(name: &str, owner: &str, source: &'a str) -> Option<&'a str> {
	if let Some(relative) = source.strip_prefix("user/libs/") {
		let (category, leaf) = relative.split_once('/')?;
		return (leaf == owner && !category.is_empty() && !category.contains('/')).then_some(category);
	}
	match (name, owner, source) {
		("lsrt", "rt", "user/runtime/rt") => Some("runtime"),
		("wire", "wire", "wire") => Some("ipc"),
		("wasm", "wasm", "wasm") => Some("component"),
		("term", "term", "term") => Some("terminal"),
		// Built from `service-logic` rather than from the service binaries. Those four modules -
		// executable naming, shell parsing, graph bounds, shutdown ordering - are pure functions of
		// their inputs, and they lived in a crate whose every binary links `rt`, so `cargo test`
		// could not build them at all. Splitting them out is what put their 24 tests in a gate.
		("service-util", "service-logic", "user/services/logic") => Some("service"),
		_ => None,
	}
}

fn validate_program_shape(raw: &RawProgram, name: &Name, destination: &RelativePath, location: &str, errors: &mut Vec<ValidationError>) {
	// A DRIVER HAS BINDING RULES AND NOTHING ELSE DOES, both directions.
	//
	// A driver with no rules is one DeviceManager can never select - it would be staged, cost image
	// space, and never bind, which reads exactly like a driver whose hardware is absent. Rules on a
	// service are rules nothing consults. Both are silent at runtime and loud here.
	match (&raw.driver, raw.role) {
		(None, ProgramRole::Driver) => push_error(errors, format!("{location}.driver"), "a driver must declare its binding rules; without them DeviceManager can never select it"),
		(Some(_), role) if role != ProgramRole::Driver => push_error(errors, format!("{location}.driver"), format!("binding rules on a {role:?} program are rules nothing will consult")),
		_ => {}
	}
	if let Some(driver) = &raw.driver {
		if driver.rules.is_empty() {
			push_error(errors, format!("{location}.driver.match"), "an empty match list selects nothing");
		}
		if driver.rules.iter().any(|rule| *rule == RawMatchRule::default()) {
			push_error(errors, format!("{location}.driver.match"), "a rule with no predicates selects EVERY device");
		}
		// A BOOT-CRITICAL DRIVER MUST BE IN `init.pkg`, which is the self-hosting cycle stated as a
		// rule rather than checked by one special case: a driver needed to reach the system volume
		// cannot be loaded from the system volume.
		if driver.lifecycle == DriverLifecycle::BootCritical && raw.stage != Stage::Pinned {
			push_error(errors, format!("{location}.driver.lifecycle"), "a boot-critical driver is needed to mount the system volume, so it cannot be staged on it - pin it into init.pkg");
		}
		if driver.lifecycle != DriverLifecycle::BootCritical && raw.stage == Stage::Pinned {
			push_error(errors, format!("{location}.stage"), "only a boot-critical driver belongs in init.pkg; everything else loads from the system volume");
		}
		// THE KEYS HAVE RELATIONS, AND A CLOSED SET IS NOT A COHERENT ONE.
		//
		// Everything above asks whether a rule has predicates; nothing asked whether they mean
		// anything together. `{ pci-subclass = 3 }` with no class is a rule about a number whose
		// meaning is defined by the field beside it, and `{ pci-product = 0x1000 }` with no vendor
		// names a part number in nobody's catalogue. Both parse, both generate, and both match
		// whatever happens to share the number.
		for (index, rule) in driver.rules.iter().enumerate() {
			let at = format!("{location}.driver.match[{index}]");
			// 1. The transport and the virtio type cite one specification between them. Either
			//    alone is half a citation: a transport with no type matches every virtio function,
			//    and a type with no transport is the `device-type` defect under a new name.
			//    A transport this system does not discover is refused by the parser rather than
			//    here: `RawTransport` is a closed enum, so the manifest fails to load.
			match (rule.transport, rule.virtio_type) {
				(Some(RawTransport::VirtioPci), None) => push_error(errors, at.clone(), "transport names a specification and virtio-type names the number in it; virtio-pci with no type matches every virtio function"),
				(None, Some(_)) => push_error(errors, at.clone(), "virtio-type is a number defined by the virtio specification, so the rule must say it is a virtio-pci function"),
				(Some(RawTransport::PlainPci), Some(_)) => push_error(errors, at.clone(), "a function that speaks no virtio transport has no virtio type"),
				(Some(RawTransport::PlainPci), None) if rule.pci_class.is_none() => push_error(errors, at.clone(), "plain-pci says what a function is NOT; the rule still has to say what it is, which for a plain PCI function is its class"),
				_ => {}
			}
			// 2 and 3. The class triple is a hierarchy, and each level is meaningless without the
			//    one above it: subclass 3 means one thing under class 0c and another under 03.
			if rule.pci_subclass.is_some() && rule.pci_class.is_none() {
				push_error(errors, at.clone(), "pci-subclass is defined WITHIN a class, so a subclass with no class matches whatever happens to share the number");
			}
			if rule.pci_interface.is_some() && (rule.pci_class.is_none() || rule.pci_subclass.is_none()) {
				push_error(errors, at.clone(), "pci-interface is defined within a class AND subclass; both must be named");
			}
			// 4. A product number is a part in a vendor's catalogue, and two vendors number
			//    independently.
			if rule.pci_product.is_some() && rule.pci_vendor.is_none() {
				push_error(errors, at.clone(), "pci-product is a part number in a vendor's catalogue, so pci-vendor must say whose");
			}
			// 5. VENDOR, PRODUCT AND ADDRESS NARROW; THEY NEVER SELECT.
			//
			//    A vendor number says who made a device and an address says where it is plugged in;
			//    neither says what it IS. A rule built only from them binds a driver to whatever
			//    occupies a slot, which is how a driver comes to be handed hardware it cannot
			//    drive. AND THERE IS NO EXCEPTION FOR THE DEVELOPMENT CONSOLE - its rule is
			//    transport, virtio-type AND address together, which is what the manifest already
			//    says: the binder's own comment records why OR-ing them would "bind any device at
			//    that address to the console driver".
			let names_a_standard: bool = rule.virtio_type.is_some() || rule.pci_class.is_some();
			if !names_a_standard && (rule.pci_vendor.is_some() || rule.pci_product.is_some() || rule.pci_address.is_some()) {
				push_error(errors, at, "pci-vendor, pci-product and pci-address may only NARROW a rule that already names a standard identity - a virtio type or a PCI class - because none of them says what a device is");
			}
		}
		// Two rules in ONE entry that overlap are a declaration written twice, which is harmless at
		// runtime and a sign the author meant two different things.
		for (index, rule) in driver.rules.iter().enumerate() {
			for other in &driver.rules[index + 1..] {
				if rule == other {
					push_error(errors, format!("{location}.driver.match"), "the same rule is declared twice");
				}
			}
		}
	}
	if raw.linkage == Linkage::Dynamic && raw.stage != Stage::Volume {
		push_error(errors, format!("{location}.stage"), "dynamic programs must be volume staged");
	}
	if raw.role == ProgramRole::Launcher && (raw.linkage != Linkage::Static || raw.stage != Stage::Pinned) {
		push_error(errors, format!("{location}.role"), "launchers must be static and pinned");
	}
	if matches!(raw.role, ProgramRole::Tool | ProgramRole::Helper) && raw.stage != Stage::Volume {
		push_error(errors, format!("{location}.stage"), "tools and helpers must be volume staged");
	}
	let expected_name = format!("{name}.lsexe");
	if raw.stage == Stage::Pinned {
		if destination.as_str() != expected_name {
			push_error(errors, format!("{location}.destination"), format!("expected {expected_name}"));
		}
		return;
	}
	let expected = match raw.role {
		ProgramRole::Tool => format!("bin/{expected_name}"),
		ProgramRole::Driver => format!("drivers/{expected_name}"),
		ProgramRole::Service | ProgramRole::Probe | ProgramRole::Helper if name.as_str() == "config_service" => format!("libexec/config_service/{expected_name}"),
		ProgramRole::Service | ProgramRole::Probe | ProgramRole::Helper => format!("libexec/{expected_name}"),
		ProgramRole::Launcher => unreachable!("launchers are pinned"),
	};
	if destination.as_str() != expected {
		push_error(errors, format!("{location}.destination"), format!("expected {expected}"));
	}
}

fn validate_factory_file_shape(kind: FactoryFileKind, source: Option<&RelativePath>, destination: &RelativePath, location: &str, errors: &mut Vec<ValidationError>) {
	match kind {
		FactoryFileKind::Source => {
			let Some(source) = source else { return };
			let Some(source_destination) = source.as_str().strip_prefix("volume/") else { return };
			if source_destination != destination.as_str() {
				push_error(errors, format!("{location}.destination"), format!("expected {source_destination}"));
			}
			// The factory files a source tree may install, by SHAPE rather than by a growing list of
			// names: a greeting, a test tone, a wallpaper, and a LiberCommander syntax descriptor.
			//
			// The descriptors are data the applications read and never execute - the format carries
			// literals and contexts, no commands and no paths - which is why a new language is one
			// more file here rather than a change to either application. Bounding them to
			// `bin/lico/syntax/*.syntax` with no deeper nesting is what keeps "install another
			// descriptor" from becoming "install anything anywhere under bin".
			let descriptor = destination.as_str().strip_prefix("bin/lico/syntax/").is_some_and(|name| !name.is_empty() && !name.contains('/') && name.ends_with(".syntax"));
			let valid = matches!(destination.as_str(), "hello.txt" | "motd.txt" | "audio/test.mp3") || descriptor || destination.as_str().strip_prefix("wallpapers/").is_some_and(|name| !name.is_empty() && !name.contains('/') && name.ends_with(".webp"));
			if !valid {
				push_error(errors, format!("{location}.destination"), "factory source files must be hello.txt, motd.txt, audio/test.mp3, or a wallpapers/*.webp file");
			}
		}
		FactoryFileKind::SdkComponent => {
			if destination.as_str() != "components/liber_component/app.wasm" {
				push_error(errors, format!("{location}.destination"), "SDK component payloads must stage at components/liber_component/app.wasm");
			}
		}
	}
}

fn validate_runtime_path_shape(name: &Name, owner: &Name, destination: &RelativePath, location: &str, errors: &mut Vec<ValidationError>) {
	let expected = match (name.as_str(), owner.as_str()) {
		("command-directory", "shell") => Some("bin"),
		("config-tree", "config_service") => Some("libexec/config_service/config.tree"),
		("liber-component-output", "component_host") => Some("components/liber_component/out.txt"),
		("system-journal", "log_service") => Some("log"),
		_ => None,
	};
	match expected {
		Some(expected) if destination.as_str() == expected => {}
		Some(expected) => push_error(errors, format!("{location}.destination"), format!("expected {expected}")),
		None => push_error(errors, format!("{location}.name"), format!("unsupported runtime path {} for {}", name.as_str(), owner.as_str())),
	}
}

fn validate_references(libraries: &BTreeMap<Name, Library>, programs: &BTreeMap<Name, Program>, services: &BTreeMap<Name, Service>, errors: &mut Vec<ValidationError>) {
	for (name, library) in libraries {
		for provider in &library.providers {
			if provider == name {
				push_error(errors, format!("libraries.{name}.providers"), "self provider edge");
			} else if !libraries.contains_key(provider) {
				push_error(errors, format!("libraries.{name}.providers"), format!("unknown library {provider}"));
			}
		}
	}
	// WHAT A DRIVER NEEDS AND WHAT IT MAY PUBLISH, checked against each other across the whole
	// registry - which is the only place either question can be answered.
	//
	// A requirement nothing produces is a driver that will wait for ever, and it reads at runtime
	// exactly like hardware that is absent. A cycle is two drivers each waiting for the other, which
	// reads the same way again. Both are decidable here, over a closed set of kinds, and neither is
	// discoverable on a machine without knowing what SHOULD have come up.
	let mut producers: BTreeMap<ProviderKindName, Vec<&Name>> = BTreeMap::new();
	for (name, program) in programs {
		let Some(driver) = &program.driver else { continue };
		for provides in &driver.provides {
			producers.entry(provides.kind).or_default().push(name);
		}
	}
	for (name, program) in programs {
		let Some(driver) = &program.driver else { continue };
		for kind in &driver.requires {
			if !producers.contains_key(kind) {
				push_error(errors, format!("programs.{name}.driver.requires"), format!("{kind:?} is required and no entry in this image declares it in `provides`, so this driver would wait for ever - which reads exactly like hardware that is absent"));
			}
			// A driver requiring what it publishes is a cycle of one, and the shortest kind to miss.
			if driver.provides.iter().any(|provides| provides.kind == *kind) {
				push_error(errors, format!("programs.{name}.driver.requires"), format!("{kind:?} is both required and provided by this entry, which is a driver waiting for itself"));
			}
		}
		for provides in &driver.provides {
			if provides.most == 0 {
				push_error(errors, format!("programs.{name}.driver.provides"), "`most = 0` declares a publication that may never happen, which is what leaving the row out already says");
			}
		}
	}
	// A STRUCTURAL CYCLE ACROSS ENTRIES. Reachability over "this driver requires a kind that driver
	// publishes", walked from each entry back to itself.
	for (name, program) in programs {
		let Some(driver) = &program.driver else { continue };
		let mut seen: BTreeSet<&Name> = BTreeSet::new();
		let mut frontier: Vec<&Name> = Vec::new();
		for kind in &driver.requires {
			frontier.extend(producers.get(kind).into_iter().flatten().copied());
		}
		while let Some(next) = frontier.pop() {
			if next == name {
				push_error(errors, format!("programs.{name}.driver.requires"), "a structural cycle: following what this entry requires leads back to itself, so neither driver can ever bind");
				break;
			}
			if !seen.insert(next) {
				continue;
			}
			let Some(other) = programs.get(next).and_then(|program| program.driver.as_ref()) else { continue };
			for kind in &other.requires {
				frontier.extend(producers.get(kind).into_iter().flatten().copied());
			}
		}
	}
	// THE AMBIGUOUS MATCH, refused where it can be decided: at registry-build time.
	//
	// Two entries whose rules can both match one node at the same priority means the selection
	// depends on enumeration order, which the milestone forbids by name. It is decidable because the
	// rule set is closed - see `MatchRule::overlaps` - so this is a real check rather than a
	// best-effort one, and the assertion in the selector downstream rests on it.
	//
	// Different priorities are not ambiguous: that is what a priority is for. A quirk beating a
	// generic entry on the same device is the mechanism working.
	let drivers: Vec<(&Name, &Driver)> = programs.iter().filter_map(|(name, program)| program.driver.as_ref().map(|driver| (name, driver))).collect();
	for (index, (name, driver)) in drivers.iter().enumerate() {
		for (other_name, other) in &drivers[index + 1..] {
			if driver.priority != other.priority {
				continue;
			}
			for rule in &driver.rules {
				for other_rule in &other.rules {
					if rule.overlaps(*other_rule) {
						push_error(errors, format!("programs.{name}.driver.match"), format!("{rule:?} can also match {other_name}, at the same priority - a device would be bound by enumeration order"));
					}
				}
			}
		}
	}
	for (name, program) in programs {
		for provider in &program.providers {
			if !libraries.contains_key(provider) {
				push_error(errors, format!("programs.{name}.providers"), format!("unknown library {provider}"));
			}
		}
	}
	for (name, service) in services {
		if !programs.contains_key(&service.program) {
			push_error(errors, format!("services.{name}.program"), format!("unknown program {}", service.program));
		}
		for dependency in &service.dependencies {
			if dependency == name {
				push_error(errors, format!("services.{name}.dependencies"), "self dependency edge");
			} else if !services.contains_key(dependency) {
				push_error(errors, format!("services.{name}.dependencies"), format!("unknown service {dependency}"));
			}
		}
	}
}

fn validate_graph<T, F>(namespace: &str, nodes: &BTreeMap<Name, T>, edges: F, max_depth: usize, max_modules: usize, errors: &mut Vec<ValidationError>)
where
	F: Fn(&T) -> &[Name],
{
	#[allow(clippy::too_many_arguments)]
	fn visit<T, F>(name: &Name, nodes: &BTreeMap<Name, T>, edges: &F, visiting: &mut Vec<Name>, visited: &mut BTreeSet<Name>, max_depth: usize, errors: &mut Vec<ValidationError>, namespace: &str)
	where
		F: Fn(&T) -> &[Name],
	{
		if visited.contains(name) || !nodes.contains_key(name) {
			return;
		}
		if let Some(index) = visiting.iter().position(|current| current == name) {
			let cycle = visiting[index..].iter().chain(std::iter::once(name)).map(Name::as_str).collect::<Vec<_>>().join(" -> ");
			push_error(errors, format!("{namespace}.{name}"), format!("dependency cycle: {cycle}"));
			return;
		}
		if visiting.len() >= max_depth {
			push_error(errors, format!("{namespace}.{name}"), format!("dependency depth exceeds {max_depth}"));
			return;
		}
		visiting.push(name.clone());
		for dependency in edges(&nodes[name]) {
			visit(dependency, nodes, edges, visiting, visited, max_depth, errors, namespace);
		}
		visiting.pop();
		visited.insert(name.clone());
	}

	if nodes.len() > max_modules {
		push_error(errors, namespace, format!("module count {} exceeds {max_modules}", nodes.len()));
	}
	let mut visited = BTreeSet::new();
	for name in nodes.keys() {
		visit(name, nodes, &edges, &mut Vec::new(), &mut visited, max_depth, errors, namespace);
	}
}

fn validate_program_closures(programs: &BTreeMap<Name, Program>, libraries: &BTreeMap<Name, Library>, errors: &mut Vec<ValidationError>) {
	fn collect(name: &Name, libraries: &BTreeMap<Name, Library>, modules: &mut BTreeSet<Name>) {
		if !modules.insert(name.clone()) {
			return;
		}
		if let Some(library) = libraries.get(name) {
			for provider in &library.providers {
				collect(provider, libraries, modules);
			}
		}
	}
	for (name, program) in programs {
		let mut modules = BTreeSet::new();
		for provider in &program.providers {
			collect(provider, libraries, &mut modules);
		}
		if modules.len() > MAX_PROVIDER_MODULES {
			push_error(errors, format!("programs.{name}.providers"), format!("provider closure {} exceeds {MAX_PROVIDER_MODULES}", modules.len()));
		}
	}
}

fn validate_user_source_coverage(workspace_root: &Path, sources: &BTreeMap<Name, Source>, errors: &mut Vec<ValidationError>) {
	fn collect(directory: &Path, workspace_root: &Path, output: &mut BTreeSet<String>) {
		let Ok(entries) = fs::read_dir(directory) else { return };
		for entry in entries.flatten() {
			let path = entry.path();
			if path.is_dir() {
				if path.join("Cargo.toml").is_file()
					&& let Ok(relative) = path.strip_prefix(workspace_root)
				{
					output.insert(relative.to_string_lossy().replace('\\', "/"));
				}
				collect(&path, workspace_root, output);
			}
		}
	}
	let mut physical = BTreeSet::new();
	collect(&workspace_root.join("user"), workspace_root, &mut physical);
	let declared = sources.values().filter(|source| source.path.as_str().starts_with("user/")).map(|source| source.path.as_str().to_string()).collect::<BTreeSet<_>>();
	for missing in physical.difference(&declared) {
		push_error(errors, "sources", format!("physical userspace crate {missing} has no source owner"));
	}
	for missing in declared.difference(&physical) {
		push_error(errors, "sources", format!("declared userspace crate {missing} is not physical"));
	}
}

fn validate_factory_source_coverage(workspace_root: &Path, factory_files: &BTreeMap<Name, FactoryFile>, errors: &mut Vec<ValidationError>) {
	fn collect(directory: &Path, workspace_root: &Path, output: &mut BTreeSet<String>) {
		let Ok(entries) = fs::read_dir(directory) else { return };
		for entry in entries.flatten() {
			let path = entry.path();
			if path.is_dir() {
				collect(&path, workspace_root, output);
			} else if path.is_file()
				&& let Ok(relative) = path.strip_prefix(workspace_root)
			{
				output.insert(relative.to_string_lossy().replace('\\', "/"));
			}
		}
	}
	let mut physical = BTreeSet::new();
	collect(&workspace_root.join("volume"), workspace_root, &mut physical);
	let declared = factory_files.values().filter_map(|file| file.source.as_ref().map(|source| source.as_str().to_string())).collect::<BTreeSet<_>>();
	for missing in physical.difference(&declared) {
		push_error(errors, "factory_files", format!("physical factory file {missing} is not declared"));
	}
	for missing in declared.difference(&physical) {
		push_error(errors, "factory_files", format!("declared factory file {missing} is not physical"));
	}
}

fn validate_executable_aliases(programs: &BTreeMap<Name, Program>, errors: &mut Vec<ValidationError>) {
	for (first_index, first) in programs.keys().enumerate() {
		for second in programs.keys().skip(first_index + 1) {
			let ambiguous = second.as_str() == format!("{}.lsexe", first.as_str()) || first.as_str() == format!("{}.lsexe", second.as_str());
			if ambiguous {
				push_error(errors, format!("programs.{first}.name"), format!("ambiguous executable alias {second}"));
			}
		}
	}
}

fn push_error(errors: &mut Vec<ValidationError>, location: impl Into<String>, message: impl Into<String>) {
	errors.push(ValidationError { location: location.into(), message: message.into() });
}

#[cfg(test)]
mod tests;
