#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use eframe::egui;
use eframe::egui::{
    Color32, CornerRadius, RichText, ScrollArea, Sense, Vec2,
};
use rfd::FileDialog;
use serde::{Deserialize, Serialize};
use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
    process::Command,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    thread,
};
use walkdir::WalkDir;

// ============================================================
// APPLICATION
// ============================================================

const APP_TITLE: &str = "Schrödinger Audio Converter";

const ACCENT: Color32 = Color32::from_rgb(0x5b, 0x8d, 0xef);

const ICON_PNG: &[u8] = include_bytes!("../assets/icon.png");

#[cfg(target_os = "windows")]
const EMBEDDED_FFMPEG: &[u8] =
    include_bytes!("../assets/ffmpeg/windows/ffmpeg.exe");

#[cfg(target_os = "linux")]
const EMBEDDED_FFMPEG: &[u8] =
    include_bytes!("../assets/ffmpeg/linux/ffmpeg");

const AUDIO_EXTENSIONS: &[&str] = &[
    "mp3",
    "wav",
    "flac",
    "m4a",
    "aac",
    "opus",
    "ogg",
    "wma",
    "aiff",
    "alac",
    "ape",
];

// ============================================================
// SETTINGS
// ============================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
struct AppSettings {
    output_directory: Option<PathBuf>,
    recursive: bool,
    channels: Channels,
    quality: Quality,
    overwrite: bool,
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            output_directory: None,
            recursive: true,
            channels: Channels::Stereo,
            quality: Quality::Q5,
            overwrite: false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
enum Channels {
    Mono,
    Stereo,
}

impl Channels {
    fn ffmpeg_value(self) -> &'static str {
        match self {
            Self::Mono => "1",
            Self::Stereo => "2",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
enum Quality {
    Q3,
    Q5,
    Q7,
    Q9,
}

impl Quality {
    fn ffmpeg_value(self) -> &'static str {
        match self {
            Self::Q3 => "3",
            Self::Q5 => "5",
            Self::Q7 => "7",
            Self::Q9 => "9",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Q3 => "Q3 — экономия места",
            Self::Q5 => "Q5 — стандарт",
            Self::Q7 => "Q7 — высокое качество",
            Self::Q9 => "Q9 — максимальное",
        }
    }
}

// ============================================================
// FILE STATE
// ============================================================

#[derive(Debug, Clone)]
struct InputFile {
    path: PathBuf,
    selected: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FileStatus {
    Waiting,
    Processing,
    Done,
    Error,
    Skipped,
}

#[derive(Debug, Clone)]
struct ConversionResult {
    path: PathBuf,
    status: FileStatus,
    message: String,
}

// ============================================================
// PATHS
// ============================================================

fn base_dir() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(Path::to_path_buf))
        .unwrap_or_else(|| PathBuf::from("."))
}

fn settings_path() -> PathBuf {
    base_dir().join("settings.json")
}

fn load_settings() -> AppSettings {
    let path = settings_path();

    if !path.exists() {
        return AppSettings::default();
    }

    fs::read_to_string(path)
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

fn save_settings(settings: &AppSettings) {
    if let Ok(text) = serde_json::to_string_pretty(settings) {
        let _ = fs::write(settings_path(), text);
    }
}

// ============================================================
// FFMPEG
// ============================================================

fn ffmpeg_path() -> Result<PathBuf, String> {
    let temp = std::env::temp_dir();

    let dir = temp.join("schrodinger-audio-converter");

    fs::create_dir_all(&dir)
        .map_err(|e| format!("Не удалось создать временную папку: {e}"))?;

    #[cfg(target_os = "windows")]
    let path = dir.join("ffmpeg.exe");

    #[cfg(target_os = "linux")]
    let path = dir.join("ffmpeg");

    if !path.exists() {
        fs::write(&path, EMBEDDED_FFMPEG)
            .map_err(|e| format!("Не удалось извлечь FFmpeg: {e}"))?;
    }

    #[cfg(target_os = "linux")]
    {
        use std::os::unix::fs::PermissionsExt;

        let mut permissions = fs::metadata(&path)
            .map_err(|e| e.to_string())?
            .permissions();

        permissions.set_mode(0o755);

        fs::set_permissions(&path, permissions)
            .map_err(|e| e.to_string())?;
    }

    Ok(path)
}

// ============================================================
// AUDIO DISCOVERY
// ============================================================

fn is_audio_file(path: &Path) -> bool {
    path.extension()
        .and_then(|x| x.to_str())
        .map(|ext| {
            AUDIO_EXTENSIONS
                .iter()
                .any(|x| x.eq_ignore_ascii_case(ext))
        })
        .unwrap_or(false)
}

fn collect_from_folder(folder: &Path, recursive: bool) -> Vec<PathBuf> {
    let mut result = Vec::new();

    if recursive {
        for entry in WalkDir::new(folder)
            .follow_links(false)
            .into_iter()
            .filter_map(Result::ok)
        {
            let path = entry.path();

            if path.is_file() && is_audio_file(path) {
                result.push(path.to_path_buf());
            }
        }
    } else if let Ok(entries) = fs::read_dir(folder) {
        for entry in entries.flatten() {
            let path = entry.path();

            if path.is_file() && is_audio_file(&path) {
                result.push(path);
            }
        }
    }

    result
}

// ============================================================
// OUTPUT
// ============================================================

fn output_path(input: &Path, output_directory: &Path) -> PathBuf {
    let stem = input
        .file_stem()
        .and_then(|x| x.to_str())
        .unwrap_or("audio");

    output_directory.join(format!("{stem}.ogg"))
}

// ============================================================
// CONVERSION
// ============================================================

fn convert_file(
    ffmpeg: &Path,
    input: &Path,
    output: &Path,
    channels: Channels,
    quality: Quality,
    overwrite: bool,
) -> Result<(), String> {
    if output.exists() && !overwrite {
        return Err("Файл уже существует".to_string());
    }

    if let Some(parent) = output.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| format!("Ошибка создания каталога: {e}"))?;
    }

