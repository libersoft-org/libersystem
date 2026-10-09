//! Execute Scene3D's HDR pass graph through Render3D commands and the software backend.
//! Targets and command plans are prepared up front; first execution reserves reusable scratch.
//! Subsequent frames allocate nothing here.

use alloc::{vec, vec::Vec};
use graphics_core::geom::Extent2D;
use graphics_core::layout::{ImageLayout, RowOrigin};
use graphics_core::pixel::{Rgba, Working};
use graphics_core::sample::{Quality, Sampler, Spread};
use graphics_core::semantics::ImageSemantics;
use graphics_core::{AlphaMode, ColorSpace, OwnedImage, PixelFormat, PixelStorage};
use render_math::Viewport;
use render_shader::Module;
use render3d::command::{CommandList, GraphicsPipeline, PipelineState, Rect};
use render3d::resource::{Aspect, LoadOp, RenderTargetSet, RenderTargetView, StoreOp, TextureDimension, TextureViewDesc};
use render3d::{CompareOp, Cull, Error, Render3DLimits, Topology};
use scene3d::postprocess::{self, shaders};
use soft3d::frame::{Attachments, Pipeline, Prepared, Source, Workers};
use soft3d::pass::Colour;
use soft3d::{Indices, Val};

struct Pass {
	plan: Prepared,
	inputs: (usize, Option<usize>),
	output: Option<usize>,
	colour: [Colour; 1],
	comparison: Option<[Colour; 1]>,
}

pub struct Postprocess {
	images: Vec<OwnedImage>,
	passes: Vec<Pass>,
}

impl Postprocess {
	pub fn new(width: u32, height: u32, compare: bool) -> Option<Self> {
		let levels = postprocess::bloom_pyramid_desc(width, height).ok()?;
		let graph = postprocess::chain(&postprocess::Chain { scene: 0, pyramid: &[1, 2, 3, 4, 5, 6], ascent: &[7, 8, 9, 10, 11] }, 0).ok()?;
		let mut images = Vec::new();
		images.push(image(width, height)?);
		for desc in &levels {
			images.push(image(desc.width, desc.height)?);
		}
		for desc in &levels[..5] {
			images.push(image(desc.width, desc.height)?);
		}
		let mut passes = Vec::new();
		for id in graph.graph.order().ok()? {
			if id == graph.scene {
				continue;
			}
			let pass = graph.graph.passes().iter().find(|pass| pass.id == id)?;
			let first = pass.reads[0] as usize;
			let second = pass.reads.get(1).map(|id| *id as usize);
			let output = pass.writes.first().map(|id| *id as usize);
			let extent = output.map(|id| images[id].layout().extent).unwrap_or(Extent2D::new(width, height));
			let source = images[first].layout().extent;
			let fragment = if graph.downsample.contains(&id) {
				shaders::downsample(source.width, source.height, first == 0)
			} else if graph.upsample.contains(&id) {
				shaders::upsample(source.width, source.height)
			} else {
				shaders::resolve()
			};
			passes.push(Pass { plan: prepare(fragment, extent)?, inputs: (first, second), output, colour: [Colour::try_new(extent.width, extent.height, 1, false).ok()?], comparison: if compare { Some([Colour::try_new(extent.width, extent.height, 1, false).ok()?]) } else { None } });
		}
		Some(Self { images, passes })
	}

	pub fn bytes(&self) -> u64 {
		self.images.iter().map(|image| image.bytes().len() as u64).sum::<u64>() + self.passes.iter().map(|pass| pass.plan.reserved_bytes() as u64 + pass.colour[0].reserved_bytes() as u64 + pass.comparison.as_ref().map_or(0, |colour| colour[0].reserved_bytes() as u64)).sum::<u64>()
	}

	/// Returns the number of worker/serial comparisons and differences, without allocating.
	pub fn execute(&mut self, scene: &mut Colour, workers: &dyn Workers) -> Result<(u32, u32), Error> {
		copy_to_half(scene, &mut self.images[0]);
		let mut compared = 0;
		let mut differed = 0;
		for pass in &mut self.passes {
			let source = Inputs::new(&self.images[pass.inputs.0], pass.inputs.1.map(|id| &self.images[id]));
			let viewport = Viewport { x: 0.0, y: 0.0, width: pass.colour[0].width as f32, height: pass.colour[0].height as f32, min_depth: 0.0, max_depth: 1.0 };
			let stats = soft3d::frame::execute_with(&mut pass.plan, &mut Attachments { colour: &mut pass.colour, depth_stencil: None, viewport, scissor: None }, &source, workers)?;
			if let Some(comparison) = &mut pass.comparison {
				let serial = soft3d::frame::execute(&mut pass.plan, &mut Attachments { colour: comparison, depth_stencil: None, viewport, scissor: None }, &source)?;
				compared += 1;
				if serial != stats || !same(&pass.colour[0], &comparison[0]) {
					differed += 1;
				}
			}
			if let Some(output) = pass.output {
				copy_to_half(&pass.colour[0], &mut self.images[output]);
			} else {
				for y in 0..scene.height {
					for x in 0..scene.width {
						scene.set(x, y, 0, pass.colour[0].at(x, y, 0));
					}
				}
			}
		}
		Ok((compared, differed))
	}
}

