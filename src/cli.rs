use anyhow::{bail, Context, Result};
use clap::{Args, Parser, Subcommand, ValueEnum};
use indicatif::{ProgressBar, ProgressStyle};
use std::collections::BTreeSet;
use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::{mpsc, Arc};

use crate::dict::build::{self, BuildOptions};
use crate::dict::Dictionary;
use crate::fonts::{collect_font_paths, FontSet};
use crate::frame::queries_from_rgb;
use crate::matching::{MatchResult, Matcher};
use crate::render::{Compositor, Layout, RenderConfig};
use crate::stats;
use crate::video::decode::FrameReader;
use crate::video::encode::{AudioMode, EncodeOptions, Encoder, EncoderKind};
use crate::video::probe::{probe, validate_input};

#[derive(Parser)]
#[command(
    name = "glyphmosaic",
    version,
    about = "Turn video frames into a mosaic of Unicode font glyphs"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[allow(clippy::large_enum_variant)]
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

#[derive(Clone, Copy, Debug, ValueEnum)]
enum EncoderArg {
    Auto,
    #[value(name = "libx264")]
    Libx264,
    Qsv,
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
    /// Target number of characters per frame; chooses cols×rows automatically.
    #[arg(long, conflicts_with = "grid")]
    chars: Option<usize>,
    #[arg(long, value_enum, default_value_t = LayoutArg::Mosaic)]
    layout: LayoutArg,
    #[arg(long, default_value_t = 240)]
    cell_height: u32,
    #[arg(long, default_value_t = 480)]
    panel_width: u32,
    #[arg(long, default_value_t = 1024)]
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
    #[arg(long, value_enum, default_value_t = EncoderArg::Auto)]
    encoder: EncoderArg,
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

fn grid_for_chars(n: usize, src_w: u32, src_h: u32) -> (usize, usize) {
    if n == 0 || src_w == 0 || src_h == 0 {
        return (1, 1);
    }
    let target = src_w as f64 / src_h as f64;
    let mut best = (1usize, n, f64::INFINITY);
    for cols in 1..=n {
        let rows = n.div_ceil(cols);
        let ratio = cols as f64 / rows as f64;
        let extra = (cols * rows - n) as f64 * 10.0;
        let score = (ratio - target).abs() + extra;
        if score < best.2 {
            best = (cols, rows, score);
        }
    }
    (best.0, best.1)
}

fn parse_grid(s: &str) -> Result<(usize, usize)> {
    let s = s.replace(['×', '*', 'X'], "x");
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
    let font_paths = if args.fonts.is_empty() {
        let mut found = Vec::new();
        for pattern in ["fonts/**/*.ttf", "fonts/**/*.otf"] {
            if let Ok(mut paths) =
                collect_font_paths(&[pattern.to_string()], args.font_root.as_deref())
            {
                found.append(&mut paths);
            }
        }
        found.sort();
        found.dedup();
        if found.is_empty() {
            bail!("no font files found under fonts/; run scripts/fetch-fonts.ps1");
        }
        found
    } else {
        collect_font_paths(&args.fonts, args.font_root.as_deref())?
    };
    let opts = BuildOptions {
        font_paths,
        include_pua: args.include_pua,
        min_ink: args.min_ink,
        jobs: args.jobs,
    };
    let dict = build::build(&opts)?;
    dict.write(&args.out)?;
    if dict.len() >= 20_000 {
        let idx = crate::matching::ivf::IvfIndex::train(
            &dict,
            crate::matching::ivf::IVF_K,
            6,
            args.jobs,
        )?;
        let idx_path = crate::matching::ivf::index_path(&args.out);
        idx.write(&idx_path)?;
        println!(
            "wrote {} ({} clusters, {} bytes)",
            idx_path.display(),
            idx.k,
            std::fs::metadata(&idx_path)?.len()
        );
    } else {
        println!("skipped IVF index: dictionary is too small to benefit");
    }
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
    println!("created by: {}", dict.header.created_by);
    println!(
        "build params: include_pua={} min_ink={} render_px={} stretch_px={}",
        dict.header.include_pua, dict.header.min_ink, dict.header.render_px, dict.header.stretch_px
    );
    println!("entries: {}", dict.len());
    println!("distinct codepoints: {}", unique.len());
    match &dict.index {
        Some(idx) => println!(
            "ivf index: {} clusters, {} entries",
            idx.k,
            idx.assigns.len()
        ),
        None => println!("ivf index: none (full 4x4 scan)"),
    }
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
    let (cols, rows) = if args.chars.is_some() {
        (1, 1)
    } else {
        parse_grid(&args.grid)?
    };
    let layout = match args.layout {
        LayoutArg::Mosaic => Layout::Mosaic,
        LayoutArg::Panel => Layout::Panel,
    };
    let audio = match args.audio {
        AudioArg::Aac => AudioMode::Aac,
        AudioArg::Copy => AudioMode::Copy,
        AudioArg::None => AudioMode::None,
    };
    let encoder = match args.encoder {
        EncoderArg::Auto => EncoderKind::Auto,
        EncoderArg::Libx264 => EncoderKind::X264,
        EncoderArg::Qsv => EncoderKind::Qsv,
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
        chars: args.chars,
        top_k: args.top_k,
        exact: args.exact,
        threads: args.threads,
        fonts: &args.fonts,
        font_root: args.font_root.as_deref(),
        audio,
        encoder,
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
        chars: None,
        top_k,
        exact,
        threads: 0,
        fonts: &no_fonts,
        font_root: None,
        audio: AudioMode::Aac,
        encoder: EncoderKind::Auto,
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
    chars: Option<usize>,
    top_k: usize,
    exact: bool,
    threads: usize,
    fonts: &'a [String],
    font_root: Option<&'a Path>,
    audio: AudioMode,
    encoder: EncoderKind,
    crf: u8,
    limit: Option<u64>,
    dump: Option<&'a Path>,
    stats: Option<&'a Path>,
    no_encode: bool,
}

/// What the matching stage waits on. It comes from the reader thread, plus a
/// notice from the writer thread if that one ends early.
enum FrameMsg {
    Frame(Vec<u8>),
    Eof,
    Failed(anyhow::Error),
    /// The writer thread has exited. If this arrives while frames are still
    /// being matched, the writer failed: stop instead of waiting for frames
    /// that will never come (the writer's queued frame buffers die with it, so
    /// the reader could otherwise wait forever for a free buffer).
    WriterDone,
}

/// Sends `WriterDone` when the writer thread ends, however it ends (including
/// a panic).
struct WriterNotice(mpsc::Sender<FrameMsg>);

impl Drop for WriterNotice {
    fn drop(&mut self) {
        let _ = self.0.send(FrameMsg::WriterDone);
    }
}

/// Makes sure the matching stage hears about a reader thread that ends without
/// reporting end-of-stream or an error (i.e. a panic).
struct ReaderEnd {
    tx: mpsc::Sender<FrameMsg>,
    reported: bool,
}

impl Drop for ReaderEnd {
    fn drop(&mut self) {
        if !self.reported {
            let _ = self.tx.send(FrameMsg::Failed(anyhow::anyhow!(
                "the video reader stopped unexpectedly"
            )));
        }
    }
}

/// What the matching stage sends to the compose-and-encode thread.
enum WriteJob {
    Frame {
        frame: Vec<u8>,
        matches: Vec<MatchResult>,
        frame_no: u64,
    },
    /// Every frame has been sent and the input ended cleanly: close the file.
    Finish,
}

/// Memory budget for decoded frames that are in flight between stages.
const PIPELINE_BYTES: usize = 128 << 20;

fn render_impl(job: RenderJob<'_>) -> Result<()> {
    if !job.no_encode && job.out.is_none() {
        bail!("--out is required unless --no-encode is set");
    }
    let dict = Dictionary::read(job.dict_path)?;
    if dict.is_empty() {
        bail!("dictionary is empty: rebuild it with `glyphmosaic dict build`");
    }
    let fontset = FontSet::load_for_dictionary(&dict.header.fonts, job.fonts, job.font_root)?;
    let info = probe(job.input)?;
    validate_input(&info)?;
    let some_frame_count = info.frame_count;
    let reader = FrameReader::open(job.input, info)?;
    let (frame_w, frame_h) = reader.frame_dims();
    let mut config = job.config;
    if let Some(chars) = job.chars {
        let (cols, rows) = grid_for_chars(chars, frame_w, frame_h);
        config.grid_cols = cols;
        config.grid_rows = rows;
    }

    let fonts = Arc::new(
        fontset
            .fonts
            .into_iter()
            .map(|f| f.font)
            .collect::<Vec<_>>(),
    );
    let mut compositor = Compositor::new(config, fonts, frame_w, frame_h)?;
    let (canvas_w, canvas_h) = compositor.canvas_size();

    let encoder = if job.no_encode {
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
                encoder: job.encoder,
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
    if std::env::var_os("GLYPHMOSAIC_NO_PROGRESS").is_some() {
        bar.set_draw_target(indicatif::ProgressDrawTarget::hidden());
    }
    bar.set_style(
        ProgressStyle::with_template("{spinner} {pos}/{len} frames")
            .unwrap()
            .progress_chars("=> "),
    );

    let mut chars = Vec::new();
    let mut frame_no = 0u64;
    let mut unique_total = 0usize;
    let cells = config.grid_cols * config.grid_rows;
    let mut matcher = Matcher::new(job.top_k, job.exact, job.threads)?;

    // The work is a three-stage pipeline so the stages overlap instead of
    // taking turns on every frame:
    //   reader thread:   ffmpeg -> decoded frame
    //   this thread:     frame -> cell features -> glyph matches (-> codepoint dump)
    //   writer thread:   compose the mosaic and feed the encoder
    // Frames are handled strictly in order, so the output is the same as doing
    // everything in one loop. Frame buffers are recycled through `free_*`, which
    // also bounds how far the reader can run ahead.
    let frame_size = reader.frame_size();
    let pool_size = (PIPELINE_BYTES / frame_size.max(1)).clamp(3, 8);
    let (free_tx, free_rx) = mpsc::channel::<Vec<u8>>();
    for _ in 0..pool_size {
        free_tx
            .send(vec![0u8; frame_size])
            .expect("the receiver is still alive");
    }
    let (frame_tx, frame_rx) = mpsc::channel::<FrameMsg>();
    let limit = job.limit;
    let dict_ref = &dict;

    let pipeline: Result<()> = std::thread::scope(|scope| {
        let (job_tx, writer_thread) = match encoder {
            Some(mut enc) => {
                let (tx, rx) = mpsc::channel::<WriteJob>();
                let free_tx = free_tx.clone();
                let notice = WriterNotice(frame_tx.clone());
                let handle = scope.spawn(move || -> Result<()> {
                    let _notice = notice;
                    loop {
                        match rx.recv() {
                            Ok(WriteJob::Frame {
                                frame,
                                matches,
                                frame_no,
                            }) => {
                                let canvas =
                                    compositor.compose(&frame, &matches, dict_ref, frame_no)?;
                                enc.write(canvas)?;
                                let _ = free_tx.send(frame);
                            }
                            Ok(WriteJob::Finish) => return enc.finish(),
                            // The matching stage gave up (an error): the encoder
                            // is dropped here, which stops ffmpeg.
                            Err(_) => return Ok(()),
                        }
                    }
                });
                (Some(tx), Some(handle))
            }
            None => (None, None),
        };

        let reader_thread = scope.spawn(move || {
            let mut reader = reader;
            let mut end = ReaderEnd {
                tx: frame_tx,
                reported: false,
            };
            let mut read = 0u64;
            loop {
                if limit.is_some_and(|limit| read >= limit) {
                    end.reported = true;
                    let _ = end.tx.send(FrameMsg::Eof);
                    return;
                }
                // Every other holder of a buffer is gone: nothing left to feed.
                let Ok(mut buf) = free_rx.recv() else {
                    end.reported = true;
                    return;
                };
                match reader.read_frame(&mut buf) {
                    Ok(true) => {
                        read += 1;
                        if end.tx.send(FrameMsg::Frame(buf)).is_err() {
                            end.reported = true;
                            return;
                        }
                    }
                    Ok(false) => {
                        end.reported = true;
                        let _ = end.tx.send(FrameMsg::Eof);
                        return;
                    }
                    Err(e) => {
                        end.reported = true;
                        let _ = end.tx.send(FrameMsg::Failed(e));
                        return;
                    }
                }
            }
        });

        let outcome = (|| -> Result<()> {
            while job.limit.is_none_or(|limit| frame_no < limit) {
                let frame = match frame_rx.recv() {
                    Ok(FrameMsg::Frame(frame)) => frame,
                    Ok(FrameMsg::Eof) => break,
                    Ok(FrameMsg::Failed(e)) => return Err(e),
                    // The writer's own error is reported when it is joined below.
                    Ok(FrameMsg::WriterDone) => break,
                    Err(_) => bail!("the video reader stopped unexpectedly"),
                };
                let queries = queries_from_rgb(
                    &frame,
                    frame_w as usize,
                    frame_h as usize,
                    config.grid_cols,
                    config.grid_rows,
                )?;
                let (matches, match_stats) = matcher.match_cells(dict_ref, &queries);
                unique_total += match_stats.unique;

                for (cell, result) in matches.iter().enumerate() {
                    let blank = result.index == crate::matching::BLANK;
                    let cp = if blank {
                        0x20
                    } else {
                        dict.codepoints[result.index]
                    };
                    let ch = char::from_u32(cp).unwrap_or('\u{FFFD}');
                    chars.push(ch);
                    if let Some(writer) = &mut dump {
                        let printable = if matches!(ch, '\t' | '\n' | '\r') {
                            ' '
                        } else {
                            ch
                        };
                        writeln!(
                            writer,
                            "{frame_no}\t{cell}\tU+{cp:04X}\t{printable}\t{}\t{}",
                            if blank {
                                0
                            } else {
                                dict.font_indices[result.index]
                            },
                            result.mse
                        )?;
                    }
                }

                frame_no += 1;
                if frame_no % 16 == 0 {
                    bar.set_position(frame_no);
                }
                match &job_tx {
                    Some(tx) => {
                        let sent = tx.send(WriteJob::Frame {
                            frame,
                            matches,
                            frame_no: frame_no - 1,
                        });
                        if sent.is_err() {
                            // The writer thread stopped; its error is reported below.
                            break;
                        }
                    }
                    None => {
                        let _ = free_tx.send(frame);
                    }
                }
            }
            Ok(())
        })();

        // Shut down in dependency order: tell the writer whether to finish the
        // file, wait for it, then release the reader.
        if let (Ok(()), Some(tx)) = (&outcome, &job_tx) {
            let _ = tx.send(WriteJob::Finish);
        }
        drop(job_tx);
        let writer_outcome = writer_thread.map(|handle| handle.join());
        drop(free_tx);
        drop(frame_rx);
        let reader_outcome = reader_thread.join();

        outcome?;
        if let Some(joined) = writer_outcome {
            match joined {
                Ok(result) => result?,
                Err(_) => bail!("the encoder thread panicked"),
            }
        }
        if reader_outcome.is_err() {
            bail!("the video reader thread panicked");
        }
        Ok(())
    });
    pipeline?;
    bar.set_position(frame_no);
    bar.finish_and_clear();

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn char_count_picks_sensible_grids() {
        assert_eq!(grid_for_chars(12, 640, 480), (4, 3));
        assert_eq!(grid_for_chars(48, 640, 480), (8, 6));
        assert_eq!(grid_for_chars(144, 640, 480), (12, 12));
        assert_eq!(grid_for_chars(144, 1920, 1080), (16, 9));
    }
}