    let mut command = Command::new(ffmpeg);

    command
        .arg("-hide_banner")
        .arg("-loglevel")
        .arg("error");

    if overwrite {
        command.arg("-y");
    } else {
        command.arg("-n");
    }

    command
        .arg("-i")
        .arg(input)
        .arg("-vn")
        .arg("-ac")
        .arg(channels.ffmpeg_value())
        .arg("-c:a")
        .arg("libvorbis")
        .arg("-q:a")
        .arg(quality.ffmpeg_value())
        .arg(output);

    let result = command
        .output()
        .map_err(|e| format!("Не удалось запустить FFmpeg: {e}"))?;

    if result.status.success() {
        Ok(())
    } else {
        let stderr = String::from_utf8_lossy(&result.stderr);

        Err(if stderr.trim().is_empty() {
            format!("FFmpeg завершился с кодом {:?}", result.status.code())
        } else {
            stderr.trim().to_string()
        })
    }
}

// ============================================================
// ICON
// ============================================================

fn decode_icon() -> (Vec<u8>, u32, u32) {
    let image = image::load_from_memory(ICON_PNG)
        .expect("Не удалось прочитать icon.png")
        .into_rgba8();

    let (width, height) = image.dimensions();

    (image.into_raw(), width, height)
}

// ============================================================
// VISUALS
// ============================================================

fn build_visuals() -> egui::Visuals {
    let mut visuals = egui::Visuals::dark();

    visuals.panel_fill = Color32::from_rgb(0x1e, 0x21, 0x28);
    visuals.window_fill = Color32::from_rgb(0x24, 0x28, 0x30);
    visuals.extreme_bg_color = Color32::from_rgb(0x17, 0x19, 0x1f);
    visuals.faint_bg_color = Color32::from_rgb(0x2a, 0x2e, 0x37);

    visuals.hyperlink_color = ACCENT;

    visuals.selection.bg_fill =
        ACCENT.linear_multiply(0.55);

    visuals.window_corner_radius =
        CornerRadius::same(12);

    visuals.menu_corner_radius =
        CornerRadius::same(8);

    for widget in [
        &mut visuals.widgets.noninteractive,
        &mut visuals.widgets.inactive,
        &mut visuals.widgets.hovered,
        &mut visuals.widgets.active,
        &mut visuals.widgets.open,
    ] {
        widget.corner_radius = CornerRadius::same(8);
    }

    visuals.widgets.hovered.bg_fill =
        ACCENT.linear_multiply(0.35);

    visuals.widgets.active.bg_fill =
        ACCENT.linear_multiply(0.55);

    visuals
}

// ============================================================
// APP
// ============================================================

struct App {
    settings: AppSettings,

    files: Vec<InputFile>,

    output_directory: Option<PathBuf>,

    results: Vec<ConversionResult>,

    converting: bool,

    progress: usize,

    total: usize,

    status: String,

    icon_texture: egui::TextureHandle,
}

impl App {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        cc.egui_ctx.set_visuals(build_visuals());

        let (rgba, width, height) = decode_icon();

        let color_image =
            egui::ColorImage::from_rgba_unmultiplied(
                [width as usize, height as usize],
                &rgba,
            );

        let icon_texture =
            cc.egui_ctx.load_texture(
                "app_icon",
                color_image,
                egui::TextureOptions::LINEAR,
            );

        let settings = load_settings();

