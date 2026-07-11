# Фикс: демон не находит `tmux` → «Сессия не в tmux» при отправке из UI

> Заметка для будущего меня. Если после обновления Jarvis снова появится баг
> «Сессия не в tmux — управлять из Jarvis нельзя», а в коде фикса нет — дай
> прочитать этот файл и попроси применить заново. Здесь всё: симптом, корень,
> точная правка и как проверить.

Ветка/версия, где впервые нашли и починили: `0.3.3` (форк).

## Симптом

- Сессию **видно** в панели Jarvis, чат/транскрипт **читается**.
- При попытке **отправить** сообщение из UI — ошибка:
  > Сессия не в tmux — управлять из Jarvis нельзя. Запусти в терминале:
  > `claude --resume <id>` — shim подхватит её в tmux.
- Бьёт по **любой** сессии (claude и codex одинаково), хотя сессия реально в tmux.

## Корень (root cause)

Читает и пишет — **разные процессы с разным PATH**:

- **Пейны в реестр пишет хук** `~/.jarvis/bin/jarvis-hook`. Он запускается как
  дочерний процесс claude/codex в твоём шелле → PATH нормальный → чтение работает.
- **Ответы в пану вставляет сам демон** (`Jarvis.app`) через `tmux send-keys`.

Демон запущен как GUI-приложение из логин-айтемов, поэтому унаследовал **урезанный
launchd-PATH**:

```
PATH=/usr/bin:/bin:/usr/sbin:/sbin      # без /opt/homebrew/bin
```

А `tmux` (Homebrew) лежит в `/opt/homebrew/bin/tmux`. Проверка:

```sh
env -i PATH=/usr/bin:/bin tmux -L jarvis display-message -p -t %14 ok
# → env: tmux: No such file or directory   (exit 127)
env -i PATH=/opt/homebrew/bin:/usr/bin tmux -L jarvis display-message -p -t %14 ok
# → ok
```

В коде это было так: `src-tauri/src/tmux.rs` вызывал **голый** `Command::new("tmux")`
без правки PATH. `Command::new` резолвит бинарь по PATH процесса → у демона его нет
→ `tmux_j` возвращает ошибку → `pane_alive()` = false → `reply_core` уходит в
`tmux_needed()` → «Сессия не в tmux».

Причём проект **уже знал** про эту ловушку: в `src-tauri/src/install/mod.rs` есть
`augmented_path()` (комментарий прямо про «GUI-приложение наследует урезанный PATH
без Homebrew») и он применяется к вызовам claude/codex/python и даже к tmux в
`install/mod.rs`. Забыли только `tmux.rs`.

Почему `launchctl setenv PATH …` временно чинит: он подменяет PATH всей launchd-
сессии, и перезапущенный демон наследует уже нормальный PATH. Но это слетает при
следующем логине — не решение.

## Правка

Файл: `src-tauri/src/tmux.rs`.

1. Добавлен резолвер бинаря `tmux` (ищет по PATH процесса + типовым каталогам
   Homebrew, кэширует через `OnceLock`):

```rust
use std::sync::OnceLock;

fn tmux_bin() -> &'static str {
    static BIN: OnceLock<String> = OnceLock::new();
    BIN.get_or_init(|| {
        let mut dirs: Vec<std::path::PathBuf> = std::env::var("PATH")
            .unwrap_or_default()
            .split(':')
            .filter(|s| !s.is_empty())
            .map(std::path::PathBuf::from)
            .collect();
        for extra in ["/opt/homebrew/bin", "/usr/local/bin"] {
            let p = std::path::PathBuf::from(extra);
            if !dirs.contains(&p) {
                dirs.push(p);
            }
        }
        for d in dirs {
            let p = d.join("tmux");
            if p.is_file() {
                return p.to_string_lossy().into_owned();
            }
        }
        "tmux".to_string()
    })
}
```

2. Все `Command::new("tmux")` в `tmux.rs` заменены на `Command::new(tmux_bin())`
   (их четыре: `tmux_j`, `list_panes_meta`, и два в `focus` —
   `switch-client` / `select-window`).

`install/mod.rs` **менять не нужно** — там tmux-вызовы уже идут с
`.env("PATH", augmented_path())`.

### Как проверить, что правка на месте

```sh
grep -n 'Command::new("tmux")' src-tauri/src/tmux.rs   # должно быть пусто
grep -n 'tmux_bin()' src-tauri/src/tmux.rs             # 5 совпадений: определение + 4 вызова
```

## Сборка

Нужен Rust (`rustup`). Затем dev-запуск/сборка:

```sh
npm start          # собрать (cargo build --release) + ad-hoc codesign + запустить dev
# или бандл .app для замены /Applications/Jarvis.app:
npm run bundle
```

Тест именно этого фикса важно делать при **GUI-запуске** (логин-айтем / open -a),
а не из терминала: из терминала PATH и так с Homebrew, и баг не воспроизводится.

## Временный обход (если фикс ещё не собран)

```sh
launchctl setenv PATH "/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin"
killall jarvis; sleep 1; open -a Jarvis
```

Снять обход после установки собранного фикса:

