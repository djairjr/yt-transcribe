//! yt-transcribe: turn speech into `<name>.srt` + `<name>.txt`.
//!
//! Two kinds of input, one decode core:
//!
//! * a **YouTube URL** -- captions first (never transcribe what YouTube already
//!   transcribed); with no caption track, the audio is streamed over HTTP Range
//!   into a custom AVIOContext and decoded;
//! * a **local media file** (.mp4, .m4a, .webm, .mkv, ...) -- decoded straight
//!   from disk.
//!
//! Either way: FFmpeg (statically linked) -> PCM 16 kHz mono -> Parakeet TDT
//! 0.6B v3 (ONNX) -> `.srt` + `.txt`.

mod asr;
mod audio;
mod ingest;
mod output;

use clap::Parser;
use std::error::Error;
use std::path::{Path, PathBuf};

#[derive(Parser)]
#[command(name = "yt-transcribe", version, about)]
struct Cli {
    /// A YouTube URL, or a path to a local media file.
    input: String,

    /// Preferred caption language; when omitted, the video's own default track
    /// is used (so a Portuguese video yields Portuguese captions).
    #[arg(short, long)]
    lang: Option<String>,

    /// Output directory.
    #[arg(short, long, default_value = ".")]
    output: PathBuf,

    /// Model directory (encoder/decoder_joint ONNX + vocab.txt).
    #[arg(short, long, default_value = "models/tdt")]
    model: PathBuf,

    /// Ignore existing captions and always transcribe the audio.
    #[arg(long)]
    force_transcribe: bool,
}

fn main() -> Result<(), Box<dyn Error>> {
    let cli = Cli::parse();
    std::fs::create_dir_all(&cli.output)?;

    if Path::new(&cli.input).is_file() {
        transcribe_local(Path::new(&cli.input), &cli)
    } else {
        transcribe_youtube(&cli)
    }
}

/// Local media file: decode from disk, then transcribe.
fn transcribe_local(path: &Path, cli: &Cli) -> Result<(), Box<dyn Error>> {
    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .ok_or("input file has no usable name")?;
    eprintln!("file: {} ({stem})", path.display());

    let pcm = audio::pcm::decode_file_to_pcm16k(path, None)?;
    eprintln!(
        "audio decoded: {:.1} s of PCM 16k",
        pcm.len() as f64 / 2.0 / 16000.0
    );

    let result = asr::transcribe_pcm16k(&pcm, &cli.model)?;
    let cues: Vec<output::Cue> = result.tokens.iter().map(Into::into).collect();

    let srt_path = cli.output.join(format!("{stem}.srt"));
    let txt_path = cli.output.join(format!("{stem}.txt"));
    let n = output::srt::write_srt(&cues, &srt_path)?;
    output::txt::write_txt(&result.text, &txt_path)?;
    eprintln!(
        "transcribed: wrote {} ({} cues) and {}",
        srt_path.display(),
        n,
        txt_path.display()
    );
    Ok(())
}

/// YouTube URL: captions first, else stream + transcribe.
fn transcribe_youtube(cli: &Cli) -> Result<(), Box<dyn Error>> {
    let video_id = ingest::innertube::video_id_from_url(&cli.input)
        .ok_or("could not read the video id from the URL")?;
    eprintln!("video: {video_id}");

    let page = ingest::watch::watch_page(&cli.input)?;
    let srt_path = cli.output.join(format!("{video_id}.srt"));
    let txt_path = cli.output.join(format!("{video_id}.txt"));

    // 1. Captions first -- the iOS client is the one that yields a working track.
    if !cli.force_transcribe {
        match ingest::captions::fetch(
            &video_id,
            &page.api_key,
            page.visitor_data.as_deref(),
            cli.lang.as_deref(),
        )? {
            Some((text, cues)) => {
                let n = output::srt::write_srt(&cues, &srt_path)?;
                output::txt::write_txt(&text, &txt_path)?;
                eprintln!(
                    "captions: wrote {} ({} cues) and {}",
                    srt_path.display(),
                    n,
                    txt_path.display()
                );
                return Ok(());
            }
            None => eprintln!("captions: none available, transcribing the audio"),
        }
    }

    // 2. No captions: stream the audio and transcribe it.
    let client = match std::env::var("YT_CLIENT").as_deref() {
        Ok("android_vr") => ingest::clients::android_vr(),
        _ => ingest::clients::visionos(),
    };
    let pr =
        ingest::innertube::player(&client, &page.api_key, &video_id, page.visitor_data.as_deref())?;

    let fmts = ingest::formats::audio_formats(&pr);
    let best = fmts
        .iter()
        .find(|f| f.url.is_some())
        .ok_or("no audio-only format with a direct URL was returned")?;
    eprintln!("audio: itag {} ({})", best.itag, best.mime);
    if std::env::var("YT_DUMP_URL").is_ok() {
        println!("{}", best.url.as_deref().unwrap());
    }

    let pcm = audio::pcm::decode_to_pcm16k(
        best.url.as_deref().unwrap(),
        best.content_length.unwrap_or(0) as u64,
        None,
    )?;
    eprintln!(
        "audio decoded: {:.1} s of PCM 16k",
        pcm.len() as f64 / 2.0 / 16000.0
    );

    let result = asr::transcribe_pcm16k(&pcm, &cli.model)?;
    let cues: Vec<output::Cue> = result.tokens.iter().map(Into::into).collect();

    let n = output::srt::write_srt(&cues, &srt_path)?;
    output::txt::write_txt(&result.text, &txt_path)?;
    eprintln!(
        "transcribed: wrote {} ({} cues) and {}",
        srt_path.display(),
        n,
        txt_path.display()
    );
    Ok(())
}
