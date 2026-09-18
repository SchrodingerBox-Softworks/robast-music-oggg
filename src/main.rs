mod ffmpeg;

use eframe::egui::{self, Color32, Frame, Margin, RichText, Rounding, ScrollArea, Stroke, Vec2};
use rfd::FileDialog;
use serde::{Deserialize, Serialize};
use std::{
    collections::HashSet,
    env, fs,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, Sender},
        Arc,
    },
    thread,
};

#[cfg(target_os = "windows")]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

const AUDIO_EXTENSIONS: &[&str] = &[
    "mp3", "m4a", "wav", "flac", "aac", "ogg", "opus", "wma", "aiff", "aif", "alac", "ape", "ac3",
    "amr",
];

const ACCENT: Color32 = Color32::from_rgb(98, 168, 255);
const ACCENT_SOFT: Color32 = Color32::from_rgb(64, 112, 186);
const BG: Color32 = Color32::from_rgb(12, 14, 20);
const PANEL: Color32 = Color32::from_rgb(20, 24, 34);
const PANEL_INNER: Color32 = Color32::from_rgb(16, 19, 28);
const STROKE: Color32 = Color32::from_rgb(46, 54, 72);
const TEXT: Color32 = Color32::from_rgb(230, 235, 245);
const MUTED: Color32 = Color32::from_rgb(140, 150, 168);
const OK: Color32 = Color32::from_rgb(92, 214, 148);
const ERR: Color32 = Color32::from_rgb(255, 110, 118);
const WARN: Color32 = Color32::from_rgb(255, 196, 92);

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

    fn label(self) -> &'static str {
        match self {
            Self::Mono => "Моно",
            Self::Stereo => "Стерео",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
enum SampleRate {
    #[default]
    Source,
    Hz44100,
    Hz48000,
}

impl SampleRate {
    fn label(self) -> &'static str {
        match self {
            Self::Source => "Как в исходнике",
            Self::Hz44100 => "44.1 кГц",
            Self::Hz48000 => "48 кГц",
        }
    }

    fn ffmpeg_value(self) -> Option<&'static str> {
        match self {
            Self::Source => None,
            Self::Hz44100 => Some("44100"),
            Self::Hz48000 => Some("48000"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Settings {
    channels: Channels,
    quality: i32,
    recursive: bool,
    overwrite: bool,
    #[serde(default)]
    sample_rate: SampleRate,
    #[serde(default)]
    output_dir: Option<PathBuf>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            channels: Channels::Stereo,
            quality: 5,
            recursive: true,
            overwrite: true,
            sample_rate: SampleRate::Source,
            output_dir: None,
        }
    }
}

impl Settings {
    fn quality_hint(quality: i32) -> &'static str {
        match quality {
            0..=2 => "~64–96 кбит/с · компактно",
            3..=4 => "~112–128 кбит/с · речь / подкаст",
            5..=6 => "~160–192 кбит/с · музыка",
            7..=8 => "~224–256 кбит/с · высокое",
            _ => "~320 кбит/с · почти без потерь",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FileStatus {
    Queued,
    Running,
    Success,
    Skipped,
    Failed,
}

impl FileStatus {
    fn label(self) -> &'static str {
        match self {
            Self::Queued => "в очереди",
            Self::Running => "идёт",
            Self::Success => "готово",
            Self::Skipped => "пропуск",
            Self::Failed => "ошибка",
        }
    }

    fn color(self) -> Color32 {
        match self {
            Self::Queued => MUTED,
            Self::Running => ACCENT,
            Self::Success => OK,
            Self::Skipped => WARN,
            Self::Failed => ERR,
        }
    }
}

#[derive(Debug, Clone)]
struct QueueFile {
    path: PathBuf,
    status: FileStatus,
    message: String,
}

#[derive(Debug, Clone)]
struct AudioFile {
    input: PathBuf,
    output: PathBuf,
}

#[derive(Debug)]
enum WorkerMessage {
    Started {
        total: usize,
    },
    FileStarted {
        path: PathBuf,
    },
    FileFinished {
        path: PathBuf,
        status: FileStatus,
        message: String,
    },
    Finished,
}

struct App {
    files: Vec<QueueFile>,
    output_dir: Option<PathBuf>,
    settings: Settings,
    converting: bool,
    current_file: Option<PathBuf>,
    completed: usize,
    total: usize,
    success_count: usize,
    fail_count: usize,
    skip_count: usize,
    logs: Vec<(Color32, String)>,
    rx: Option<Receiver<WorkerMessage>>,
    cancel: Option<Arc<AtomicBool>>,
    ffmpeg_path: Option<PathBuf>,
    ffmpeg_rx: Option<Receiver<Result<PathBuf, String>>>,
    status: String,
    dragging: bool,
}

impl Default for App {
    fn default() -> Self {
        Self {
            files: Vec::new(),
            output_dir: None,
            settings: Settings::default(),
            converting: false,
            current_file: None,
            completed: 0,
            total: 0,
            success_count: 0,
            fail_count: 0,
            skip_count: 0,
            logs: Vec::new(),
            rx: None,
            cancel: None,
            ffmpeg_path: None,
            ffmpeg_rx: None,
            status: "Готов к работе".to_string(),
            dragging: false,
        }
    }
}

impl App {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        apply_theme(&cc.egui_ctx);

        let mut app = Self::default();
        app.load_settings();
        app.start_ffmpeg_setup();
        app
    }

    fn start_ffmpeg_setup(&mut self) {
        self.status = "Подготовка FFmpeg…".to_string();
        self.log(MUTED, "Готовлю FFmpeg для конвертации…");

        let (tx, rx) = mpsc::channel();
        self.ffmpeg_rx = Some(rx);

        thread::spawn(move || {
            let _ = tx.send(ffmpeg::prepare());
        });
    }

    fn poll_ffmpeg(&mut self) {
        let result = {
            let Some(rx) = &self.ffmpeg_rx else {
                return;
            };
            match rx.try_recv() {
                Ok(result) => result,
                Err(_) => return,
            }
        };

        self.ffmpeg_rx = None;
        match result {
            Ok(path) => {
                self.ffmpeg_path = Some(path);
                if !self.converting {
                    self.status = "Готов к работе".to_string();
                }
                self.log(MUTED, "FFmpeg готов. Можно конвертировать в OGG Vorbis.");
            }
            Err(e) => {
                self.status = "Нет FFmpeg".to_string();
                self.log(ERR, format!("FFmpeg: {e}"));
            }
        }
    }

    fn settings_path() -> PathBuf {
        let exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("."));
        exe.parent().unwrap_or(Path::new(".")).join("settings.json")
    }

    fn load_settings(&mut self) {
        let path = Self::settings_path();
        if let Ok(data) = fs::read_to_string(path) {
            if let Ok(settings) = serde_json::from_str::<Settings>(&data) {
                self.output_dir = settings.output_dir.clone();
                self.settings = settings;
            }
        }
    }

    fn save_settings(&self) {
        let path = Self::settings_path();
        let mut settings = self.settings.clone();
        settings.output_dir = self.output_dir.clone();
        if let Ok(data) = serde_json::to_string_pretty(&settings) {
            let _ = fs::write(path, data);
        }
    }

    fn log(&mut self, color: Color32, message: impl Into<String>) {
        self.logs.push((color, message.into()));
        if self.logs.len() > 400 {
            self.logs.remove(0);
        }
    }

    fn is_audio(path: &Path) -> bool {
        path.extension()
            .and_then(|e| e.to_str())
            .map(|ext| AUDIO_EXTENSIONS.iter().any(|x| x.eq_ignore_ascii_case(ext)))
            .unwrap_or(false)
    }

    fn add_file(&mut self, path: PathBuf) {
        if path.is_file() {
            if !Self::is_audio(&path) {
                return;
            }
            if self.files.iter().any(|f| f.path == path) {
                return;
            }
            self.files.push(QueueFile {
                path,
                status: FileStatus::Queued,
                message: String::new(),
            });
        } else if path.is_dir() {
            self.add_directory(path);
        }
    }

    fn add_directory(&mut self, dir: PathBuf) {
        let mut stack = vec![dir];

        while let Some(current) = stack.pop() {
            let entries = match fs::read_dir(&current) {
                Ok(e) => e,
                Err(e) => {
                    self.log(
                        ERR,
                        format!("Не удалось прочитать {}: {}", current.display(), e),
                    );
                    continue;
                }
            };

            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    if self.settings.recursive {
                        stack.push(path);
                    }
                } else if path.is_file() && Self::is_audio(&path) {
                    self.add_file(path);
                }
            }
        }
    }

    fn build_output_paths(&self, inputs: &[PathBuf]) -> Vec<PathBuf> {
        let mut used: HashSet<PathBuf> = HashSet::new();
        let mut result = Vec::with_capacity(inputs.len());

        for input in inputs {
            let output_dir = self
                .output_dir
                .clone()
                .unwrap_or_else(|| input.parent().unwrap_or(Path::new(".")).to_path_buf());

            let stem = input
                .file_stem()
                .and_then(|x| x.to_str())
                .unwrap_or("output")
                .to_string();

            let mut candidate = output_dir.join(format!("{stem}.ogg"));
            let mut suffix = 1;

            while used.contains(&candidate) {
                candidate = output_dir.join(format!("{stem} ({suffix}).ogg"));
                suffix += 1;
            }

            used.insert(candidate.clone());
            result.push(candidate);
        }

        result
    }

    fn start_conversion(&mut self) {
        if self.converting {
            return;
        }
        if self.files.is_empty() {
            self.log(WARN, "Нет файлов для конвертации.");
            return;
        }

        let ffmpeg = match &self.ffmpeg_path {
            Some(path) => path.clone(),
            None => {
                self.log(ERR, "FFmpeg недоступен.");
                return;
            }
        };

        for file in &mut self.files {
            file.status = FileStatus::Queued;
            file.message.clear();
        }

        let inputs: Vec<PathBuf> = self.files.iter().map(|f| f.path.clone()).collect();
        let outputs = self.build_output_paths(&inputs);
        let files = inputs
            .into_iter()
            .zip(outputs)
            .map(|(input, output)| AudioFile { input, output })
            .collect::<Vec<_>>();

        let channels = self.settings.channels;
        let quality = self.settings.quality;
        let overwrite = self.settings.overwrite;
        let sample_rate = self.settings.sample_rate;

        let (tx, rx) = mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));

        self.rx = Some(rx);
        self.cancel = Some(cancel.clone());
        self.converting = true;
        self.completed = 0;
        self.total = files.len();
        self.success_count = 0;
        self.fail_count = 0;
        self.skip_count = 0;
        self.current_file = None;
        self.status = "Конвертация…".to_string();
        self.log(
            ACCENT,
            format!("Старт: {} файл(ов) → OGG Vorbis Q{quality}", files.len()),
        );

        thread::spawn(move || {
            convert_job(
                ffmpeg,
                files,
                channels,
                quality,
                overwrite,
                sample_rate,
                cancel,
                tx,
            );
        });
    }

    fn request_cancel(&mut self) {
        if let Some(cancel) = &self.cancel {
            cancel.store(true, Ordering::SeqCst);
            self.status = "Остановка…".to_string();
            self.log(WARN, "Остановка после текущего файла.");
        }
    }

    fn poll_conversion(&mut self) {
        let messages: Vec<WorkerMessage> = match &self.rx {
            Some(rx) => {
                let mut messages = Vec::new();
                while let Ok(message) = rx.try_recv() {
                    messages.push(message);
                }
                messages
            }
            None => return,
        };

        for message in messages {
            match message {
                WorkerMessage::Started { total } => {
                    self.total = total;
                }
                WorkerMessage::FileStarted { path } => {
                    self.current_file = Some(path.clone());
                    if let Some(file) = self.files.iter_mut().find(|f| f.path == path) {
                        file.status = FileStatus::Running;
                    }
                    let name = path.file_name().and_then(|x| x.to_str()).unwrap_or("файл");
                    self.status = format!("Конвертация: {name}");
                }
                WorkerMessage::FileFinished {
                    path,
                    status,
                    message,
                } => {
                    self.completed += 1;
                    match status {
                        FileStatus::Success => self.success_count += 1,
                        FileStatus::Failed => self.fail_count += 1,
                        FileStatus::Skipped => self.skip_count += 1,
                        _ => {}
                    }
                    if let Some(file) = self.files.iter_mut().find(|f| f.path == path) {
                        file.status = status;
                        file.message = message.clone();
                    }
                    let color = status.color();
                    let short = path.file_name().and_then(|x| x.to_str()).unwrap_or("файл");
                    self.log(color, format!("{} · {short} · {message}", status.label()));
                }
                WorkerMessage::Finished => {
                    self.converting = false;
                    self.current_file = None;
                    self.cancel = None;
                    self.status = format!(
                        "Готово · ок {}, ошибок {}, пропуск {}",
                        self.success_count, self.fail_count, self.skip_count
                    );
                    self.log(TEXT, "Конвертация завершена.");
                }
            }
        }
    }

    fn open_path(path: &Path) {
        #[cfg(target_os = "windows")]
        {
            let mut cmd = Command::new("explorer");
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(CREATE_NO_WINDOW);
            let _ = cmd.arg(path).spawn();
        }
        #[cfg(target_os = "macos")]
        {
            let _ = Command::new("open").arg(path).spawn();
        }
        #[cfg(target_os = "linux")]
        {
            let _ = Command::new("xdg-open").arg(path).spawn();
        }
    }

    fn output_folder_to_open(&self) -> Option<PathBuf> {
        if let Some(dir) = &self.output_dir {
            return Some(dir.clone());
        }
        self.files
            .iter()
            .find(|f| f.status == FileStatus::Success)
            .and_then(|f| f.path.parent().map(|p| p.to_path_buf()))
            .or_else(|| {
                self.files
                    .first()
                    .and_then(|f| f.path.parent().map(|p| p.to_path_buf()))
            })
    }

    fn ui_header(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.vertical(|ui| {
                ui.label(
                    RichText::new("SCHRÖDINGER")
                        .color(ACCENT)
                        .size(11.0)
                        .strong(),
                );
                ui.label(
                    RichText::new("Конвертер в OGG")
                        .color(TEXT)
                        .size(20.0)
                        .strong(),
                );
            });

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let status_color = if self.ffmpeg_path.is_none() {
                    ERR
                } else if self.converting {
                    ACCENT
                } else if self.fail_count > 0 {
                    ERR
                } else {
                    MUTED
                };
                ui.label(RichText::new(&self.status).color(status_color).size(14.0));
            });
        });
    }

    fn ui_settings(&mut self, ui: &mut egui::Ui) {
        ui.label(RichText::new("Настройки").strong().size(16.0).color(TEXT));
        ui.add_space(12.0);

        ui.label(RichText::new("Каналы").small().color(MUTED));
        ui.horizontal(|ui| {
            for channels in [Channels::Mono, Channels::Stereo] {
                let selected = self.settings.channels == channels;
                let btn = egui::Button::new(channels.label())
                    .selected(selected)
                    .min_size(Vec2::new(96.0, 28.0));
                if ui.add_enabled(!self.converting, btn).clicked() {
                    self.settings.channels = channels;
                }
            }
        });

        ui.add_space(12.0);
        ui.label(RichText::new("Качество").small().color(MUTED));
        ui.add_enabled_ui(!self.converting, |ui| {
            ui.add(egui::Slider::new(&mut self.settings.quality, 0..=10).show_value(true));
        });
        ui.label(
            RichText::new(format!(
                "Q{} · {}",
                self.settings.quality,
                Settings::quality_hint(self.settings.quality)
            ))
            .small()
            .color(MUTED),
        );

        ui.add_space(12.0);
        ui.label(RichText::new("Частота").small().color(MUTED));
        ui.add_enabled_ui(!self.converting, |ui| {
            egui::ComboBox::from_id_salt("sample_rate")
                .width(ui.available_width().min(220.0))
                .selected_text(self.settings.sample_rate.label())
                .show_ui(ui, |ui| {
                    for rate in [SampleRate::Source, SampleRate::Hz44100, SampleRate::Hz48000] {
                        ui.selectable_value(&mut self.settings.sample_rate, rate, rate.label());
                    }
                });
        });

        ui.add_space(16.0);
        ui.add_enabled(
            !self.converting,
            egui::Checkbox::new(&mut self.settings.recursive, "Искать во вложенных папках"),
        );
        ui.add_enabled(
            !self.converting,
            egui::Checkbox::new(&mut self.settings.overwrite, "Перезаписывать готовые OGG"),
        );
    }

    fn ui_files(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label(
                RichText::new(format!("Файлы ({})", self.files.len()))
                    .strong()
                    .size(16.0)
                    .color(TEXT),
            );

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let enabled = !self.converting;

                if ui
                    .add_enabled(
                        enabled && self.files.iter().any(|f| f.status == FileStatus::Failed),
                        egui::Button::new("Убрать ошибки"),
                    )
                    .clicked()
                {
                    self.files.retain(|f| f.status != FileStatus::Failed);
                }

                if ui
                    .add_enabled(
                        enabled && !self.files.is_empty(),
                        egui::Button::new("Очистить"),
                    )
                    .clicked()
                {
                    self.files.clear();
                    self.total = 0;
                    self.completed = 0;
                    self.success_count = 0;
                    self.fail_count = 0;
                    self.skip_count = 0;
                }

                if ui
                    .add_enabled(enabled, egui::Button::new("Папка…"))
                    .on_hover_text("Добавить аудио из папки")
                    .clicked()
                {
                    if let Some(folder) = FileDialog::new().pick_folder() {
                        self.add_directory(folder);
                    }
                }

                if ui
                    .add_enabled(enabled, egui::Button::new("Файлы…"))
                    .on_hover_text("Добавить отдельные файлы")
                    .clicked()
                {
                    if let Some(files) = FileDialog::new()
                        .add_filter("Аудио", AUDIO_EXTENSIONS)
                        .pick_files()
                    {
                        for file in files {
                            self.add_file(file);
                        }
                    }
                }
            });
        });

        if self.total > 0 {
            ui.label(
                RichText::new(format!(
                    "готово {} · ошибок {} · пропуск {}",
                    self.success_count, self.fail_count, self.skip_count
                ))
                .small()
                .color(MUTED),
            );
        }

        ui.add_space(8.0);

        if self.files.is_empty() {
            let drop_fill = if self.dragging {
                Color32::from_rgba_unmultiplied(98, 168, 255, 28)
            } else {
                PANEL_INNER
            };
            Frame::none()
                .fill(drop_fill)
                .stroke(Stroke::new(
                    1.0,
                    if self.dragging { ACCENT } else { STROKE },
                ))
                .rounding(Rounding::same(10.0))
                .inner_margin(Margin::same(24.0))
                .show(ui, |ui| {
                    ui.set_min_height((ui.available_height() - 8.0).max(120.0));
                    ui.vertical_centered(|ui| {
                        ui.add_space(ui.available_height() * 0.28);
                        ui.label(
                            RichText::new("Перетащите аудио сюда")
                                .size(18.0)
                                .color(if self.dragging { ACCENT } else { TEXT }),
                        );
                        ui.label(RichText::new("или нажмите «Файлы…» / «Папка…»").color(MUTED));
                    });
                });
        } else {
            ScrollArea::vertical()
                .id_salt("file_list")
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    let mut remove = None;
                    for (index, file) in self.files.iter().enumerate() {
                        let name = file
                            .path
                            .file_name()
                            .and_then(|x| x.to_str())
                            .unwrap_or("unknown")
                            .to_string();
                        let parent = file
                            .path
                            .parent()
                            .unwrap_or(Path::new(""))
                            .display()
                            .to_string();

                        Frame::none()
                            .fill(PANEL_INNER)
                            .rounding(Rounding::same(8.0))
                            .inner_margin(Margin::symmetric(10.0, 6.0))
                            .stroke(Stroke::new(1.0, STROKE))
                            .show(ui, |ui| {
                                ui.horizontal(|ui| {
                                    ui.label(
                                        RichText::new(format!("{:02}", index + 1))
                                            .color(MUTED)
                                            .monospace(),
                                    );
                                    ui.label(RichText::new(&name).color(TEXT))
                                        .on_hover_text(&parent);
                                    ui.with_layout(
                                        egui::Layout::right_to_left(egui::Align::Center),
                                        |ui| {
                                            if !self.converting
                                                && ui
                                                    .small_button("✕")
                                                    .on_hover_text("Убрать из списка")
                                                    .clicked()
                                            {
                                                remove = Some(index);
                                            }
                                            ui.label(
                                                RichText::new(file.status.label())
                                                    .small()
                                                    .color(file.status.color()),
                                            );
                                        },
                                    );
                                });
                            })
                            .response
                            .on_hover_text(if file.message.is_empty() {
                                parent
                            } else {
                                format!("{parent}\n{}", file.message)
                            });
                        ui.add_space(4.0);
                    }
                    if let Some(index) = remove {
                        self.files.remove(index);
                    }
                });
        }
    }

    fn ui_destination(&mut self, ui: &mut egui::Ui) {
        ui.label(RichText::new("Сохранять OGG").small().color(MUTED));
        ui.horizontal(|ui| {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .add_enabled(
                        self.output_folder_to_open().is_some(),
                        egui::Button::new("Открыть"),
                    )
                    .on_hover_text("Открыть папку с результатом")
                    .clicked()
                {
                    if let Some(dir) = self.output_folder_to_open() {
                        Self::open_path(&dir);
                    }
                }

                if ui
                    .add_enabled(
                        !self.converting && self.output_dir.is_some(),
                        egui::Button::new("Как у исходника"),
                    )
                    .on_hover_text("Писать OGG рядом с каждым исходным файлом")
                    .clicked()
                {
                    self.output_dir = None;
                    self.save_settings();
                }

                if ui
                    .add_enabled(!self.converting, egui::Button::new("Выбрать…"))
                    .on_hover_text("Все файлы в одну папку")
                    .clicked()
                {
                    if let Some(folder) = FileDialog::new().pick_folder() {
                        self.output_dir = Some(folder);
                        self.save_settings();
                    }
                }

                let text = self
                    .output_dir
                    .as_ref()
                    .map(|x| x.display().to_string())
                    .unwrap_or_else(|| "Рядом с каждым исходным файлом".to_string());

                Frame::none()
                    .fill(PANEL_INNER)
                    .rounding(Rounding::same(8.0))
                    .inner_margin(Margin::symmetric(10.0, 6.0))
                    .stroke(Stroke::new(1.0, STROKE))
                    .show(ui, |ui| {
                        ui.set_min_width(ui.available_width());
                        ui.label(RichText::new(ellipsize(&text, 72)).color(TEXT))
                            .on_hover_text(&text);
                    });
            });
        });
    }

    fn ui_logs(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label(RichText::new("Журнал").strong().size(14.0).color(TEXT));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.small_button("Очистить").clicked() {
                    self.logs.clear();
                }
            });
        });
        ui.add_space(4.0);

        Frame::none()
            .fill(PANEL_INNER)
            .rounding(Rounding::same(8.0))
            .inner_margin(Margin::same(8.0))
            .stroke(Stroke::new(1.0, STROKE))
            .show(ui, |ui| {
                ScrollArea::vertical()
                    .id_salt("logs")
                    .max_height(132.0)
                    .min_scrolled_height(132.0)
                    .stick_to_bottom(true)
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        if self.logs.is_empty() {
                            ui.label(
                                RichText::new("Пока пусто — события конвертации появятся здесь.")
                                    .color(MUTED),
                            );
                        } else {
                            for (color, line) in &self.logs {
                                ui.add(
                                    egui::Label::new(
                                        RichText::new(line).color(*color).monospace().size(12.0),
                                    )
                                    .wrap(),
                                );
                            }
                        }
                    });
            });
    }

    fn ui_actions(&mut self, ui: &mut egui::Ui) {
        let progress = if self.total == 0 {
            0.0
        } else {
            self.completed as f32 / self.total as f32
        };

        ui.horizontal(|ui| {
            let can_convert =
                !self.converting && !self.files.is_empty() && self.ffmpeg_path.is_some();

            let convert = ui.add_enabled(
                can_convert,
                egui::Button::new(RichText::new("Конвертировать").strong().size(16.0))
                    .fill(if can_convert {
                        ACCENT_SOFT
                    } else {
                        PANEL_INNER
                    })
                    .min_size(Vec2::new(200.0, 40.0)),
            );
            if convert.clicked() {
                self.save_settings();
                self.start_conversion();
            }

            if self.converting
                && ui
                    .add(
                        egui::Button::new("Стоп")
                            .fill(Color32::from_rgb(90, 40, 48))
                            .min_size(Vec2::new(88.0, 40.0)),
                    )
                    .clicked()
            {
                self.request_cancel();
            }

            ui.add(
                egui::ProgressBar::new(progress)
                    .desired_width(ui.available_width())
                    .desired_height(18.0)
                    .text(if self.total == 0 {
                        "Добавьте файлы".to_string()
                    } else {
                        format!("{} / {}", self.completed, self.total)
                    })
                    .animate(self.converting),
            );
        });
    }

    fn handle_dropped_files(&mut self, ctx: &egui::Context) {
        self.dragging = ctx.input(|input| !input.raw.hovered_files.is_empty());
        if self.converting {
            return;
        }
        let dropped_files = ctx.input(|input| input.raw.dropped_files.clone());
        for file in dropped_files {
            if let Some(path) = file.path {
                self.add_file(path);
            }
        }
    }
}

