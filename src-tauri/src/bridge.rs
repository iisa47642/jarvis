//! Multi-agent bridge: explicit opt-in coordination between existing sessions.
//!
//! V1 is deliberately conservative. It does not create hidden agents and it
//! only forwards messages when a bridge is active. A finished worker reports to
//! the lead; explicit `@role:` / `@agent:` directives fan work out to targets.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::Arc;

use crate::daemon::Daemon;
use crate::model::Status;
use crate::util::{ellipsize, now_ms, one_line};

const DEFAULT_MAX_ROUNDS: u32 = 12;
const MAX_RELAY_CHARS: usize = 6000;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Bridge {
    pub id: String,
    pub active: bool,
    pub paused: bool,
    pub lead_sid: String,
    pub max_rounds: u32,
    pub rounds: u32,
    pub members: Vec<BridgeMember>,
    pub events: Vec<BridgeEvent>,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BridgeMember {
    pub sid: String,
    pub role: String,
    pub agent: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub in_flight: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BridgeEvent {
    pub at: i64,
    pub kind: String,
    pub from_sid: Option<String>,
    pub to_sid: Option<String>,
    pub text: String,
}

#[derive(Debug, Clone)]
pub struct BridgeAction {
    pub to_sid: String,
    pub text: String,
}

fn default_true() -> bool {
    true
}

impl Bridge {
    pub fn new(lead_sid: String, members: Vec<BridgeMember>, max_rounds: Option<u32>) -> Self {
        let now = now_ms();
        let max_rounds = max_rounds.unwrap_or(DEFAULT_MAX_ROUNDS).clamp(1, 64);
        Bridge {
            id: format!("bridge-{now}"),
            active: true,
            paused: false,
            lead_sid,
            max_rounds,
            rounds: 0,
            members,
            events: Vec::new(),
            created_at: now,
            updated_at: now,
        }
    }

    fn member(&self, sid: &str) -> Option<&BridgeMember> {
        self.members.iter().find(|m| m.sid == sid && m.enabled)
    }

    fn member_mut(&mut self, sid: &str) -> Option<&mut BridgeMember> {
        self.members.iter_mut().find(|m| m.sid == sid && m.enabled)
    }

    fn lead(&self) -> Option<&BridgeMember> {
        self.member(&self.lead_sid)
    }

    fn target(&self, name: &str) -> Option<&BridgeMember> {
        let key = norm_key(name);
        self.members.iter().find(|m| {
            m.enabled
                && (norm_key(&m.role) == key
                    || norm_key(&m.agent) == key
                    || norm_key(&m.sid).starts_with(&key))
        })
    }

    fn event(
        &mut self,
        kind: &str,
        from_sid: Option<String>,
        to_sid: Option<String>,
        text: String,
    ) {
        self.updated_at = now_ms();
        self.events.push(BridgeEvent {
            at: self.updated_at,
            kind: kind.to_string(),
            from_sid,
            to_sid,
            text: ellipsize(&one_line(&text), 240),
        });
        if self.events.len() > 200 {
            self.events.drain(0..self.events.len() - 200);
        }
    }
}

pub fn member_from_json(v: &Value) -> Option<BridgeMember> {
    let sid = v
        .get("sid")
        .or_else(|| v.get("sessionId"))?
        .as_str()?
        .trim();
    if sid.is_empty() {
        return None;
    }
    let role = v
        .get("role")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("agent");
    let agent = v
        .get("agent")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("agent");
    Some(BridgeMember {
        sid: sid.to_string(),
        role: role.to_string(),
        agent: agent.to_string(),
        enabled: v.get("enabled").and_then(Value::as_bool).unwrap_or(true),
        in_flight: false,
    })
}

pub fn parse_directives(text: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut in_fence = false;
    for raw in text.lines() {
        let line = raw.trim();
        if line.starts_with("```") {
            in_fence = !in_fence;
            continue;
        }
        if in_fence || !line.starts_with('@') {
            continue;
        }
        if norm_key(line.trim_start_matches('@')) == "done" {
            out.push(("done".to_string(), String::new()));
            continue;
        }
        let Some((target, body)) = line[1..].split_once(':') else {
            continue;
        };
        let target = target.trim();
        let body = body.trim();
        if !target.is_empty() && !body.is_empty() {
            out.push((target.to_string(), body.to_string()));
        }
    }
    out
}

pub fn step_on_stop(bridge: &mut Bridge, sid: &str, reply: &str) -> Vec<BridgeAction> {
    if !bridge.active || bridge.paused {
        return Vec::new();
    }
    let Some(member) = bridge.member(sid).cloned() else {
        return Vec::new();
    };
    if let Some(m) = bridge.member_mut(sid) {
        m.in_flight = false;
    }
    bridge.rounds = bridge.rounds.saturating_add(1);
    bridge.event(
        "stop",
        Some(sid.to_string()),
        None,
        format!("{} ответил", member.role),
    );

    if bridge.rounds >= bridge.max_rounds {
        bridge.paused = true;
        bridge.event(
            "paused",
            None,
            None,
            "Достигнут лимит раундов bridge".into(),
        );
        return Vec::new();
    }

    let directives = parse_directives(reply);
    if directives.iter().any(|(target, _)| target == "done") {
        bridge.active = false;
        bridge.event("done", Some(sid.to_string()), None, "@done".into());
        return Vec::new();
    }

    let mut actions = Vec::new();
    for (target, body) in directives {
        if target == "done" {
            continue;
        }
        let Some(dst) = bridge.target(&target).cloned() else {
            bridge.event(
                "miss",
                Some(sid.to_string()),
                None,
                format!("@{target} не найден"),
            );
            continue;
        };
        if dst.sid == sid {
            continue;
        }
        let text = format_forward(&member, &body);
        bridge.event(
            "relay",
            Some(sid.to_string()),
            Some(dst.sid.clone()),
            text.clone(),
        );
        if let Some(m) = bridge.member_mut(&dst.sid) {
            m.in_flight = true;
        }
        actions.push(BridgeAction {
            to_sid: dst.sid,
            text,
        });
    }

    if actions.is_empty() && sid != bridge.lead_sid {
        if let Some(lead) = bridge.lead().cloned() {
            let text = format_worker_report(&member, reply);
            bridge.event(
                "report",
                Some(sid.to_string()),
                Some(lead.sid.clone()),
                text.clone(),
            );
            if let Some(m) = bridge.member_mut(&lead.sid) {
                m.in_flight = true;
            }
            actions.push(BridgeAction {
                to_sid: lead.sid,
                text,
            });
        }
    }
    actions
}

pub async fn relay_after_stop(d: Arc<Daemon>, sid: String) {
    tokio::time::sleep(std::time::Duration::from_millis(800)).await;
    let Some(reply) = final_reply(&d, &sid) else {
        return;
    };
    let actions = {
        let mut guard = d.bridge.lock().unwrap();
        let Some(bridge) = guard.as_mut() else { return };
        step_on_stop(bridge, &sid, &reply)
    };
    if !actions.is_empty() {
        d.push_bridge();
    }
    for action in actions {
        let _ = crate::ipc::reply_core(&d, action.to_sid, action.text).await;
    }
}

pub fn final_reply(d: &Daemon, sid: &str) -> Option<String> {
    let s = d.session(sid)?;
    if s.status == Status::Working {
        return None;
    }
    let tr = s.transcript.as_deref()?;
    let agent = crate::backend::Agent::from_opt(s.agent.as_deref());
    let text = match agent {
        crate::backend::Agent::Claude => crate::transcript::full_final_reply(tr)?,
        crate::backend::Agent::Codex => {
            let entries =
                crate::backend::backend(agent).read_entries(std::path::Path::new(tr), 512 * 1024);
            crate::backend::codex_transcript::full_final_reply(&entries)?
        }
        crate::backend::Agent::Gemini => {
            let entries =
                crate::backend::backend(agent).read_entries(std::path::Path::new(tr), 512 * 1024);
            crate::backend::gemini_transcript::full_final_reply(&entries)?
        }
    };
    let text = text.trim();
    (!text.is_empty()).then(|| ellipsize(text, MAX_RELAY_CHARS))
}

fn format_forward(from: &BridgeMember, task: &str) -> String {
    format!(
        "Multi-agent bridge.\nОт: {} ({})\n\nЗадача:\n{}",
        from.role,
        from.agent,
        ellipsize(task.trim(), MAX_RELAY_CHARS)
    )
}

fn format_worker_report(from: &BridgeMember, reply: &str) -> String {
    format!(
        "Multi-agent bridge: ответ от {} ({}).\n\n{}",
        from.role,
        from.agent,
        ellipsize(reply.trim(), MAX_RELAY_CHARS)
    )
}

fn norm_key(s: &str) -> String {
    s.trim()
        .trim_start_matches('@')
        .trim_end_matches(':')
        .to_ascii_lowercase()
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .collect()
}

pub fn bridge_json(bridge: &Option<Bridge>) -> Value {
    match bridge {
        Some(b) => serde_json::to_value(b).unwrap_or_else(|_| json!(null)),
        None => json!(null),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn member(sid: &str, role: &str, agent: &str) -> BridgeMember {
        BridgeMember {
            sid: sid.into(),
            role: role.into(),
            agent: agent.into(),
            enabled: true,
            in_flight: false,
        }
    }

    #[test]
    fn parse_directives_ignores_fenced_code() {
        let text = "@codex: проверь\n```md\n@claude: не команда\n```\n@done";
        assert_eq!(
            parse_directives(text),
            vec![
                ("codex".to_string(), "проверь".to_string()),
                ("done".to_string(), String::new())
            ]
        );
    }

    #[test]
    fn lead_directive_routes_to_role_or_agent() {
        let mut b = Bridge::new(
            "c1".into(),
            vec![
                member("c1", "lead", "claude"),
                member("x2", "reviewer", "codex"),
            ],
            Some(4),
        );
        let actions = step_on_stop(&mut b, "c1", "@reviewer: найди риски");
        assert_eq!(actions.len(), 1);
        assert_eq!(actions[0].to_sid, "x2");
        assert!(actions[0].text.contains("найди риски"));
        assert!(b.members.iter().find(|m| m.sid == "x2").unwrap().in_flight);
    }

    #[test]
    fn worker_reports_to_lead_without_directives() {
        let mut b = Bridge::new(
            "c1".into(),
            vec![
                member("c1", "lead", "claude"),
                member("x2", "reviewer", "codex"),
            ],
            Some(4),
        );
        let actions = step_on_stop(&mut b, "x2", "нашёл баг");
        assert_eq!(actions.len(), 1);
        assert_eq!(actions[0].to_sid, "c1");
        assert!(actions[0].text.contains("нашёл баг"));
    }

    #[test]
    fn round_limit_pauses_bridge() {
        let mut b = Bridge::new(
            "c1".into(),
            vec![
                member("c1", "lead", "claude"),
                member("x2", "reviewer", "codex"),
            ],
            Some(1),
        );
        let actions = step_on_stop(&mut b, "x2", "готово");
        assert!(actions.is_empty());
        assert!(b.paused);
    }
}
