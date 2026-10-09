//! One transcription, end to end, into a directory.
//!
//! The CLI and the MCP server both come through here. The CLI is this run printed to the terminal;
//! the MCP is this run in the background with its state on disk. Two implementations of the same
//! walk would drift, and the one that is not exercised would be the one that rots.
//!
//! The **registry** (`<stem>.cues.jsonl`) is the source of truth, and it is written once per ASR
//! block -- see [`crate::output::cues`] for why that makes a run stoppable and resumable.

use crate::asr;
use crate::audio::pcm;
use crate::ingest;
use crate::ingest::cookies::Jar;
use crate::output::{Cue, cues as registry};
use crate::task::{self, Task};
use std::error::Error;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

/// Everything a run needs that is not the source itself.
pub struct Options<'a> {
    pub model: &'a Path,
    pub dir: &'a Path,
    pub lang: Option<&'a str>,
    pub cookies: Option<&'a Jar>,
    pub force_transcribe: bool,
    /// Ignore the registry and transcribe from the first block again.
    pub force: bool,
    /// Report progress on **stderr**. Never stdout: over stdio, stdout is the MCP protocol.
    pub verbose: bool,
    /// Print the resolved audio URL on **stdout**. Only the CLI asks for this, because over stdio
    /// a stray `println!` corrupts the MCP conversation -- the server never sets it.
    pub dump_url: bool,
}

/// A finished run, and where its three files are.
pub struct Outcome {
    pub task: Task,
    pub cues_path: PathBuf,
    pub srt: PathBuf,
    pub txt: PathBuf,
}

/// Transcribe `source` (a path, a YouTube URL, or any http(s) media URL) into `opts.dir`.
///
/// `cancel` is consulted between ASR blocks: setting it stops the run with the finished blocks
/// kept, and the next call continues from there.
pub fn transcribe(
    source: &str,
    opts: &Options,
    cancel: &AtomicBool,
) -> Result<Outcome, Box<dyn Error>> {
    let (stem, kind) = classify(source)?;
    std::fs::create_dir_all(opts.dir)?;

    let cues_path = registry::path(opts.dir, &stem);
    if opts.force {
        let _ = std::fs::remove_file(&cues_path);
    }
    // The registry is the truth about what was already paid for.
    let recorded = registry::read(&cues_path)?;
    let resume = !opts.force && !recorded.is_empty();
    let from_chunk = registry::last_chunk(&recorded).map(|c| c + 1).unwrap_or(0);

    let mut task = Task {
        stem: stem.clone(),
        source: source.to_string(),
        kind: kind.to_string(),
        state: "running".to_string(),
        started_at: task::now(),
        ..Default::default()
    };
    task::write(opts.dir, &task)?;
    if opts.verbose {
        eprintln!(
            "job: {stem} ({kind}){}",
            if resume {
                format!(" -- resuming at block {from_chunk}")
            } else {
                String::new()
            }
        );
    }

    let work = walk(source, kind, &stem, opts, cancel, from_chunk, resume, &mut task);
    match work {
        Ok(()) => {
            let all = registry::read(&cues_path)?;
            let n = registry::derive(&all, opts.dir, &stem)?;
            task.cues = all.len();
            task.seconds = all.last().map(|c| c.end as f64).unwrap_or(0.0);
            if task.state == "running" {
                task.state = "finished".to_string();
            }
            task.finished_at = Some(task::now());
            task::write(opts.dir, &task)?;
            if opts.verbose {
                eprintln!(
                    "transcribed: {n} cues, {:.1} s of audio ({})",
                    task.seconds, task.state
                );
            }
            Ok(Outcome {
                task,
                srt: opts.dir.join(format!("{stem}.srt")),
                txt: opts.dir.join(format!("{stem}.txt")),
                cues_path,
            })
        }
        Err(e) => {
            task.state = "failed".to_string();
            task.error = Some(e.to_string());
            task.finished_at = Some(task::now());
            let _ = task::write(opts.dir, &task);
            Err(e)
        }
    }
}

/// Which door the source goes through, and the name its files are given.
pub fn classify(source: &str) -> Result<(String, &'static str), Box<dyn Error>> {
    // A path that happens to look like a URL is still a path.
    if Path::new(source).is_file() {
        let stem = Path::new(source)
            .file_stem()
            .and_then(|s| s.to_str())
            .ok_or("input file has no usable name")?
            .to_string();
        return Ok((stem, "local"));
    }
    if crate::is_youtube_url(source) {
        let id = ingest::innertube::video_id_from_url(source)
            .ok_or("could not read the video id from the URL")?;
        return Ok((id, "youtube"));
    }
    if crate::is_http_url(source) {
        return Ok((crate::url_stem(source), "network"));
    }
    Err(format!(
        "expected a local media file, a YouTube URL or an http(s) media URL \
         (direct file or HLS/DASH playlist), got: {source}"
    )
    .into())
}

