use anyhow::{bail, Context, Result};
use clap::{Args, Parser, Subcommand, ValueEnum};
use indicatif::{ProgressBar, ProgressStyle};
use std::collections::BTreeSet;
use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::dict::build::{self, BuildOptions};
use crate::dict::Dictionary;
use crate::fonts::{collect_font_paths, FontSet};
use crate::frame::queries_from_rgb;
use crate::matching::match_cells;
use crate::render::{Compositor, Layout, RenderConfig};
use crate::stats;
use crate::video::decode::FrameReader;
use crate::video::encode::{AudioMode, EncodeOptions, Encoder};
use crate::video::probe::{probe, validate_input};

#[derive(Parser)]
#[command(name = "glyphmosaic", version, about = "Turn video frames into a mosaic of Unicode font glyphs")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Build and inspect the font glyph dictionary.
    Dict {
        #[command(subcommand)]
        command: DictCommand,
    },
    /// Render a video as a glyph mosaic.
    Render(RenderArgs),
    /// Print dictionary metadata.
    Inspect { dict: PathBuf },
    /// Print statistics from a codepoint TSV.
    Report { codepoints: PathBuf },
}

#[derive(Subcommand)]
enum DictCommand {
    /// Rasterize fonts into a .gmd dictionary.
    Build(DictBuildArgs),
}

#[derive(Args)]
struct DictBuildArgs {
    #[arg(long = "fonts", value_name = "GLOB", num_args = 1..)]
    fonts: Vec<String>,
    #[arg(long)]
    out: PathBuf,
    #[arg(long)]
    include_pua: bool,
    #[arg(long, default_value_t = 1)]
    min_ink: u8,
    #[arg(long, default_value_t = 0)]
    jobs: usize,
    #[arg(long)]
    font_root: Option<PathBuf>,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum LayoutArg {
    Mosaic,
    Panel,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum AudioArg {
    Aac,
    Copy,
    None,
}

#[derive(Args)]
struct RenderArgs {
    #[arg(short, long)]
    input: PathBuf,
    #[arg(short, long)]
    dict: PathBuf,
    #[arg(short, long)]
    out: Option<PathBuf>,
    #[arg(long, default_value = "4x3")]
    grid: String,
    #[arg(long, value_enum, default_value_t = LayoutArg::Mosaic)]
    layout: LayoutArg,
    #[arg(long, default_value_t = 240)]
    cell_height: u32,
    #[arg(long, default_value_t = 480)]
    panel_width: u32,
    #[arg(long, default_value_t = 256)]
    top_k: usize,
    #[arg(long)]
    exact: bool,
    #[arg(long = "fonts", value_name = "GLOB", num_args = 1..)]
    fonts: Vec<String>,
    #[arg(long)]
    font_root: Option<PathBuf>,
    #[arg(long)]
    dump_codepoints: Option<PathBuf>,
    #[arg(long)]
    stats: Option<PathBuf>,
    #[arg(long, value_enum, default_value_t = AudioArg::Aac)]
    audio: AudioArg,
    #[arg(long, default_value_t = 18)]
    crf: u8,
    #[arg(long)]
    limit: Option<u64>,
    #[arg(long, default_value_t = 0)]
    threads: usize,
    #[arg(long)]
    no_encode: bool,
    #[arg(long, default_value_t = 512)]
    cache_mb: usize,
}

pub fn run() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Dict { command } => match command {
            DictCommand::Build(args) => build_dict(args),
        },
        Command::Render(args) => render_command(args),
        Command::Inspect { dict } => inspect(&dict),
        Command::Report { codepoints } => report(&codepoints),
    }
}

fn parse_grid(s: &str) -> Result<(usize, usize)> {
    let s = s.replace('×', "x").replace('*', "x").replace('X', "x");
    let (a, b) = s
        .split_once('x')
        .ok_or_else(|| anyhow::anyhow!("invalid grid '{s}'; expected COLSxROWS like 4x3"))?;
    let cols: usize = a
        .parse()
        .with_context(|| format!("invalid grid columns in '{s}'"))?;
    let rows: usize = b
        .parse()
        .with_context(|| format!("invalid grid rows in '{s}'"))?;
    if cols == 0 || rows == 0 {
        bail!("grid dimensions must be positive");
    }
    Ok((cols, rows))
}

