mod ffmpeg;

use eframe::egui::{
    self, Align, Align2, Color32, FontId, Frame, Key, Layout, Margin, Modifiers, RichText,
    Rounding, ScrollArea, Sense, Stroke, TextStyle, Vec2,
};
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
    time::Duration,
};

#[cfg(target_os = "windows")]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

const AUDIO_EXTENSIONS: &[&str] = &[
    "mp3", "m4a", "wav", "flac", "aac", "ogg", "opus", "wma", "aiff", "aif", "alac", "ape", "ac3",
    "amr",
];

const ACCENT: Color32 = Color32::from_rgb(98, 168, 255);
const ACCENT_SOFT: Color32 = Color32::from_rgb(52, 104, 182);
const BG: Color32 = Color32::from_rgb(12, 14, 20);
const PANEL: Color32 = Color32::from_rgb(20, 24, 34);
const PANEL_INNER: Color32 = Color32::from_rgb(16, 19, 28);
const ROW_HOVER: Color32 = Color32::from_rgb(28, 34, 48);
const SELECTED_BG: Color32 = Color32::from_rgb(30, 50, 84);
const STROKE: Color32 = Color32::from_rgb(46, 54, 72);
const TEXT: Color32 = Color32::from_rgb(230, 235, 245);
const MUTED: Color32 = Color32::from_rgb(140, 150, 168);
const OK: Color32 = Color32::from_rgb(92, 214, 148);
const ERR: Color32 = Color32::from_rgb(255, 110, 118);
const WARN: Color32 = Color32::from_rgb(255, 196, 92);
const STOP_BG: Color32 = Color32::from_rgb(110, 44, 54);

/// Готовые уровни качества: (значение -q:a, название).
const QUALITY_PRESETS: &[(i32, &str)] = &[
    (2, "Компактно"),
    (4, "Речь, подкасты"),
    (6, "Музыка"),
    (8, "Высокое"),
    (10, "Максимум"),
];

const NOTICE_SECONDS: f64 = 6.0;

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

    fn hint(self) -> &'static str {
        match self {
            Self::Mono => "Один канал: меньше размер, подходит для речи",
            Self::Stereo => "Два канала: для музыки",
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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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
            quality: 6,
            recursive: true,
            overwrite: true,
            sample_rate: SampleRate::Source,
            output_dir: None,
        }
    }
}

impl Settings {
    /// Номинальный битрейт libvorbis для стерео 44.1 кГц.
    fn quality_kbps(quality: i32) -> u32 {
        match quality {
            i32::MIN..=0 => 64,
            1 => 80,
            2 => 96,
            3 => 112,
            4 => 128,
            5 => 160,
            6 => 192,
            7 => 224,
            8 => 256,
            9 => 320,
            _ => 500,
        }
    }

