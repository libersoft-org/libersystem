use super::*;
use std::time::{SystemTime, UNIX_EPOCH};

fn fixture_workspace() -> PathBuf {
	let nonce = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
	let root = std::env::temp_dir().join(format!("liber-system-manifest-{}-{nonce}", std::process::id()));
	fs::create_dir_all(root.join("user/libs/audio/pcm")).unwrap();
	fs::create_dir_all(root.join("user/apps/tool")).unwrap();
	fs::write(root.join("user/libs/audio/pcm/Cargo.toml"), "[package]\nname='pcm'\nversion='0.0.0'\n").unwrap();
	fs::write(root.join("user/apps/tool/Cargo.toml"), "[package]\nname='tool'\nversion='0.0.0'\n").unwrap();
	root
}

fn valid_fixture() -> &'static str {
	r#"
schema = 1

[[sources]]
owner = "pcm"
path = "user/libs/audio/pcm"

[[sources]]
owner = "tool"
path = "user/apps/tool"

[[libraries]]
name = "pcm"
owner = "pcm"
destination = "lib/audio/pcm.lslib"

[[programs]]
name = "tool"
owner = "tool"
role = "tool"
linkage = "dynamic"
stage = "volume"
destination = "bin/tool.lsexe"
providers = ["pcm"]

[[factory_files]]
name = "liber-component"
kind = "sdk-component"
owner = "liber_component"
destination = "components/liber_component/app.wasm"

[[services]]
name = "tool_service"
program = "tool"
restart = "escalate"
# Every service says what a restart does to its state. Required rather than defaulted: a default
# would let a new service arrive unclassified, and the classification exists because nobody could
# otherwise say what a restart loses.
state_class = "ephemeral"
state_scope = "service"
dependencies = []

# An image without a kernel or a loader does not boot, so `Manifest::parse` requires all four
# kinds. This fixture predated that rule and had stopped parsing at all - which nothing noticed,
# because no gate ran this crate's suite. That is the inventory defect the model was written about,
# found by the gate that now runs all fifty-eight of them.
#
# One owner for all four: the rule under test is that each KIND is provided exactly once, and
# spreading them over four sources would also change what `sources.len()` asserts below.
[[boot_artifacts]]
name = "kernel"
owner = "tool"
kind = "kernel"
destination = "kernel"

[[boot_artifacts]]
name = "loader"
owner = "tool"
kind = "loader"
destination = "EFI/BOOT/BOOTX64.EFI"

[[boot_artifacts]]
name = "init-package"
owner = "tool"
kind = "init-package"
destination = "init.pkg"

[[boot_artifacts]]
name = "volume-package"
owner = "tool"
kind = "volume-package"
destination = "volume.pkg"
"#
}

#[test]
fn every_boot_artifact_kind_is_required() {
	// The rule the fixture above had silently stopped satisfying. Asserted directly so the next
	// person to touch the fixture learns which four kinds are mandatory from a failure that says
	// so, rather than from four unrelated tests going red at once.
	let root = fixture_workspace();
	for (name, kind, destination) in [
		("kernel", "kernel", "kernel"),
		("loader", "loader", "EFI/BOOT/BOOTX64.EFI"),
		("init-package", "init-package", "init.pkg"),
		("volume-package", "volume-package", "volume.pkg"),
	] {
		let stanza = format!("[[boot_artifacts]]\nname = \"{name}\"\nowner = \"tool\"\nkind = \"{kind}\"\ndestination = \"{destination}\"\n");
		let without = valid_fixture().replace(&stanza, "");
		assert_ne!(without, valid_fixture(), "the {kind} stanza must actually be removed, or this proves nothing");
		let error = Manifest::parse(&without, &root).unwrap_err().to_string();
		assert!(error.contains("no artifact provides"), "an image with no {kind} must be refused, got: {error}");
	}
	fs::remove_dir_all(root).unwrap();
}

