//! Парсер chat-транскрипта Gemini CLI → общий `ChatItem`.
//!
//! Формат (`~/.gemini/tmp/<user>/chats/session-<ts>-<id8>.jsonl`, v0.46):
//! строка 1 — метаданные `{sessionId, projectHash, startTime, kind}`; дальше
//! СНАПШОТЫ состояния `{"$set":{"messages":[{id,timestamp,type,content:[{text}]}]}}`
//! — не аппенд-лог, как у Claude: последний `$set` содержит ВСЮ переписку.
//! Defensive: неизвестное поле → пропуск, битая строка → скип, не паникуем.

use serde_json::Value;
use std::path::Path;

use crate::transcript::{parse_ts, ChatItem};
use crate::util::{ellipsize, now_ms, one_line};

/// «Записи» gemini-транскрипта = messages ПОСЛЕДНЕГО $set-снапшота хвоста файла.
/// Сигнатура повторяет Backend::read_entries — дальше конвейер общий.
pub fn read_entries(file: &Path, max_bytes: u64) -> Vec<Value> {
    let lines = crate::transcript::read_recent_entries(file, max_bytes);
    lines
        .iter()
        .rev()
        .find_map(|v| {
            v.pointer("/$set/messages")
                .and_then(Value::as_array)
                .cloned()
        })
        .unwrap_or_default()
}

/// Первая строка файла несёт полный sessionId — точная сверка для поиска по sid.
pub fn file_has_session_id(file: &Path, sid: &str) -> bool {
    let Ok(f) = std::fs::File::open(file) else {
        return false;
    };
    use std::io::{BufRead, BufReader};
    let mut line = String::new();
    if BufReader::new(f).read_line(&mut line).is_err() {
        return false;
    }
    serde_json::from_str::<Value>(&line)
        .ok()
        .and_then(|v| v.get("sessionId").and_then(Value::as_str).map(|s| s == sid))
        .unwrap_or(false)
}

/// Роль сообщения: gemini пишет `type:"user"` и `type:"gemini"` (терпимо
/// принимаем и assistant/model — формат дрейфует между версиями CLI).
fn role_of(t: &str) -> Option<&'static str> {
    match t {
        "user" => Some("user"),
        "gemini" | "assistant" | "model" => Some("assistant"),
        _ => None,
    }
}

/// Один message → элементы чата. Служебные инъекции (`<session_context>` и
/// прочие `<...>`) — мимо ленты, как у Claude/Codex.
pub fn to_chat_items(entry: &Value) -> Vec<ChatItem> {
    let Some(role) = entry.get("type").and_then(Value::as_str).and_then(role_of) else {
        return vec![];
    };
    let ts = entry
        .get("timestamp")
        .and_then(Value::as_str)
        .and_then(parse_ts)
        .unwrap_or_else(now_ms);
    let mut out = Vec::new();
    for b in entry
        .get("content")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[])
    {
        let text = b.get("text").and_then(Value::as_str).unwrap_or("");
        if text.is_empty() || text.trim_start().starts_with('<') {
            continue;
        }
        out.push(ChatItem {
            role,
            kind: "text",
            text: ellipsize(text, 4000),
            ts,
            diff: None,
            stat: None,
        });
    }
    out
}

/// Модель: best-effort из поля model сообщения (может отсутствовать вовсе).
pub fn extract_model(entries: &[Value]) -> Option<String> {
    entries.iter().rev().find_map(|e| {
        e.get("model")
            .and_then(Value::as_str)
            .filter(|m| !m.is_empty())
            .map(String::from)
    })
}

/// Заголовок: первая содержательная реплика юзера.
pub fn extract_title(entries: &[Value]) -> Option<String> {
    for e in entries {
        for item in to_chat_items(e) {
            if item.role == "user" {
                let t = ellipsize(&one_line(&item.text), 60);
                if !t.is_empty() {
                    return Some(t);
                }
            }
        }
    }
    None
}

/// Финальный ответ ассистента — последний assistant-текст (семантика как у
/// codex_transcript::full_final_reply). Для моста: полный текст, обрезает демон.
pub fn full_final_reply(entries: &[Value]) -> Option<String> {
    let mut last: Option<String> = None;
    for e in entries {
        for item in to_chat_items(e) {
            if item.role == "assistant" && !item.text.trim().is_empty() {
                last = Some(item.text);
            }
        }
    }
    last
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn set_line(messages: Value) -> Value {
        json!({ "$set": { "messages": messages } })
    }

    fn msg(t: &str, text: &str) -> Value {
        json!({
            "id": "m1",
            "timestamp": "2026-06-28T01:38:48.770Z",
            "type": t,
            "content": [{ "text": text }]
        })
    }

    #[test]
    fn last_set_snapshot_wins() {
        let file = std::env::temp_dir().join(format!("jarvis-gemtr-{}.jsonl", std::process::id()));
        let lines = [
            json!({"sessionId":"abc-1","kind":"main"}).to_string(),
            set_line(json!([msg("user", "старый")])).to_string(),
            set_line(json!([msg("user", "привет"), msg("gemini", "здравствуй")])).to_string(),
        ];
        std::fs::write(&file, lines.join("\n")).unwrap();
        let entries = read_entries(&file, 512 * 1024);
        assert_eq!(entries.len(), 2, "messages из ПОСЛЕДНЕГО $set");
        assert_eq!(full_final_reply(&entries).as_deref(), Some("здравствуй"));
        assert_eq!(extract_title(&entries).as_deref(), Some("привет"));
        let _ = std::fs::remove_file(&file);
    }

    #[test]
    fn session_context_injection_skipped() {
        let items = to_chat_items(&msg("user", "<session_context>\nмусор"));
        assert!(items.is_empty(), "служебная инъекция не в ленте");
        let items = to_chat_items(&msg("user", "нормальный текст"));
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].role, "user");
    }

    #[test]
    fn roles_map_and_unknown_skipped() {
        assert_eq!(to_chat_items(&msg("gemini", "ответ"))[0].role, "assistant");
        assert_eq!(to_chat_items(&msg("model", "ответ"))[0].role, "assistant");
        assert!(to_chat_items(&msg("tool", "x")).is_empty());
        // битые структуры не паникуют
        assert!(to_chat_items(&json!({"type":"user"})).is_empty());
        assert!(to_chat_items(&json!("строка")).is_empty());
    }

    #[test]
    fn file_has_session_id_checks_first_line() {
        let file = std::env::temp_dir().join(format!("jarvis-gemid-{}.jsonl", std::process::id()));
        std::fs::write(&file, "{\"sessionId\":\"full-id-123\"}\n{\"$set\":{}}\n").unwrap();
        assert!(file_has_session_id(&file, "full-id-123"));
        assert!(!file_has_session_id(&file, "other"));
        let _ = std::fs::remove_file(&file);
    }
}
