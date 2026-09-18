//! THE PASS GRAPH AND LIMITS GROUP: which passes run in what order, and what a scene will not admit.
//!
//! THE ORDER IS DERIVED FROM DEPENDENCIES AND NOT DECLARED. A caller that stated the order would have
//! to restate it every time a pass was added, and the first time somebody forgot, a shadow map would
//! be sampled before it was rendered - which looks like a shadow bug rather than an ordering one. So
//! the order is a function of what each pass READS and WRITES, and the scenes below are about the
//! cases where a wrong answer is still a plausible one: independent passes, a diamond, and a cycle.
//!
//! AND THE LIMITS ARE THE PROFILE'S FLOOR. A limit without a floor lets an implementation pass
//! conformance while being useless, so the numbers are checked against the registry BY NAME - not by
//! position, which is how a table and its list drift apart.

use crate::Outcome;
use crate::scene::material;
use graphics_profile::scene3d::SCENE3D_PROFILE_1_MIN_LIMITS;
use scene3d::{Blending, Drawable, Limits, MaterialKind, Node, Pass, PassGraph};

pub fn render_pass_graph() -> Outcome {
	// A FRAME IS A GRAPH OF PASSES AND NOT A LIST. What makes it a graph is the EDGES: a pass reads
	// what another wrote, and that is the only thing that orders them. A shadow map, a depth prepass
	// and a bloom chain are all the same shape - render into a target, sample it later.
	let mut graph = PassGraph::new();
	graph.add(Pass { id: 10, writes: alloc::vec![1], reads: alloc::vec![] });
	graph.add(Pass { id: 20, writes: alloc::vec![], reads: alloc::vec![1] });
	require!(graph.passes().len() == 2, "a graph holds the passes it was given");
	let order = graph.order()?;
	require!(order == alloc::vec![10, 20], "and runs the writer before the reader: {order:?}");

	// A PASS THAT READS WHAT NOTHING WRITES IS A MISTAKE WITH A NAME. The alternative is a pass that
	// samples an undefined target, which shows as a black reflection nobody can explain.
	let mut dangling = PassGraph::new();
	dangling.add(Pass { id: 1, writes: alloc::vec![], reads: alloc::vec![99] });
	require!(dangling.order().is_err(), "a pass reading a target nothing writes is refused");
	Ok(())
}

pub fn derived_pass_order() -> Outcome {
	// THE ORDER IS DERIVED, AND WHERE THE DEPENDENCIES DO NOT DECIDE IT, THE ORDER PASSES WERE ADDED
	// DOES. A frame whose pass order varies between runs is a frame that differs between runs, so two
	// independent passes are not left to whichever order a set happened to iterate in.
	let mut graph = PassGraph::new();
	graph.add(Pass { id: 5, writes: alloc::vec![1], reads: alloc::vec![] });
	graph.add(Pass { id: 6, writes: alloc::vec![2], reads: alloc::vec![] });
	require!(graph.order()? == alloc::vec![5, 6], "two independent passes run in the order they were added: {:?}", graph.order()?);

	// A DIAMOND: one writer, two readers of it, and one pass that reads both. The order has to put the
	// source first and the join last, whatever order they were ADDED in - so this one is built
	// backwards.
	let mut diamond = PassGraph::new();
	diamond.add(Pass { id: 40, writes: alloc::vec![], reads: alloc::vec![20, 30] });
	diamond.add(Pass { id: 30, writes: alloc::vec![30], reads: alloc::vec![10] });
	diamond.add(Pass { id: 20, writes: alloc::vec![20], reads: alloc::vec![10] });
	diamond.add(Pass { id: 10, writes: alloc::vec![10], reads: alloc::vec![] });
	let order = diamond.order()?;
	require!(order.first() == Some(&10), "the source of a diamond runs first, and the order was {order:?}");
	require!(order.last() == Some(&40), "and the join runs last: {order:?}");
	let position = |id: u32| order.iter().position(|held| *held == id).unwrap_or(usize::MAX);
	require!(position(20) < position(40) && position(30) < position(40), "and both middles run before the join: {order:?}");
	require!(position(10) < position(20) && position(10) < position(30), "and after the source");
	Ok(())
}