    fn quality_hint(quality: i32) -> String {
        let kbps = Self::quality_kbps(quality);
        let mb_per_min = kbps as f32 * 60.0 / 8.0 / 1024.0;
        format!("≈{kbps} кбит/с · ≈{mb_per_min:.1} МБ за минуту")
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
            Self::Queued => "ожидает",
            Self::Running => "конвертируется…",
            Self::Success => "готово",
            Self::Skipped => "пропущен",
            Self::Failed => "ошибка",
        }
    }

    fn icon(self) -> &'static str {
        match self {
            Self::Queued => "○",
            Self::Running => "▶",
            Self::Success => "✔",
            Self::Skipped => "⏭",
            Self::Failed => "✖",
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

enum FfmpegState {
    Preparing,
    Ready(PathBuf),
    Failed(String),
}

#[derive(Default)]
struct Stats {
    queued: usize,
    success: usize,
    skipped: usize,
    failed: usize,
}

struct Notice {
    color: Color32,
    text: String,
    until: f64,
}

struct App {
    files: Vec<QueueFile>,
    settings: Settings,
    saved_settings: Settings,
    converting: bool,
    stopping: bool,
    current_file: Option<PathBuf>,
    completed: usize,
    total: usize,
    logs: Vec<(Color32, String)>,
    log_open: bool,
    rx: Option<Receiver<WorkerMessage>>,
    cancel: Option<Arc<AtomicBool>>,
    ffmpeg: FfmpegState,
    ffmpeg_rx: Option<Receiver<Result<PathBuf, String>>>,
    notice: Option<Notice>,
    pending_notice: Option<(Color32, String)>,
    dragging: bool,
}

impl Default for App {
    fn default() -> Self {
        Self {
            files: Vec::new(),
            settings: Settings::default(),
            saved_settings: Settings::default(),
            converting: false,
            stopping: false,
            current_file: None,
            completed: 0,
            total: 0,
            logs: Vec::new(),
            log_open: false,
            rx: None,
            cancel: None,
            ffmpeg: FfmpegState::Preparing,
            ffmpeg_rx: None,
            notice: None,
            pending_notice: None,
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

    fn ffmpeg_path(&self) -> Option<&PathBuf> {
        match &self.ffmpeg {
            FfmpegState::Ready(path) => Some(path),
            _ => None,
        }
    }

    fn start_ffmpeg_setup(&mut self) {
        self.ffmpeg = FfmpegState::Preparing;
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
                self.ffmpeg = FfmpegState::Ready(path);
                self.log(MUTED, "FFmpeg готов.");
            }
            Err(e) => {
                self.log(ERR, format!("FFmpeg: {e}"));
                self.ffmpeg = FfmpegState::Failed(e);
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
                self.settings = settings;
            }
        }
        self.saved_settings = self.settings.clone();
    }

    fn save_settings(&mut self) {
        if self.settings == self.saved_settings {
            return;
        }
        if let Ok(data) = serde_json::to_string_pretty(&self.settings) {
            if fs::write(Self::settings_path(), data).is_ok() {
                self.saved_settings = self.settings.clone();
            }
        }
    }

    fn log(&mut self, color: Color32, message: impl Into<String>) {
        self.logs.push((color, message.into()));
        if self.logs.len() > 400 {
            self.logs.remove(0);
        }
    }

    fn notify(&mut self, color: Color32, text: impl Into<String>) {
        // Время кадра доступно только в update, поэтому показ откладывается до него.
        self.pending_notice = Some((color, text.into()));
    }

    fn stats(&self) -> Stats {
        let mut stats = Stats::default();
        for file in &self.files {
            match file.status {
                FileStatus::Queued | FileStatus::Running => stats.queued += 1,
                FileStatus::Success => stats.success += 1,
                FileStatus::Skipped => stats.skipped += 1,
                FileStatus::Failed => stats.failed += 1,
            }
        }
        stats
    }

    fn is_audio(path: &Path) -> bool {
        path.extension()
            .and_then(|e| e.to_str())
            .map(|ext| AUDIO_EXTENSIONS.iter().any(|x| x.eq_ignore_ascii_case(ext)))
            .unwrap_or(false)
    }

    /// Возвращает true, если файл добавлен в очередь.
    fn push_file(&mut self, path: PathBuf) -> bool {
        if self.files.iter().any(|f| f.path == path) {
            return false;
        }
        self.files.push(QueueFile {
            path,
            status: FileStatus::Queued,
            message: String::new(),
        });
        true
    }

    fn add_paths(&mut self, paths: Vec<PathBuf>) {
        let mut added = 0;
        let mut not_audio = 0;
        let mut duplicates = 0;
        let mut empty_dirs = 0;

        for path in paths {
            if path.is_dir() {
                let found = self.add_directory(path);
                if found == 0 {
                    empty_dirs += 1;
                }
                added += found;
            } else if path.is_file() {
                if !Self::is_audio(&path) {
                    not_audio += 1;
                } else if self.push_file(path) {
                    added += 1;
                } else {
                    duplicates += 1;
                }
            }
        }

        let mut parts = Vec::new();
        if added > 0 {
            parts.push(format!("Добавлено: {added}"));
        }
        if duplicates > 0 {
            parts.push(format!("уже в списке: {duplicates}"));
        }
        if not_audio > 0 {
            parts.push(format!("не аудио: {not_audio}"));
        }
        if empty_dirs > 0 && added == 0 {
            parts.push(if self.settings.recursive {
                "в папке нет аудиофайлов".to_string()
            } else {
                "в папке нет аудио (включите «С подпапками»)".to_string()
            });
        }
        if parts.is_empty() {
            return;
        }
        let color = if added > 0 { OK } else { WARN };
        let mut text = parts.join(" · ");
        if let Some(first) = text.get(0..1) {
            text = first.to_uppercase() + &text[1..];
        }
        self.notify(color, text);
    }

    /// Добавляет аудио из папки, возвращает число новых файлов.
    fn add_directory(&mut self, dir: PathBuf) -> usize {
        let mut stack = vec![dir];
        let mut found = Vec::new();

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
                    found.push(path);
                }
            }
        }

        found.sort();
        found
            .into_iter()
            .filter(|p| self.push_file(p.clone()))
            .count()
    }

    fn pick_files(&mut self) {
        if self.converting {
            return;
        }
        if let Some(files) = FileDialog::new()
            .add_filter("Аудио", AUDIO_EXTENSIONS)
            .pick_files()
        {
            self.add_paths(files);
        }
    }

    fn pick_folder(&mut self) {
        if self.converting {
            return;
        }
        if let Some(folder) = FileDialog::new().pick_folder() {
            self.add_paths(vec![folder]);
        }
    }

    fn build_output_paths(&self, inputs: &[PathBuf]) -> Vec<PathBuf> {
        let mut used: HashSet<PathBuf> = HashSet::new();
        let mut result = Vec::with_capacity(inputs.len());

        for input in inputs {
            let output_dir = self
                .settings
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

    /// Сколько файлов уйдёт в работу по кнопке «Конвертировать».
    fn files_to_convert(&self) -> usize {
        let pending = self
            .files
            .iter()
            .filter(|f| f.status != FileStatus::Success)
            .count();
        if pending == 0 {
            self.files.len()
        } else {
            pending
        }
    }

    fn start_conversion(&mut self) {
        if self.converting {
            return;
        }
        if self.files.is_empty() {
            self.notify(WARN, "Сначала добавьте файлы");
            return;
        }

        let Some(ffmpeg) = self.ffmpeg_path().cloned() else {
            self.notify(ERR, "FFmpeg ещё не готов");
            return;
        };

        self.save_settings();

        // Пути считаются по всему списку, чтобы имена не менялись между запусками.
        let inputs: Vec<PathBuf> = self.files.iter().map(|f| f.path.clone()).collect();
        let outputs = self.build_output_paths(&inputs);

        // Готовые файлы повторно не конвертируются, если только всё уже не готово.
        let redo_all = self.files.iter().all(|f| f.status == FileStatus::Success);
        let mut jobs = Vec::new();
        for (file, output) in self.files.iter_mut().zip(outputs) {
            if redo_all || file.status != FileStatus::Success {
                file.status = FileStatus::Queued;
                file.message.clear();
                jobs.push(AudioFile {
                    input: file.path.clone(),
                    output,
                });
            }
        }

        let channels = self.settings.channels;
        let quality = self.settings.quality;
        let overwrite = self.settings.overwrite;
        let sample_rate = self.settings.sample_rate;

        let (tx, rx) = mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));

        self.rx = Some(rx);
        self.cancel = Some(cancel.clone());
        self.converting = true;
        self.stopping = false;
        self.completed = 0;
        self.total = jobs.len();
        self.current_file = None;
        self.log(
            ACCENT,
            format!(
                "Старт: {} файл(ов) → OGG Vorbis Q{quality}, {}",
                jobs.len(),
                channels.label().to_lowercase()
            ),
        );

        thread::spawn(move || {
            convert_job(
                ffmpeg,
                jobs,
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
            self.stopping = true;
            self.log(WARN, "Остановка после текущего файла.");
        }
    }

    fn poll_conversion(&mut self) {
        let messages: Vec<WorkerMessage> = match &self.rx {
            Some(rx) => rx.try_iter().collect(),
            None => return,
        };

        for message in messages {
            match message {
                WorkerMessage::Started { total } => {
                    self.total = total;
                }
                WorkerMessage::FileStarted { path } => {
                    if let Some(file) = self.files.iter_mut().find(|f| f.path == path) {
                        file.status = FileStatus::Running;
                    }
                    self.current_file = Some(path);
                }
                WorkerMessage::FileFinished {
                    path,
                    status,
                    message,
                } => {
                    self.completed += 1;
                    if let Some(file) = self.files.iter_mut().find(|f| f.path == path) {
                        file.status = status;
                        file.message = message.clone();
                    }
                    let short = file_name(&path);
                    self.log(
                        status.color(),
                        format!("{} · {short} · {message}", status.label()),
                    );
                }
                WorkerMessage::Finished => {
                    self.converting = false;
                    self.stopping = false;
                    self.current_file = None;
                    self.cancel = None;
                    self.rx = None;
                    let stats = self.stats();
                    self.log(
                        TEXT,
                        format!(
                            "Завершено: готово {}, ошибок {}, пропущено {}.",
                            stats.success, stats.failed, stats.skipped
                        ),
                    );
                    if stats.failed > 0 {
                        self.notify(
                            ERR,
                            format!("Ошибок: {}. Причина указана под именем файла", stats.failed),
                        );
                    }
                    return;
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
        if let Some(dir) = &self.settings.output_dir {
            return Some(dir.clone());
        }
        self.files
            .iter()
            .find(|f| f.status == FileStatus::Success)
            .or_else(|| self.files.first())
            .and_then(|f| f.path.parent().map(|p| p.to_path_buf()))
    }

    fn handle_shortcuts(&mut self, ctx: &egui::Context) {
        let (open_folder, open_files, convert, stop) = ctx.input_mut(|i| {
            (
                i.consume_key(Modifiers::COMMAND | Modifiers::SHIFT, Key::O),
                i.consume_key(Modifiers::COMMAND, Key::O),
                i.consume_key(Modifiers::COMMAND, Key::Enter),
                i.consume_key(Modifiers::NONE, Key::Escape),
            )
        });
        if open_folder {
            self.pick_folder();
        } else if open_files {
            self.pick_files();
        }
        if convert {
            self.start_conversion();
        }
        if stop && self.converting && !self.stopping {
            self.request_cancel();
        }
    }

    fn handle_dropped_files(&mut self, ctx: &egui::Context) {
        self.dragging = ctx.input(|input| !input.raw.hovered_files.is_empty());
        let dropped: Vec<PathBuf> = ctx.input(|input| {
            input
                .raw
                .dropped_files
                .iter()
                .filter_map(|f| f.path.clone())
                .collect()
        });
        if dropped.is_empty() {
            return;
        }
        if self.converting {
            self.notify(WARN, "Во время конвертации добавлять файлы нельзя");
            return;
        }
        self.add_paths(dropped);
    }

    // ───────────────────────────── UI ─────────────────────────────

    fn ui_header(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label(
                RichText::new("Конвертер в OGG")
                    .color(TEXT)
                    .size(20.0)
                    .strong(),
            );
            ui.label(RichText::new("Vorbis · Schrödinger").color(MUTED));

            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let mut retry = false;
                match &self.ffmpeg {
                    FfmpegState::Preparing => {
                        ui.label(RichText::new("Подготовка FFmpeg…").color(MUTED));
                        ui.spinner();
                    }
                    FfmpegState::Ready(path) => {
                        ui.label(RichText::new("● FFmpeg готов").color(OK))
                            .on_hover_text(path.display().to_string());
                    }
                    FfmpegState::Failed(error) => {
                        retry = ui
                            .button("Повторить")
                            .on_hover_text("Снова попробовать найти или скачать FFmpeg")
                            .clicked();
                        ui.label(RichText::new("✖ FFmpeg недоступен").color(ERR))
                            .on_hover_text(error);
                    }
                }
                if retry {
                    self.start_ffmpeg_setup();
                }
            });
        });
    }

    fn ui_settings(&mut self, ui: &mut egui::Ui) {
        let enabled = !self.converting;
        ui.add_enabled_ui(enabled, |ui| {
            step_title(ui, "2", "Качество");
            ui.add_space(4.0);

            for &(quality, title) in QUALITY_PRESETS {
                let selected = self.settings.quality == quality;
                let kbps = format!("≈{} кбит/с", Settings::quality_kbps(quality));
                if choice_row(ui, selected, title, &kbps).clicked() {
                    self.settings.quality = quality;
                }
            }

            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.label(RichText::new("Точно:").color(MUTED));
                ui.add(
                    egui::Slider::new(&mut self.settings.quality, 0..=10)
                        .prefix("Q")
                        .clamping(egui::SliderClamping::Always),
                )
                .on_hover_text("Чем выше Q, тем лучше звук и больше файл");
            });
            ui.label(
                RichText::new(Settings::quality_hint(self.settings.quality))
                    .small()
                    .color(MUTED),
            );

            ui.add_space(10.0);
            ui.label(RichText::new("Каналы").color(MUTED));
            ui.horizontal(|ui| {
                let width = (ui.available_width() - ui.spacing().item_spacing.x) / 2.0;
                for channels in [Channels::Mono, Channels::Stereo] {
                    let selected = self.settings.channels == channels;
                    if ui
                        .add(
                            egui::Button::new(channels.label())
                                .selected(selected)
                                .min_size(Vec2::new(width, 30.0)),
                        )
                        .on_hover_text(channels.hint())
                        .clicked()
                    {
                        self.settings.channels = channels;
                    }
                }
            });

            ui.add_space(6.0);
            ui.label(RichText::new("Частота дискретизации").color(MUTED));
            egui::ComboBox::from_id_salt("sample_rate")
                .width(ui.available_width())
                .selected_text(self.settings.sample_rate.label())
                .show_ui(ui, |ui| {
                    for rate in [SampleRate::Source, SampleRate::Hz44100, SampleRate::Hz48000] {
                        ui.selectable_value(&mut self.settings.sample_rate, rate, rate.label());
                    }
                })
                .response
                .on_hover_text("Если не уверены — оставьте «Как в исходнике»");

            ui.add_space(18.0);
            step_title(ui, "3", "Куда сохранить");
            ui.add_space(4.0);

            let custom = self.settings.output_dir.is_some();
            if choice_row(ui, !custom, "Рядом с исходными файлами", "").clicked()
            {
                self.settings.output_dir = None;
            }
            let custom_clicked = choice_row(ui, custom, "В одну папку…", "").clicked();
            if custom_clicked {
                if let Some(folder) = FileDialog::new().pick_folder() {
                    self.settings.output_dir = Some(folder);
                }
            }

            if let Some(dir) = self.settings.output_dir.clone() {
                let text = dir.display().to_string();
                Frame::none()
                    .fill(PANEL_INNER)
                    .rounding(Rounding::same(6.0))
                    .inner_margin(Margin::symmetric(8.0, 6.0))
                    .stroke(Stroke::new(1.0, STROKE))
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.horizontal(|ui| {
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                if ui
                                    .small_button("Изменить")
                                    .on_hover_text("Выбрать другую папку")
                                    .clicked()
                                {
                                    if let Some(folder) = FileDialog::new().pick_folder() {
                                        self.settings.output_dir = Some(folder);
                                    }
                                }
                                ui.add(
                                    egui::Label::new(RichText::new(&text).color(TEXT).small())
                                        .truncate(),
                                )
                                .on_hover_text(&text);
                            });
                        });
                    });
            }

            ui.add_space(6.0);
            ui.checkbox(
                &mut self.settings.overwrite,
                "Перезаписывать существующие OGG",
            )
            .on_hover_text("Если выключено, файлы, для которых OGG уже есть, будут пропущены");
        });
    }

    fn ui_files(&mut self, ui: &mut egui::Ui) {
        let enabled = !self.converting;
        let stats = self.stats();

        ui.horizontal(|ui| {
            step_title(ui, "1", "Файлы");
            if !self.files.is_empty() {
                ui.label(RichText::new(files_word(self.files.len())).color(MUTED));
            }
        });
        ui.add_space(2.0);

        ui.horizontal(|ui| {
            ui.add_enabled_ui(enabled, |ui| {
                if ui
                    .button("➕ Добавить файлы")
                    .on_hover_text("Ctrl+O")
                    .clicked()
                {
                    self.pick_files();
                }
                if ui
                    .button("📁 Добавить папку")
                    .on_hover_text("Ctrl+Shift+O")
                    .clicked()
                {
                    self.pick_folder();
                }
                ui.checkbox(&mut self.settings.recursive, "С подпапками")
                    .on_hover_text("При добавлении папки искать аудио и во вложенных папках");

                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if ui
                        .add_enabled(!self.files.is_empty(), egui::Button::new("Очистить"))
                        .on_hover_text("Убрать все файлы из списка (сами файлы не удаляются)")
                        .clicked()
                    {
                        self.files.clear();
                        self.total = 0;
                        self.completed = 0;
                    }
                    if stats.failed > 0
                        && ui
                            .button("Убрать ошибки")
                            .on_hover_text("Убрать из списка файлы с ошибкой")
                            .clicked()
                    {
                        self.files.retain(|f| f.status != FileStatus::Failed);
                    }
                    if stats.success > 0
                        && ui
                            .button("Убрать готовые")
                            .on_hover_text("Убрать из списка уже сконвертированные файлы")
                            .clicked()
                    {
                        self.files.retain(|f| f.status != FileStatus::Success);
                    }
                });
            });
        });

        // Строка уведомления держит место всегда, чтобы список не прыгал.
        ui.horizontal(|ui| {
            ui.set_min_height(18.0);
            if let Some(notice) = &self.notice {
                ui.label(RichText::new(&notice.text).color(notice.color));
            }
        });

        let list_rect = ui.available_rect_before_wrap();

        if self.files.is_empty() {
            self.ui_drop_zone(ui);
        } else {
            self.ui_file_list(ui);
            if self.dragging {
                paint_drop_overlay(ui, list_rect, self.converting);
            }
        }
    }

    fn ui_drop_zone(&mut self, ui: &mut egui::Ui) {
        let (fill, stroke) = if self.dragging {
            (Color32::from_rgba_unmultiplied(98, 168, 255, 24), ACCENT)
        } else {
            (PANEL_INNER, STROKE)
        };
        Frame::none()
            .fill(fill)
            .stroke(Stroke::new(1.5, stroke))
            .rounding(Rounding::same(10.0))
            .inner_margin(Margin::same(24.0))
            .show(ui, |ui| {
                ui.set_min_size(ui.available_size() - Vec2::splat(4.0));
                ui.vertical_centered(|ui| {
                    ui.add_space((ui.available_height() * 0.5 - 90.0).max(0.0));
                    ui.label(RichText::new("🎵").size(40.0).color(MUTED));
                    ui.add_space(4.0);
                    ui.label(
                        RichText::new(if self.dragging {
                            "Отпустите, чтобы добавить"
                        } else {
                            "Перетащите сюда аудиофайлы или папки"
                        })
                        .size(18.0)
                        .color(if self.dragging { ACCENT } else { TEXT }),
                    );
                    ui.add_space(4.0);
                    ui.label(
                        RichText::new("MP3, M4A, WAV, FLAC, AAC, OGG, OPUS, WMA, AIFF и др.")
                            .color(MUTED),
                    );
                    ui.add_space(14.0);

                    let button_size = Vec2::new(170.0, 34.0);
                    let total = button_size.x * 2.0 + ui.spacing().item_spacing.x;
                    ui.allocate_ui_with_layout(
                        Vec2::new(total, button_size.y),
                        Layout::left_to_right(Align::Center),
                        |ui| {
                            if ui
                                .add(
                                    egui::Button::new("Выбрать файлы…")
                                        .fill(ACCENT_SOFT)
                                        .min_size(button_size),
                                )
                                .clicked()
                            {
                                self.pick_files();
                            }
                            if ui
                                .add(egui::Button::new("Выбрать папку…").min_size(button_size))
                                .clicked()
                            {
                                self.pick_folder();
                            }
                        },
                    );
                });
            });
    }

    fn ui_file_list(&mut self, ui: &mut egui::Ui) {
        let mut remove = None;
        let converting = self.converting;

        ScrollArea::vertical()
            .id_salt("file_list")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                for (index, file) in self.files.iter().enumerate() {
                    let name = file_name(&file.path);
                    let parent = file
                        .path
                        .parent()
                        .unwrap_or(Path::new(""))
                        .display()
                        .to_string();

                    let (detail, detail_color) = match file.status {
                        FileStatus::Failed => (file.message.clone(), ERR),
                        FileStatus::Skipped => (file.message.clone(), WARN),
                        FileStatus::Success => (format!("→ {}", file.message), MUTED),
                        _ => (parent.clone(), MUTED),
                    };

                    let fill = if file.status == FileStatus::Running {
                        SELECTED_BG
                    } else {
                        PANEL_INNER
                    };

                    let row = Frame::none()
                        .fill(fill)
                        .rounding(Rounding::same(6.0))
                        .inner_margin(Margin::symmetric(10.0, 6.0))
                        .stroke(Stroke::new(
                            1.0,
                            if file.status == FileStatus::Failed {
                                Color32::from_rgb(92, 46, 54)
                            } else {
                                STROKE
                            },
                        ))
                        .show(ui, |ui| {
                            ui.set_width(ui.available_width());
                            ui.horizontal(|ui| {
                                ui.add_sized(
                                    [18.0, 18.0],
                                    egui::Label::new(
                                        RichText::new(file.status.icon())
                                            .color(file.status.color()),
                                    ),
                                );
                                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                    if !converting
                                        && ui
                                            .small_button("✖")
                                            .on_hover_text("Убрать из списка")
                                            .clicked()
                                    {
                                        remove = Some(index);
                                    }
                                    if file.status == FileStatus::Running {
                                        ui.spinner();
                                    }
                                    ui.label(
                                        RichText::new(file.status.label())
                                            .small()
                                            .color(file.status.color()),
                                    );
                                    ui.with_layout(Layout::top_down(Align::Min), |ui| {
                                        ui.spacing_mut().item_spacing.y = 2.0;
                                        ui.add(
                                            egui::Label::new(RichText::new(&name).color(TEXT))
                                                .truncate(),
                                        );
                                        ui.add(
                                            egui::Label::new(
                                                RichText::new(&detail).small().color(detail_color),
                                            )
                                            .truncate(),
                                        );
                                    });
                                });
                            });
                        });

                    let mut tip = file.path.display().to_string();
                    if !file.message.is_empty() {
                        tip.push_str("\n\n");
                        tip.push_str(&file.message);
                    }
                    row.response.on_hover_text(tip);
                    ui.add_space(3.0);
                }
            });

        if let Some(index) = remove {
            self.files.remove(index);
        }
    }

    fn ui_footer(&mut self, ui: &mut egui::Ui) {
        let stats = self.stats();
        let to_convert = self.files_to_convert();
        let ffmpeg_ready = self.ffmpeg_path().is_some();

        ui.horizontal(|ui| {
            ui.set_min_height(44.0);
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                // Главная кнопка: «Конвертировать» или «Стоп» на её месте.
                let big = Vec2::new(230.0, 40.0);
                if self.converting {
                    let label = if self.stopping {
                        "Останавливаю…"
                    } else {
                        "⏹ Остановить"
                    };
                    if ui
                        .add_enabled(
                            !self.stopping,
                            egui::Button::new(RichText::new(label).strong().size(16.0))
                                .fill(STOP_BG)
                                .min_size(big),
                        )
                        .on_hover_text("Остановить после текущего файла (Esc)")
                        .clicked()
                    {
                        self.request_cancel();
                    }
                } else {
                    let label = if self.files.is_empty() {
                        "Конвертировать".to_string()
                    } else if stats.success == self.files.len() {
                        format!("Конвертировать заново ({to_convert})")
                    } else if stats.success > 0 {
                        format!("Конвертировать остальные ({to_convert})")
                    } else {
                        format!("Конвертировать ({to_convert})")
                    };
                    let can_convert = !self.files.is_empty() && ffmpeg_ready;
                    let response = ui
                        .add_enabled(
                            can_convert,
                            egui::Button::new(RichText::new(label).strong().size(16.0))
                                .fill(if can_convert {
                                    ACCENT_SOFT
                                } else {
                                    PANEL_INNER
                                })
                                .min_size(big),
                        )
                        .on_hover_text("Ctrl+Enter")
                        .on_disabled_hover_text(if self.files.is_empty() {
                            "Сначала добавьте файлы"
                        } else {
                            "FFmpeg ещё не готов"
                        });
                    if response.clicked() {
                        self.start_conversion();
                    }
                }

                let open_target = self.output_folder_to_open();
                if ui
                    .add_enabled(
                        open_target.is_some(),
                        egui::Button::new("📂 Открыть папку").min_size(Vec2::new(0.0, 40.0)),
                    )
                    .on_hover_text("Открыть папку с готовыми OGG")
                    .clicked()
                {
                    if let Some(dir) = open_target {
                        Self::open_path(&dir);
                    }
                }

                let log_label = if self.log_open {
                    "Журнал ▾"
                } else {
                    "Журнал ▸"
                };
                if ui
                    .add(
                        egui::Button::new(log_label)
                            .selected(self.log_open)
                            .min_size(Vec2::new(0.0, 40.0)),
                    )
                    .on_hover_text("Подробный журнал событий")
                    .clicked()
                {
                    self.log_open = !self.log_open;
                }

                ui.add_space(8.0);
                ui.with_layout(Layout::top_down(Align::Min), |ui| {
                    ui.spacing_mut().item_spacing.y = 4.0;
                    self.ui_footer_status(ui, &stats);
                });
            });
        });
    }

    fn ui_footer_status(&self, ui: &mut egui::Ui, stats: &Stats) {
        if self.converting {
            let current = self
                .current_file
                .as_deref()
                .map(file_name)
                .unwrap_or_default();
            let headline = if self.stopping {
                "Останавливаю после текущего файла…".to_string()
            } else {
                format!("{} из {} · {current}", self.completed + 1, self.total)
            };
            ui.add(egui::Label::new(RichText::new(headline).color(TEXT)).truncate());
            let progress = if self.total == 0 {
                0.0
            } else {
                self.completed as f32 / self.total as f32
            };
            ui.add(
                egui::ProgressBar::new(progress)
                    .desired_width(ui.available_width())
                    .desired_height(10.0)
                    .animate(true),
            );
            return;
        }

        if self.files.is_empty() {
            ui.label(RichText::new("Добавьте файлы, чтобы начать").color(MUTED));
            return;
        }

        if self.total > 0 {
            ui.horizontal(|ui| {
                ui.label(RichText::new(format!("✔ готово {}", stats.success)).color(OK));
                if stats.failed > 0 {
                    ui.label(RichText::new(format!("✖ ошибок {}", stats.failed)).color(ERR));
                }
                if stats.skipped > 0 {
                    ui.label(RichText::new(format!("⏭ пропущено {}", stats.skipped)).color(WARN));
                }
                if stats.queued > 0 {
                    ui.label(RichText::new(format!("○ ожидают {}", stats.queued)).color(MUTED));
                }
            });
        }

        let destination = match &self.settings.output_dir {
            Some(dir) => format!("в {}", dir.display()),
            None => "рядом с исходниками".to_string(),
        };
        let summary = format!(
            "OGG Vorbis · Q{} (≈{} кбит/с) · {} · {destination}",
            self.settings.quality,
            Settings::quality_kbps(self.settings.quality),
            self.settings.channels.label().to_lowercase(),
        );
        ui.add(egui::Label::new(RichText::new(summary).color(MUTED)).truncate());
    }

    fn ui_logs(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label(RichText::new("Журнал").strong().color(TEXT));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui.small_button("Скрыть").clicked() {
                    self.log_open = false;
                }
                if ui.small_button("Очистить").clicked() {
                    self.logs.clear();
                }
            });
        });

        Frame::none()
            .fill(PANEL_INNER)
            .rounding(Rounding::same(6.0))
            .inner_margin(Margin::same(8.0))
            .stroke(Stroke::new(1.0, STROKE))
            .show(ui, |ui| {
                ScrollArea::vertical()
                    .id_salt("logs")
                    .stick_to_bottom(true)
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        if self.logs.is_empty() {
                            ui.label(RichText::new("Пока пусто.").color(MUTED));
                        }
                        for (color, line) in &self.logs {
                            ui.add(
                                egui::Label::new(
                                    RichText::new(line).color(*color).monospace().size(12.0),
                                )
                                .wrap(),
                            );
                        }
                    });
            });
    }
}