```sh
launchctl unsetenv PATH
```

---

# Фикс 2: codex-сессия видна в Jarvis только после первого сообщения

## Симптом

Запускаешь `codex` в терминале — в панели Jarvis сессии нет. Появляется лишь
после того, как отправишь первое сообщение в codex-чат. У claude такого нет.

## Корень

**Codex не шлёт хук `SessionStart` при запуске TUI** — первый хук
(`SessionStart` + `UserPromptSubmit` вместе) прилетает только в момент первого
сообщения (когда создаётся rollout). Проверено вживую по `jarvis.log`: при
запуске codex — ноль событий; при первом сообщении — сразу `session-start` +
`prompt`. Claude же шлёт `SessionStart` при запуске, поэтому виден мгновенно.

При этом шим **уже обернул codex в tmux-пейн** на сервере jarvis, и демон эту
пану видит (`tmux list-panes` → `pane_current_command` = codex, `pane_pid` =
pid codex). Просто по прежней логике «сессии заводятся ТОЛЬКО из хуков» демон
эту пану игнорировал.

## Правка

1. `tmux.rs`: `PaneInfo` получил поле `command` (из `#{pane_current_command}`) —
   по нему узнаём codex-паны.
2. `daemon.rs`: чистая функция `reconcile_panes(sessions, panes, now)` (рядом с
   `evict_pane`, покрыта тестами) делает три вещи:
   - **починка по pid**: живой сессии без живой паны ставит пану по
     `pane_pid == pid` (чинит и «codex потерял `$TMUX_PANE` на resume»);
   - **дедуп**: снимает провизорную сессию, если появилась реальная с тем же pid;
   - **обнаружение**: для codex-паны без сессии заводит **провизорную** сессию
     (видна сразу, можно отправить первое сообщение прямо из Jarvis).
3. `main.rs`: новый таймер `pane_sweep()` раз в 5с (быстрее, чем 30с reconcile).
4. `model.rs`: у `Session` добавлено поле `provisional: bool`.
5. `ipc.rs` (`reply_core`): в провизорную сессию вставляем ровно один раз без
   ожидания ack/ретрая — иначе первое сообщение задвоится (ack придёт на
   реальный, ещё неизвестный, `session_id`).

Когда приходит настоящий хук codex, `reconcile_panes` дедупает провизорную по
pid, и остаётся одна реальная сессия с починенной паной.

### Как проверить

```sh
grep -n 'reconcile_panes\|pane_sweep\|is_codex_pane' src-tauri/src/daemon.rs
grep -n 'pane_current_command' src-tauri/src/tmux.rs
```
Тесты: `cargo test --bin jarvis reconcile_panes` (4 теста).

Живьём: запусти `codex`, ничего не печатай — в течение ~5с сессия должна
появиться в панели со статусом «запущен — ждёт первого сообщения».

---

# Фикс 3: UI-правки (панель + провизорные сессии + очистка)

Мелкие правки поверх фикса 2, все связаны с провизорными codex-сессиями и
удобством панели.

1. **Панель не прячется сама.** `main.rs` (`WindowEvent::Focused(false)`):
   авто-скрытие по потере фокуса за настройкой `autoHidePanel` (по умолчанию
   `false` → панель держится, пока не закроешь ⌘J/⌘W/крестиком). Вернуть старое
   поведение — `autoHidePanel: true` в settings.json.
2. **Фуллскрин панели по ⌘⇧F** (⌃⌘F нельзя — его перехватывает macOS «Enter Full
   Screen»). `macos.rs::place_panel_full` (весь `visibleFrame`),
   `windows.rs::toggle_panel_fullscreen` (+ статик `PANEL_FULL`, сброс в
   `position_panel`), команда `ipc.rs::panel_toggle_fullscreen`, регистрация в
   `main.rs`, мост `bridge.js` (`toggleFullscreen`), клавиша + кнопка-иконка
   (`tabFullscreen`) в `renderer.js`/`index.html`.
3. **Чат провизорной сессии.** `ipc.rs::chat_open`: если транскрипта нет, но
   `s.provisional` — открываем пустой чат (можно написать первое сообщение).
   `reply_core`: в провизорную вставляем один раз без ожидания ack (иначе
   задвоение). `renderer.js`: понятный текст пустого чата для `res.provisional`.
4. **Очистка завершённых + живые codex.** Провизорную codex-сессию `state_clear`
   удалял, но `pane_sweep` тут же воскрешал (codex-процесс жив). Фикс: демон
   держит `dismissed_panes: HashSet<i64>` — pid убранных codex-пан;
   `reconcile_panes` их не воскрешает, `pane_sweep` чистит набор от мёртвых pid.
   `state_clear` (`ipc.rs`) помечает pid убираемых codex-сессий через
   `dismiss_panes`.

## Осталось на будущее (ещё не сделано)

- **Сессия в твоём личном tmux** (`$TMUX=.../default`): шим не оборачивает
  (нельзя вкладывать tmux в tmux), сессия на сервере `default`, а демон ходит
  только в `-L jarvis`. Решение — отдавать сокет из хука и таргетить `-S <socket>`.