fn apply_theme(ctx: &egui::Context) {
    ctx.set_pixels_per_point(ctx.pixels_per_point().max(1.0));

    let mut visuals = egui::Visuals::dark();
    visuals.dark_mode = true;
    visuals.window_fill = BG;
    visuals.panel_fill = BG;
    visuals.extreme_bg_color = PANEL_INNER;
    visuals.faint_bg_color = PANEL;
    visuals.override_text_color = Some(TEXT);
    visuals.hyperlink_color = ACCENT;
    visuals.selection.bg_fill = Color32::from_rgb(48, 92, 168);
    visuals.widgets.noninteractive.fg_stroke = Stroke::new(1.0, MUTED);
    visuals.widgets.inactive.bg_fill = Color32::from_rgb(34, 40, 54);
    visuals.widgets.inactive.weak_bg_fill = Color32::from_rgb(34, 40, 54);
    visuals.widgets.inactive.fg_stroke = Stroke::new(1.0, TEXT);
    visuals.widgets.inactive.rounding = Rounding::same(8.0);
    visuals.widgets.hovered.bg_fill = Color32::from_rgb(48, 62, 88);
    visuals.widgets.hovered.weak_bg_fill = Color32::from_rgb(48, 62, 88);
    visuals.widgets.hovered.fg_stroke = Stroke::new(1.0, TEXT);
    visuals.widgets.hovered.rounding = Rounding::same(8.0);
    visuals.widgets.active.bg_fill = ACCENT_SOFT;
    visuals.widgets.active.rounding = Rounding::same(8.0);
    visuals.widgets.open.bg_fill = Color32::from_rgb(40, 50, 70);
    visuals.widgets.open.rounding = Rounding::same(8.0);
    visuals.widgets.open.fg_stroke = Stroke::new(1.0, TEXT);
    visuals.widgets.inactive.bg_stroke = Stroke::new(1.0, STROKE);
    visuals.widgets.hovered.bg_stroke = Stroke::new(1.0, ACCENT);
    ctx.set_visuals(visuals);

    ctx.style_mut(|style| {
        style.spacing.item_spacing = Vec2::new(8.0, 8.0);
        style.spacing.button_padding = Vec2::new(12.0, 7.0);
        style.spacing.window_margin = Margin::same(0.0);
        style.visuals.window_rounding = Rounding::same(12.0);
    });
}