#[test]
fn canonical_model_sorts_entities_and_edges() {
	let root = fixture_workspace();
	let manifest = Manifest::parse(valid_fixture(), &root).unwrap();
	assert_eq!(manifest.sources.len(), 2);
	assert_eq!(manifest.programs.len(), 1);
	assert_eq!(manifest.services.len(), 1);
	assert_eq!(manifest.libraries.len(), 1);
	assert_eq!(manifest.source_path("pcm"), Some("user/libs/audio/pcm"));
	let canonical = manifest.canonical_json().unwrap();
	assert!(canonical.contains("\"tool_service\""));
	let first = "[[sources]]\nowner = \"pcm\"\npath = \"user/libs/audio/pcm\"\n\n[[sources]]\nowner = \"tool\"\npath = \"user/apps/tool\"";
	let second = "[[sources]]\nowner = \"tool\"\npath = \"user/apps/tool\"\n\n[[sources]]\nowner = \"pcm\"\npath = \"user/libs/audio/pcm\"";
	let reordered = valid_fixture().replace(first, second);
	assert_eq!(Manifest::parse(&reordered, &root).unwrap().canonical_json().unwrap(), canonical);
	fs::remove_dir_all(root).unwrap();
}

#[test]
fn validation_aggregates_schema_reference_path_and_cycle_errors() {
	let root = fixture_workspace();
	let invalid = valid_fixture().replace("schema = 1", "schema = 9").replace("providers = [\"pcm\"]", "providers = [\"missing\"]").replace("dependencies = []", "dependencies = [\"tool_service\"]").replace("destination = \"bin/tool.lsexe\"", "destination = \"../tool\"");
	let error = Manifest::parse(&invalid, &root).unwrap_err().to_string();
	assert!(error.contains("unsupported version 9"));
	assert!(error.contains("unknown library missing"));
	assert!(error.contains("invalid normalized relative path"));
	assert!(error.contains("self dependency edge"));
	fs::remove_dir_all(root).unwrap();
}

#[test]
fn unknown_fields_fail_during_deserialization() {
	let root = fixture_workspace();
	let error = Manifest::parse(&valid_fixture().replace("schema = 1", "schema = 1\nextra = true"), &root).unwrap_err().to_string();
	assert!(error.contains("unknown field"));
	fs::remove_dir_all(root).unwrap();
}

#[test]
fn a_built_payload_must_name_what_builds_it() {
	// The SDK payload is the one staged artifact with no source path, and its provenance is what
	// puts `src/sdk` in the system volume's freshness model. Without an owner the volume's staleness
	// digest could not see an SDK edit at all: change the SDK, rebuild nothing, and a guest ran the
	// previous `app.wasm` against a digest that agreed the volume was current.
	let root = fixture_workspace();
	let without_owner = valid_fixture().replace("owner = \"liber_component\"\n", "");
	let error = Manifest::parse(&without_owner, &root).expect_err("an sdk-component with no owner must be refused");
	assert!(format!("{error:?}").contains("require the component that builds them"), "got {error:?}");

	// And the converse: a checked-in source file takes its provenance from its path, so naming an
	// owner there would be a second answer to a question that already has one.
	fs::create_dir_all(root.join("volume")).unwrap();
	fs::write(root.join("volume/hello.txt"), "hello").unwrap();
	let source_with_owner = format!("{}\n[[factory_files]]\nname = \"hello\"\nkind = \"source\"\nsource = \"volume/hello.txt\"\nowner = \"liber_component\"\ndestination = \"hello.txt\"\n", valid_fixture());
	let error = Manifest::parse(&source_with_owner, &root).expect_err("a source factory file with an owner must be refused");
	assert!(format!("{error:?}").contains("take their provenance from their source path"), "got {error:?}");
}

