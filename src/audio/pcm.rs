//! Compressed audio -> PCM 16 kHz mono s16, decoded by the statically linked
//! FFmpeg.
//!
//! Two sources feed the very same decode core:
//!
//! * a **remote** media URL -- bytes arrive through a custom `AVIOContext`
//!   whose read callback pulls HTTP Range slices on demand;
//! * a **local** file (.mp4, .m4a, .webm, ...) -- an `AVIOContext` over the
//!   file handle.
//!
//! Either way the demuxer never opens a protocol itself, and the decode/resample
//! path is shared.
//!
//! Seek matters: an mp4/m4a keeps its `moov` atom at the END of the file, so the
//! mov demuxer always seeks to the end before reading a packet. The seek callback
//! must therefore honour AVSEEK_SIZE and SEEK_END, not just forward offsets.

use rsmpeg::avformat::{AVFormatContextInput, AVIOContextContainer, AVIOContextCustom};
use rsmpeg::avutil::AVMem;
use rsmpeg::ffi;
use std::error::Error;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;
use std::sync::{Arc, Mutex};

use crate::ingest::stream;

/// Target PCM format the ASR consumes: 16-bit signed, 16 kHz, mono.
pub const PCM_SAMPLE_RATE: i32 = 16_000;

/// Convert little-endian s16 PCM bytes to the f32 samples ASR models expect
/// (normalised to [-1, 1]).
pub fn bytes_to_f32(pcm: &[u8]) -> Vec<f32> {
    pcm.chunks_exact(2)
        .map(|c| i16::from_le_bytes([c[0], c[1]]) as f32 / 32768.0)
        .collect()
}

/// Decode the audio of a **local file** into PCM 16k mono s16.
pub fn decode_file_to_pcm16k(
    path: &Path,
    cap_samples: Option<usize>,
) -> Result<Vec<u8>, Box<dyn Error>> {
    debug_log();
    let file = File::open(path)?;
    let size = file.metadata()?.len();

    let state = Arc::new(Mutex::new(FileState {
        file,
        read: 0,
        size,
        eof: false,
    }));

    let read_state = Arc::clone(&state);
    let seek_state = Arc::clone(&state);

    let read_cb: Box<dyn FnMut(&mut Vec<u8>, &mut [u8]) -> i32 + Send> =
        Box::new(move |_io_data, buf: &mut [u8]| {
            let mut st = match read_state.lock() {
                Ok(s) => s,
                Err(_) => return ffi::AVERROR_EXIT,
            };
            if st.eof {
                return ffi::AVERROR_EOF;
            }
            let pos = st.read;
            if st.file.seek(SeekFrom::Start(pos)).is_err() {
                return ffi::AVERROR_EXIT;
            }
            match st.file.read(buf) {
                Ok(0) => {
                    st.eof = true;
                    ffi::AVERROR_EOF
                }
                Ok(n) => {
                    st.read += n as u64;
                    n as i32
                }
                Err(_) => ffi::AVERROR_EXIT,
            }
        });

    let seek_cb: Box<dyn FnMut(&mut Vec<u8>, i64, i32) -> i64 + Send> =
        Box::new(move |_io_data, offset: i64, whence: i32| {
            let mut st = match seek_state.lock() {
                Ok(s) => s,
                Err(_) => return -1,
            };
            let size = st.size;
            seek_target(&mut st.read, size, offset, whence)
        });

    let io = AVIOContextCustom::alloc_context(
        AVMem::new(64 * 1024),
        false,
        Vec::new(),
        Some(read_cb),
        None,
        Some(seek_cb),
    );
    let mut input = AVFormatContextInput::from_io_context(AVIOContextContainer::Custom(io))?;
    decode_input(&mut input, cap_samples)
}

/// Decode the audio of a **remote** media URL into PCM 16k mono s16.
///
/// `size` is the total media length (known from the format metadata); it is
/// required because the mov demuxer seeks to the end of the file, and a seek
/// needs the size. A HEAD request is used if it is 0.
///
/// `cap_samples` stops early once that many 16k samples were produced.
pub fn decode_to_pcm16k(
    url: &str,
    size: u64,
    cap_samples: Option<usize>,
) -> Result<Vec<u8>, Box<dyn Error>> {
    debug_log();
    let client = reqwest::blocking::Client::builder().build()?;
    let size = if size > 0 {
        size
    } else {
        stream::content_length(&client, url).unwrap_or(0)
    };

    let state = Arc::new(Mutex::new(HttpState {
        client,
        url: url.to_string(),
        read: 0,
        size,
        eof: false,
    }));

    let read_state = Arc::clone(&state);
    let seek_state = Arc::clone(&state);

    let read_cb: Box<dyn FnMut(&mut Vec<u8>, &mut [u8]) -> i32 + Send> =
        Box::new(move |_io_data, buf: &mut [u8]| {
            let mut st = match read_state.lock() {
                Ok(s) => s,
                Err(_) => return ffi::AVERROR_EXIT,
            };
            if st.eof {
                return ffi::AVERROR_EOF;
            }
            // Ask only for what the buffer can hold: an open-ended `bytes=N-`
            // would make the server return the whole remaining file per read.
            let end = st.read + buf.len() as u64 - 1;
            let range = format!("bytes={}-{}", st.read, end);
            match stream::range_get_with(&st.client, &st.url, Some(&range)) {
                Ok(resp) => {
                    if resp.status == reqwest::StatusCode::FORBIDDEN {
                        eprintln!("AVIO read @{}: 403 Forbidden", st.read);
                    }
                    if resp.bytes.is_empty() {
                        st.eof = true;
                        return ffi::AVERROR_EOF;
                    }
                    let n = resp.bytes.len().min(buf.len());
                    buf[..n].copy_from_slice(&resp.bytes[..n]);
                    st.read += n as u64;
                    n as i32
                }
                Err(e) => {
                    eprintln!("AVIO read @{}: HTTP error {e}", st.read);
                    ffi::AVERROR_EXIT
                }
            }
        });

    let seek_cb: Box<dyn FnMut(&mut Vec<u8>, i64, i32) -> i64 + Send> =
        Box::new(move |_io_data, offset: i64, whence: i32| {
            let mut st = match seek_state.lock() {
                Ok(s) => s,
                Err(_) => return -1,
            };
            let size = st.size;
            seek_target(&mut st.read, size, offset, whence)
        });

    let io = AVIOContextCustom::alloc_context(
        AVMem::new(64 * 1024),
        false,
        Vec::new(),
        Some(read_cb),
        None,
        Some(seek_cb),
    );
    let mut input = AVFormatContextInput::from_io_context(AVIOContextContainer::Custom(io))?;
    decode_input(&mut input, cap_samples)
}

