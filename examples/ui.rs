//! A user interface written in Mollie (`examples/ui.mol`), drawn by the host:
//! the script builds a tree of views implementing the host's `Drawable`
//! trait, which the host measures and renders into `output.png`.
//!
//! ```sh
//! cargo run --example ui            # renders output.png
//! cargo run --example ui run stress # runs the script 200k times
//! cargo run --example ui dump       # lists compiled functions
//! ```
//!
//! Everything goes through the safe host API (`mollie::host`).

use core::fmt;
use std::{
    collections::HashMap,
    path::PathBuf,
    time::{Duration, Instant, SystemTime},
};

use ariadne::{Config, Report, ReportKind, Source};
use mollie::{
    GcPtr, MolStr,
    compiler::Compiler,
    host::{CompilerExt, Host, Opaque, ScriptObject},
    host_value_type,
};
use mollie_compiler::{error::CompileError, sandbox::Limits};
use mollie_shared::pretty_fmt::FmtIteratorExt;
use mollie_typed_ast::FileModuleLoader;
use mollie_typing::ModuleId;
use tiny_skia::{FillRule, FilterQuality, Paint, PathBuilder, Pattern, Pixmap, Point, Rect, SpreadMode, Transform};
use tracing::{Level, level_filters::LevelFilter};
use tracing_subscriber::{Layer, filter::filter_fn, layer::SubscriberExt, util::SubscriberInitExt};

/// `graphics::Color`, a value type.
#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct Color {
    pub red: u8,
    pub green: u8,
    pub blue: u8,
}

host_value_type!(Color);

impl fmt::Display for Color {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "rgb({}, {}, {})", self.red, self.green, self.blue)
    }
}

/// `graphics::CornerRadius`, a value type.
#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct CornerRadius {
    pub top_left: f32,
    pub top_right: f32,
    pub bottom_left: f32,
    pub bottom_right: f32,
}

host_value_type!(CornerRadius);

/// `graphics::Size`, a value type: measuring and rendering don't allocate.
#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct Size {
    pub width: f32,
    pub height: f32,
}

host_value_type!(Size);

/// `graphics::Point`, a value type.
#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct Position {
    pub x: f32,
    pub y: f32,
}

host_value_type!(Position);

/// `graphics::Image`, an enum: tag, then the fields of the variant.
#[derive(Debug, Clone, Copy)]
#[repr(usize)]
pub enum Image {
    Path { path: MolStr },
    Url { url: MolStr },
}

/// Stands for the trait `Drawable` in `ScriptObject<DrawableTrait>`.
pub struct DrawableTrait;

#[derive(Default)]
struct ImageStorage {
    images: HashMap<String, Pixmap>,
}

impl ImageStorage {
    fn get_image(&mut self, image: GcPtr<Image>) -> &Pixmap {
        let path = match &*image {
            Image::Path { path } => path.to_string(),
            // A panic in a host function stops the script, not the host.
            Image::Url { url } => panic!("images from URLs (`{url}`) aren't supported"),
        };

        self.images
            .entry(path)
            .or_insert_with_key(|path| Pixmap::load_png(path).unwrap_or_else(|error| panic!("can't load `{path}`: {error}")))
    }
}

pub struct DrawContext {
    root: Pixmap,
    images: ImageStorage,
}

struct Path {
    builder: PathBuilder,
}

impl Path {
    fn new() -> Self {
        Self { builder: PathBuilder::new() }
    }

    fn move_to(&mut self, point: Point) {
        self.builder.move_to(point.x, point.y);
    }

    fn line_to(&mut self, point: Point) {
        self.builder.line_to(point.x, point.y);
    }

    fn quad_to(&mut self, point1: Point, point: Point) {
        self.builder.quad_to(point1.x, point1.y, point.x, point.y);
    }

    fn close(&mut self) {
        self.builder.close();
    }

    fn finish(self) -> Option<tiny_skia::Path> {
        self.builder.finish()
    }
}