#[allow(clippy::too_many_arguments)]
fn walk(
    source: &str,
    kind: &str,
    stem: &str,
    opts: &Options,
    cancel: &AtomicBool,
    from_chunk: usize,
    resume: bool,
    task: &mut Task,
) -> Result<(), Box<dyn Error>> {
    match kind {
        "youtube" => walk_youtube(source, stem, opts, cancel, from_chunk, resume, task),
        "network" => {
            let ua = std::env::var("YT_USER_AGENT").ok();
            let headers = opts.cookies.and_then(|jar| jar.ffmpeg_headers(source));
            let bytes = pcm::decode_url_to_pcm16k(source, ua.as_deref(), headers.as_deref(), None)
                .map_err(|e| crate::media_error(source, e))?;
            report_decoded(&bytes, opts);
            transcribe_pcm(&bytes, stem, opts, cancel, from_chunk, task)
        }
        _ => {
            let bytes = pcm::decode_file_to_pcm16k(Path::new(source), None)
                .map_err(|e| crate::media_error(source, e))?;
            report_decoded(&bytes, opts);
            transcribe_pcm(&bytes, stem, opts, cancel, from_chunk, task)
        }
    }
}

fn report_decoded(bytes: &[u8], opts: &Options) {
    if opts.verbose {
        eprintln!(
            "audio decoded: {:.1} s of PCM 16k",
            bytes.len() as f64 / 2.0 / pcm::PCM_SAMPLE_RATE as f64
        );
    }
}

/// The YouTube door: captions first, and only then the audio.
///
/// A resume skips the caption track entirely: something is already transcribed, and what is being
/// continued is the ASR blocks. Re-taking the captions there would append them twice.
fn walk_youtube(
    source: &str,
    stem: &str,
    opts: &Options,
    cancel: &AtomicBool,
    from_chunk: usize,
    resume: bool,
    task: &mut Task,
) -> Result<(), Box<dyn Error>> {
    let page = ingest::watch::watch_page(source, opts.cookies)?;

    if !opts.force_transcribe && !resume {
        match ingest::captions::fetch(
            stem,
            &page.api_key,
            page.visitor_data.as_deref(),
            opts.lang,
            opts.cookies,
        )? {
            Some((_text, cues, language)) => {
                task.output_kind = "captions".to_string();
                task.language = language;
                let tagged: Vec<Cue> = cues
                    .into_iter()
                    .map(|mut c| {
                        c.chunk = 0;
                        c
                    })
                    .collect();
                registry::append(&registry::path(opts.dir, stem), &tagged)?;
                if opts.verbose {
                    eprintln!("captions: {} cues", tagged.len());
                }
                return Ok(());
            }
            None => {
                if opts.verbose {
                    eprintln!("captions: none available, transcribing the audio");
                }
            }
        }
    }

    let client = match std::env::var("YT_CLIENT").as_deref() {
        Ok("android_vr") => ingest::clients::android_vr(),
        _ => ingest::clients::visionos(),
    };
    let pr = ingest::innertube::player(
        &client,
        &page.api_key,
        stem,
        page.visitor_data.as_deref(),
        opts.cookies,
    )?;

    let fmts = ingest::formats::audio_formats(&pr);
    let best = fmts.iter().find(|f| f.url.is_some()).ok_or_else(|| {
        format!(
            "{stem}: {}",
            ingest::formats::why_no_audio(&pr, &fmts)
        )
    })?;
    if opts.verbose {
        eprintln!("audio: itag {} ({})", best.itag, best.mime);
    }
    if opts.dump_url {
        println!("{}", best.url.as_deref().unwrap());
    }

    let bytes = pcm::decode_to_pcm16k(
        best.url.as_deref().unwrap(),
        best.content_length.unwrap_or(0) as u64,
        opts.cookies,
        None,
    )?;
    report_decoded(&bytes, opts);
    transcribe_pcm(&bytes, stem, opts, cancel, from_chunk, task)
}

/// Run the ASR over the PCM, recording each block as it lands.
fn transcribe_pcm(
    bytes: &[u8],
    stem: &str,
    opts: &Options,
    cancel: &AtomicBool,
    from_chunk: usize,
    task: &mut Task,
) -> Result<(), Box<dyn Error>> {
    if cancel.load(Ordering::Relaxed) {
        task.state = "cancelled".to_string();
        return Ok(());
    }

    task.output_kind = "asr".to_string();
    task.model = Some(asr::MODEL_ID.to_string());
    task.chunks_total = asr::chunk_count(bytes);
    task.chunks_done = from_chunk.min(task.chunks_total);
    task::write(opts.dir, task)?;

    let cues_path = registry::path(opts.dir, stem);
    let dir = opts.dir.to_path_buf();
    let verbose = opts.verbose;

    let outcome = asr::transcribe_pcm16k(
        bytes,
        opts.model,
        from_chunk,
        cancel,
        &mut |chunk, tokens| {
            let block: Vec<Cue> = tokens.iter().map(|t| Cue::from_token(t, chunk)).collect();
            registry::append(&cues_path, &block)?;
            // The record follows the work, so a `status` from another session sees this block
            // before the next one starts.
            task.chunks_done = chunk + 1;
            task::write(&dir, task)?;
            if verbose {
                eprintln!(
                    "asr: block {} of {} done ({} cues)",
                    chunk + 1,
                    task.chunks_total,
                    block.len()
                );
            }
            Ok(())
        },
    )?;

    if outcome.cancelled {
        task.state = "cancelled".to_string();
    }
    Ok(())
}
