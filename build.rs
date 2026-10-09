// The FFmpeg build this crate links against enables network support, and that
// pulls in two external libraries the `rusty_ffmpeg` build script knows nothing
// about: it emits a fixed list of seven FFmpeg libraries (avcodec, avdevice,
// avfilter, avformat, avutil, swresample, swscale) and nothing else.
//
//   * OpenSSL  -- static TLS behind https://      -> undefined SSL_*/ERR_*
//   * libxml2  -- required by the dash demuxer   -> undefined xml*
//
// Both are linked statically so the final binary stays self-contained (no
// libssl.so / libcrypto.so shows up in `ldd`). The transitive dependencies were
// taken from `pkg-config --static --libs openssl` and `... libxml-2.0`, then
// confirmed against the linker's real undefined-symbol list for this build.

fn main() {
    // Only the Linux build links these system libraries. The Windows cross
    // build ships its own FFmpeg and onnxruntime and must not be touched here.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("linux") {
        return;
    }

    // Emitted in dependency order: static archives are searched left to right,
    // so a library has to appear after the objects that reference it.
    //   ssl -> crypto -> jitterentropy            [OpenSSL]
    //   crypto -> (z, zstd)                       [OpenSSL]
    //   xml2 -> z                                 [libxml2]
    //
    // jitterentropy: OpenSSL 4's default provider always pulls its jitter RNG
    // seed source in, so libcrypto.a references jent_* unconditionally. Ubuntu
    // ships the static archive in libjitterentropy3-dev.
    //
    // z and zstd are linked statically too, so the binary needs no libz.so /
    // libzstd.so at runtime -- the whole point of the vendored FFmpeg build.
    for lib in ["ssl", "crypto", "xml2", "jitterentropy", "z", "zstd"] {
        println!("cargo:rustc-link-lib=static={lib}");
    }
    for lib in ["dl", "pthread", "m"] {
        println!("cargo:rustc-link-lib=dylib={lib}");
    }
    println!("cargo:rerun-if-changed=build.rs");
}