impl DrawContext {
    /// Fills a rectangle with rounded corners.
    ///
    /// # Panics
    ///
    /// Panics if the coordinates aren't finite (the path has no bounds).
    pub fn draw_rect(&mut self, x: f32, y: f32, width: f32, height: f32, corner_radius: CornerRadius, color: Color) {
        tracing::info!(target: "DrawContext/draw_rect", origin = format!("{x}x{y}"), size = format!("{width}x{height}"), color = %color);

        let mut paint = Paint::default();

        paint.set_color_rgba8(color.red, color.green, color.blue, 255);

        let mut path = Path::new();

        path.move_to(Point::from_xy(x + corner_radius.top_left, y));
        path.line_to(Point::from_xy(x + width - corner_radius.top_right, y));
        path.quad_to(Point::from_xy(x + width, y), Point::from_xy(x + width, y + corner_radius.top_right));
        path.line_to(Point::from_xy(x + width, y + height - corner_radius.bottom_right));
        path.quad_to(
            Point::from_xy(x + width, y + height),
            Point::from_xy(x + width - corner_radius.bottom_right, y + height),
        );
        path.line_to(Point::from_xy(x + corner_radius.bottom_left, y + height));
        path.quad_to(Point::from_xy(x, y + height), Point::from_xy(x, y + height - corner_radius.bottom_left));
        path.line_to(Point::from_xy(x, y + corner_radius.top_left));
        path.quad_to(Point::from_xy(x, y), Point::from_xy(x + corner_radius.top_left, y));
        path.close();

        self.root
            .fill_path(&path.finish().unwrap(), &paint, FillRule::Winding, Transform::identity(), None);
    }

    /// Draws `image` scaled to the rectangle.
    ///
    /// # Panics
    ///
    /// Panics if the image is a URL, can't be loaded, or the rectangle has no
    /// area. A panic in a host function stops the script, not the host.
    #[allow(clippy::cast_precision_loss)]
    pub fn draw_image(&mut self, x: f32, y: f32, width: f32, height: f32, image: GcPtr<Image>) {
        tracing::info!(target: "DrawContext/draw_image", origin = format!("{x}x{y}"), size = format!("{width}x{height}"), image = format!("{image:?}"));

        let image = self.images.get_image(image);
        let image_width = image.width() as f32;
        let image_height = image.height() as f32;
        let paint = Paint {
            shader: Pattern::new(
                image.as_ref(),
                SpreadMode::Pad,
                FilterQuality::Nearest,
                1.0,
                Transform::from_scale(width / image_width, height / image_height).post_translate(x, y),
            ),
            ..Paint::default()
        };

        self.root
            .fill_rect(Rect::from_xywh(x, y, width, height).unwrap(), &paint, Transform::identity(), None);
    }

    #[allow(clippy::cast_precision_loss)]
    pub fn image_size(&mut self, image: GcPtr<Image>) -> Size {
        tracing::info!(target: "DrawContext/image_size", image = format!("{image:?}"));

        let image = self.images.get_image(image);
        let width = image.width() as f32;
        let height = image.height() as f32;

        Size { width, height }
    }
}

