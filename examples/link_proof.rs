//! E1d link-proof: open a local audio file with the statically linked FFmpeg
//! and print what the demuxer/decoder see. Proves the binary is self-contained
//! (no libav*.so at runtime).
//!
//! Run:  cargo run --example link_proof -- test-data/sample.m4a

use rsmpeg::avformat::AVFormatContextInput;

fn main() {
    let path = std::env::args().nth(1).unwrap_or_else(|| {
        eprintln!("usage: link_proof <audio-file>");
        std::process::exit(2);
    });

    let cpath = std::ffi::CString::new(path.as_str()).expect("path has NUL");
    let mut input = AVFormatContextInput::open(&cpath).expect("failed to open input");

    for (idx, stream) in input.streams().iter().enumerate() {
        let par = stream.codecpar();
        if !par.codec_type().is_audio() {
            continue;
        }
        let cid = par.codec_id;
        // SAFETY: avcodec_get_name returns a static string or "unknown".
        let codec_name = unsafe {
            std::ffi::CStr::from_ptr(rsmpeg::ffi::avcodec_get_name(cid))
                .to_string_lossy()
                .into_owned()
        };
        println!(
            "stream {}: codec={} (id={}) sample_rate={} channels={}",
            idx, codec_name, cid, par.sample_rate, par.ch_layout.nb_channels
        );
    }

    let mut packets = 0u64;
    while let Some(_pkt) = input.read_packet().expect("read_packet failed") {
        packets += 1;
    }
    println!("packets read: {}", packets);
}
