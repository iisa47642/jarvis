//! Gemini-бэкенд (Google `gemini` CLI v0.46+). Sync-методы шва по образцу
//! codex.rs. Транскрипт — снапшотный JSONL в ~/.gemini/tmp/<user>/chats/
//! (см. gemini_transcript.rs). Хуки Claude-совместимого формата
//! (`gemini hooks migrate --from-claude`) — регистрация в install/mod.rs.

use serde_json::Value;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use super::{Agent, Backend};
use crate::transcript::ChatItem;

pub struct GeminiBackend;

/// Статический инстанс для диспетчера `backend()`.
pub static GEMINI: GeminiBackend = GeminiBackend;

/// Настоящий `gemini` в PATH (+типовые каталоги), минуя наш шим `~/.jarvis/shims`.
pub fn resolve_gemini_bin() -> Option<PathBuf> {
    let mut dirs: Vec<PathBuf> = std::env::var("PATH")
        .unwrap_or_default()
        .split(':')
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .collect();
    for extra in [
        crate::util::home_dir().join(".local/bin"),
        PathBuf::from("/opt/homebrew/bin"),
        PathBuf::from("/usr/local/bin"),
    ] {
        if !dirs.contains(&extra) {
            dirs.push(extra);
        }
    }
    let shims = crate::util::jarvis_dir().join("shims");
    for d in dirs {
        if d == shims {
            continue;
        }
        let p = d.join("gemini");
        if let Ok(meta) = std::fs::metadata(&p) {
            if meta.is_file() && meta.permissions().mode() & 0o111 != 0 {
                return Some(p);
            }
        }
    }
    None
}

/// Каталог данных gemini (`~/.gemini`).
pub fn gemini_dir() -> PathBuf {
    crate::util::home_dir().join(".gemini")
}

/// Найти chat-файл gemini по `session_id`. Safety-net на случай, когда хук не
/// принёс `transcript_path`: файлы лежат в `~/.gemini/tmp/<user>/chats/
/// session-<ts>-<короткий id>.jsonl`, а ПОЛНЫЙ sessionId — в первой строке
/// файла. Матчим сперва по хвосту имени (короткий id — префикс полного), при
/// неоднозначности читаем первую строку. Самый свежий по mtime.
pub fn find_chat_by_sid(sid: &str) -> Option<PathBuf> {
    find_chat_in(&gemini_dir().join("tmp"), sid)
}

/// Чистое ядро поиска (тестируется на temp-каталоге): обход `root` глубиной ≤4.
fn find_chat_in(root: &Path, sid: &str) -> Option<PathBuf> {
    if sid.is_empty() {
        return None;
    }
    let short = sid.split('-').next().unwrap_or(sid); // имя несёт короткий префикс id
    let mut best: Option<(std::time::SystemTime, PathBuf)> = None;
    fn walk(
        dir: &Path,
        sid: &str,
        short: &str,
        best: &mut Option<(std::time::SystemTime, PathBuf)>,
        depth: u8,
    ) {
        if depth > 4 {
            return;
        }
        let Ok(rd) = std::fs::read_dir(dir) else {
            return;
        };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                walk(&p, sid, short, best, depth + 1);
                continue;
            }
            let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if !name.starts_with("session-") || !name.ends_with(".jsonl") {
                continue;
            }
            // быстрый фильтр по короткому id в имени; точная сверка — по 1-й строке
            if !name.contains(short) {
                continue;
            }
            if !super::gemini_transcript::file_has_session_id(&p, sid) {
                continue;
            }
            if let Ok(mt) = e.metadata().and_then(|m| m.modified()) {
                if best.as_ref().is_none_or(|(bt, _)| mt > *bt) {
                    *best = Some((mt, p));
                }
            }
        }
    }
    walk(root, sid, short, &mut best, 0);
    best.map(|(_, p)| p)
}

impl Backend for GeminiBackend {
    fn agent(&self) -> Agent {
        Agent::Gemini
    }
    fn cli_found(&self) -> bool {
        resolve_gemini_bin().is_some()
    }
    fn read_entries(&self, file: &Path, max_bytes: u64) -> Vec<Value> {
        // Снапшотный лог: entries = messages последнего $set-снапшота.
        super::gemini_transcript::read_entries(file, max_bytes)
    }
    fn to_chat_items(&self, entry: &Value) -> Vec<ChatItem> {
        super::gemini_transcript::to_chat_items(entry)
    }
    fn extract_title(&self, entries: &[Value]) -> Option<String> {
        super::gemini_transcript::extract_title(entries)
    }
    fn extract_branch(&self, _entries: &[Value]) -> Option<String> {
        None // git-контекст в chat-jsonl не пишется
    }
    fn extract_model(&self, entries: &[Value]) -> Option<String> {
        super::gemini_transcript::extract_model(entries)
    }
    fn transcript_dir_for(&self, _cwd: &str) -> Option<PathBuf> {
        None // gemini группирует по projectHash (соль неизвестна) — ищем по sid
    }
    fn resume_cmd(&self, sid: &str) -> String {
        format!("gemini --resume {sid}")
    }
    fn friendly_model(&self, id: &str) -> String {
        let v = id.to_lowercase();
        if v.contains("flash") {
            return "Flash".to_string();
        }
        if v.contains("pro") {
            return "Pro".to_string();
        }
        if v.contains("gemma") {
            return "Gemma".to_string();
        }
        id.split('-').next().unwrap_or("").to_string()
    }
    fn models(&self) -> &'static [(&'static str, &'static str)] {
        &[("gemini-2.5-pro", "Pro"), ("gemini-2.5-flash", "Flash")]
    }
    fn effort_levels(&self) -> &'static [&'static str] {
        &[]
    }
    fn has_separate_effort(&self) -> bool {
        false
    }
    fn price(&self, model: &str) -> (f64, f64) {
        // ОЦЕНКА ($/1M in, out): pro-класс дороже flash.
        if model.to_lowercase().contains("flash") {
            (0.30, 2.50)
        } else {
            (1.25, 10.0)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn find_chat_in_matches_by_short_id_and_full_sid() {
        let dir = std::env::temp_dir().join(format!("jarvis-gem-{}", std::process::id()));
        let chats = dir.join("isaevisa/chats");
        std::fs::create_dir_all(&chats).unwrap();
        let sid = "6c7fb3fa-eb65-4dbf-a19a-54f3f8104ab9";
        let good = chats.join("session-2026-06-28T01-38-6c7fb3fa.jsonl");
        std::fs::write(
            &good,
            format!("{{\"sessionId\":\"{sid}\",\"kind\":\"main\"}}\n"),
        )
        .unwrap();
        // файл с тем же коротким id, но другим полным sessionId — не должен матчиться
        let decoy = chats.join("session-2026-06-01T00-00-6c7fb3fa.jsonl");
        std::fs::write(
            &decoy,
            "{\"sessionId\":\"6c7fb3fa-0000-0000-0000-000000000000\"}\n",
        )
        .unwrap();

        assert_eq!(find_chat_in(&dir, sid), Some(good));
        assert_eq!(find_chat_in(&dir, ""), None);
        assert_eq!(find_chat_in(&dir, "nope-nope"), None);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