#[test]
fn factory_files_cover_the_source_tree_and_enforce_the_layout() {
	let root = fixture_workspace();
	fs::create_dir_all(root.join("volume/audio")).unwrap();
	fs::write(root.join("volume/hello.txt"), "hello").unwrap();
	fs::write(root.join("volume/audio/test.mp3"), "mp3").unwrap();
	let fixture = format!("{}\n[[factory_files]]\nname = \"hello\"\nkind = \"source\"\nsource = \"volume/hello.txt\"\ndestination = \"hello.txt\"\n\n[[factory_files]]\nname = \"audio-demo\"\nkind = \"source\"\nsource = \"volume/audio/test.mp3\"\ndestination = \"audio/test.mp3\"\n", valid_fixture());
	let manifest = Manifest::parse(&fixture, &root).unwrap();
	assert_eq!(manifest.factory_files.len(), 3);
	let invalid = fixture.replace("destination = \"audio/test.mp3\"", "destination = \"share/test.mp3\"");
	let error = Manifest::parse(&invalid, &root).unwrap_err().to_string();
	assert!(error.contains("expected audio/test.mp3"));
	let without_component = fixture.replace("\n[[factory_files]]\nname = \"liber-component\"\nkind = \"sdk-component\"\nowner = \"liber_component\"\ndestination = \"components/liber_component/app.wasm\"\n", "");
	let error = Manifest::parse(&without_component, &root).unwrap_err().to_string();
	assert!(error.contains("no SDK component payload is declared"));
	fs::remove_dir_all(root).unwrap();
}

#[test]
fn volume_program_roles_have_exact_destinations() {
	let root = fixture_workspace();
	let helper = valid_fixture().replace("role = \"tool\"", "role = \"helper\"").replace("destination = \"bin/tool.lsexe\"", "destination = \"libexec/tool.lsexe\"");
	Manifest::parse(&helper, &root).unwrap();
	let nested = helper.replace("destination = \"libexec/tool.lsexe\"", "destination = \"libexec/tool/private/tool.lsexe\"");
	let error = Manifest::parse(&nested, &root).unwrap_err().to_string();
	assert!(error.contains("expected libexec/tool.lsexe"));
	fs::remove_dir_all(root).unwrap();
}

#[test]
fn runtime_paths_require_their_declared_owner_and_destination() {
	let root = fixture_workspace();
	fs::create_dir_all(root.join("user/services/core")).unwrap();
	fs::write(root.join("user/services/core/Cargo.toml"), "[package]\nname='services'\nversion='0.0.0'\n").unwrap();
	let fixture = format!("{}\n[[sources]]\nowner = \"services\"\npath = \"user/services/core\"\n\n[[programs]]\nname = \"config_service\"\nowner = \"services\"\nrole = \"service\"\nlinkage = \"dynamic\"\nstage = \"volume\"\ndestination = \"libexec/config_service/config_service.lsexe\"\n\n[[runtime_paths]]\nname = \"config-tree\"\nowner = \"config_service\"\ndestination = \"libexec/config_service/config.tree\"\n", valid_fixture());
	Manifest::parse(&fixture, &root).unwrap();
	let invalid = fixture.replace("destination = \"libexec/config_service/config.tree\"", "destination = \"config.tree\"");
	let error = Manifest::parse(&invalid, &root).unwrap_err().to_string();
	assert!(error.contains("expected libexec/config_service/config.tree"));
	fs::remove_dir_all(root).unwrap();
}