fn step_title(ui: &mut egui::Ui, number: &str, title: &str) {
    ui.horizontal(|ui| {
        let (rect, _) = ui.allocate_exact_size(Vec2::splat(22.0), Sense::hover());
        ui.painter().circle_filled(rect.center(), 11.0, ACCENT_SOFT);
        ui.painter().text(
            rect.center(),
            Align2::CENTER_CENTER,
            number,
            FontId::proportional(13.0),
            TEXT,
        );
        ui.label(RichText::new(title).strong().size(16.0).color(TEXT));
    });
}

/// Строка-переключатель на всю ширину: название слева, пояснение справа.
fn choice_row(ui: &mut egui::Ui, selected: bool, title: &str, right: &str) -> egui::Response {
    let size = Vec2::new(ui.available_width(), 30.0);
    let (rect, response) = ui.allocate_exact_size(size, Sense::click());
    let enabled = ui.is_enabled();
    let response = response.on_hover_cursor(if enabled {
        egui::CursorIcon::PointingHand
    } else {
        egui::CursorIcon::Default
    });

    if ui.is_rect_visible(rect) {
        let painter = ui.painter();
        let fill = if selected {
            SELECTED_BG
        } else if response.hovered() && enabled {
            ROW_HOVER
        } else {
            Color32::TRANSPARENT
        };
        let stroke = if selected { ACCENT_SOFT } else { STROKE };
        painter.rect(rect, Rounding::same(6.0), fill, Stroke::new(1.0, stroke));

        let dim = |c: Color32| if enabled { c } else { c.gamma_multiply(0.5) };

        // Радио-кружок.
        let dot = egui::pos2(rect.left() + 14.0, rect.center().y);
        painter.circle_stroke(
            dot,
            6.0,
            Stroke::new(1.5, dim(if selected { ACCENT } else { MUTED })),
        );
        if selected {
            painter.circle_filled(dot, 3.0, dim(ACCENT));
        }

        painter.text(
            egui::pos2(rect.left() + 28.0, rect.center().y),
            Align2::LEFT_CENTER,
            title,
            TextStyle::Body.resolve(ui.style()),
            dim(TEXT),
        );
        if !right.is_empty() {
            painter.text(
                egui::pos2(rect.right() - 10.0, rect.center().y),
                Align2::RIGHT_CENTER,
                right,
                TextStyle::Small.resolve(ui.style()),
                dim(MUTED),
            );
        }
    }
    ui.add_space(2.0);
    response
}