fn ellipsize(text: &str, max_chars: usize) -> String {
    let count = text.chars().count();
    if count <= max_chars {
        return text.to_string();
    }
    let skip = count.saturating_sub(max_chars.saturating_sub(1));
    format!("…{}", text.chars().skip(skip).collect::<String>())
}

fn prepend_path(dir: &Path) {
    if let Some(old) = env::var_os("PATH") {
        let mut paths = env::split_paths(&old).collect::<Vec<_>>();
        paths.insert(0, dir.to_path_buf());
        if let Ok(joined) = env::join_paths(paths) {
            env::set_var("PATH", joined);
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn convert_job(
    ffmpeg: PathBuf,
    files: Vec<AudioFile>,
    channels: Channels,
    quality: i32,
    overwrite: bool,
    sample_rate: SampleRate,
    cancel: Arc<AtomicBool>,
    tx: Sender<WorkerMessage>,
) {
    let _ = tx.send(WorkerMessage::Started { total: files.len() });

    if let Some(parent) = ffmpeg.parent() {
        prepend_path(parent);
    }

    for file in &files {
        if cancel.load(Ordering::SeqCst) {
            let _ = tx.send(WorkerMessage::FileFinished {
                path: file.input.clone(),
                status: FileStatus::Skipped,
                message: "Отменено".to_string(),
            });
            continue;
        }

        let _ = tx.send(WorkerMessage::FileStarted {
            path: file.input.clone(),
        });

        if let Some(parent) = file.output.parent() {
            if let Err(e) = fs::create_dir_all(parent) {
                let _ = tx.send(WorkerMessage::FileFinished {
                    path: file.input.clone(),
                    status: FileStatus::Failed,
                    message: format!("Нет папки {}: {e}", parent.display()),
                });
                continue;
            }
        }

        if file.output.exists() && !overwrite {
            let _ = tx.send(WorkerMessage::FileFinished {
                path: file.input.clone(),
                status: FileStatus::Skipped,
                message: format!("уже есть {}", file.output.display()),
            });
            continue;
        }

        match run_ffmpeg(
            &ffmpeg,
            &file.input,
            &file.output,
            channels,
            quality,
            sample_rate,
        ) {
            Ok(()) => {
                let _ = tx.send(WorkerMessage::FileFinished {
                    path: file.input.clone(),
                    status: FileStatus::Success,
                    message: file
                        .output
                        .file_name()
                        .and_then(|n| n.to_str())
                        .unwrap_or("ogg")
                        .to_string(),
                });
            }
            Err(message) => {
                let _ = tx.send(WorkerMessage::FileFinished {
                    path: file.input.clone(),
                    status: FileStatus::Failed,
                    message,
                });
            }
        }
    }

    let _ = tx.send(WorkerMessage::Finished);
}

fn run_ffmpeg(
    ffmpeg: &Path,
    input: &Path,
    output: &Path,
    channels: Channels,
    quality: i32,
    sample_rate: SampleRate,
) -> Result<(), String> {
    let mut command = Command::new(ffmpeg);

    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(CREATE_NO_WINDOW);
    }

    if let Some(parent) = ffmpeg.parent() {
        command.current_dir(parent);
    }

    command
        .arg("-hide_banner")
        .arg("-nostdin")
        .arg("-loglevel")
        .arg("error")
        .arg("-y")
        .arg("-fflags")
        .arg("+genpts+discardcorrupt")
        .arg("-err_detect")
        .arg("ignore_err")
        .arg("-i")
        .arg(input)
        // Только первый аудиопоток: обложка MP3 (MJPEG/PNG) ломает контейнер OGG.
        .arg("-map")
        .arg("0:a:0")
        .arg("-vn")
        .arg("-sn")
        .arg("-dn")
        .arg("-c:a")
        .arg("libvorbis")
        .arg("-q:a")
        .arg(quality.to_string())
        .arg("-ac")
        .arg(channels.ffmpeg_value())
        .arg("-map_metadata")
        .arg("0");

    if let Some(rate) = sample_rate.ffmpeg_value() {
        command.arg("-ar").arg(rate);
    }

    command
        .arg("-f")
        .arg("ogg")
        .arg(output)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let output = command
        .output()
        .map_err(|e| format!("Не удалось запустить FFmpeg: {e}"))?;

    if output.status.success() {
        return Ok(());
    }

    let stderr_text = String::from_utf8_lossy(&output.stderr).trim().to_string();
    let stdout_text = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let code = output
        .status
        .code()
        .map(|c| c.to_string())
        .unwrap_or_else(|| "сигнал".to_string());

    let mut message = format!("код {code}");
    if !stderr_text.is_empty() {
        message.push_str(&format!(" · {stderr_text}"));
    }
    if !stdout_text.is_empty() {
        message.push_str(&format!(" · {stdout_text}"));
    }
    if stderr_text.is_empty() && stdout_text.is_empty() {
        message.push_str(
            " · FFmpeg ничего не вывел (файл занят, нет прав или антивирус перехватил процесс)",
        );
    }
    Err(message)
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll_ffmpeg();
        self.poll_conversion();
        self.handle_dropped_files(ctx);

        egui::TopBottomPanel::top("header")
            .frame(
                Frame::none()
                    .fill(PANEL)
                    .inner_margin(Margin::symmetric(20.0, 12.0))
                    .stroke(Stroke::new(1.0, STROKE)),
            )
            .show(ctx, |ui| {
                self.ui_header(ui);
            });

        egui::TopBottomPanel::bottom("footer")
            .resizable(false)
            .frame(
                Frame::none()
                    .fill(PANEL)
                    .inner_margin(Margin::symmetric(16.0, 12.0))
                    .stroke(Stroke::new(1.0, STROKE)),
            )
            .show(ctx, |ui| {
                self.ui_destination(ui);
                ui.add_space(10.0);
                self.ui_actions(ui);
                ui.add_space(10.0);
                self.ui_logs(ui);
            });

        egui::SidePanel::right("settings")
            .resizable(false)
            .exact_width(280.0)
            .frame(
                Frame::none()
                    .fill(PANEL)
                    .inner_margin(Margin::same(16.0))
                    .stroke(Stroke::new(1.0, STROKE)),
            )
            .show(ctx, |ui| {
                self.ui_settings(ui);
            });

        egui::CentralPanel::default()
            .frame(Frame::none().fill(BG).inner_margin(Margin::same(16.0)))
            .show(ctx, |ui| {
                self.ui_files(ui);
            });

        if self.converting || self.dragging || self.ffmpeg_rx.is_some() {
            ctx.request_repaint();
        }
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.save_settings();
    }
}

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1120.0, 780.0])
            .with_min_inner_size([880.0, 620.0])
            .with_title("Schrodinger Audio Converter"),
        ..Default::default()
    };

    eframe::run_native(
        "Schrodinger Audio Converter",
        options,
        Box::new(|cc| Ok(Box::new(App::new(cc)))),
    )
}
