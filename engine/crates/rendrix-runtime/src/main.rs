//! `rendrix-runtime`: run a Rendrix application.
//!
//! ```text
//! rendrix-runtime [run] <dir | app.rdxapp> [--headless] [--screenshot out.png] [--script steps.json] [--size WxH] [--scale S]
//! rendrix-runtime pack <dir | app.rdxapp> --out app.exe [--runtime path/to/rendrix-runtime.exe]
//! rendrix-runtime                         (runs the bundle attached to this executable)
//!
//! Render outputs (headless only): `--screenshot out.png` (single PNG), `--pdf out.pdf`
//! (one-page PDF), `--frame-dir dir [--frames N] [--frame-ms MS]` (PNG sequence for /video).
//! mp4 encoding belongs to /video and is intentionally absent here.
//! ```

/// Native-only CLI: on wasm32 the app is driven by `rendrix-web`, not this binary.
#[cfg(target_arch = "wasm32")]
fn main() {}

#[cfg(not(target_arch = "wasm32"))]
use rendrix_runtime::{bundle::AppBundle, headless::Headless, window};
#[cfg(not(target_arch = "wasm32"))]
use std::path::{Path, PathBuf};
#[cfg(not(target_arch = "wasm32"))]
use std::time::Duration;

#[cfg(not(target_arch = "wasm32"))]
fn usage() -> ! {
    eprintln!(
        "usage:\n  rendrix-runtime [run] <dir | app.rdxapp> [--headless] [--screenshot out.png] [--script steps.json] [--size WxH] [--scale S]\n  rendrix-runtime [run] <dir | app.rdxapp> --headless --pdf out.pdf [--size WxH]\n  rendrix-runtime [run] <dir | app.rdxapp> --headless --frame-dir dir [--frames N] [--frame-ms MS] [--size WxH]\n  rendrix-runtime pack <dir | app.rdxapp> --out app[.exe] [--runtime path]\n  rendrix-runtime            (run the bundle attached to this executable)"
    );
    std::process::exit(2);
}

#[cfg(not(target_arch = "wasm32"))]
struct Args {
    cmd: String,
    path: Option<PathBuf>,
    headless: bool,
    screenshot: Option<PathBuf>,
    pdf: Option<PathBuf>,
    frame_dir: Option<PathBuf>,
    frames: usize,
    frame_ms: u64,
    script: Option<PathBuf>,
    inspect: Option<PathBuf>,
    size: Option<(u32, u32)>,
    scale: f32,
    out: Option<PathBuf>,
    runtime: Option<PathBuf>,
}

#[cfg(not(target_arch = "wasm32"))]
fn parse() -> Args {
    let mut it = std::env::args().skip(1).peekable();
    let mut a = Args { cmd: "run".into(), path: None, headless: false, screenshot: None, pdf: None, frame_dir: None, frames: 60, frame_ms: 50, script: None, inspect: None, size: None, scale: 1.0, out: None, runtime: None };
    if let Some(first) = it.peek() {
        if first == "run" || first == "pack" || first == "doctor" || first == "bench" {
            a.cmd = it.next().unwrap();
        }
    }
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--headless" => a.headless = true,
            "--screenshot" => a.screenshot = it.next().map(PathBuf::from),
            "--pdf" => a.pdf = it.next().map(PathBuf::from),
            "--frame-dir" => a.frame_dir = it.next().map(PathBuf::from),
            "--frames" => a.frames = it.next().and_then(|s| s.parse().ok()).unwrap_or_else(|| usage()),
            "--frame-ms" => a.frame_ms = it.next().and_then(|s| s.parse().ok()).unwrap_or_else(|| usage()),
            "--script" => a.script = it.next().map(PathBuf::from),
            "--inspect" => a.inspect = it.next().map(PathBuf::from),
            "--out" => a.out = it.next().map(PathBuf::from),
            "--runtime" => a.runtime = it.next().map(PathBuf::from),
            "--scale" => a.scale = it.next().and_then(|s| s.parse().ok()).unwrap_or_else(|| usage()),
            "--size" => {
                let s = it.next().unwrap_or_else(|| usage());
                let (w, h) = s.split_once('x').unwrap_or_else(|| usage());
                a.size = Some((w.parse().unwrap_or_else(|_| usage()), h.parse().unwrap_or_else(|_| usage())));
            }
            "-h" | "--help" => usage(),
            other if other.starts_with("--") => usage(),
            other => a.path = Some(PathBuf::from(other)),
        }
    }
    a
}

