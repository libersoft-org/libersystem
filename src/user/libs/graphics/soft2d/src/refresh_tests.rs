use super::*;
use graphics_core::geom::PixelRect;
use graphics_core::pixel::Rgba;
use render2d::list::ImageRecord;
use render2d::paint::ImageQuality;
use std::cell::Cell;

struct Observed<'a> {
	pool: &'a dyn Workers,
	calls: Cell<usize>,
}
impl Workers for Observed<'_> {
	fn lanes(&self) -> usize {
		self.pool.lanes()
	}
	fn run<'u>(&self, lanes: &mut [Lane], units: &mut [Unit<'u>], work: &(dyn Fn(&mut Lane, &mut Unit<'u>) + Sync)) {
		self.calls.set(self.calls.get() + 1);
		assert!(units.len() <= lanes.len(), "refresh uses at most the admitted lanes");
		self.pool.run(lanes, units, work);
	}
}
/// Use the actual rt::pool worker budget; the general test helper uses half of it.
struct RefreshThreads;
impl Workers for RefreshThreads {
	fn lanes(&self) -> usize {
		4
	}
	fn run<'u>(&self, lanes: &mut [Lane], units: &mut [Unit<'u>], work: &(dyn Fn(&mut Lane, &mut Unit<'u>) + Sync)) {
		let queue = std::sync::Mutex::new(units.iter_mut());
		std::thread::scope(|scope| {
			for lane in lanes.iter_mut() {
				let queue = &queue;
				std::thread::Builder::new()
					.stack_size(256 * 1024)
					.spawn_scoped(scope, move || {
						loop {
							let Some(unit) = queue.lock().unwrap().next() else { break };
							work(lane, unit);
						}
					})
					.unwrap();
			}
		});
	}
}
fn source(alpha: bool) -> OneImage {
	let mut result = OneImage { identity: 7, image: target(37, 2) };
	fill_source(&mut result, alpha);
	result
}
fn fill_source(source: &mut OneImage, alpha: bool) {
	for y in 0..2 {
		for x in 0..37 {
			graphics_core::pixel::write(&mut source.image.view_mut(), x, y, Rgba::new(x as f32 / 37.0, y as f32 / 2.0, 0.3, if alpha { (x % 3) as f32 / 2.0 } else { 1.0 }));
		}
	}
}
const RECORD: ImageRecord = ImageRecord { identity: 7, layout_generation: 1, content_generation: 1 };
fn list(quality: ImageQuality, destination: RectF) -> DrawList {
	let mut canvas = Canvas::new();
	canvas.draw_image(RECORD, RectF::new(0.0, 0.0, 37.0, 2.0), destination, quality).unwrap();
	canvas.finish().unwrap()
}

#[test]
fn parallel_refresh_matches_serial_with_changed_alpha_and_keeps_all_storage() {
	for quality in [ImageQuality::Nearest, ImageQuality::Bilinear, ImageQuality::Bicubic, ImageQuality::Mipmapped] {
		for pool in [&Rotating(4) as &dyn Workers, &Backwards, &Twice, &RefreshThreads] {
			let mut source = source(false);
			let list = list(quality, RectF::new(0.0, 0.0, 193.0, 7.0));
			let mut output = target(193, 7);
			let mut backend = Soft2d::new().with_images(&source).with_workers(pool);
			let mut prepared = backend.prepare(&list, &description(&output)).unwrap();
			assert!(prepared.units() >= prepared.lanes());
			if quality == ImageQuality::Nearest {
				assert!(prepared.tiles_without_backdrop() > 0);
			}
			let mut backend = backend.unbind();
			fill_source(&mut source, true);
			let observed = Observed { pool, calls: Cell::new(0) };
			backend = backend.with_images(&source).with_workers(&observed);
			backend.refresh_images(&mut prepared, &[ImageRecord { content_generation: 2, ..RECORD }]).unwrap();
			assert_eq!(observed.calls.get(), 1, "actual cached rows must be dispatched, {quality:?}");
			assert_eq!(prepared.tiles_without_backdrop(), 0);
			let mut backend = backend.unbind().with_images(&source).with_workers(&Rotating(4));
			for generation in 3..5 {
				let before = crate::counted::count();
				backend.refresh_images(&mut prepared, &[ImageRecord { content_generation: generation, ..RECORD }]).unwrap();
				assert_eq!(crate::counted::count(), before, "all admitted lanes refresh without allocation");
			}
			backend.render(&prepared, &mut output.view_mut()).unwrap();
			let mut reference = target(193, 7);
			let mut serial = Soft2d::new().with_images(&source);
			let rebuilt = serial.prepare(&list, &description(&reference)).unwrap();
			serial.render(&rebuilt, &mut reference.view_mut()).unwrap();
			assert_eq!(output.bytes(), reference.bytes(), "{quality:?}: exact serial shader rows");
		}
	}
}

