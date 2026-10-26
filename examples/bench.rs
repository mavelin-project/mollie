use std::{
    hint::black_box,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use mollie::{
    compiler::{Compiler, sandbox::Limits},
    host::{CompilerExt, ScriptValue},
    typed_ast::FileModuleLoader,
};

const RUNS: usize = 5;

fn fastest<R>(mut run: impl FnMut() -> R) -> (Duration, R) {
    black_box(run());

    let mut best = Duration::MAX;
    let mut result = None;

    for _ in 0..RUNS {
        let started = Instant::now();
        let value = black_box(run());

        best = best.min(started.elapsed());
        result = Some(value);
    }

    (best, result.expect("it ran"))
}

fn compile<R: ScriptValue>(compiler: &mut Compiler<FileModuleLoader>, folder: &Path, name: &str, param: &str) -> Duration {
    let source = std::fs::read_to_string(folder.join(format!("{name}.mol"))).unwrap_or_else(|error| panic!("can't read {name}.mol: {error}"));
    let started = Instant::now();

    compiler
        .compile_script::<(i32,), R>(name, &[param], &source)
        .unwrap_or_else(|error| panic!("{name}.mol doesn't compile:\n{}", error.display(&compiler.type_context.tcx)));

    started.elapsed()
}

fn run<R: ScriptValue>(compiler: &Compiler<FileModuleLoader>, name: &str, argument: i32, fuel: bool) -> R {
    let limits = Limits {
        fuel: fuel.then_some(u64::MAX / 2),
        ..Limits::default()
    };

    compiler
        .script_fn::<(i32,), R>(name)
        .unwrap_or_else(|error| panic!("`{name}` isn't compiled: {error}"))
        .call((argument,), limits)
        .unwrap_or_else(|trap| panic!("`{name}` stopped: {trap}"))
}

fn report(name: &str, compiled: Duration, mollie: Duration, rust: Duration, same: bool) {
    println!(
        "{name:<24} compile {:>7.2} ms   mollie {:>9.2} ms   rust {:>9.2} ms   {:>5.2}x{}",
        compiled.as_secs_f64() * 1000.0,
        mollie.as_secs_f64() * 1000.0,
        rust.as_secs_f64() * 1000.0,
        mollie.as_secs_f64() / rust.as_secs_f64().max(1e-9),
        if same { "" } else { "   (results differ!)" }
    );
}

fn main() {
    let folder = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("examples/bench");
    let new_compiler =
        || Compiler::with_symbols(FileModuleLoader { current_dir: folder.clone() }, []).unwrap_or_else(|error| panic!("can't create the compiler: {error}"));
    let mut compiler = new_compiler();
    let mut fueled_compiler = new_compiler();

    compiler.set_fuel(false);

    println!("{RUNS} runs each, the fastest shown (x: how many times Rust's time Mollie takes)\n");

    // Noise: 16 chunks of fractal Perlin noise (4 octaves): 4M samples.
    let chunks = 16;
    let compiled = compile::<f32>(&mut compiler, &folder, "noise", "chunks");
    let (mollie, from_mollie) = fastest(|| run::<f32>(&compiler, "noise", chunks, false));
    let fueled_compiled = compile::<f32>(&mut fueled_compiler, &folder, "noise", "chunks");
    let (fueled, _) = fastest(|| run::<f32>(&fueled_compiler, "noise", chunks, true));
    let (rust, from_rust) = fastest(|| twins::noise_chunks(chunks));
    let same = (from_mollie - from_rust).abs() <= from_rust.abs() * 1e-3 + 1e-2;

    report("noise (16 chunks)", compiled, mollie, rust, same);
    report("noise, fuel counted", fueled_compiled, fueled, rust, same);

    // Terrain: 64 chunks of heights from value noise, every block written.
    let chunks = 64;
    let compiled = compile::<i32>(&mut compiler, &folder, "terrain", "chunks");
    let (mollie, from_mollie) = fastest(|| run::<i32>(&compiler, "terrain", chunks, false));
    let (rust, from_rust) = fastest(|| twins::terrain(chunks));

    report("terrain (64 chunks)", compiled, mollie, rust, from_mollie == from_rust);

    // Integers: 100M steps of a xorshift generator.
    let steps = 100_000_000;
    let compiled = compile::<i64>(&mut compiler, &folder, "hash", "n");
    let (mollie, from_mollie) = fastest(|| run::<i64>(&compiler, "hash", steps, false));
    let (rust, from_rust) = fastest(|| twins::hash(steps));

    report("integers (100M)", compiled, mollie, rust, from_mollie == from_rust);

    // Calls: fib(32), recursively (7M calls).
    let compiled = compile::<i32>(&mut compiler, &folder, "calls", "n");
    let (mollie, from_mollie) = fastest(|| run::<i32>(&compiler, "calls", 32, false));
    let (rust, from_rust) = fastest(|| twins::fib(black_box(32)));

    report("fib(32)", compiled, mollie, rust, from_mollie == from_rust);

    // Allocation: 10M objects, garbage collected.
    let objects = 10_000_000;
    let compiled = compile::<i64>(&mut compiler, &folder, "alloc", "n");
    let (mollie, from_mollie) = fastest(|| run::<i64>(&compiler, "alloc", objects, false));
    let (rust, from_rust) = fastest(|| twins::alloc(objects));

    report("objects (10M)", compiled, mollie, rust, from_mollie == from_rust);
}

mod twins {
    use std::hint::black_box;

    fn fade(t: f32) -> f32 {
        t * t * t * (t * (t * 6.0 - 15.0) + 10.0)
    }

    fn lerp(t: f32, a: f32, b: f32) -> f32 {
        a + t * (b - a)
    }

    fn grad(hash: i32, x: f32, y: f32, z: f32) -> f32 {
        let h = hash & 15;
        let u = if h < 8 { x } else { y };
        let v = if h < 4 {
            y
        } else if h == 12 || h == 14 {
            x
        } else {
            z
        };

        (if h & 1 == 0 { u } else { -u }) + (if h & 2 == 0 { v } else { -v })
    }

    fn permutation(seed: i32) -> Vec<i32> {
        let mut table = (0..256).collect::<Vec<i32>>();
        let mut state = seed;

        for i in (1..256).rev() {
            state = state.wrapping_mul(1_103_515_245).wrapping_add(12345);

            let j = ((state >> 16) & 32767) % (i + 1);

            table.swap(i as usize, j as usize);
        }

        table.extend_from_within(..);
        table
    }

    #[allow(clippy::many_single_char_names)]
    fn noise(p: &[i32], x: f32, y: f32, z: f32) -> f32 {
        let (fx, fy, fz) = (x.floor(), y.floor(), z.floor());
        let (xi, yi, zi) = ((fx as i32) & 255, (fy as i32) & 255, (fz as i32) & 255);
        let (xf, yf, zf) = (x - fx, y - fy, z - fz);
        let (u, v, w) = (fade(xf), fade(yf), fade(zf));
        let a = p[xi as usize] + yi;
        let aa = p[a as usize] + zi;
        let ab = p[(a + 1) as usize] + zi;
        let b = p[(xi + 1) as usize] + yi;
        let ba = p[b as usize] + zi;
        let bb = p[(b + 1) as usize] + zi;

        lerp(
            w,
            lerp(
                v,
                lerp(u, grad(p[aa as usize], xf, yf, zf), grad(p[ba as usize], xf - 1.0, yf, zf)),
                lerp(u, grad(p[ab as usize], xf, yf - 1.0, zf), grad(p[bb as usize], xf - 1.0, yf - 1.0, zf)),
            ),
            lerp(
                v,
                lerp(u, grad(p[(aa + 1) as usize], xf, yf, zf - 1.0), grad(p[(ba + 1) as usize], xf - 1.0, yf, zf - 1.0)),
                lerp(u, grad(p[(ab + 1) as usize], xf, yf - 1.0, zf - 1.0), grad(p[(bb + 1) as usize], xf - 1.0, yf - 1.0, zf - 1.0)),
            ),
        )
    }

    fn fbm(p: &[i32], x: f32, y: f32, z: f32) -> f32 {
        let (mut sum, mut amplitude, mut frequency) = (0.0, 1.0, 1.0);

        for _ in 0..4 {
            sum += noise(p, x * frequency, y * frequency, z * frequency) * amplitude;
            amplitude *= 0.5;
            frequency *= 2.0;
        }

        sum
    }

    #[allow(clippy::cast_precision_loss)]
    pub fn noise_chunks(chunks: i32) -> f32 {
        let p = permutation(1234);
        let mut total = 0.0;

        for chunk in 0..chunks {
            for y in 0..256 {
                for z in 0..16 {
                    for x in 0..16 {
                        total += fbm(&p, ((chunk * 16 + x) as f32) * 0.031, (y as f32) * 0.031, (z as f32) * 0.031);
                    }
                }
            }
        }

        total
    }

    fn hash2(x: i32, z: i32) -> f32 {
        let mut h = x.wrapping_mul(374_761_393).wrapping_add(z.wrapping_mul(668_265_263));

        h = (h ^ (h >> 13)).wrapping_mul(1_274_126_177);

        ((h ^ (h >> 16)) & 65535) as f32 / 65535.0
    }

    fn smooth(x: f32, z: f32) -> f32 {
        let (fx, fz) = (x.floor(), z.floor());
        let (ix, iz) = (fx as i32, fz as i32);
        let (tx, tz) = (x - fx, z - fz);
        let top = hash2(ix, iz) + (hash2(ix + 1, iz) - hash2(ix, iz)) * tx;
        let bottom = hash2(ix, iz + 1) + (hash2(ix + 1, iz + 1) - hash2(ix, iz + 1)) * tx;

        top + (bottom - top) * tz
    }

    #[allow(clippy::cast_precision_loss)]
    pub fn terrain(chunks: i32) -> i32 {
        let mut blocks = vec![0u32; 65536];
        let mut solid = 0;

        for chunk in 0..chunks {
            for z in 0..16 {
                for x in 0..16 {
                    let wx = (chunk * 16 + x) as f32;
                    let height = (48.0 + smooth(wx / 32.0, z as f32 / 32.0) * 40.0 + smooth(wx / 8.0, z as f32 / 8.0) * 6.0) as i32;

                    for y in 0..256 {
                        let block = if y < height - 4 {
                            1
                        } else if y < height - 1 {
                            2
                        } else if y < height {
                            3
                        } else if y < 64 {
                            4
                        } else {
                            0
                        };

                        blocks[(y * 256 + z * 16 + x) as usize] = block;
                    }
                }
            }

            for block in &blocks {
                if *block != 0 {
                    solid += 1;
                }
            }

            black_box(&mut blocks);
        }

        solid
    }

    pub fn hash(steps: i32) -> i64 {
        let mut state: i64 = 88_172_645_463_325_252;
        let mut sum = 0i64;

        for _ in 0..black_box(steps) {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            sum = sum.wrapping_add(state & 1023);
        }

        sum
    }

    pub fn fib(n: i32) -> i32 {
        if n < 2 { n } else { fib(n - 1) + fib(n - 2) }
    }

    struct Point {
        x: i32,
        y: i32,
    }

    pub fn alloc(n: i32) -> i64 {
        let mut ring = (0..1024).map(|_| Box::new(Point { x: 0, y: 0 })).collect::<Vec<_>>();
        let mut sum = 0i64;

        for i in 0..n {
            let point = black_box(Box::new(Point { x: i, y: i.wrapping_mul(2) }));

            sum += i64::from(point.x.wrapping_add(point.y));
            ring[(i & 1023) as usize] = point;
        }

        black_box(&ring);

        sum
    }
}
