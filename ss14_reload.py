#!/usr/bin/env python3
"""
SS14: умная перезарядка на R.

Проверяет патронник/затвор осмотром оружия (Shift+ЛКМ по слоту в HUD).
E на оружии ПЕРЕКЛЮЧАЕТ затвор — скрипт жмёт E только когда это нужно.

R  — проверить ствол и перезарядить
F9 — калибровка: клик по оружию в слоте руки
F4 — тест осмотра (без перезарядки)
F5 — принудительно сменить рожок
F8 — пауза
Esc+F8 в консоли / Ctrl+C — выход

Управление в игре (как ты описал):
  E       открыть/закрыть затвор (навести на оружие)
  X       смена руки
  Ctrl+B  достать рожок из рюкзака в свободную руку
  ЛКМ     по оружию, когда выбран рожок — вставить, старый вылезет
  Q       выкинуть старый рожок
"""

from __future__ import annotations

import json
import re
import sys
import threading
import time
import winsound
from ctypes import windll, wintypes, create_unicode_buffer
from pathlib import Path

import pyautogui
from pynput import keyboard, mouse
from pynput.keyboard import Key

if sys.platform == "win32":
    try:
        windll.user32.SetProcessDPIAware()
    except Exception:
        pass

pyautogui.FAILSAFE = True
pyautogui.PAUSE = 0.02

CONFIG_PATH = Path(__file__).with_name("ss14_reload_config.json")
WINDOW_HINTS = ("space station 14", "ss14", "время приключений")

# Цвета текста осмотра из gun.ftl / SharedGunSystem.ChamberMagazine
BOLT_CLOSED_RGB = (0x94, 0xE1, 0xF2)  # «Затвор закрыт»
BOLT_OPEN_RGB = (0xF2, 0x9D, 0x94)  # «Затвор открыт»

OPEN_WORDS = ("открыт", "opened", "open bolt", "bolt is open")
CLOSED_WORDS = ("закрыт", "closed", "closed bolt", "bolt is closed")
AMMO_RE = re.compile(
    r"(?:тут|has)\s+(\d+)|(\d+)\s+(?:штук|shots?|патрон)",
    re.IGNORECASE,
)

DEFAULTS = {
    "gun_hud": None,  # [x, y]
    "color_tolerance": 28,
    "swap_hands_after_get_mag": True,
    "drop_old_mag": True,
    "window_must_match": True,
    "delay_after_move": 0.05,
    "delay_examine": 0.35,
    "delay_action": 0.12,
    "delay_insert": 0.22,
    "delay_bolt": 0.18,
    "second_r_window": 2.4,
}


def beep(ok: bool = True) -> None:
    try:
        winsound.Beep(880 if ok else 420, 70)
    except Exception:
        pass


def load_config() -> dict:
    cfg = dict(DEFAULTS)
    if CONFIG_PATH.exists():
        cfg.update(json.loads(CONFIG_PATH.read_text(encoding="utf-8")))
    return cfg


def save_config(cfg: dict) -> None:
    CONFIG_PATH.write_text(json.dumps(cfg, ensure_ascii=False, indent=2), encoding="utf-8")


def foreground_title() -> str:
    hwnd = windll.user32.GetForegroundWindow()
    n = windll.user32.GetWindowTextLengthW(hwnd) + 1
    buf = create_unicode_buffer(n)
    windll.user32.GetWindowTextW(hwnd, buf, n)
    return buf.value or ""


def foreground_rect() -> tuple[int, int, int, int] | None:
    hwnd = windll.user32.GetForegroundWindow()
    if not hwnd:
        return None
    rect = wintypes.RECT()
    if not windll.user32.GetWindowRect(hwnd, rect):
        return None
    return rect.left, rect.top, rect.right, rect.bottom


def ss14_focused(cfg: dict) -> bool:
    if not cfg.get("window_must_match", True):
        return True
    title = foreground_title().lower()
    return any(h in title for h in WINDOW_HINTS)


