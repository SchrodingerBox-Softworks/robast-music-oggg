# robast-music-oggg - Конвертер в OGG

[![Made with Rust](https://img.shields.io/badge/made%20with-Rust%20🦀-dea584?style=for-the-badge&logo=rust&logoColor=white)](https://www.rust-lang.org/)
[![Blazingly fast](https://img.shields.io/badge/blazingly-fast%20🚀-ff4500?style=for-the-badge)](https://www.rust-lang.org/)
[![Memory safe](https://img.shields.io/badge/memory-safe%20🔒-2ea44f?style=for-the-badge)](https://doc.rust-lang.org/book/ch04-00-understanding-ownership.html)
[![Zero cost abstractions](https://img.shields.io/badge/zero--cost-abstractions-8a2be2?style=for-the-badge)](https://doc.rust-lang.org/book/)

[![Release](https://img.shields.io/github/v/release/SchrodingerBox-Softworks/robast-music-oggg?style=flat-square&logo=github)](https://github.com/SchrodingerBox-Softworks/robast-music-oggg/releases/latest)
[![Build](https://img.shields.io/github/actions/workflow/status/SchrodingerBox-Softworks/robast-music-oggg/release.yml?style=flat-square&label=build)](https://github.com/SchrodingerBox-Softworks/robast-music-oggg/actions/workflows/release.yml)
[![Downloads](https://img.shields.io/github/downloads/SchrodingerBox-Softworks/robast-music-oggg/total?style=flat-square)](https://github.com/SchrodingerBox-Softworks/robast-music-oggg/releases)
![Platforms](https://img.shields.io/badge/platform-Windows%20%7C%20Linux-informational?style=flat-square)
[![egui](https://img.shields.io/badge/GUI-egui-blue?style=flat-square)](https://github.com/emilk/egui)
[![FFmpeg](https://img.shields.io/badge/powered%20by-FFmpeg-007808?style=flat-square&logo=ffmpeg&logoColor=white)](https://ffmpeg.org/)
[![AI assisted](https://img.shields.io/badge/AI%20assisted-Claude%20Code%20%7C%20Cursor-d97757?style=flat-square&logo=claude&logoColor=white)](#разработка-с-помощью-ии)

Настольное приложение для пакетной конвертации аудио в **OGG Vorbis**. Перетащите файлы или папки в окно, выберите качество и нажмите «Конвертировать».

Работает на Windows и Linux. Внутри используется FFmpeg: в релизной сборке он уже вшит в exe, так что ничего ставить не нужно.

## Скачать

Готовые сборки лежат на странице [Releases](https://github.com/SchrodingerBox-Softworks/robast-music-oggg/releases):

- **Windows:** `schrodinger-audio-converter-windows-x64.exe` — один файл, установка не требуется.
- **Linux:** `schrodinger-audio-converter-<версия>-linux-x64.tar.gz`.

## Как пользоваться

Окно разделено на три шага.

1. **Файлы.** Перетащите в окно аудиофайлы или целые папки либо нажмите «Добавить файлы» / «Добавить папку». Галочка «С подпапками» включает поиск во вложенных папках. Неподдерживаемые файлы и дубликаты пропускаются, а сводка о добавленном появляется над списком.
2. **Качество.** Выберите готовый пресет или задайте значение Q от 0 до 10 ползунком. Под ним видно примерный битрейт и размер за минуту звука. Там же выбираются каналы (моно или стерео) и частота дискретизации.
3. **Куда сохранить.** OGG можно сохранять рядом с исходными файлами или собрать все в одну папку. Если выключить «Перезаписывать существующие OGG», уже готовые файлы будут пропущены.

После этого нажмите **«Конвертировать»**. Во время работы на месте этой кнопки появляется **«Остановить»**: конвертация прервётся после текущего файла.

- У каждого файла в списке виден статус. Для ошибки или пропуска причина написана прямо под именем файла.
- Повторное нажатие кнопки обрабатывает только файлы, которые ещё не готовы. Если готовы все, кнопка называется «Конвертировать заново» и запускает весь список снова.
- «Открыть папку» открывает папку с результатом, «Журнал» показывает подробный лог.

Настройки сохраняются автоматически в `settings.json` рядом с exe.

### Пресеты качества

| Пресет          | Q  | Битрейт      |
|-----------------|----|--------------|
| Компактно       | 2  | ≈96 кбит/с   |
| Речь, подкасты  | 4  | ≈128 кбит/с  |
| Музыка          | 6  | ≈192 кбит/с  |
| Высокое         | 8  | ≈256 кбит/с  |
| Максимум        | 10 | ≈500 кбит/с  |

### Поддерживаемые форматы

MP3, M4A, WAV, FLAC, AAC, OGG, OPUS, WMA, AIFF/AIF, ALAC, APE, AC3, AMR.

Из каждого файла берётся первая аудиодорожка. Метаданные (теги) переносятся, обложки отбрасываются.

### Горячие клавиши

| Клавиши        | Действие                  |
|----------------|---------------------------|
| `Ctrl+O`       | Добавить файлы            |
| `Ctrl+Shift+O` | Добавить папку            |
| `Ctrl+Enter`   | Начать конвертацию        |
| `Esc`          | Остановить после текущего |

## FFmpeg

При запуске приложение ищет FFmpeg в таком порядке:

1. Встроенный в exe, если сборка сделана с `--features bundle-ffmpeg`.
2. Файл рядом с программой или в `assets/ffmpeg/<os>/`.
3. На Windows — скачивание статической сборки [BtbN/FFmpeg-Builds](https://github.com/BtbN/FFmpeg-Builds).

Рабочая копия кэшируется в `%LOCALAPPDATA%\schrodinger-audio-converter\ffmpeg` (на Linux — в `~/.local/share/schrodinger-audio-converter/ffmpeg`). Статус FFmpeg показан в правом верхнем углу окна. Если FFmpeg подготовить не удалось, там же появится кнопка «Повторить».

## Сборка из исходников

Нужен Rust stable.

Отладочная сборка (без FFmpeg внутри; на Windows он скачается при первом запуске):

```bash
cargo run
```

Релизная сборка с FFmpeg внутри exe:

```bash
cargo build --release --features bundle-ffmpeg
```

На Windows `build.rs` сам скачает `ffmpeg.exe`. На Linux заранее положите бинарник в `assets/ffmpeg/linux/ffmpeg`.

На Linux для сборки понадобятся пакеты `build-essential pkg-config libx11-dev libxkbcommon-dev libwayland-dev libegl1-mesa-dev`.

## Релиз

Релиз собирается GitHub Actions при пуше тега вида `v*`. Сборки под Windows и Linux прикрепляются к GitHub Release автоматически.

```bash
git tag -a v0.1.1 -m "v0.1.1"
```

```bash
git push origin v0.1.1
```

## Разработка с помощью ИИ

В разработке этого репозитория участвовали ИИ-агенты: [Claude Code](https://claude.com/claude-code) (Anthropic) и [Cursor](https://cursor.com). Они писали и правили код, интерфейс, CI и документацию под руководством автора. Коммиты с их участием помечены строкой `Co-Authored-By`.