// The driver registry's own checks, each watched to REFUSE. A validation nobody has seen say no is
// a validation that might not be wired up at all - which is how this manifest came to have three
// hand-maintained lists that agreed with each other and with nothing else.
#[test]
fn the_driver_registry_refuses_what_it_says_it_refuses() {
	let root = fixture_workspace();
	let errors = |text: &str| -> String { Manifest::parse(text, &root).err().map(|error| error.to_string()).unwrap_or_default() };
	// A driver program appended to the ordinary fixture, so everything else about it is valid and
	// only the thing under test is wrong.
	let with = |driver: &str| -> String { format!("{}\n[[programs]]\nname = \"a_driver\"\nowner = \"tool\"\nrole = \"driver\"\nlinkage = \"static\"\nstage = \"volume\"\ndestination = \"drivers/a_driver.lsexe\"\n{driver}", valid_fixture()) };
	const ORDINARY: &str = "\n[programs.driver]\nlifecycle = \"controller\"\nmatch = [{ transport = \"virtio-pci\", virtio-type = 2 }]\n";

	// The shape it is built for parses, so every refusal below is about the one thing it changes.
	assert_eq!(errors(&with(ORDINARY)), "", "the ordinary shape must validate");

	// A DRIVER WITH NO RULES is one nothing can ever select - staged, costing image space, and
	// indistinguishable at runtime from a driver whose hardware is absent.
	assert!(errors(&with("")).contains("binding rules"), "a driver without rules must be refused: {}", errors(&with("")));

	// An empty match list is the same thing written differently.
	let empty = ORDINARY.replace("[{ transport = \"virtio-pci\", virtio-type = 2 }]", "[]");
	assert!(errors(&with(&empty)).contains("selects nothing"), "{}", errors(&with(&empty)));

	// A RULE THE SYSTEM CANNOT ANSWER fails to PARSE rather than never matching. That is what the
	// closed rule set buys: the difference between a typo caught at build time and a driver that
	// silently never binds.
	let unknown = ORDINARY.replace("virtio-type = 2", "usb-class = 3");
	assert!(!errors(&with(&unknown)).is_empty(), "a rule naming something discovery does not report must not parse");

	// A RULE WITH NO PREDICATES selects every device on the machine.
	let everything = ORDINARY.replace("[{ transport = \"virtio-pci\", virtio-type = 2 }]", "[{}]");
	assert!(errors(&with(&everything)).contains("EVERY device"), "{}", errors(&with(&everything)));

	// A CONJUNCTION NARROWS, so two entries that share a predicate but differ on another do not
	// overlap - which is exactly the development console beside the ordinary one. Watched, because
	// getting this wrong in the other direction is what the first version did: predicates named for
	// one field under two names could not be compared at all, and the check refused the real
	// manifest on its first run.
	let narrowed = format!("{}{}\n[programs.driver]\nlifecycle = \"controller\"\nmatch = [{{ transport = \"virtio-pci\", virtio-type = 2, pci-address = {{ bus = 0, dev = 30, func = 0 }} }}]\npriority = \"quirk\"\n", with(ORDINARY), "\n[[programs]]\nname = \"b_driver\"\nowner = \"tool\"\nrole = \"driver\"\nlinkage = \"static\"\nstage = \"volume\"\ndestination = \"drivers/b_driver.lsexe\"\n");
	assert_eq!(errors(&narrowed), "", "a narrower rule at a higher priority is arbitration, not ambiguity");

	// ------------------------------------------------- the keys have relations
	//
	// A CLOSED SET IS NOT A COHERENT ONE. Everything above asks whether a rule HAS predicates;
	// these ask whether they mean anything together. Each one is watched to refuse, because a
	// validator nobody has seen refuse is a validator that accepts.
	let rule = |predicates: &str| -> String { ORDINARY.replace("transport = \"virtio-pci\", virtio-type = 2", predicates) };

	// 1. The transport and the virtio type cite one specification between them, and either alone is
	//    half a citation.
	let bare_transport = rule("transport = \"virtio-pci\"");
	assert!(errors(&with(&bare_transport)).contains("matches every virtio function"), "{}", errors(&with(&bare_transport)));
	let bare_type = rule("virtio-type = 2");
	assert!(errors(&with(&bare_type)).contains("virtio-pci function"), "{}", errors(&with(&bare_type)));
	// And `plain-pci` is a predicate, not an absence: it says what a function is NOT, so the rule
	// still has to say what it is.
	let plain_alone = rule("transport = \"plain-pci\"");
	assert!(errors(&with(&plain_alone)).contains("still has to say what it is"), "{}", errors(&with(&plain_alone)));
	let plain_with_type = rule("transport = \"plain-pci\", virtio-type = 2");
	assert!(errors(&with(&plain_with_type)).contains("no virtio type"), "{}", errors(&with(&plain_with_type)));

	// 2 and 3. The class triple is a hierarchy: subclass 3 means one thing under class 0c and
	//    another under 03, so a level without the one above it matches whatever shares the number.
	let orphan_subclass = rule("pci-subclass = 3");
	assert!(errors(&with(&orphan_subclass)).contains("defined WITHIN a class"), "{}", errors(&with(&orphan_subclass)));
	let orphan_interface = rule("pci-class = 12, pci-interface = 48");
	assert!(errors(&with(&orphan_interface)).contains("class AND subclass"), "{}", errors(&with(&orphan_interface)));

	// 4. A product number is a part in a vendor's catalogue, and two vendors number independently.
	let orphan_product = rule("pci-class = 12, pci-product = 4096");
	assert!(errors(&with(&orphan_product)).contains("vendor's catalogue"), "{}", errors(&with(&orphan_product)));

	// 5. VENDOR, PRODUCT AND ADDRESS NARROW; THEY NEVER SELECT. A rule built only from them binds a
	//    driver to whatever occupies a slot - which is how a driver comes to be handed hardware it
	//    cannot drive.
	let vendor_alone = rule("pci-vendor = 6900");
	assert!(errors(&with(&vendor_alone)).contains("only NARROW"), "{}", errors(&with(&vendor_alone)));
	let address_alone = rule("pci-address = { bus = 0, dev = 30, func = 0 }");
	assert!(errors(&with(&address_alone)).contains("only NARROW"), "{}", errors(&with(&address_alone)));
	// AND THERE IS NO EXCEPTION FOR THE DEVELOPMENT CONSOLE, which is the rule this milestone very
	// nearly wrote one for. Its real rule names the transport, the virtio type AND the address -
	// all three - and that is what the manifest says today.
	let console = rule("transport = \"virtio-pci\", virtio-type = 3, pci-address = { bus = 0, dev = 30, func = 0 }");
	assert_eq!(errors(&with(&console)), "", "an address NARROWING a standard identity is exactly what a pinned device is");

	// A QUIRK IS A NARROWING TOO, and this is what one looks like written correctly: a rule that
	// says which part it is a quirk for, on top of a rule that already says what the device is.
	let quirk = rule("transport = \"virtio-pci\", virtio-type = 2, pci-vendor = 6900, pci-product = 4162");
	assert_eq!(errors(&with(&quirk)), "", "a vendor and product narrowing a virtio rule is a quirk, which is what they are for");

	// A BOOT-CRITICAL DRIVER ON THE VOLUME is the self-hosting cycle: it is needed to mount the
	// volume it would be loaded from.
	let cycle = ORDINARY.replace("controller", "boot-critical");
	assert!(errors(&with(&cycle)).contains("cannot be staged on it"), "{}", errors(&with(&cycle)));

	// AND THE CONVERSE, WHICH IS WHAT KEEPS THE BOOTSTRAP EXCEPTION SMALL.
	//
	// The cycle check alone stops a boot-critical driver from being staged on the volume; on its own
	// it says nothing about the other direction, and `init.pkg` is the thing that grows quietly - it
	// is read before any storage exists, so everything in it is loaded on every boot whether the
	// hardware is there or not. Pinning anything else is refused, so widening the exception means
	// declaring a driver BOOT-CRITICAL, which is a deliberate act with its own refusal above it.
	//
	// The two together are the rule: pinned if and only if boot-critical.
	//
	// BUILT RATHER THAN PATCHED. A `.replace("stage = \"volume\"", ..)` over the whole fixture hits
	// the FIRST program in it, which is not the driver under test - so the case passed while proving
	// something about an unrelated entry. Watched: with the check removed the assertion still failed,
	// but the message named `programs.tool`.
	let pinned = format!("{}\n[[programs]]\nname = \"a_driver\"\nowner = \"tool\"\nrole = \"driver\"\nlinkage = \"static\"\nstage = \"pinned\"\ndestination = \"a_driver.lsexe\"\n{ORDINARY}", valid_fixture());
	let reported = errors(&pinned);
	assert!(reported.contains("only a boot-critical driver belongs in init.pkg"), "a driver pinned without being boot-critical must be refused: {reported}");
	assert!(reported.contains("a_driver"), "and the refusal must name the entry it is about: {reported}");

	// TWO ENTRIES THAT CAN MATCH ONE NODE at the same priority means the answer depends on
	// enumeration order, which is what the milestone forbids by name.
	let second = "\n[[programs]]\nname = \"b_driver\"\nowner = \"tool\"\nrole = \"driver\"\nlinkage = \"static\"\nstage = \"volume\"\ndestination = \"drivers/b_driver.lsexe\"\n";
	let ambiguous = format!("{}{second}{ORDINARY}", with(ORDINARY));
	assert!(errors(&ambiguous).contains("enumeration order"), "{}", errors(&ambiguous));

	// The SAME pair at different priorities is not ambiguous - that is what a priority is for, and
	// a quirk outranking a generic entry on one device is the mechanism working.
	let arbitrated = format!("{}{second}{ORDINARY}priority = \"quirk\"\n", with(ORDINARY));
	assert_eq!(errors(&arbitrated), "", "a quirk and a generic entry are arbitrated, not ambiguous");

	// BINDING RULES ON A NON-DRIVER are rules nothing consults.
	let misplaced = with(ORDINARY).replace("role = \"driver\"", "role = \"tool\"").replace("destination = \"drivers/a_driver.lsexe\"", "destination = \"bin/a_driver.lsexe\"");
	assert!(errors(&misplaced).contains("nothing will consult"), "{}", errors(&misplaced));
}

