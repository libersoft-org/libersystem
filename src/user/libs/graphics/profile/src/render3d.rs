//! `Render3D Core Profile 1`: the closed list, on the same terms as the 2D one.
//!
//! A conforming backend implements every entry, and `Unsupported` is reserved for post-profile
//! extensions. Written before `render3d` and `soft3d` exist on purpose: the generated checklist is
//! what an implementer works through, and a checklist produced after the implementation is a
//! description of what was built rather than a statement of what was required.
//!
//! WHAT IS DELIBERATELY NOT HERE. The shader model (`render-shader`) is a profile of its own with
//! its own types, operations, control flow and built-ins; enumerating its instruction set as
//! `Render3DFeature` variants would put two different kinds of thing - "a backend can fail to do
//! this" and "the IR can express this" - into one closed list and one conformance matrix. It is
//! named here only by the two stages a pipeline has.

use crate::{FeatureOwner, ProfileEntry};

/// One feature of `Render3D Core Profile 1`.
///
/// THE GRANULARITY IS "SOMETHING A BACKEND CAN FAIL TO DO", as in 2D: the depth formats are
/// separate variants because an implementation can carry `Depth32F` and get `Depth24Stencil8`
/// wrong, and a matrix that could only say "depth" would call that conforming.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Render3DFeature {
	// Resources.
	Buffer,
	Texture2D,
	TextureCube,
	Texture2DArray,
	Texture3D,
	Sampler,
	Mipmaps,
	RenderTarget,
	// Geometry.
	IndexU16,
	IndexU32,
	MultipleVertexStreams,
	ConfigurableAttributes,
	TriangleList,
	TriangleStrip,
	TriangleFan,
	LineList,
	LineStrip,
	PointList,
	PrimitiveRestart,
	IndexedDraw,
	NonIndexedDraw,
	Instancing,
	BaseVertex,
	BaseInstance,
	// Pipeline.
	ProgrammableVertexStage,
	ProgrammableFragmentStage,
	VertexLayout,
	PrimitiveTopology,
	RasteriserState,
	DepthStencilState,
	BlendStatePerAttachment,
	ColorWriteMask,
	Viewport,
	Scissor,
	// Passes. Load and store are per attachment, which is why each option is its own entry: a
	// backend that honours `Clear` and ignores `Discard` has a working clear and a broken pass.
	LoadClear,
	LoadLoad,
	LoadDiscard,
	StoreStore,
	StoreDiscard,
	MultipleColorAttachments,
	DepthStencilAttachment,
	OffscreenRenderTarget,
	RenderToTexture,
	// Depth and stencil.
	Depth16,
	Depth24,
	Depth32F,
	Depth24Stencil8,
	Depth32FStencil8,
	DepthCompareOps,
	DepthWrite,
	DepthBias,
	StencilCompare,
	StencilReadMask,
	StencilWriteMask,
	StencilFailOp,
	StencilDepthFailOp,
	StencilPassOp,
	StencilSeparateFrontBack,
	// Multisampling. `MsaaResolve` is enumerated here rather than under passes, where the plain
	// sentence also mentions it: one feature, one entry, or a conformance matrix reports it twice.
	Msaa1x,
	Msaa2x,
	Msaa4x,
	SampleMask,
	AlphaToCoverage,
	MsaaResolve,
	// Sampling.
	MinNearest,
	MinLinear,
	MagNearest,
	MagLinear,
	MipNearest,
	MipLinear,
	Trilinear,
	SamplerWrapClamp,
	SamplerWrapRepeat,
	SamplerWrapMirror,
	SamplerWrapBorder,
	LodBias,
	LodClamp,
	DepthCompareSampling,
	Anisotropy8,
	// Colour formats. The depth formats are enumerated above with what they are used for; the sRGB
	// encoding is `ColorSpace`'s and is never a second storage format.
	FormatR8,
	FormatRG8,
	FormatRGBA8,
	FormatRGB10A2,
	FormatRGBA16F,
	FormatRGBA32F,
	FormatR32Uint,
	// Readback.
	ReadbackColor,
	ReadbackDepth,
	ReadbackObjectId,
}

