use std::{
    env, fs, io,
    path::{Path, PathBuf},
    time::Duration,
};

fn main() {
    println!("cargo:rerun-if-changed=assets/icon.ico");
    println!("cargo:rerun-if-env-changed=BUNDLE_FFMPEG");

    embed_ffmpeg();

    #[cfg(target_os = "windows")]
    {
        winresource::WindowsResource::new()
            .set_icon("assets/icon.ico")
            .compile()
            .expect("failed to embed icon resource");
    }
}

fn embed_ffmpeg() {
    let out_dir = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR"));
    let rs_path = out_dir.join("embedded_ffmpeg.rs");
    let bin_path = out_dir.join(ffmpeg_bin_name());

    let bundle = env::var("CARGO_FEATURE_BUNDLE_FFMPEG").is_ok()
        || env::var("BUNDLE_FFMPEG").ok().as_deref() == Some("1");

    if !bundle {
        fs::write(rs_path, "pub static EMBEDDED_FFMPEG: &[u8] = &[];\n")
            .expect("write embedded_ffmpeg.rs");
        return;
    }

    if !bin_path.is_file() || file_len(&bin_path) < 1_000_000 {
        if let Some(src) = local_ffmpeg_bin() {
            fs::copy(&src, &bin_path).expect("copy local ffmpeg into OUT_DIR");
        } else {
            download_ffmpeg(&bin_path);
        }
    }

    let escaped = bin_path.to_string_lossy().replace('\\', "/");
    fs::write(
        rs_path,
        format!("pub static EMBEDDED_FFMPEG: &[u8] = include_bytes!(\"{escaped}\");\n"),
    )
    .expect("write embedded_ffmpeg.rs");
}

fn ffmpeg_bin_name() -> &'static str {
    if cfg!(target_os = "windows") {
        "ffmpeg.exe"
    } else {
        "ffmpeg"
    }
}

fn file_len(path: &Path) -> u64 {
    fs::metadata(path).map(|m| m.len()).unwrap_or(0)
}

fn local_ffmpeg_bin() -> Option<PathBuf> {
    let folder = if cfg!(target_os = "windows") {
        "windows"
    } else if cfg!(target_os = "linux") {
        "linux"
    } else {
        "macos"
    };
    let path = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap())
        .join("assets/ffmpeg")
        .join(folder)
        .join(ffmpeg_bin_name());
    path.is_file().then_some(path)
}

fn download_ffmpeg(dest: &Path) {
    #[cfg(target_os = "windows")]
    {
        let url = "https://github.com/BtbN/FFmpeg-Builds/releases/download/latest/ffmpeg-master-latest-win64-gpl.zip";
        let zip_path = dest.with_extension("zip");
        download_file(url, &zip_path);
        extract_win_ffmpeg(&zip_path, dest);
        let _ = fs::remove_file(zip_path);
        return;
    }

    #[cfg(not(target_os = "windows"))]
    {
        panic!(
            "bundle-ffmpeg on this OS needs a local binary at {}",
            dest.display()
        );
    }
}

fn download_file(url: &str, dest: &Path) {
    eprintln!("Downloading FFmpeg from {url}");
    let agent = ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(30))
        .timeout_read(Duration::from_secs(600))
        .user_agent("schrodinger-audio-converter-build")
        .build();
    let response = agent.get(url).call().expect("download FFmpeg");
    let mut reader = response.into_reader();
    let mut file = fs::File::create(dest).expect("create FFmpeg download");
    io::copy(&mut reader, &mut file).expect("write FFmpeg download");
}

#[cfg(target_os = "windows")]
fn extract_win_ffmpeg(zip_path: &Path, dest: &Path) {
    let file = fs::File::open(zip_path).expect("open FFmpeg zip");
    let mut archive = zip::ZipArchive::new(file).expect("parse FFmpeg zip");
    let mut found = None;
    for i in 0..archive.len() {
        let name = archive
            .by_index(i)
            .expect("zip entry")
            .name()
            .replace('\\', "/");
        let file_name = name.rsplit('/').next().unwrap_or(&name).to_string();
        if file_name.eq_ignore_ascii_case("ffmpeg.exe") && name.contains("/bin/") {
            found = Some(i);
            break;
        }
    }
    let index = found.expect("bin/ffmpeg.exe missing in FFmpeg zip");
    let mut entry = archive.by_index(index).expect("zip ffmpeg.exe");
    let mut out = fs::File::create(dest).expect("create bundled ffmpeg.exe");
    io::copy(&mut entry, &mut out).expect("extract ffmpeg.exe");
}