pub fn pass_cycle_refusal() -> Outcome {
	// A CYCLE IS REFUSED WITH THE PASS THAT CLOSES IT NAMED. "The graph has a cycle" is not something
	// a caller can act on in a graph of thirty passes, and a graph that ran anyway would run a pass
	// before the thing it samples exists.
	let mut graph = PassGraph::new();
	graph.add(Pass { id: 1, writes: alloc::vec![10], reads: alloc::vec![20] });
	graph.add(Pass { id: 2, writes: alloc::vec![20], reads: alloc::vec![10] });
	let outcome = graph.order();
	require!(outcome.is_err(), "two passes that read each other's output are refused");
	require!(matches!(outcome, Err(scene3d::Error::PassCycle { .. })), "and the refusal is a cycle rather than a missing target: {outcome:?}");

	// A LONGER CYCLE IS THE SAME ANSWER, which is what makes the check a topological sort rather than
	// a pairwise one: three passes round a ring is the shape a real frame gets wrong.
	let mut ring = PassGraph::new();
	ring.add(Pass { id: 1, writes: alloc::vec![10], reads: alloc::vec![30] });
	ring.add(Pass { id: 2, writes: alloc::vec![20], reads: alloc::vec![10] });
	ring.add(Pass { id: 3, writes: alloc::vec![30], reads: alloc::vec![20] });
	require!(matches!(ring.order(), Err(scene3d::Error::PassCycle { .. })), "and so is a ring of three: {:?}", ring.order());

	// AND A GRAPH THAT MERELY LOOKS LIKE ONE IS NOT REFUSED: two passes writing the same target is not
	// a cycle, and a suite that only checked "is an error returned" would not see the difference.
	let mut shared = PassGraph::new();
	shared.add(Pass { id: 1, writes: alloc::vec![10], reads: alloc::vec![] });
	shared.add(Pass { id: 2, writes: alloc::vec![10], reads: alloc::vec![] });
	shared.add(Pass { id: 3, writes: alloc::vec![], reads: alloc::vec![10] });
	require!(shared.order().is_ok(), "two passes writing one target and a third reading it is a graph: {:?}", shared.order());
	Ok(())
}

pub fn offscreen_target() -> Outcome {
	// A PASS THAT WRITES A TARGET NOTHING PRESENTS IS AN OFFSCREEN PASS, and it is what every
	// intermediate in a frame is: a shadow map, a bloom level, a picking attachment. What makes it a
	// FEATURE rather than a detail is that a pass with NO writes is the one that draws to the screen -
	// so the distinction is in the graph rather than in a flag.
	let mut graph = PassGraph::new();
	graph.add(Pass { id: 1, writes: alloc::vec![7], reads: alloc::vec![] });
	graph.add(Pass { id: 2, writes: alloc::vec![], reads: alloc::vec![7] });
	require!(!graph.passes()[0].writes.is_empty(), "an offscreen pass names the target it writes");
	require!(graph.passes()[1].writes.is_empty(), "and the pass that presents writes none");
	require!(graph.order()? == alloc::vec![1, 2], "and the offscreen one runs first: {:?}", graph.order()?);

	// A CHAIN OF THEM IS THE ORDINARY CASE - a bloom is four - and the order follows the chain without
	// anybody stating it.
	let mut chain = PassGraph::new();
	chain.add(Pass { id: 4, writes: alloc::vec![], reads: alloc::vec![3] });
	chain.add(Pass { id: 3, writes: alloc::vec![3], reads: alloc::vec![2] });
	chain.add(Pass { id: 2, writes: alloc::vec![2], reads: alloc::vec![1] });
	chain.add(Pass { id: 1, writes: alloc::vec![1], reads: alloc::vec![] });
	require!(chain.order()? == alloc::vec![1, 2, 3, 4], "a chain of offscreen passes runs in its own order: {:?}", chain.order()?);
	Ok(())
}