fn build_dict(args: DictBuildArgs) -> Result<()> {
    let font_paths = collect_font_paths(&args.fonts, args.font_root.as_deref())?;
    let opts = BuildOptions {
        font_paths,
        include_pua: args.include_pua,
        min_ink: args.min_ink,
        jobs: args.jobs,
    };
    let dict = build::build(&opts)?;
    dict.write(&args.out)?;
    let size = std::fs::metadata(&args.out)?.len();
    println!(
        "wrote {} ({} entries, {} bytes)",
        args.out.display(),
        dict.len(),
        size
    );
    Ok(())
}

fn inspect(path: &Path) -> Result<()> {
    let dict = Dictionary::read(path)?;
    let unique: BTreeSet<u32> = dict.codepoints.iter().copied().collect();
    let size = std::fs::metadata(path)?.len();
    println!("dictionary: {}", path.display());
    println!("size: {size} bytes");
    println!("format version: {}", dict.header.version);
    println!(
        "features: {}x{} full, {}x{} coarse",
        dict.header.cell_feature,
        dict.header.cell_feature,
        dict.header.coarse_feature,
        dict.header.coarse_feature
    );
    println!("unicode version: {}", dict.header.unicode_version);
    println!("entries: {}", dict.len());
    println!("distinct codepoints: {}", unique.len());
    println!("fonts:");
    for f in &dict.header.fonts {
        println!("  {}  {}  {}", f.name, f.sha256, f.path);
    }
    Ok(())
}

fn report(path: &Path) -> Result<()> {
    let chars = stats::read_tsv(path)?;
    let s = stats::compute(chars);
    print!("{}", stats::format_report(&s));
    Ok(())
}

fn render_command(args: RenderArgs) -> Result<()> {
    let (cols, rows) = parse_grid(&args.grid)?;
    let layout = match args.layout {
        LayoutArg::Mosaic => Layout::Mosaic,
        LayoutArg::Panel => Layout::Panel,
    };
    let audio = match args.audio {
        AudioArg::Aac => AudioMode::Aac,
        AudioArg::Copy => AudioMode::Copy,
        AudioArg::None => AudioMode::None,
    };
    let config = RenderConfig {
        grid_cols: cols,
        grid_rows: rows,
        cell_height: args.cell_height,
        layout,
        panel_width: args.panel_width,
        cache_mb: args.cache_mb,
    };
    render_impl(RenderJob {
        input: &args.input,
        dict_path: &args.dict,
        out: args.out.as_deref(),
        config,
        top_k: args.top_k,
        exact: args.exact,
        threads: args.threads,
        fonts: &args.fonts,
        font_root: args.font_root.as_deref(),
        audio,
        crf: args.crf,
        limit: args.limit,
        dump: args.dump_codepoints.as_deref(),
        stats: args.stats.as_deref(),
        no_encode: args.no_encode,
    })
}

/// End-to-end entry point used by the integration tests.
pub fn render_for_test(
    input: &Path,
    dict_path: &Path,
    out: &Path,
    config: RenderConfig,
    top_k: usize,
    exact: bool,
    limit: u64,
) -> Result<()> {
    let no_fonts: [String; 0] = [];
    render_impl(RenderJob {
        input,
        dict_path,
        out: Some(out),
        config,
        top_k,
        exact,
        threads: 0,
        fonts: &no_fonts,
        font_root: None,
        audio: AudioMode::Aac,
        crf: 18,
        limit: Some(limit),
        dump: None,
        stats: None,
        no_encode: false,
    })
}

struct RenderJob<'a> {
    input: &'a Path,
    dict_path: &'a Path,
    out: Option<&'a Path>,
    config: RenderConfig,
    top_k: usize,
    exact: bool,
    threads: usize,
    fonts: &'a [String],
    font_root: Option<&'a Path>,
    audio: AudioMode,
    crf: u8,
    limit: Option<u64>,
    dump: Option<&'a Path>,
    stats: Option<&'a Path>,
    no_encode: bool,
}

