use std::{
    env, fs,
    io::{self, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::Duration,
};

#[cfg(target_os = "windows")]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

include!(concat!(env!("OUT_DIR"), "/embedded_ffmpeg.rs"));

#[cfg(target_os = "windows")]
const WIN_FFMPEG_ZIP: &str =
    "https://github.com/BtbN/FFmpeg-Builds/releases/download/latest/ffmpeg-master-latest-win64-gpl.zip";

pub fn bin_name() -> &'static str {
    if cfg!(target_os = "windows") {
        "ffmpeg.exe"
    } else {
        "ffmpeg"
    }
}

pub fn prepare() -> Result<PathBuf, String> {
    let cache_dir = cache_dir()?;
    fs::create_dir_all(&cache_dir).map_err(|e| format!("Не удалось создать кэш FFmpeg: {e}"))?;
    let dest = cache_dir.join(bin_name());

    if works(&dest) {
        return Ok(dest);
    }

    if EMBEDDED_FFMPEG.len() > 1_000_000 {
        write_bytes(&dest, EMBEDDED_FFMPEG)?;
        chmod_unix(&dest)?;
        if works(&dest) {
            return Ok(dest);
        }
        return Err("Встроенный FFmpeg не запустился. Антивирус мог заблокировать файл.".into());
    }

    if let Some(local) = find_local_install() {
        if works(&local) {
            return Ok(local);
        }
        // Shared-сборка: копируем exe + dll рядом в кэш.
        if let Some(parent) = local.parent() {
            copy_dir_files(parent, &cache_dir)?;
            if works(&dest) {
                return Ok(dest);
            }
        }
    }

    download_static(&dest)?;
    chmod_unix(&dest)?;
    if works(&dest) {
        return Ok(dest);
    }

    Err("FFmpeg скачан, но не запускается.".into())
}

fn cache_dir() -> Result<PathBuf, String> {
    #[cfg(target_os = "windows")]
    {
        let base = env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(|| env::temp_dir());
        return Ok(base.join("schrodinger-audio-converter").join("ffmpeg"));
    }

    #[cfg(not(target_os = "windows"))]
    {
        let base = env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| env::temp_dir());
        Ok(base
            .join(".local/share/schrodinger-audio-converter")
            .join("ffmpeg"))
    }
}

fn find_local_install() -> Option<PathBuf> {
    let folder = if cfg!(target_os = "windows") {
        "windows"
    } else if cfg!(target_os = "linux") {
        "linux"
    } else {
        "macos"
    };
    let bin = bin_name();
    let mut candidates = Vec::new();

    candidates.push(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("assets/ffmpeg")
            .join(folder)
            .join(bin),
    );

    if let Ok(exe) = env::current_exe() {
        if let Some(dir) = exe.parent() {
            candidates.push(dir.join(bin));
            candidates.push(dir.join("ffmpeg").join(bin));
            candidates.push(dir.join("assets/ffmpeg").join(folder).join(bin));
            if let Some(root) = dir.parent().and_then(|p| p.parent()) {
                candidates.push(root.join("assets/ffmpeg").join(folder).join(bin));
            }
        }
    }

    if let Ok(cwd) = env::current_dir() {
        candidates.push(cwd.join("assets/ffmpeg").join(folder).join(bin));
        candidates.push(cwd.join("ffmpeg").join(bin));
    }

    candidates.into_iter().find(|p| p.is_file())
}

pub fn works(path: &Path) -> bool {
    if !path.is_file() {
        return false;
    }

    let mut command = Command::new(path);
    command
        .arg("-hide_banner")
        .arg("-version")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());

    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(CREATE_NO_WINDOW);
        if let Some(parent) = path.parent() {
            command.current_dir(parent);
        }
    }

    command.status().map(|s| s.success()).unwrap_or(false)
}

fn write_bytes(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let should_write = match fs::metadata(path) {
        Ok(meta) => meta.len() != bytes.len() as u64,
        Err(_) => true,
    };
    if should_write {
        let mut file = fs::File::create(path)
            .map_err(|e| format!("Не удалось записать {}: {e}", path.display()))?;
        file.write_all(bytes)
            .map_err(|e| format!("Не удалось записать {}: {e}", path.display()))?;
    }
    Ok(())
}