#[test]
fn an_ordinary_ethernet_controller_is_not_offered_to_the_virtio_network_driver() {
	// THE TEST THAT KEEPS THE FIRST DRAFT'S BUG OUT.
	//
	// P02M0163's first draft prescribed moving the virtio rows to PCI class. For `virtio_net` that
	// is class `02/00` - Ethernet controller - which matches EVERY network card ever made, and
	// hands a virtio driver hardware it cannot drive. The prescription was deleted; this is what
	// stops it coming back.
	//
	// The real rule pins the TRANSPORT, so the question a rule asks is "is this a virtio-pci
	// function whose virtio type is 1", and a plain Ethernet controller answers no at the first
	// predicate.
	let virtio_net = MatchRule { transport: Some(TRANSPORT_VIRTIO_PCI), virtio_type: Some(1), ..MatchRule::default() };
	let ordinary_nic = MatchRule { transport: Some(TRANSPORT_PLAIN_PCI), pci_class: Some(0x02), pci_subclass: Some(0x00), ..MatchRule::default() };
	assert!(!virtio_net.overlaps(ordinary_nic), "a virtio-pci rule and a plain-pci rule name disjoint sets of functions");

	// AND THE VERSION THAT WOULD HAVE BEEN WRONG, so the difference is on the record rather than in
	// a comment: a virtio-net rule written as a class match DOES collide with every Ethernet card.
	let by_class_alone = MatchRule { pci_class: Some(0x02), pci_subclass: Some(0x00), ..MatchRule::default() };
	assert!(by_class_alone.overlaps(ordinary_nic), "which is exactly why the rule is not written that way");
}