fn render_impl(job: RenderJob<'_>) -> Result<()> {
    if !job.no_encode && job.out.is_none() {
        bail!("--out is required unless --no-encode is set");
    }
    let dict = Dictionary::read(job.dict_path)?;
    let fontset = FontSet::load_for_dictionary(&dict.header.fonts, job.fonts, job.font_root)?;
    let info = probe(job.input)?;
    validate_input(&info)?;
    let some_frame_count = info.frame_count;
    let mut reader = FrameReader::open(job.input, info)?;
    let (frame_w, frame_h) = reader.frame_dims();

    let fonts = Arc::new(
        fontset
            .fonts
            .into_iter()
            .map(|f| f.font)
            .collect::<Vec<_>>(),
    );
    let mut compositor = Compositor::new(job.config, fonts, frame_w, frame_h)?;
    let (canvas_w, canvas_h) = compositor.canvas_size();

    let mut encoder = if job.no_encode {
        None
    } else {
        let out = job.out.expect("checked above");
        Some(Encoder::new(
            out,
            job.input,
            canvas_w,
            canvas_h,
            &EncodeOptions {
                crf: job.crf,
                audio: job.audio,
                fps_num: reader.info.fps_num,
                fps_den: reader.info.fps_den,
            },
        )?)
    };

    let mut dump = match job.dump {
        Some(path) => {
            let file = File::create(path)
                .with_context(|| format!("failed to create {}", path.display()))?;
            let mut writer = BufWriter::new(file);
            writeln!(writer, "frame\tcell\tcodepoint\tchar\tfont\tmse")?;
            Some(writer)
        }
        None => None,
    };

    let total = match (job.limit, some_frame_count) {
        (Some(limit), Some(count)) => limit.min(count),
        (Some(limit), None) => limit,
        (None, Some(count)) => count,
        (None, None) => 0,
    };
    let bar = ProgressBar::new(total);
    bar.set_style(
        ProgressStyle::with_template("{spinner} {pos}/{len} frames")
            .unwrap()
            .progress_chars("=> "),
    );

    let mut frame = vec![0u8; reader.frame_size()];
    let mut chars = Vec::new();
    let mut frame_no = 0u64;
    let mut unique_total = 0usize;
    let cells = job.config.grid_cols * job.config.grid_rows;

    while job.limit.map_or(true, |limit| frame_no < limit) {
        if !reader.read_frame(&mut frame)? {
            break;
        }
        let queries = queries_from_rgb(
            &frame,
            frame_w as usize,
            frame_h as usize,
            job.config.grid_cols,
            job.config.grid_rows,
        )?;
        let (matches, match_stats) = match_cells(&dict, &queries, job.top_k, job.exact, job.threads);
        unique_total += match_stats.unique;

        for (cell, result) in matches.iter().enumerate() {
            let cp = dict.codepoints[result.index];
            let ch = char::from_u32(cp).unwrap_or('\u{FFFD}');
            chars.push(ch);
            if let Some(writer) = &mut dump {
                let printable = if matches!(ch, '\t' | '\n' | '\r') { ' ' } else { ch };
                writeln!(
                    writer,
                    "{frame_no}\t{cell}\tU+{cp:04X}\t{printable}\t{}\t{}",
                    dict.font_indices[result.index], result.mse
                )?;
            }
        }

        if let Some(enc) = &mut encoder {
            let canvas = compositor.compose(&frame, &matches, &dict, frame_no)?;
            enc.write(canvas)?;
        }
        frame_no += 1;
        bar.inc(1);
    }
    bar.finish_and_clear();

    if let Some(enc) = encoder {
        enc.finish()?;
    }
    if let Some(mut writer) = dump {
        writer.flush()?;
    }

    let stats = stats::compute(chars);
    print!("{}", stats::format_report(&stats));
    if let Some(path) = job.stats {
        stats::write_json(path, &stats)?;
    }
    println!(
        "frames: {frame_no}, cells: {}, unique queries: {unique_total} of {}",
        frame_no * cells as u64,
        frame_no * cells as u64
    );
    Ok(())
}