fn paint_drop_overlay(ui: &egui::Ui, rect: egui::Rect, blocked: bool) {
    let painter = ui.painter_at(rect);
    let (color, text) = if blocked {
        (ERR, "Во время конвертации добавлять файлы нельзя")
    } else {
        (ACCENT, "Отпустите, чтобы добавить в список")
    };
    painter.rect(
        rect.shrink(2.0),
        Rounding::same(10.0),
        Color32::from_rgba_unmultiplied(12, 14, 20, 220),
        Stroke::new(1.5, color),
    );
    painter.text(
        rect.center(),
        Align2::CENTER_CENTER,
        text,
        FontId::proportional(18.0),
        color,
    );
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .and_then(|x| x.to_str())
        .unwrap_or("файл")
        .to_string()
}

fn files_word(count: usize) -> String {
    let word = match (count % 10, count % 100) {
        (1, n) if n != 11 => "файл",
        (2..=4, n) if !(12..=14).contains(&n) => "файла",
        _ => "файлов",
    };
    format!("{count} {word}")
}

fn apply_theme(ctx: &egui::Context) {
    ctx.set_pixels_per_point(ctx.pixels_per_point().max(1.0));

    let mut visuals = egui::Visuals::dark();
    visuals.dark_mode = true;
    visuals.window_fill = PANEL;
    visuals.panel_fill = BG;
    visuals.extreme_bg_color = PANEL_INNER;
    visuals.faint_bg_color = PANEL;
    visuals.override_text_color = Some(TEXT);
    visuals.hyperlink_color = ACCENT;
    visuals.selection.bg_fill = SELECTED_BG;
    visuals.selection.stroke = Stroke::new(1.0, ACCENT);
    visuals.widgets.noninteractive.fg_stroke = Stroke::new(1.0, MUTED);
    visuals.widgets.inactive.bg_fill = Color32::from_rgb(34, 40, 54);
    visuals.widgets.inactive.weak_bg_fill = Color32::from_rgb(34, 40, 54);
    visuals.widgets.inactive.fg_stroke = Stroke::new(1.0, TEXT);
    visuals.widgets.inactive.rounding = Rounding::same(6.0);
    visuals.widgets.inactive.bg_stroke = Stroke::new(1.0, STROKE);
    visuals.widgets.hovered.bg_fill = Color32::from_rgb(48, 62, 88);
    visuals.widgets.hovered.weak_bg_fill = Color32::from_rgb(48, 62, 88);
    visuals.widgets.hovered.fg_stroke = Stroke::new(1.0, TEXT);
    visuals.widgets.hovered.rounding = Rounding::same(6.0);
    visuals.widgets.hovered.bg_stroke = Stroke::new(1.0, ACCENT);
    visuals.widgets.active.bg_fill = ACCENT_SOFT;
    visuals.widgets.active.weak_bg_fill = ACCENT_SOFT;
    visuals.widgets.active.rounding = Rounding::same(6.0);
    visuals.widgets.open.bg_fill = Color32::from_rgb(40, 50, 70);
    visuals.widgets.open.rounding = Rounding::same(6.0);
    visuals.widgets.open.fg_stroke = Stroke::new(1.0, TEXT);
    visuals.window_rounding = Rounding::same(8.0);
    ctx.set_visuals(visuals);

    ctx.style_mut(|style| {
        use egui::FontFamily::{Monospace, Proportional};
        style.text_styles = [
            (TextStyle::Heading, FontId::new(20.0, Proportional)),
            (TextStyle::Body, FontId::new(14.0, Proportional)),
            (TextStyle::Button, FontId::new(14.0, Proportional)),
            (TextStyle::Small, FontId::new(12.0, Proportional)),
            (TextStyle::Monospace, FontId::new(12.5, Monospace)),
        ]
        .into();
        style.spacing.item_spacing = Vec2::new(8.0, 6.0);
        style.spacing.button_padding = Vec2::new(12.0, 6.0);
        style.spacing.interact_size.y = 26.0;
        style.spacing.slider_width = 150.0;
    });
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
        self.handle_shortcuts(ctx);

        let now = ctx.input(|i| i.time);
        if let Some((color, text)) = self.pending_notice.take() {
            self.notice = Some(Notice {
                color,
                text,
                until: now + NOTICE_SECONDS,
            });
        }
        if self.notice.as_ref().is_some_and(|n| n.until <= now) {
            self.notice = None;
        }

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
                    .inner_margin(Margin::symmetric(16.0, 10.0))
                    .stroke(Stroke::new(1.0, STROKE)),
            )
            .show(ctx, |ui| {
                self.ui_footer(ui);
            });

        if self.log_open {
            egui::TopBottomPanel::bottom("log")
                .resizable(true)
                .default_height(170.0)
                .height_range(90.0..=400.0)
                .frame(
                    Frame::none()
                        .fill(PANEL)
                        .inner_margin(Margin::symmetric(16.0, 8.0))
                        .stroke(Stroke::new(1.0, STROKE)),
                )
                .show(ctx, |ui| {
                    self.ui_logs(ui);
                });
        }

        egui::SidePanel::right("settings")
            .resizable(false)
            .exact_width(300.0)
            .frame(
                Frame::none()
                    .fill(PANEL)
                    .inner_margin(Margin::same(16.0))
                    .stroke(Stroke::new(1.0, STROKE)),
            )
            .show(ctx, |ui| {
                ScrollArea::vertical()
                    .id_salt("settings_scroll")
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        self.ui_settings(ui);
                    });
            });

        egui::CentralPanel::default()
            .frame(Frame::none().fill(BG).inner_margin(Margin::same(16.0)))
            .show(ctx, |ui| {
                self.ui_files(ui);
            });

        self.save_settings();

        if self.converting || self.dragging || self.ffmpeg_rx.is_some() {
            ctx.request_repaint_after(Duration::from_millis(50));
        } else if let Some(notice) = &self.notice {
            ctx.request_repaint_after(Duration::from_secs_f64((notice.until - now).max(0.05)));
        }
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.save_settings();
    }
}

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1100.0, 740.0])
            .with_min_inner_size([900.0, 600.0])
            .with_title("Schrödinger — конвертер в OGG")
            .with_drag_and_drop(true),
        ..Default::default()
    };

    eframe::run_native(
        "Schrodinger Audio Converter",
        options,
        Box::new(|cc| Ok(Box::new(App::new(cc)))),
    )
}