pub fn scene_limits() -> Outcome {
	// THE LIMITS ARE THE PROFILE'S OWN, CHECKED BY NAME. A limit without a floor lets an
	// implementation pass conformance while being useless - a scene that admits sixteen nodes conforms
	// to every rule about hierarchies and cannot hold a room - so the floor is part of the profile, and
	// the check is against the REGISTRY rather than against a copy of it.
	let limits = Limits::PROFILE_MINIMUM;
	require!(!SCENE3D_PROFILE_1_MIN_LIMITS.is_empty(), "the profile publishes its minimum limits");
	for entry in SCENE3D_PROFILE_1_MIN_LIMITS {
		let held = limits.by_name(entry.name).ok_or_else(|| crate::Trouble::Failed(alloc::format!("the layer has no limit called {}", entry.name)))?;
		require!(held >= entry.minimum, "{} is at least the profile's {} and is {held}", entry.name, entry.minimum);
	}

	// AND EVERY LIMIT THE LAYER HAS IS ONE THE PROFILE NAMES, which is the other direction: a limit
	// nobody agreed to is one an application cannot rely on.
	for name in ["max_nodes", "max_hierarchy_depth", "max_drawables", "max_instances_per_drawable", "max_lights", "max_lights_per_drawable", "max_materials", "max_cameras"] {
		require!(SCENE3D_PROFILE_1_MIN_LIMITS.iter().any(|entry| entry.name == name), "the profile names {name}");
		require!(limits.by_name(name).is_some(), "and the layer has it");
	}
	require!(limits.by_name("max_shadows").is_none(), "while a name the profile does not have is not a limit either");
	Ok(())
}

pub fn limit_refusal() -> Outcome {
	// A REQUEST PAST A LIMIT IS REFUSED AND THE REFUSAL NAMES THE LIMIT. A layer without a bound is one
	// whose worst case is a caller's loop; a layer that clamped instead would drop the last drawable
	// silently, which shows as an object that is sometimes missing.
	let mut limits = Limits::PROFILE_MINIMUM;
	limits.max_nodes = 2;
	limits.max_materials = 1;
	limits.max_cameras = 1;
	limits.max_drawables = 1;
	let mut scene = scene3d::Scene::new(limits);
	let first = scene.add_node(Node::identity())?;
	let _second = scene.add_node(Node::identity())?;
	let refused = scene.add_node(Node::identity());
	require!(matches!(refused, Err(scene3d::Error::LimitExceeded { limit: "max_nodes", .. })), "a third node under a limit of two names max_nodes: {refused:?}");

	let opaque = scene.add_material(material(MaterialKind::Lambert, Blending::Opaque))?;
	require!(matches!(scene.add_material(material(MaterialKind::Unlit, Blending::Opaque)), Err(scene3d::Error::LimitExceeded { limit: "max_materials", .. })), "and a second material names max_materials");
	let _drawable = scene.add_drawable(Drawable::new(first, 0, opaque))?;
	require!(matches!(scene.add_drawable(Drawable::new(first, 0, opaque)), Err(scene3d::Error::LimitExceeded { limit: "max_drawables", .. })), "and a second drawable names max_drawables");
	let camera = scene3d::Camera::perspective(first, 1.0, 1.0, 0.1, 100.0, u32::MAX)?;
	let _ = scene.add_camera(camera)?;
	require!(matches!(scene.add_camera(camera), Err(scene3d::Error::LimitExceeded { limit: "max_cameras", .. })), "and a second camera names max_cameras");

	// AND THE SCENE IS LEFT AS IT WAS, so a refused addition is not a half-added one.
	require!(scene.nodes().len() == 2 && scene.materials().len() == 1 && scene.drawables().len() == 1 && scene.cameras().len() == 1, "nothing refused was added: {} nodes, {} materials, {} drawables, {} cameras", scene.nodes().len(), scene.materials().len(), scene.drawables().len(), scene.cameras().len());
	Ok(())
}
