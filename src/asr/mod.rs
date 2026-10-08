//! ASR: NVIDIA Parakeet TDT 0.6B v3, multilingual (auto language detection),
//! running on ONNX Runtime through parakeet-rs.
//!
//! CTC/TDT models accept only ~4-5 minutes of audio per pass, so the PCM is
//! split into chunks and the per-chunk timestamps are shifted back onto the
//! full timeline.

use parakeet_rs::{ParakeetTDT, TimedToken, TimestampMode, Transcriber, TranscriptionResult};
use std::error::Error;
use std::path::Path;

use crate::audio::pcm;

/// Seconds of audio per TDT pass (kept well under the model's ~5 minute limit).
const CHUNK_SECONDS: usize = 240;

/// Transcribe 16 kHz mono s16 PCM (the bytes produced by
/// [`pcm::decode_to_pcm16k`]), chunking internally.
///
/// `model_dir` must hold `encoder-model*.onnx`, `decoder_joint-model*.onnx`,
/// `nemo128.onnx` and `vocab.txt`.
pub fn transcribe_pcm16k(
    pcm_bytes: &[u8],
    model_dir: &Path,
) -> Result<TranscriptionResult, Box<dyn Error>> {
    let samples_per_chunk = CHUNK_SECONDS * pcm::PCM_SAMPLE_RATE as usize;
    let bytes_per_chunk = samples_per_chunk * 2;

    let mut model = ParakeetTDT::from_pretrained(model_dir, None)?;

    let mut text = String::new();
    let mut tokens: Vec<TimedToken> = Vec::new();

    for (i, chunk) in pcm_bytes.chunks(bytes_per_chunk).enumerate() {
        let audio = pcm::bytes_to_f32(chunk);
        let offset = (i * CHUNK_SECONDS) as f32;
        let result = model.transcribe_samples(
            audio,
            pcm::PCM_SAMPLE_RATE as u32,
            1,
            Some(TimestampMode::Sentences),
        )?;

        if !text.is_empty() && !result.text.is_empty() {
            text.push(' ');
        }
        text.push_str(result.text.trim());

        for mut t in result.tokens {
            t.start += offset;
            t.end += offset;
            tokens.push(t);
        }
    }

    Ok(TranscriptionResult { text, tokens })
}