#[cfg(not(target_arch = "wasm32"))]
fn main() {
    let args = parse();
    if let Err(e) = real_main(args) {
        eprintln!("rendrix-runtime: {e}");
        std::process::exit(1);
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn doctor() -> Result<(), String> {
    println!("Runtime: rendrix-runtime {}", env!("CARGO_PKG_VERSION"));
    println!("JavaScript engine: QuickJS (ES modules, async/await, promises)");
    match rendrix_gfx::GpuContext::headless() {
        Ok(ctx) => println!("GPU adapter: {}", ctx.adapter_name()),
        Err(e) => {
            println!("GPU adapter: no suitable adapter found ({e})");
            return Err("no GPU adapter".into());
        }
    }
    Ok(())
}

#[cfg(not(target_arch = "wasm32"))]
fn real_main(args: Args) -> Result<(), String> {
    if args.cmd == "doctor" {
        return doctor();
    }
    if args.cmd == "bench" {
        let items = args.path.as_ref().and_then(|p| p.to_str()).and_then(|s| s.parse().ok()).unwrap_or(1000);
        return rendrix_runtime::bench::run(items, 30);
    }
    let bundle = AppBundle::load(args.path.as_deref())?;
    if args.cmd == "pack" {
        let out = args.out.ok_or("pack needs --out <file>")?;
        let runtime = match args.runtime {
            Some(r) => r,
            None => std::env::current_exe().map_err(|e| e.to_string())?,
        };
        bundle.pack(&runtime, &out)?;
        println!("packed '{}' -> {}", bundle.name, out.display());
        return Ok(());
    }
    if args.headless || args.screenshot.is_some() || args.pdf.is_some() || args.frame_dir.is_some() || args.script.is_some() || args.inspect.is_some() {
        let size = args.size.unwrap_or((bundle.width, bundle.height));
        let mut h = Headless::new(&bundle, size, args.scale)?;
        eprintln!("[rendrix] headless on {}", h.adapter);
        h.settle(Duration::from_secs(2));
        if let Some(path) = &args.inspect {
            let snap = rendrix_runtime::inspector::snapshot(&h.session);
            std::fs::write(path, serde_json::to_string_pretty(&snap).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
            println!("[inspect] {}", path.display());
        }
        if let Some(script) = &args.script {
            let text = std::fs::read_to_string(script).map_err(|e| format!("{}: {e}", script.display()))?;
            let steps: Vec<serde_json::Value> = serde_json::from_str(&text).map_err(|e| format!("{}: {e}", script.display()))?;
            let dir = args.screenshot.as_deref().and_then(Path::parent).map(Path::to_path_buf).unwrap_or_else(|| PathBuf::from("."));
            let n = h.run_steps(&steps, &dir)?;
            eprintln!("[rendrix] ran {n} scripted steps");
        }
        if let Some(path) = &args.screenshot {
            h.settle(Duration::from_secs(1));
            h.screenshot().save_png(path)?;
            println!("[screenshot] {}", path.display());
        }
        if let Some(path) = &args.pdf {
            h.settle(Duration::from_secs(1));
            h.screenshot().save_pdf(path)?;
            println!("[pdf] {}", path.display());
        }
        if let Some(dir) = &args.frame_dir {
            let paths = h.save_frames(dir, args.frames, args.frame_ms)?;
            println!("[frames] {} frames → {}", paths.len(), dir.display());
        }
        return Ok(());
    }
    window::run(bundle)
}
