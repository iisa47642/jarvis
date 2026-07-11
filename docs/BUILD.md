# Сборка Jarvis из исходников (macOS, Apple Silicon)

Как собрать `.app`/`.dmg` из этого форка самому.

## Требования

- **macOS** на Apple Silicon (M-серия).
- **Xcode Command Line Tools**: `xcode-select --install`.
- **Homebrew**: https://brew.sh
- **Rust** (rustup):
  ```sh
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
  . "$HOME/.cargo/env"
  ```
- **Node.js** (для Tauri CLI): `brew install node`
- **tmux** (Jarvis управляет сессиями через него): `brew install tmux`

## Сборка

```sh
git clone https://github.com/iisa47642/jarvis.git
cd jarvis
npm install            # ставит @tauri-apps/cli
npm run bundle         # cargo build --release + сборка .app и .dmg
```

Артефакты:
- `src-tauri/target/release/bundle/macos/Jarvis.app`
- `src-tauri/target/release/bundle/dmg/Jarvis_<версия>_aarch64.dmg`

> Первая сборка долгая (десятки минут): тянутся и компилируются нативные
> зависимости (whisper, onnxruntime, аудио-стек). Фичи `wakeword-ort,
> whisper-native, stt-vad` включены в скрипте `bundle`.

## Установка

Перетащи `Jarvis.app` в `/Applications` (или открой `.dmg`). Приложение
подписано **ad-hoc** (без Apple Developer ID), поэтому при первом запуске
Gatekeeper может ругаться — открой через **правый клик → Открыть**, либо:
```sh
xattr -dr com.apple.quarantine /Applications/Jarvis.app
```

Первый запуск проведёт онбординг (установит шимы claude/codex и хуки).

## Дев-режим

```sh
npm start        # сборка + запуск дев-инстанса (JARVIS_DIR=~/.jarvis-dev)
npm test         # cargo test
```

## Важно про PATH (см. docs/fix-tmux-path.md)

GUI-приложение из `/Applications` наследует урезанный launchd-PATH без Homebrew.
Демон ищет `tmux`/`claude`/`codex` по типовым путям (`/opt/homebrew/bin` и т.п.),
поэтому отдельно настраивать PATH не нужно.