#[test]
fn two_identical_controllers_do_not_collide_and_a_pinned_one_narrows() {
	// Two functions of one kind are one rule matching twice, which is what a driver per device
	// means - not an ambiguity. The registry's ambiguity check is about two ENTRIES that could both
	// claim one function, and one entry claiming two functions is the ordinary case.
	let console = MatchRule { transport: Some(TRANSPORT_VIRTIO_PCI), virtio_type: Some(3), ..MatchRule::default() };
	let pinned = MatchRule { transport: Some(TRANSPORT_VIRTIO_PCI), virtio_type: Some(3), pci_address: Some(PciAddress { bus: 0, dev: 30, func: 0 }), ..MatchRule::default() };
	// They DO overlap - the pinned one is a subset - and that is arbitration by priority rather
	// than ambiguity, which is why the registry compares priorities before it refuses.
	assert!(console.overlaps(pinned), "a narrowing overlaps what it narrows; the priority is what separates them");

	// Two rules that differ on the virtio type cannot both match one function, whatever else they
	// leave unasked.
	let block = MatchRule { transport: Some(TRANSPORT_VIRTIO_PCI), virtio_type: Some(2), ..MatchRule::default() };
	assert!(!console.overlaps(block));
}

#[test]
fn the_xhci_row_is_a_class_triple_and_no_longer_a_liber_system_number() {
	// `device-type = 256` was `DEVICE_TYPE_XHCI`, a constant invented for this table, standing in
	// for the class triple the PCI scan had already resolved. A number this system made up is not a
	// standards identity, and a rule written against one cannot be checked against a specification.
	let xhci = MatchRule { transport: Some(TRANSPORT_PLAIN_PCI), pci_class: Some(0x0c), pci_subclass: Some(0x03), pci_interface: Some(0x30), ..MatchRule::default() };
	// A USB controller that is NOT xHCI - the same class and subclass, an earlier interface - is a
	// different controller, and the interface byte is what says so.
	let ehci = MatchRule { transport: Some(TRANSPORT_PLAIN_PCI), pci_class: Some(0x0c), pci_subclass: Some(0x03), pci_interface: Some(0x20), ..MatchRule::default() };
	assert!(!xhci.overlaps(ehci), "the programming interface is what distinguishes xHCI from EHCI, and the rule names it");
}