#[test]
fn refused_or_cancelled_refresh_keeps_backdrop_and_retries_the_same_generation() {
	for cancel in [false, true] {
		let mut source = source(false);
		let list = list(ImageQuality::Nearest, RectF::new(0.0, 0.0, 256.0, 192.0));
		let mut output = target(256, 192);
		let mut backend = Soft2d::new().with_images(&source).with_workers(&Rotating(4));
		let mut prepared = backend.prepare(&list, &description(&output)).unwrap();
		assert!(prepared.tiles_without_backdrop() > 0);
		let backend = backend.unbind();
		// All changed rows become transparent; a partially refreshed row must retain the live backdrop.
		source.image.bytes_mut().fill(0);
		let stop = StopAfterUnits { left: core::sync::atomic::AtomicU32::new(5) };
		let mut backend = backend.with_images(&source).with_workers(if cancel { &Rotating(4) } else { &Short });
		if cancel {
			backend = backend.with_cancellation(&stop);
		}
		let next = [ImageRecord { content_generation: 2, ..RECORD }];
		let before = crate::counted::count();
		assert_eq!(backend.refresh_images(&mut prepared, &next), Err(if cancel { render2d::Error::Cancelled } else { render2d::Error::IncompletePool }));
		assert_eq!(crate::counted::count(), before);
		assert_eq!(prepared.tiles_without_backdrop(), 0);
		let mut backend = backend.unbind().with_images(&source).with_workers(&Rotating(4));
		output.bytes_mut().fill(90);
		let mut second = target(256, 192);
		second.bytes_mut().fill(180);
		backend.render(&prepared, &mut output.view_mut()).unwrap();
		backend.render(&prepared, &mut second.view_mut()).unwrap();
		assert_ne!(pixel(&output, 0, 0), pixel(&second, 0, 0), "changed transparent rows still read the backdrop after refusal");
		let before = crate::counted::count();
		backend.refresh_images(&mut prepared, &next).unwrap();
		assert_eq!(crate::counted::count(), before, "the failed refresh returns its unit table");
		// Same generation is deliberately retried: committing it before successful completion
		// would leave the old opaque pixels in later bands and fail this comparison.
		output.bytes_mut().fill(90);
		let mut reference = copy_of(&output);
		backend.render(&prepared, &mut output.view_mut()).unwrap();
		let mut serial = Soft2d::new().with_images(&source);
		let rebuilt = serial.prepare(&list, &description(&reference)).unwrap();
		serial.render(&rebuilt, &mut reference.view_mut()).unwrap();
		assert_eq!(output.bytes(), reference.bytes());
	}
}

#[test]
fn refresh_keeps_identity_layout_and_empty_or_unrelated_backend_compatibility() {
	let source = source(false);
	let list = list(ImageQuality::Nearest, RectF::new(0.0, 0.0, 128.0, 128.0));
	let mut output = target(128, 128);
	let mut owner = Soft2d::new().with_images(&source).with_workers(&Rotating(4));
	let mut prepared = owner.prepare(&list, &description(&output)).unwrap();
	// An unrelated backend has no admitted lanes or table, as supported by the old refresh path.
	let mut other = Soft2d::new().with_images(&source);
	let before = crate::counted::count();
	other.refresh_images(&mut prepared, &[ImageRecord { content_generation: 2, ..RECORD }]).unwrap();
	assert_eq!(crate::counted::count(), before);
	for record in [ImageRecord { identity: 8, ..RECORD }, ImageRecord { layout_generation: 2, ..RECORD }] {
		assert!(other.refresh_images(&mut prepared, &[record]).is_err());
	}
	owner.render(&prepared, &mut output.view_mut()).unwrap();
	let empty = Canvas::new().finish().unwrap();
	let mut prepared = other.prepare(&empty, &description(&output)).unwrap();
	assert_eq!(prepared.units(), 0);
	other.refresh_images(&mut prepared, &[]).unwrap();
	let off_target = self::list(ImageQuality::Nearest, RectF::new(500.0, 500.0, 37.0, 2.0));
	let mut prepared = other.prepare(&off_target, &description(&output)).unwrap();
	assert_eq!(prepared.units(), 0);
	other.refresh_images(&mut prepared, &[ImageRecord { content_generation: 2, ..RECORD }]).unwrap();
}

#[test]
fn refresh_unit_keeps_the_previous_inline_table_layout() {
	// Mirror only the previous storage layout, not any rendering behavior. The large inline
	// rectangle must continue to bound Unit after adding its smaller cached-row alternative.
	#[allow(dead_code, clippy::large_enum_variant)]
	enum PreviousAccess<'u> {
		Band { view: graphics_core::ImageViewMut<'u>, top: u32 },
		Slot { source: &'u graphics_core::ImageView<'u>, slot: &'u mut [u8], tile: PixelRect, pitch: usize },
		Rect { rows: [&'u mut [u8]; crate::TILE_SIZE as usize], tile: PixelRect, layout: ImageLayout },
	}
	#[allow(dead_code)]
	struct PreviousUnit<'u> {
		index: usize,
		first_tile: u32,
		tiles: u32,
		access: PreviousAccess<'u>,
		ran: bool,
		whole: bool,
	}
	assert_eq!(core::mem::size_of::<Unit<'_>>(), core::mem::size_of::<PreviousUnit<'_>>());
	assert_eq!(core::mem::align_of::<Unit<'_>>(), core::mem::align_of::<PreviousUnit<'_>>());
	println!("Unit before/after={} bytes alignment={}", core::mem::size_of::<Unit<'_>>(), core::mem::align_of::<Unit<'_>>());
}