        Self {
            output_directory: settings.output_directory.clone(),

            settings,

            files: Vec::new(),

            results: Vec::new(),

            converting: false,

            progress: 0,

            total: 0,

            status: "Готово".to_string(),

            icon_texture,
        }
    }

    fn add_files(&mut self, paths: Vec<PathBuf>) {
        let existing: HashSet<PathBuf> =
            self.files.iter()
                .map(|x| x.path.clone())
                .collect();

        for path in paths {
            if is_audio_file(&path)
                && !existing.contains(&path)
            {
                self.files.push(InputFile {
                    path,
                    selected: true,
                });
            }
        }

        self.status =
            format!("Файлов: {}", self.files.len());
    }

    fn add_folder(&mut self, folder: PathBuf) {
        let files = collect_from_folder(
            &folder,
            self.settings.recursive,
        );

        self.add_files(files);
    }

    fn clear_files(&mut self) {
        self.files.clear();
        self.results.clear();
        self.status = "Список очищен".to_string();
    }

    fn start_conversion(&mut self) {
        if self.converting {
            return;
        }

        let output = match &self.output_directory {
            Some(path) => path.clone(),
            None => {
                self.status =
                    "Выберите выходную папку".to_string();

                return;
            }
        };

        let selected: Vec<PathBuf> =
            self.files
                .iter()
                .filter(|x| x.selected)
                .map(|x| x.path.clone())
                .collect();

        if selected.is_empty() {
            self.status =
                "Нет выбранных файлов".to_string();

            return;
        }

        let ffmpeg = match ffmpeg_path() {
            Ok(path) => path,

            Err(error) => {
                self.status = error;
                return;
            }
        };

        self.converting = true;
        self.progress = 0;
        self.total = selected.len();
        self.results.clear();

        let channels = self.settings.channels;
        let quality = self.settings.quality;
        let overwrite = self.settings.overwrite;

        let progress =
            Arc::new(AtomicUsize::new(0));

        let progress_thread =
            Arc::clone(&progress);

        thread::spawn(move || {
            rayon::scope(|scope| {
                for input in selected {
                    let ffmpeg = ffmpeg.clone();
                    let output = output.clone();

                    let progress =
                        Arc::clone(&progress_thread);

                    scope.spawn(move |_| {
                        let destination =
                            output_path(
                                &input,
                                &output,
                            );

                        let result =
                            convert_file(
                                &ffmpeg,
                                &input,
                                &destination,
                                channels,
                                quality,
                                overwrite,
                            );

                        let _ = result;

                        progress.fetch_add(
                            1,
                            Ordering::Relaxed,
                        );
                    });
                }
            });
        });

        self.status =
            "Конвертация запущена".to_string();
    }

    fn poll_conversion(&mut self) {
        if !self.converting {
            return;
        }

        let current =
            self.progress;

        let _ = current;

        // В данной версии progress будет обновляться
        // через UI-пуллинг ниже.
    }

    fn select_output(&mut self) {
        if let Some(path) =
            FileDialog::new().pick_folder()
        {
            self.output_directory = Some(path.clone());

            self.settings.output_directory =
                Some(path);

            save_settings(&self.settings);
        }
    }
}

// ============================================================
// GUI
// ============================================================