fn chmod_unix(_path: &Path) -> Result<(), String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = fs::metadata(_path)
            .map_err(|e| e.to_string())?
            .permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(_path, permissions)
            .map_err(|e| format!("Не удалось сделать FFmpeg executable: {e}"))?;
    }
    Ok(())
}

fn copy_dir_files(src: &Path, dest: &Path) -> Result<(), String> {
    let entries = fs::read_dir(src).map_err(|e| e.to_string())?;
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let Some(name) = path.file_name() else {
            continue;
        };
        if name.to_string_lossy().eq_ignore_ascii_case("ffmpeg.txt") {
            continue;
        }
        let dest_path = dest.join(name);
        let skip = match (fs::metadata(&path), fs::metadata(&dest_path)) {
            (Ok(a), Ok(b)) => a.len() == b.len(),
            _ => false,
        };
        if skip {
            continue;
        }
        fs::copy(&path, &dest_path).map_err(|e| {
            format!(
                "Не удалось скопировать {} → {}: {e}",
                path.display(),
                dest_path.display()
            )
        })?;
    }
    Ok(())
}

fn download_static(dest: &Path) -> Result<(), String> {
    #[cfg(target_os = "windows")]
    {
        let zip_path = dest
            .parent()
            .unwrap_or(Path::new("."))
            .join("ffmpeg-download.zip");
        download_file(WIN_FFMPEG_ZIP, &zip_path)?;
        extract_ffmpeg_from_zip(&zip_path, dest)?;
        let _ = fs::remove_file(zip_path);
        return Ok(());
    }

    #[cfg(not(target_os = "windows"))]
    {
        let _ = dest;
        Err(
            "Автоскачивание FFmpeg есть только для Windows. Положите бинарник ffmpeg в assets/ffmpeg/linux/.".into(),
        )
    }
}

#[cfg(target_os = "windows")]
fn download_file(url: &str, dest: &Path) -> Result<(), String> {
    let agent = ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(30))
        .timeout_read(Duration::from_secs(600))
        .user_agent("schrodinger-audio-converter/0.1")
        .build();

    let response = agent
        .get(url)
        .call()
        .map_err(|e| format!("Не удалось скачать FFmpeg: {e}"))?;

    let mut reader = response.into_reader();
    let mut file =
        fs::File::create(dest).map_err(|e| format!("Не удалось сохранить загрузку: {e}"))?;
    io::copy(&mut reader, &mut file).map_err(|e| format!("Ошибка записи загрузки: {e}"))?;
    Ok(())
}

#[cfg(target_os = "windows")]
fn extract_ffmpeg_from_zip(zip_path: &Path, dest: &Path) -> Result<(), String> {
    let file = fs::File::open(zip_path).map_err(|e| format!("Не открыть архив FFmpeg: {e}"))?;
    let mut archive =
        zip::ZipArchive::new(file).map_err(|e| format!("Повреждён архив FFmpeg: {e}"))?;

    let mut found = None;
    for i in 0..archive.len() {
        let entry = archive.by_index(i).map_err(|e| e.to_string())?;
        let name = entry.name().replace('\\', "/");
        let file_name = name.rsplit('/').next().unwrap_or(&name);
        if file_name.eq_ignore_ascii_case("ffmpeg.exe") && name.contains("/bin/") {
            found = Some(i);
            break;
        }
    }

    let index = found.ok_or_else(|| "В архиве нет bin/ffmpeg.exe".to_string())?;
    let mut entry = archive.by_index(index).map_err(|e| e.to_string())?;
    let mut out = fs::File::create(dest).map_err(|e| format!("Не создать ffmpeg.exe: {e}"))?;
    io::copy(&mut entry, &mut out).map_err(|e| format!("Не извлечь ffmpeg.exe: {e}"))?;
    Ok(())
}