profile! {
	/// `Render3D Core Profile 1`, closed and enumerated.
	RENDER3D_CORE_PROFILE_1: Render3DFeature;
	"resources", Backend, Buffer;
	"resources", Backend, Texture2D;
	"resources", Backend, TextureCube;
	"resources", Backend, Texture2DArray;
	"resources", Backend, Texture3D;
	"resources", Backend, Sampler;
	"resources", Backend, Mipmaps;
	"resources", Backend, RenderTarget;
	"geometry", Backend, IndexU16;
	"geometry", Backend, IndexU32;
	"geometry", Backend, MultipleVertexStreams;
	"geometry", Backend, ConfigurableAttributes;
	"geometry", Backend, TriangleList;
	"geometry", Backend, TriangleStrip;
	"geometry", Backend, TriangleFan;
	"geometry", Backend, LineList;
	"geometry", Backend, LineStrip;
	"geometry", Backend, PointList;
	"geometry", Backend, PrimitiveRestart;
	"geometry", Backend, IndexedDraw;
	"geometry", Backend, NonIndexedDraw;
	"geometry", Backend, Instancing;
	"geometry", Backend, BaseVertex;
	"geometry", Backend, BaseInstance;
	"pipeline", Backend, ProgrammableVertexStage;
	"pipeline", Backend, ProgrammableFragmentStage;
	"pipeline", Render3D, VertexLayout;
	"pipeline", Render3D, PrimitiveTopology;
	"pipeline", Backend, RasteriserState;
	"pipeline", Backend, DepthStencilState;
	"pipeline", Backend, BlendStatePerAttachment;
	"pipeline", Backend, ColorWriteMask;
	"pipeline", Backend, Viewport;
	"pipeline", Backend, Scissor;
	"passes", Backend, LoadClear;
	"passes", Backend, LoadLoad;
	"passes", Backend, LoadDiscard;
	"passes", Backend, StoreStore;
	"passes", Backend, StoreDiscard;
	"passes", Backend, MultipleColorAttachments;
	"passes", Backend, DepthStencilAttachment;
	"passes", Backend, OffscreenRenderTarget;
	"passes", Backend, RenderToTexture;
	"depth", Backend, Depth16;
	"depth", Backend, Depth24;
	"depth", Backend, Depth32F;
	"depth", Backend, Depth24Stencil8;
	"depth", Backend, Depth32FStencil8;
	"depth", Backend, DepthCompareOps;
	"depth", Backend, DepthWrite;
	"depth", Backend, DepthBias;
	"depth", Backend, StencilCompare;
	"depth", Backend, StencilReadMask;
	"depth", Backend, StencilWriteMask;
	"depth", Backend, StencilFailOp;
	"depth", Backend, StencilDepthFailOp;
	"depth", Backend, StencilPassOp;
	"depth", Backend, StencilSeparateFrontBack;
	"msaa", Backend, Msaa1x;
	"msaa", Backend, Msaa2x;
	"msaa", Backend, Msaa4x;
	"msaa", Backend, SampleMask;
	"msaa", Backend, AlphaToCoverage;
	"msaa", Backend, MsaaResolve;
	"sampling", Backend, MinNearest;
	"sampling", Backend, MinLinear;
	"sampling", Backend, MagNearest;
	"sampling", Backend, MagLinear;
	"sampling", Backend, MipNearest;
	"sampling", Backend, MipLinear;
	"sampling", Backend, Trilinear;
	"sampling", Backend, SamplerWrapClamp;
	"sampling", Backend, SamplerWrapRepeat;
	"sampling", Backend, SamplerWrapMirror;
	"sampling", Backend, SamplerWrapBorder;
	"sampling", Backend, LodBias;
	"sampling", Backend, LodClamp;
	"sampling", Backend, DepthCompareSampling;
	"sampling", Backend, Anisotropy8;
	"formats", GraphicsCore, FormatR8;
	"formats", GraphicsCore, FormatRG8;
	"formats", GraphicsCore, FormatRGBA8;
	"formats", GraphicsCore, FormatRGB10A2;
	"formats", GraphicsCore, FormatRGBA16F;
	"formats", GraphicsCore, FormatRGBA32F;
	"formats", GraphicsCore, FormatR32Uint;
	"readback", Backend, ReadbackColor;
	"readback", Backend, ReadbackDepth;
	"readback", Backend, ReadbackObjectId;
}

/// The groups, in the order the profile documents them.
pub const RENDER3D_GROUPS: &[&str] = &["resources", "geometry", "pipeline", "passes", "depth", "msaa", "sampling", "formats", "readback"];

/// Is this feature in Profile 1?
pub fn in_profile_1(feature: Render3DFeature) -> bool {
	RENDER3D_CORE_PROFILE_1.iter().any(|entry| entry.feature == feature)
}

/// The entry for a feature named as the profile spells it.
pub fn entry_by_name(name: &str) -> Option<&'static ProfileEntry<Render3DFeature>> {
	RENDER3D_CORE_PROFILE_1.iter().find(|entry| entry.name == name)
}