#[test]
fn a_requirement_nothing_in_the_image_produces_is_refused_when_the_registry_is_built() {
	// A driver waiting for a kind no entry declares waits FOR EVER, and at runtime that reads
	// exactly like hardware that is absent - which is the one thing a machine is allowed to look
	// like. Decidable here, over a closed set of kinds, and nowhere else.
	let root = fixture_workspace();
	let errors = |text: &str| -> String { Manifest::parse(text, &root).err().map(|error| error.to_string()).unwrap_or_default() };
	let driver = |extra: &str| -> String { format!("{}\n[[programs]]\nname = \"a_driver\"\nowner = \"tool\"\nrole = \"driver\"\nlinkage = \"static\"\nstage = \"volume\"\ndestination = \"drivers/a_driver.lsexe\"\n[programs.driver]\nlifecycle = \"controller\"\nmatch = [{{ transport = \"virtio-pci\", virtio-type = 2 }}]\n{extra}", valid_fixture()) };

	// The shape it is built for parses.
	assert_eq!(errors(&driver("provides = [{ kind = \"block\", most = 1 }]\n")), "", "a driver that publishes what it declares must validate");

	// A requirement with no producer anywhere in the image.
	let orphan = driver("requires = [\"usb-bus\"]\n");
	assert!(errors(&orphan).contains("wait for ever"), "{}", errors(&orphan));

	// A driver requiring what it publishes: a cycle of one, and the shortest kind to miss.
	let itself = driver("requires = [\"block\"]\nprovides = [{ kind = \"block\", most = 1 }]\n");
	assert!(errors(&itself).contains("waiting for itself"), "{}", errors(&itself));

	// `most = 0` declares a publication that may never happen, which is what leaving the row out
	// already says.
	let never = driver("provides = [{ kind = \"block\", most = 0 }]\n");
	assert!(errors(&never).contains("leaving the row out"), "{}", errors(&never));

	// A kind this system does not have fails to PARSE, like every other closed set here.
	let unknown = driver("provides = [{ kind = \"quantum\", most = 1 }]\n");
	assert!(!errors(&unknown).is_empty(), "an unknown provider kind must not parse");
}