/// Registers the API of the host: functions, types, the `Drawable` trait and
/// methods of `DrawContext`.
fn register(compiler: &mut Compiler<FileModuleLoader>) {
    let mut host = Host::new(compiler);

    host.function_named("println_str", &["text"], |text: MolStr| println!("{text}"));
    host.function_named("println_bool", &["value"], |value: bool| println!("{value}"));

    host.module("system").function("timestamp", || {
        SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map_or(0, |time| usize::try_from(time.as_secs()).unwrap_or(usize::MAX))
    });

    host.module("graphics");
    host.value_type::<Color>("Color")
        .field::<u8>("red")
        .field::<u8>("green")
        .field::<u8>("blue")
        .finish();
    host.value_type::<CornerRadius>("CornerRadius")
        .field::<f32>("top_left")
        .field::<f32>("top_right")
        .field::<f32>("bottom_left")
        .field::<f32>("bottom_right")
        .finish();
    host.value_type::<Size>("Size").field::<f32>("width").field::<f32>("height").finish();
    host.value_type::<Position>("Point").field::<f32>("x").field::<f32>("y").finish();

    host.enum_::<Image>("Image")
        .variant("Path")
        .field::<MolStr>("value")
        .variant("Url")
        .field::<MolStr>("value")
        .finish();

    host.everywhere().opaque::<DrawContext>("DrawContext");
    host.methods::<Opaque<DrawContext>>()
        .method_named(
            "draw_rect",
            &["x", "y", "width", "height", "corner_radius", "color"],
            |mut context: Opaque<DrawContext>, x: f32, y: f32, width: f32, height: f32, corner_radius: CornerRadius, color: Color| {
                // SAFETY: the context outlives the script's run.
                unsafe { context.get() }.draw_rect(x, y, width, height, corner_radius, color);
            },
        )
        .method_named(
            "draw_image",
            &["x", "y", "width", "height", "image"],
            |mut context: Opaque<DrawContext>, x: f32, y: f32, width: f32, height: f32, image: GcPtr<Image>| {
                unsafe { context.get() }.draw_image(x, y, width, height, image);
            },
        )
        .method_named("image_size", &["image"], |mut context: Opaque<DrawContext>, image: GcPtr<Image>| {
            unsafe { context.get() }.image_size(image)
        })
        .finish();

    host.trait_::<DrawableTrait>("Drawable")
        .method::<(Size, Opaque<DrawContext>), Size>("measure", &["size", "draw_context"])
        .method::<(Position, Size, Opaque<DrawContext>), ()>("render", &["origin", "size", "draw_context"])
        .finish();
}

pub enum Command {
    Run {
        name: Option<String>,
    },
    Dump,
    /// Writes the stub of the host's API to `.mollie/host` in the examples,
    /// for the language server.
    Stub,
}

type Main = (Opaque<DrawContext>,);

/// Sources of modules for error reports.
struct ModuleSources {
    root: PathBuf,
    sources: HashMap<ModuleId, (String, Source<String>)>,
}

impl ModuleSources {
    fn load(&mut self, registry: &mollie_typing::DefRegistry, id: ModuleId, root_source: &str) {
        if self.sources.contains_key(&id) {
            return;
        }

        // The program's root module has no parent, paths of its submodules
        // are relative to it.
        let (name, source) = if registry.modules[id].parent.is_none() {
            (String::from("ui.mol"), root_source.to_owned())
        } else {
            let mut path = PathBuf::new();
            let mut current = id;

            while let Some(parent) = registry.modules[current].parent {
                path = PathBuf::from(&registry.modules[current].name).join(path);
                current = parent;
            }

            let path = self.root.join(path).with_extension("mol");

            (path.display().to_string(), std::fs::read_to_string(&path).unwrap_or_default())
        };

        self.sources.insert(id, (name, Source::from(source)));
    }
}

impl ariadne::Cache<ModuleId> for ModuleSources {
    type Storage = String;

    fn fetch(&mut self, id: &ModuleId) -> Result<&Source<String>, impl fmt::Debug> {
        self.sources.get(id).map(|(_, source)| source).ok_or("unknown module")
    }

    fn display<'a>(&self, id: &'a ModuleId) -> Option<impl fmt::Display + 'a> {
        self.sources.get(id).map(|(name, _)| name.clone())
    }
}