struct HttpState {
    client: reqwest::blocking::Client,
    url: String,
    read: u64,
    size: u64,
    eof: bool,
}

struct FileState {
    file: File,
    read: u64,
    size: u64,
    eof: bool,
}

/// Shared seek policy for both sources.
fn seek_target(read: &mut u64, size: u64, offset: i64, whence: i32) -> i64 {
    if whence as u32 == ffi::AVSEEK_SIZE {
        return size as i64;
    }
    let base = if whence == ffi::SEEK_SET as i32 {
        0i64
    } else if whence == ffi::SEEK_CUR as i32 {
        *read as i64
    } else if whence == ffi::SEEK_END as i32 {
        size as i64
    } else {
        return -1;
    };
    let target = base + offset;
    if target < 0 {
        return -1;
    }
    *read = target as u64;
    target
}

fn debug_log() {
    if std::env::var("YT_DEBUG").is_ok() {
        unsafe { ffi::av_log_set_level(ffi::AV_LOG_INFO as i32) };
    }
}

/// Demux + decode + resample any opened input to PCM 16k mono s16.
fn decode_input(
    input: &mut AVFormatContextInput,
    cap_samples: Option<usize>,
) -> Result<Vec<u8>, Box<dyn Error>> {
    let (stream_index, codec) = input
        .find_best_stream(ffi::AVMEDIA_TYPE_AUDIO)?
        .ok_or("no audio stream")?;

    let mut codec_ctx = rsmpeg::avcodec::AVCodecContext::new(&codec);

    // codecpar borrows `input` immutably; read_packet() below needs it mutably.
    let (sample_rate, in_fmt, in_layout) = {
        let codecpar = input.streams()[stream_index].codecpar();
        codec_ctx.apply_codecpar(&codecpar)?;
        (codecpar.sample_rate, codecpar.format, codecpar.ch_layout)
    };
    codec_ctx.open(None)?;

    let mut out_layout: ffi::AVChannelLayout = unsafe { std::mem::zeroed() };
    unsafe { ffi::av_channel_layout_default(&mut out_layout, 1) };
    let mut swr = rsmpeg::swresample::SwrContext::new(
        &out_layout,
        ffi::AV_SAMPLE_FMT_S16,
        PCM_SAMPLE_RATE,
        &in_layout,
        in_fmt,
        sample_rate,
    )?;
    swr.init()?;

    let mut pcm: Vec<u8> = Vec::new();
    let cap_bytes = cap_samples.map(|s| s * 2);

    while let Some(packet) = input.read_packet()? {
        // read_packet() returns packets from every stream; the video packets of
        // a multi-track file must not reach the audio decoder.
        let pkt_stream = unsafe { (*packet.as_ptr()).stream_index };
        if pkt_stream != stream_index as i32 {
            continue;
        }
        codec_ctx.send_packet(Some(&packet))?;
        drain_frames(&mut codec_ctx, &swr, &mut pcm, cap_bytes)?;
        drop(packet);
        if cap_bytes.map_or(false, |c| pcm.len() >= c) {
            break;
        }
    }
    let _ = codec_ctx.send_packet(None);
    let _ = drain_frames(&mut codec_ctx, &swr, &mut pcm, cap_bytes);

    if let Some(c) = cap_bytes {
        pcm.truncate(c);
    }
    Ok(pcm)
}

/// Pull every available frame out of the decoder and resample it into `pcm`.
fn drain_frames(
    codec_ctx: &mut rsmpeg::avcodec::AVCodecContext,
    swr: &rsmpeg::swresample::SwrContext,
    pcm: &mut Vec<u8>,
    cap_bytes: Option<usize>,
) -> Result<(), Box<dyn Error>> {
    loop {
        let frame = match codec_ctx.receive_frame() {
            Ok(f) => f,
            Err(_) => break, // EAGAIN / EOF
        };
        let nb = unsafe { (*frame.as_ptr()).nb_samples };
        if nb <= 0 {
            continue;
        }

        let out_cap =
            unsafe { ffi::swr_get_out_samples(swr.as_ptr() as *mut _, nb) }.max(1) as usize;
        let mut scratch = vec![0u8; out_cap * 2]; // mono s16

        let in_data = unsafe { (*frame.as_ptr()).data.as_ptr() as *const *const u8 };
        let mut out_ptr = scratch.as_mut_ptr();
        let got = unsafe {
            ffi::swr_convert(
                swr.as_ptr() as *mut _,
                &mut out_ptr as *mut *mut u8,
                out_cap as i32,
                in_data,
                nb,
            )
        };
        if got > 0 {
            scratch.truncate(got as usize * 2);
            pcm.extend_from_slice(&scratch);
        }
        if cap_bytes.map_or(false, |c| pcm.len() >= c) {
            break;
        }
    }
    Ok(())
}
