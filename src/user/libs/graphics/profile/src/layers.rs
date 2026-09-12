//! THE NAMES, because `framebuffer` currently means five things.
//!
//! IT MEANS: the UEFI boot surface in `bootproto`, the virtio-gpu resource's DMA backing, the
//! DisplayService scanout, an application's surface, and `pix::Target`. A word that means five things
//! in one tree is a word every conversation has to disambiguate and every interface eventually gets
//! wrong - and the way it gets wrong is that somebody passes one of the five where another was meant,
//! which type-checks whenever both are a pointer and a length.
//!
//! SO EACH OF THE FIVE HAS ITS OWN NAME HERE, and the name is what the rest of the tree uses.
//!
//! AND THE OWNERSHIP IS AN ENUMERATION RATHER THAN A DIAGRAM. A diagram in a document is checked by
//! whoever reads it; a list of edges is checked by a fixture, which is what stops a layer growing a
//! dependency nobody agreed to - the one failure that turns a stack into a knot.

/// One of the five things `framebuffer` used to mean, with the name it has now.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Layer {
	pub name: &'static str,
	/// What it IS, in one sentence that excludes the other four.
	pub what: &'static str,
	/// Who owns it - which is the question "whose bug is it" reduces to.
	pub owner: &'static str,
}

/// The five names, and nothing else may be called a framebuffer.
pub const LAYERS: &[Layer] = &[
	Layer { name: "FRAMEBUFFER", what: "the LEGACY LINEAR BOOT SURFACE and nothing else: what firmware hands over, described by `bootproto::Framebuffer` as a base, a pitch and six shift/size fields", owner: "the loader, until the display driver takes the device" },
	Layer { name: "SCANOUT", what: "what the display controller is SHOWING. One per connected output, and its format is the controller's rather than an application's choice", owner: "the display driver" },
	Layer { name: "IMAGE", what: "any 2D region of pixels with a layout and a meaning. A glyph mask, a decoded photograph, a filter intermediate and a depth buffer are all images", owner: "whoever allocated it" },
	Layer { name: "SURFACE", what: "a PRESENTABLE application object: an image plus the presentation state that makes it something a compositor can show", owner: "the application, with DisplayService holding the presentation half" },
	Layer { name: "RENDER TARGET", what: "an image a renderer is drawing INTO. A surface's image becomes one while a frame is being drawn and stops being one when it is presented", owner: "the renderer, for the duration of a frame" },
];

/// One edge of the ownership graph: who sits on whom.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Edge {
	pub above: &'static str,
	pub below: &'static str,
	/// What crosses it. An edge with no stated payload is a dependency nobody has thought about.
	pub carries: &'static str,
}

/// THE OWNERSHIP GRAPH, frozen. Nothing below depends on anything above it.
pub const OWNERSHIP: &[Edge] = &[
	Edge { above: "application", below: "render2d", carries: "a `DrawList`: paths, paints, clips, layers, filters and glyph runs" },
	Edge { above: "application", below: "scene3d", carries: "a retained hierarchy: cameras, lights, materials, culling and animation" },
	Edge { above: "scene3d", below: "render3d", carries: "a pass graph and the resources it names" },
	Edge { above: "render2d", below: "soft2d", carries: "the backend interface: `prepare` then `render`" },
	Edge { above: "render3d", below: "soft3d", carries: "the same, for three dimensions" },
	Edge { above: "soft2d", below: "graphics-core", carries: "images, colour, sampling and compositing" },
	Edge { above: "soft3d", below: "graphics-core", carries: "the same" },
	Edge { above: "graphics-core", below: "surface", carries: "a presentable image and its present queue" },
	Edge { above: "surface", below: "DisplayService", carries: "presentation: what is shown, when, and with what damage" },
	Edge { above: "DisplayService", below: "display driver", carries: "the device transport: scanout configuration and the pages it reads" },
];