fn same(a: &Colour, b: &Colour) -> bool {
	(0..a.height).all(|y| (0..a.width).all(|x| a.at(x, y, 0) == b.at(x, y, 0)))
}

fn image(width: u32, height: u32) -> Option<OwnedImage> {
	let semantics = ImageSemantics::Color { color_space: ColorSpace::Srgb.linear_counterpart(), alpha_mode: AlphaMode::Premultiplied };
	let layout = ImageLayout::new(Extent2D::new(width, height), width.checked_mul(8)?, PixelStorage::Known(PixelFormat::R16G16B16A16Float), RowOrigin::TopLeft, semantics).ok()?;
	OwnedImage::new(layout).ok()
}

fn copy_to_half(colour: &Colour, image: &mut OwnedImage) {
	let mut view = image.view_mut();
	for y in 0..colour.height {
		for x in 0..colour.width {
			let value = colour.at(x, y, 0);
			graphics_core::pixel::write(&mut view, x, y, Rgba::new(value.x, value.y, value.z, value.w));
		}
	}
}

struct Inputs<'a> {
	first: Sampler<'a>,
	second: Option<Sampler<'a>>,
	first_extent: Extent2D,
	second_extent: Extent2D,
}
impl<'a> Inputs<'a> {
	fn new(first: &'a OwnedImage, second: Option<&'a OwnedImage>) -> Self {
		let sampler = |image: &'a OwnedImage| Sampler::new(image.view(), Working::linear(ColorSpace::Srgb), Spread::Clamp).expect("prepared linear half image");
		Self { first: sampler(first), second: second.map(sampler), first_extent: first.layout().extent, second_extent: second.map(|image| image.layout().extent).unwrap_or(first.layout().extent) }
	}
}
impl Source for Inputs<'_> {
	fn attribute(&self, location: u32, vertex: u32, _instance: u32) -> Option<Val> {
		let (position, uv) = match vertex {
			0 => ([-1.0, 1.0, 0.0, 1.0], [0.0, 0.0]),
			1 => ([3.0, 1.0, 0.0, 1.0], [2.0, 0.0]),
			2 => ([-1.0, -3.0, 0.0, 1.0], [0.0, 2.0]),
			_ => return None,
		};
		match location {
			0 => Some(Val::vector_f32(&position)),
			1 => Some(Val::vector_f32(&uv)),
			_ => None,
		}
	}
	fn uniform(&self, _block: u32, _member: u32) -> Option<Val> {
		None
	}
	fn sample(&self, texture: u32, _sampler: u32, coordinate: &Val) -> Option<Val> {
		let (sampler, extent) = match texture {
			0 => (&self.first, self.first_extent),
			1 => (self.second.as_ref()?, self.second_extent),
			_ => return None,
		};
		let value = sampler.sample(coordinate.f32_at(0) * extent.width as f32, coordinate.f32_at(1) * extent.height as f32, Quality::Bilinear);
		Some(Val::vector_f32(&[value.red, value.green, value.blue, value.alpha]))
	}
	fn indices(&self) -> Indices<'_> {
		Indices::None
	}
}

fn prepare(fragment: Module, extent: Extent2D) -> Option<Prepared> {
	let state = PipelineState { topology: Topology::TriangleList, cull: Cull::None, depth_test: None, depth_write: false, samples: 1, per_sample_shading: false };
	let pipeline = Pipeline { state, vertex: shaders::vertex(), fragment, blend: vec![render3d::AttachmentBlend { enabled: false, colour: render3d::BlendEquation::REPLACE, alpha: render3d::BlendEquation::REPLACE, write_mask: render3d::ColorWriteMask::ALL }], stencil: None, stencil_back: None, depth_compare: CompareOp::Always, depth_write: false, bias: (0.0, 0.0, 0.0), alpha_to_coverage: false, sample_mask: u32::MAX };
	let colour = [RenderTargetView { texture: 0, view: TextureViewDesc { dimension: TextureDimension::D2, aspect: Aspect::Colour, base_mip: 0, mip_count: 1, base_layer: 0, layer_count: 1 }, format: "R16G16B16A16_FLOAT", samples: 1, width: extent.width, height: extent.height, load: LoadOp::Discard, store: StoreOp::Store }];
	let mut commands = CommandList::new(Render3DLimits::PROFILE_MINIMUM);
	commands.begin_render_pass(0, &RenderTargetSet { colour: &colour, depth_stencil: None, resolve: &[] }).ok()?;
	commands.bind_pipeline(GraphicsPipeline(0), &state, 1).ok()?;
	commands.bind_vertex_buffer(0, render3d::command::Buffer(0), 0).ok()?;
	commands.bind_resources(0, 2).ok()?;
	commands.set_viewport(Rect { x: 0, y: 0, width: extent.width, height: extent.height }).ok()?;
	commands.draw(Topology::TriangleList, 3, 1, 0, 0).ok()?;
	commands.end_render_pass().ok()?;
	commands.finish().ok()?;
	let draws = soft3d::frame::draws_from(commands.commands(), Topology::TriangleList);
	soft3d::frame::prepare(Render3DLimits::PROFILE_MINIMUM, vec![pipeline], draws, extent.width, extent.height).ok()
}

#[cfg(test)]
#[path = "../../../../../tools/soft3d-bench/src/postprocess_tests.rs"]
mod tests;