def sleep(cfg: dict, key: str) -> None:
    time.sleep(float(cfg.get(key, DEFAULTS[key])))


def color_dist(a: tuple[int, int, int], b: tuple[int, int, int]) -> int:
    return abs(a[0] - b[0]) + abs(a[1] - b[1]) + abs(a[2] - b[2])


def count_color(img, target: tuple[int, int, int], tolerance: int) -> int:
    """Сжимаем кадр и считаем пиксели цвета текста осмотра."""
    small = img.resize((max(1, img.width // 3), max(1, img.height // 3)))
    hits = 0
    for r, g, b in small.getdata():
        if color_dist((r, g, b), target) <= tolerance:
            hits += 1
    return hits


def try_ocr(img) -> str:
    try:
        import pytesseract

        return pytesseract.image_to_string(img, lang="rus+eng") or ""
    except Exception:
        pass
    try:
        import winocr

        out = winocr.recognize_pil_sync(img)
        if isinstance(out, dict):
            return str(out.get("text") or "")
        return str(out or "")
    except Exception:
        return ""


def parse_ammo(text: str) -> int | None:
    text = text.lower().replace("\n", " ")
    best: int | None = None
    for m in AMMO_RE.finditer(text):
        raw = m.group(1) or m.group(2)
        if raw is None:
            continue
        n = int(raw)
        if 0 <= n <= 200:
            best = n if best is None else min(best, n)
    if best is not None:
        return best
    if "нет патронов" in text or "no ammo" in text:
        return 0
    return None


def parse_bolt_from_text(text: str) -> bool | None:
    t = text.lower()
    # Сначала более длинные/однозначные фразы
    if "затвор открыт" in t or "opened bolt" in t or "bolt is open" in t:
        return True
    if "затвор закрыт" in t or "closed bolt" in t or "bolt is closed" in t:
        return False
    open_hit = any(w in t for w in OPEN_WORDS)
    closed_hit = any(w in t for w in CLOSED_WORDS)
    if open_hit and not closed_hit:
        return True
    if closed_hit and not open_hit:
        return False
    return None


class Reloader:
    def __init__(self) -> None:
        self.cfg = load_config()
        self.busy = False
        self.paused = False
        self.calibrating = False
        self.chat_open = False
        self._ignore_keys = False
        self._last_try_close = 0.0
        self._stop = threading.Event()

    def log(self, msg: str) -> None:
        print(f"[{time.strftime('%H:%M:%S')}] {msg}", flush=True)

    def gun_pos(self) -> tuple[int, int] | None:
        pos = self.cfg.get("gun_hud")
        if not isinstance(pos, (list, tuple)) or len(pos) != 2:
            return None
        return int(pos[0]), int(pos[1])

    def can_act(self) -> bool:
        if self.paused or self.busy or self.calibrating or self.chat_open:
            return False
        return ss14_focused(self.cfg)

    def tap(self, key: str) -> None:
        pyautogui.press(key)

    def hotkey(self, *keys: str) -> None:
        pyautogui.hotkey(*keys)

    def hover_gun(self) -> tuple[int, int]:
        pos = self.gun_pos()
        if not pos:
            raise RuntimeError("Сначала калибровка: нажми F9 и кликни по оружию в слоте руки")
        pyautogui.moveTo(pos[0], pos[1], duration=0.05)
        sleep(self.cfg, "delay_after_move")
        return pos

    def press_e_on_gun(self) -> None:
        self.hover_gun()
        self.tap("e")
        sleep(self.cfg, "delay_bolt")

    def examine_gun(self, save_debug: bool = False) -> tuple[bool | None, int | None, str]:
        """
        Возвращает (bolt_open, ammo_count, raw_text).
        bolt_open True = затвор открыт.
        """
        x, y = self.hover_gun()
        pyautogui.keyDown("shift")
        try:
            pyautogui.click(x, y)
        finally:
            pyautogui.keyUp("shift")
        sleep(self.cfg, "delay_examine")

        rect = foreground_rect()
        if rect:
            img = pyautogui.screenshot(region=(rect[0], rect[1], rect[2] - rect[0], rect[3] - rect[1]))
        else:
            img = pyautogui.screenshot()

        if save_debug:
            debug_path = Path(__file__).with_name("ss14_examine_debug.png")
            img.save(debug_path)
            self.log(f"скрин осмотра: {debug_path}")

        tol = int(self.cfg.get("color_tolerance", 28))
        open_hits = count_color(img, BOLT_OPEN_RGB, tol)
        closed_hits = count_color(img, BOLT_CLOSED_RGB, tol)

        text = try_ocr(img)
        bolt = parse_bolt_from_text(text)
        ammo = parse_ammo(text)

        if bolt is None:
            # Цвет текста осмотра надёжнее OCR, если пикселей достаточно
            if open_hits >= 8 and open_hits > closed_hits * 1.4:
                bolt = True
            elif closed_hits >= 8 and closed_hits > open_hits * 1.4:
                bolt = False

        # Esc закрывает осмотр. Не жмём, если окно не открылось — иначе меню паузы.
        if bolt is not None or ammo is not None or open_hits + closed_hits >= 8:
            pyautogui.press("esc")
            time.sleep(0.05)

        return bolt, ammo, text

    def mag_swap(self) -> None:
        """Ctrl+B → выбрать рожок → клик по оружию → Q старый рожок → назад к пушке."""
        self.log("магазин пуст → достаю рожок (Ctrl+B)")
        self.hotkey("ctrl", "b")
        sleep(self.cfg, "delay_action")

        if self.cfg.get("swap_hands_after_get_mag", True):
            self.tap("x")
            sleep(self.cfg, "delay_action")

        x, y = self.hover_gun()
        pyautogui.click(x, y)
        sleep(self.cfg, "delay_insert")

        if self.cfg.get("drop_old_mag", True):
            self.tap("q")
            sleep(self.cfg, "delay_action")

        if self.cfg.get("swap_hands_after_get_mag", True):
            self.tap("x")
            sleep(self.cfg, "delay_action")

    def close_bolt(self) -> None:
        self.log("закрываю затвор (E)")
        self.press_e_on_gun()

    def rack_closed_empty(self) -> None:
        """Затвор закрыт, патронов 0: вставили рожок → открыть и закрыть, чтобы дослать патрон."""
        self.log("досылаю патрон: открыть и закрыть затвор")
        self.press_e_on_gun()
        self.press_e_on_gun()

    def smart_reload(self, force_mag: bool = False) -> None:
        if not self.can_act() and not (force_mag and ss14_focused(self.cfg) and not self.paused):
            return
        if self.busy:
            return
        self.busy = True
        self._ignore_keys = True
        try:
            if force_mag:
                self.mag_swap()
                self.close_bolt()
                beep(True)
                return

            now = time.time()
            if now - self._last_try_close < float(self.cfg.get("second_r_window", 2.4)):
                self.log("повторный R — считаю, что магазин пустой")
                self._last_try_close = 0.0
                self.mag_swap()
                self.close_bolt()
                beep(True)
                return

            bolt_open, ammo, text = self.examine_gun()
            snippet = " ".join(text.split())[:120] if text else "—"
            self.log(f"осмотр: затвор={'открыт' if bolt_open else 'закрыт' if bolt_open is False else '?'}  патронов={ammo if ammo is not None else '?'}  ({snippet})")

            # Есть патроны и затвор закрыт — в стволе/магазине ещё есть, стрелять можно
            if bolt_open is False and (ammo is None or ammo > 0):
                self.log("пуля/патроны на месте, затвор закрыт")
                beep(True)
                return

            # Затвор открыт, в магазине ещё есть — просто закрыть, дошлётся патрон
            if bolt_open is True and ammo is not None and ammo > 0:
                self.close_bolt()
                beep(True)
                return

            # Магазин кончился
            if ammo == 0:
                self.mag_swap()
                if bolt_open is False:
                    self.rack_closed_empty()
                else:
                    self.close_bolt()
                beep(True)
                return

            # Затвор открыт, число патронов не прочитали: пробуем закрыть.
            # Если пушка всё ещё пустая — жми R ещё раз, сменит рожок.
            if bolt_open is True:
                self.close_bolt()
                self._last_try_close = time.time()
                self.log("не увидел число патронов. Если не стреляет — нажми R ещё раз")
                beep(False)
                return

            self.log("не смог прочитать осмотр. F9 — калибровка, F4 — тест. Или F5 — сменить рожок вручную")
            beep(False)
        except Exception as exc:
            self.log(f"ошибка: {exc}")
            beep(False)
        finally:
            self._ignore_keys = False
            self.busy = False

    def start_calibrate(self) -> None:
        self.calibrating = True
        self.log("калибровка: кликни ЛКМ по ОРУЖИЮ в слоте руки (HUD внизу)")

    def on_click(self, x: int, y: int, button, pressed: bool) -> None:
        if not self.calibrating or not pressed:
            return
        if button != mouse.Button.left:
            return
        self.cfg["gun_hud"] = [int(x), int(y)]
        save_config(self.cfg)
        self.calibrating = False
        self.log(f"сохранил слот оружия: {x}, {y}")
        beep(True)

    def on_press(self, key) -> None:
        if self._ignore_keys:
            return

        # Чат SS14: T/Y открывают, Enter/Esc закрывают — не перехватываем R в чате
        try:
            ch = key.char.lower() if getattr(key, "char", None) else ""
        except Exception:
            ch = ""

        if key in (Key.enter, Key.esc):
            self.chat_open = False
        elif ch in ("t", "y", "ё") and not self.busy:
            if ss14_focused(self.cfg):
                self.chat_open = True

        if key == Key.f8:
            self.paused = not self.paused
            self.log("пауза" if self.paused else "снято с паузы")
            return
        if key == Key.f9:
            self.start_calibrate()
            return
        if key == Key.f4:
            if not ss14_focused(self.cfg) or self.paused:
                return
            threading.Thread(target=self._test_examine, daemon=True).start()
            return
        if key == Key.f5:
            if not self.can_act():
                return
            threading.Thread(target=lambda: self.smart_reload(force_mag=True), daemon=True).start()
            return
        if ch == "r":
            if not self.can_act():
                return
            threading.Thread(target=self.smart_reload, daemon=True).start()

    def _test_examine(self) -> None:
        self.busy = True
        self._ignore_keys = True
        try:
            bolt, ammo, text = self.examine_gun(save_debug=True)
            self.log(
                f"тест: затвор={'открыт' if bolt else 'закрыт' if bolt is False else 'не прочитан'}, "
                f"патронов={ammo if ammo is not None else 'не прочитано'}"
            )
            if text.strip():
                self.log("OCR: " + " ".join(text.split())[:300])
            else:
                self.log("OCR нет (поставь tesseract или winocr). Цвет затвора всё равно читается.")
        except Exception as exc:
            self.log(f"тест не удался: {exc}")
        finally:
            self._ignore_keys = False
            self.busy = False

    def run(self) -> None:
        print("=" * 60)
        print("SS14 reload  |  R перезарядка   F9 калибровка   F4 тест")
        print("             |  F5 смена рожка   F8 пауза   Ctrl+C выход")
        print("=" * 60)
        if self.gun_pos():
            print(f"слот оружия: {self.gun_pos()}")
        else:
            print("слот оружия не задан — нажми F9 и кликни по пушке в HUD")
        print("окно игры должно быть в фокусе (Space Station 14).")
        print()

        m_listener = mouse.Listener(on_click=self.on_click)
        k_listener = keyboard.Listener(on_press=self.on_press)
        m_listener.start()
        k_listener.start()
        try:
            while k_listener.is_alive():
                time.sleep(0.2)
        except KeyboardInterrupt:
            pass
        finally:
            k_listener.stop()
            m_listener.stop()
            self.log("стоп")


if __name__ == "__main__":
    Reloader().run()