#[test]
fn two_drivers_each_waiting_for_the_other_are_refused_rather_than_discovered_on_a_machine() {
	// A STRUCTURAL CYCLE ACROSS ENTRIES. Neither can ever bind, and on a machine both simply never
	// come up - which looks like two absent devices and is actually a broken image.
	let root = fixture_workspace();
	let errors = |text: &str| -> String { Manifest::parse(text, &root).err().map(|error| error.to_string()).unwrap_or_default() };
	let entry = |name: &str, kind: &str, wants: &str, virtio: u32| -> String { format!("\n[[programs]]\nname = \"{name}\"\nowner = \"tool\"\nrole = \"driver\"\nlinkage = \"static\"\nstage = \"volume\"\ndestination = \"drivers/{name}.lsexe\"\n[programs.driver]\nlifecycle = \"controller\"\nmatch = [{{ transport = \"virtio-pci\", virtio-type = {virtio} }}]\nrequires = [\"{wants}\"]\nprovides = [{{ kind = \"{kind}\", most = 1 }}]\n") };
	// a publishes block and wants usb-bus; b publishes usb-bus and wants block.
	let cycle = format!("{}{}{}", valid_fixture(), entry("a_driver", "block", "usb-bus", 2), entry("b_driver", "usb-bus", "block", 16));
	assert!(errors(&cycle).contains("structural cycle"), "{}", errors(&cycle));

	// AND THE SAME TWO WITHOUT THE CYCLE VALIDATE, so the check is refusing the cycle and not the
	// shape of the declaration.
	let fine = format!("{}{}{}", valid_fixture(), entry("a_driver", "block", "usb-bus", 2), entry("b_driver", "usb-bus", "block", 16).replace("requires = [\"block\"]\n", ""));
	assert_eq!(errors(&fine), "", "one driver requiring another's output is the ordinary case: {}", errors(&fine));
}

#[test]
fn a_heartbeat_deadline_of_zero_or_past_the_ceiling_is_refused() {
	// A DEADLINE OF ZERO IS NOT STRICT SUPERVISION, IT IS NONE: `wait_any` reads 0 as no timeout,
	// so an entry declaring it would look like the most responsive driver in the machine. And an
	// entry that may name any deadline it likes is an entry that can opt out, which is why the
	// ceiling is one shared policy constant rather than a per-entry opinion.
	let root = fixture_workspace();
	let errors = |text: &str| -> String { Manifest::parse(text, &root).err().map(|error| error.to_string()).unwrap_or_default() };
	let driver = |extra: &str| -> String { format!("{}\n[[programs]]\nname = \"a_driver\"\nowner = \"tool\"\nrole = \"driver\"\nlinkage = \"static\"\nstage = \"volume\"\ndestination = \"drivers/a_driver.lsexe\"\n[programs.driver]\nlifecycle = \"controller\"\nmatch = [{{ transport = \"virtio-pci\", virtio-type = 2 }}]\n{extra}", valid_fixture()) };

	// Absent is legitimate: a driver that stands on its channel and does nothing else is not
	// heartbeat-supervised, and saying so by leaving the key out is honest.
	assert_eq!(errors(&driver("")), "", "a driver with no declared deadline must validate");
	assert_eq!(errors(&driver("heartbeat-deadline = 1\n")), "", "the shortest legal deadline");
	assert_eq!(errors(&driver("heartbeat-deadline = 100\n")), "", "the ceiling itself is legal");

	let zero = driver("heartbeat-deadline = 0\n");
	assert!(errors(&zero).contains("would not be supervised at all"), "{}", errors(&zero));
	let past = driver("heartbeat-deadline = 101\n");
	assert!(errors(&past).contains("can opt out"), "{}", errors(&past));
}