fn main() {
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::fmt::layer()
                .with_filter(LevelFilter::INFO)
                .with_filter(filter_fn(|metadata| {
                    !(metadata.target() == "cranelift_jit::backend" && metadata.level() == &Level::INFO)
                })),
        )
        .init();

    let source = include_str!("./ui.mol");
    let examples_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("examples");
    let mut args = std::env::args();

    let command = match args.nth(1).as_deref() {
        Some("dump") => Command::Dump,
        Some("stub") => Command::Stub,
        Some("run") | None => Command::Run { name: args.next() },
        Some(command) => panic!("unknown command: {command}"),
    };

    // Functions of the host are registered with `Host`, not as symbols.
    let mut compiler = Compiler::with_symbols(
        FileModuleLoader {
            current_dir: examples_dir.clone(),
        },
        [],
    )
    .unwrap_or_else(|error| panic!("can't create the compiler: {error}"));

    register(&mut compiler);

    // Before compiling: the stub helps fixing the script when it doesn't
    // compile.
    if matches!(command, Command::Stub) {
        match compiler.write_host_stub(&examples_dir) {
            Ok(true) => println!("wrote {}", examples_dir.join(".mollie/host").display()),
            Ok(false) => println!("{} is up to date", examples_dir.join(".mollie/host").display()),
            Err(error) => eprintln!("can't write the stub: {error}"),
        }

        return;
    }

    match compiler.compile_script::<Main, ScriptObject<DrawableTrait>>("<main>", &["context"], source) {
        Ok(()) => (),
        Err(CompileError::Type(errors)) => {
            let mut sources = ModuleSources {
                root: examples_dir,
                sources: HashMap::new(),
            };
            let tcx = &compiler.type_context.tcx;

            for error in errors {
                let Some(span) = error.primary_span else {
                    eprintln!("{}", tcx.display_of_diagnostic(&error));

                    continue;
                };

                sources.load(&tcx.def_registry, span.0, source);

                if let Some(secondary) = error.secondary_span {
                    sources.load(&tcx.def_registry, secondary.0, source);
                }

                let mut report = Report::build(ReportKind::Error, span).with_config(Config::new().with_compact(true));

                error.add_to_report(&mut report, tcx);
                report
                    .finish()
                    .eprint(&mut sources)
                    .unwrap_or_else(|error| eprintln!("can't print the error: {error}"));
            }

            return;
        }
        Err(error) => {
            eprintln!("{}", error.display(&compiler.type_context.tcx));

            return;
        }
    }

    match command {
        Command::Run { name } => {
            let mut draw_context = DrawContext {
                root: Pixmap::new(512, 512).expect("the size is valid"),
                images: ImageStorage::default(),
            };
            // SAFETY: the context outlives every run of the script.
            let context = unsafe { Opaque::new(&mut draw_context) };
            let main = compiler
                .script_fn::<Main, ScriptObject<DrawableTrait>>("<main>")
                .unwrap_or_else(|error| panic!("`<main>` must be compiled: {error}"));

            if name.as_deref() == Some("stress") {
                let mut taken = Duration::ZERO;
                let mut highest = Duration::ZERO;
                let mut lowest = Duration::MAX;
                let mut pauses = Vec::new();

                // The collection threshold in KiB (`MOLLIE_GC_THRESHOLD`, 1 MiB
                // by default): smaller ones give shorter, more frequent pauses.
                let threshold = std::env::var("MOLLIE_GC_THRESHOLD")
                    .ok()
                    .and_then(|kib| kib.parse::<usize>().ok())
                    .unwrap_or(1024);

                compiler.inner.heap().set_collection_threshold(threshold * 1024);

                for _ in 0..200_000 {
                    let instant = Instant::now();
                    // Like a game: the script doesn't pause to collect garbage
                    // while it runs, the host collects between frames.
                    let limits = Limits {
                        auto_collect: Some(false),
                        ..Limits::default()
                    };

                    // Errors of the script (like an index out of bounds) are
                    // returned, with where they happened.
                    if let Err(trap) = main.call((context,), limits) {
                        eprintln!("the script stopped: {trap}");

                        return;
                    }

                    if compiler.inner.collect_garbage_if_due() {
                        pauses.push(compiler.inner.heap_stats());
                    }

                    let current_taken = instant.elapsed();

                    taken += current_taken;
                    highest = highest.max(current_taken);
                    lowest = lowest.min(current_taken);
                }

                let mid_execution_time = taken / 200_000;
                let gc = compiler.inner.heap_stats();

                println!("Allocated objects: {}", gc.objects);
                println!("Bytes allocated: {}", gc.allocated_bytes);
                println!("Bytes deallocated: {}", gc.deallocated_bytes);
                println!("Collections: {} (last pause {:?}, longest {:?})", gc.collections, gc.last_pause, gc.max_pause);

                if let Some(first) = pauses.first() {
                    let mut sorted = pauses.clone();

                    sorted.sort_unstable_by_key(|stats| stats.last_pause);

                    let median = &sorted[sorted.len() / 2];
                    let tail = &sorted[(sorted.len() * 99 / 100).min(sorted.len() - 1)];
                    let longest = &sorted[sorted.len() - 1];

                    println!("Threshold: {threshold} KiB");

                    for (label, stats) in [("first", first), ("median", median), ("99%", tail), ("longest", longest)] {
                        let phases = stats.last_phases;

                        println!(
                            "Pause {label}: {:?} (roots {:?}, mark {:?}, sweep {:?}), {} objects freed, {} live",
                            stats.last_pause, phases.roots, phases.mark, phases.sweep, stats.last_freed_objects, stats.objects,
                        );
                    }
                }

                println!("Execution time (sum): {taken:?} for 200k calls");
                println!("Execution time (avg): ~{mid_execution_time:?}");
                println!("Execution time (max): ~{highest:?}");
                println!("Execution time (min): ~{lowest:?}");

                println!("Calls per sec: ~{:.2}", 1.0 / mid_execution_time.as_secs_f32());
            } else {
                let drawable = match main.call((context,), Limits::default()) {
                    Ok(drawable) => drawable,
                    Err(trap) => {
                        eprintln!("the script stopped: {trap}");

                        return;
                    }
                };
                // The root view is rooted while the host holds it.
                let origin = Position { x: 0.0, y: 0.0 };
                let size = Size { width: 512.0, height: 512.0 };
                let rendered = drawable.call::<_, ()>("render", (origin, size, context), Limits::default());

                if let Err(trap) = rendered {
                    eprintln!("rendering stopped: {trap}");

                    return;
                }

                drop(drawable);
                draw_context
                    .root
                    .save_png("./output.png")
                    .unwrap_or_else(|error| panic!("can't save the image: {error}"));
            }
        }
        Command::Stub => (),
        Command::Dump => {
            println!(">> Dumping functions");

            let inner = &mut compiler.inner;
            let tcx = &compiler.type_context.tcx;
            let impls = &tcx.impl_registry.impls;

            inner.vtables.sort_by_key(|(_, impl_ref), _| impls[*impl_ref].origin_trait);

            if let Some(func_id) = inner.name_to_func_id.get("<main>")
                && let Some(decl) = inner.func_id_to_func.get(func_id)
            {
                tracing::info!("Main function decl: {decl}");
            }

            for (&(_, impl_ref), vfuncs) in &inner.vtables {
                let generator = &impls[impl_ref];
                let trait_name = generator
                    .origin_trait
                    .map_or_default(|trait_ref| tcx.def_registry.traits[trait_ref].name.as_str());

                for (vfunc, func) in generator.functions.iter() {
                    let decl = vfuncs.get(&vfunc).and_then(|func_id| inner.func_id_to_func.get(func_id));

                    if let Some(decl) = decl {
                        tracing::info!("Next function decl: {decl}");
                    }

                    tracing::info!(
                        "{}::{} {:?} (generics = [{}], trait_name = {trait_name})",
                        tcx.display_of(generator.ty),
                        func.name,
                        decl.map(|decl| &decl.name),
                        generator.generics.iter().map(|generic| tcx.display_of(*generic)).join(", ")
                    );
                }
            }
        }
    }
}
