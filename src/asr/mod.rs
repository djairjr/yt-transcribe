//! ASR: NVIDIA Parakeet TDT 0.6B v3, multilingual (auto language detection),
//! running on ONNX Runtime through parakeet-rs.
//!
//! CTC/TDT models accept only ~4-5 minutes of audio per pass, so the PCM is
//! split into chunks and the per-chunk timestamps are shifted back onto the
//! full timeline.
//!
//! Two things make this loop the unit of work for everything above it:
//!
//! * a **cancellation flag**, read *between* blocks -- never inside one, because a half-written
//!   block is worse than a block never started;
//! * a **callback per block**, so the caller can put that block's cues on disk before the next one
//!   begins. That is what makes a stopped run resumable instead of lost.

use parakeet_rs::{ParakeetTDT, TimedToken, TimestampMode, Transcriber};
use std::error::Error;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::audio::pcm;

/// Seconds of audio per TDT pass (kept well under the model's ~5 minute limit).
pub const CHUNK_SECONDS: usize = 240;

/// The model every ASR run here goes through. Recorded on the job, because "which model produced
/// this" is part of what a transcript is.
pub const MODEL_ID: &str = "parakeet-tdt-0.6b-v3-onnx";

/// What a run produced.
///
/// The words are not in here: every block was handed to the caller the moment it finished, and the
/// registry is where they live. What is left to report is whether the run reached the end.
pub struct Transcription {
    /// True when the cancel flag stopped the run before the last block.
    pub cancelled: bool,
}

/// How many blocks of `CHUNK_SECONDS` the PCM holds.
pub fn chunk_count(pcm_bytes: &[u8]) -> usize {
    let bytes_per_chunk = CHUNK_SECONDS * pcm::PCM_SAMPLE_RATE as usize * 2;
    pcm_bytes.len().div_ceil(bytes_per_chunk)
}

/// Transcribe 16 kHz mono s16 PCM (the bytes produced by
/// [`pcm::decode_to_pcm16k`]), chunking internally.
///
/// `model_dir` must hold `encoder-model*.onnx`, `decoder_joint-model*.onnx`,
/// `nemo128.onnx` and `vocab.txt`.
///
/// `from_chunk` skips the blocks already recorded by a previous run; `cancel` is consulted before
/// each block; `on_block` is called with one block's tokens -- already shifted onto the absolute
/// timeline -- as soon as that block is done.
pub fn transcribe_pcm16k(
    pcm_bytes: &[u8],
    model_dir: &Path,
    from_chunk: usize,
    cancel: &AtomicBool,
    on_block: &mut dyn FnMut(usize, &[TimedToken]) -> Result<(), Box<dyn Error>>,
) -> Result<Transcription, Box<dyn Error>> {
    let bytes_per_chunk = CHUNK_SECONDS * pcm::PCM_SAMPLE_RATE as usize * 2;

    let mut model = ParakeetTDT::from_pretrained(model_dir, None)?;

    let mut cancelled = false;

    for (i, chunk) in pcm_bytes.chunks(bytes_per_chunk).enumerate() {
        if i < from_chunk {
            continue;
        }
        // Between blocks, never inside one.
        if cancel.load(Ordering::Relaxed) {
            cancelled = true;
            break;
        }

        let audio = pcm::bytes_to_f32(chunk);
        let offset = (i * CHUNK_SECONDS) as f32;
        let result = model.transcribe_samples(
            audio,
            pcm::PCM_SAMPLE_RATE as u32,
            1,
            Some(TimestampMode::Sentences),
        )?;

        // Shift the block onto the whole timeline before anything else looks at it: this is what
        // keeps the timecode absolute past the first four minutes.
        let mut block = result.tokens;
        for t in &mut block {
            t.start += offset;
            t.end += offset;
        }

        // The caller records what was finished before the next block starts.
        on_block(i, &block)?;
    }

    Ok(Transcription { cancelled })
}