/// A route by which somebody else's graphics API reaches this stack.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Route {
	pub api: &'static str,
	/// How it gets here, named rather than left open.
	pub through: &'static str,
	/// What this tree does NOT do for it, because the absence is the decision.
	pub refused: &'static str,
}

/// THE FUTURE INTEGRATION ROUTES, frozen now so that a demo cannot introduce a provisional one.
///
/// A PROVISIONAL `gl*` OR `vk*` API INTRODUCED TO DRAW A DEMO IS THE API THE TREE THEN HAS. Naming
/// the routes before any of them exists is what makes "we will do it properly later" a plan rather
/// than a hope.
pub const ROUTES: &[Route] = &[
	Route { api: "OpenGL and OpenGL ES", through: "EGL, onto a Mesa state tracker over this stack's own surfaces", refused: "a hand-written GL front end in this tree: a second implementation of a thirty-year-old specification is not something this project can keep correct" },
	Route { api: "Vulkan", through: "the Khronos loader and an ICD, with Venus the likely transport to a host", refused: "a common GL/Vulkan command language invented here, which would be a third API that neither upstream tests" },
	Route { api: "presentation", through: "DisplayService, which owns the WSI and the present queue", refused: "an application reaching a scanout directly" },
	Route { api: "the device", through: "the display driver, which owns the transport", refused: "virtqueue descriptors exposed to applications, at any layer" },
];

/// A boundary that validates untrusted input, and what it checks there.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Boundary {
	pub at: &'static str,
	/// What arrives untrusted.
	pub untrusted: &'static str,
	/// What is checked, and it is checked HERE rather than deeper.
	pub checks: &'static str,
}

/// WHERE EVERY UNTRUSTED LENGTH, OPCODE AND RESOURCE REFERENCE IS VALIDATED.
///
/// THE POINT OF NAMING THE BOUNDARY IS THAT THE LAYER BELOW MAY THEN ASSUME. A stack where every
/// layer re-checks is a stack where the check that matters is the one nobody wrote because everybody
/// assumed somebody else had; a stack where none does is the other failure. Each row below is a place
/// where the check happens and a statement that it does not happen again deeper.
pub const BOUNDARIES: &[Boundary] = &[
	Boundary { at: "the loader, over firmware's hand-off", untrusted: "the framebuffer base, pitch, and the six shift and size fields of a `PIXEL_BIT_MASK` mode", checks: "contiguous, disjoint channel masks whose ELEMENT SIZE comes from the masks rather than from an assumed thirty-two bits" },
	Boundary { at: "the image view constructor", untrusted: "a caller's extent, pitch, format and byte slice", checks: "a known format, a non-zero extent, a pitch at least the minimum row, and a slice at least the minimum visible bytes - all in checked arithmetic" },
	Boundary { at: "`render2d`'s draw-list builder", untrusted: "an application's paths, paints, clips, filter graphs and resource indices", checks: "every resource index against the list's own table, every count against the profile's ceilings, and every recursive structure against its depth bound" },
	Boundary { at: "the backend's `prepare`", untrusted: "a draw list that was built elsewhere and may have crossed a process boundary", checks: "the resource table again, because a list that arrived over a channel is not the list this process built - and the scratch it will need, which is refused up front rather than halfway through a filter chain" },
	Boundary { at: "DisplayService, over a client's surface", untrusted: "a surface configuration, a damage region and a presented image's identity", checks: "the configuration against the output's own modes, damage against the surface extent, and the image against what the client actually owns" },
	Boundary { at: "the display driver, over a device's replies", untrusted: "everything the device writes back, including lengths and resource identifiers it echoes", checks: "every length against the buffer it was given and every identifier against what this driver issued; a device is not trusted merely because it is a device" },
];

/// A layer by name, for a report and a gate.
pub fn layer(name: &str) -> Option<&'static Layer> {
	LAYERS.iter().find(|entry| entry.name == name)
}