impl eframe::App for App {
    fn update(
        &mut self,
        ctx: &egui::Context,
        _frame: &mut eframe::Frame,
    ) {
        self.poll_conversion();

        egui::TopBottomPanel::top("top_bar")
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.add(
                        egui::Image::new((
                            self.icon_texture.id(),
                            Vec2::new(32.0, 32.0),
                        )),
                    );

                    ui.add_space(10.0);

                    ui.heading(
                        RichText::new(APP_TITLE)
                            .strong(),
                    );

                    ui.add_space(20.0);

                    ui.label(
                        RichText::new("MP3 / M4A / WAV / FLAC → OGG")
                            .weak(),
                    );
                });
            });

        egui::CentralPanel::default()
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    if ui
                        .add_enabled(
                            !self.converting,
                            egui::Button::new(
                                "📁 Добавить папку",
                            ),
                        )
                        .clicked()
                    {
                        if let Some(folder) =
                            FileDialog::new().pick_folder()
                        {
                            self.add_folder(folder);
                        }
                    }

                    if ui
                        .add_enabled(
                            !self.converting,
                            egui::Button::new(
                                "＋ Добавить файлы",
                            ),
                        )
                        .clicked()
                    {
                        if let Some(files) =
                            FileDialog::new()
                                .add_filter(
                                    "Аудио",
                                    &[
                                        "mp3",
                                        "wav",
                                        "flac",
                                        "m4a",
                                        "aac",
                                        "ogg",
                                        "opus",
                                        "wma",
                                        "aiff",
                                        "alac",
                                        "ape",
                                    ],
                                )
                                .pick_files()
                        {
                            self.add_files(files);
                        }
                    }

                    if ui
                        .add_enabled(
                            !self.converting,
                            egui::Button::new(
                                "Очистить",
                            ),
                        )
                        .clicked()
                    {
                        self.clear_files();
                    }
                });

                ui.add_space(12.0);

                // ------------------------------------------------
                // SETTINGS
                // ------------------------------------------------

                egui::Frame::group(
                    ui.style(),
                )
                .show(ui, |ui| {
                    ui.heading("Настройки");

                    ui.add_space(8.0);

                    ui.horizontal(|ui| {
                        ui.label("Каналы:");

                        ui.selectable_value(
                            &mut self.settings.channels,
                            Channels::Mono,
                            "Mono",
                        );

                        ui.selectable_value(
                            &mut self.settings.channels,
                            Channels::Stereo,
                            "Stereo",
                        );
                    });

                    ui.horizontal(|ui| {
                        ui.label("Качество:");

                        egui::ComboBox::from_id_salt(
                            "quality",
                        )
                        .selected_text(
                            self.settings.quality.label(),
                        )
                        .show_ui(ui, |ui| {
                            ui.selectable_value(
                                &mut self.settings.quality,
                                Quality::Q3,
                                Quality::Q3.label(),
                            );

                            ui.selectable_value(
                                &mut self.settings.quality,
                                Quality::Q5,
                                Quality::Q5.label(),
                            );

                            ui.selectable_value(
                                &mut self.settings.quality,
                                Quality::Q7,
                                Quality::Q7.label(),
                            );

                            ui.selectable_value(
                                &mut self.settings.quality,
                                Quality::Q9,
                                Quality::Q9.label(),
                            );
                        });
                    });

                    ui.checkbox(
                        &mut self.settings.recursive,
                        "Обрабатывать вложенные папки",
                    );

                    ui.checkbox(
                        &mut self.settings.overwrite,
                        "Перезаписывать существующие OGG",
                    );

                    ui.horizontal(|ui| {
                        ui.label("Выход:");

                        let text =
                            self.output_directory
                                .as_ref()
                                .map(|p| p.display().to_string())
                                .unwrap_or_else(|| {
                                    "Не выбрана".to_string()
                                });

                        ui.label(
                            RichText::new(text)
                                .weak(),
                        );

                        if ui.button("Выбрать").clicked() {
                            self.select_output();
                        }
                    });
                });

                ui.add_space(12.0);

                // ------------------------------------------------
                // FILE LIST
                // ------------------------------------------------

                ui.heading(
                    format!(
                        "Файлы ({})",
                        self.files.len()
                    ),
                );

                ui.add_space(5.0);

                egui::Frame::group(
                    ui.style(),
                )
                .show(ui, |ui| {
                    ScrollArea::vertical()
                        .max_height(250.0)
                        .show(ui, |ui| {
                            for file in
                                &mut self.files
                            {
                                ui.horizontal(|ui| {
                                    ui.checkbox(
                                        &mut file.selected,
                                        "",
                                    );

                                    ui.label(
                                        file.path
                                            .display()
                                            .to_string(),
                                    );
                                });
                            }
                        });
                });

                ui.add_space(12.0);

                // ------------------------------------------------
                // CONVERT
                // ------------------------------------------------

                let button =
                    egui::Button::new(
                        RichText::new(
                            "▶  Конвертировать в OGG",
                        )
                        .strong(),
                    )
                    .min_size(
                        Vec2::new(
                            ui.available_width(),
                            42.0,
                        ),
                    );

                if ui
                    .add_enabled(
                        !self.converting,
                        button,
                    )
                    .clicked()
                {
                    save_settings(
                        &self.settings,
                    );

                    self.start_conversion();
                }

                ui.add_space(8.0);

                ui.label(
                    RichText::new(
                        &self.status,
                    )
                    .weak(),
                );

                ctx.request_repaint_after(
                    std::time::Duration::from_millis(200),
                );
            });
    }
}

// ============================================================
// MAIN
// ============================================================

fn main() -> eframe::Result<()> {
    let (rgba, width, height) =
        decode_icon();

    let options =
        eframe::NativeOptions {
            viewport:
                egui::ViewportBuilder::default()
                    .with_title(APP_TITLE)
                    .with_inner_size([
                        850.0,
                        700.0,
                    ])
                    .with_min_inner_size([
                        600.0,
                        500.0,
                    ])
                    .with_icon(
                        egui::IconData {
                            rgba,
                            width,
                            height,
                        },
                    ),

            ..Default::default()
        };

    eframe::run_native(
        APP_TITLE,
        options,
        Box::new(|cc| {
            Ok(Box::new(
                App::new(cc),
            ))
        }),
    )
}
