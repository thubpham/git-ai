use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::types::parse_rfc3339_secs;

const REQUEST_MARKER: &str = r#""requestId""#;
const PROMPT_MARKER: &str = r#""promptId""#;
const TURN_MARKER: &str = r#""turn_duration""#;

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CallFact {
    pub request_id: String,
    pub message_id: Option<String>,
    pub uuid: Option<String>,
    pub parent_uuid: Option<String>,
    pub prompt_id: Option<String>,
    pub model: Option<String>,
    pub advisor_model: Option<String>,
    pub effort: Option<String>,
    pub turn_effort: Option<String>,
    pub speed: Option<String>,
    pub stop_reason: Option<String>,
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub cache_read_tokens: Option<u64>,
    pub cache_write_tokens: Option<u64>,
    pub cache_write_5m_tokens: Option<u64>,
    pub cache_write_1h_tokens: Option<u64>,
    pub thinking_tokens: Option<u64>,
    pub iterations_json: Option<String>,
    pub timestamp: Option<u32>,
    pub thinking_ms: Option<u64>,
    pub tool_use_ids: Vec<String>,
    pub advisor_uses: Vec<AdvisorFact>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdvisorFact {
    pub index: u32,
    pub model: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TurnFact {
    pub prompt_id: String,
    pub duration_ms: Option<u64>,
    pub message_count: Option<u64>,
    pub timestamp: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TranscriptFact {
    Call(CallFact),
    Turn(TurnFact),
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct ClaudeFactsParser {
    prompt_id: Option<String>,
}

impl ClaudeFactsParser {
    pub fn wants_line(&self, line: &str) -> bool {
        line.contains(REQUEST_MARKER) || line.contains(PROMPT_MARKER) || line.contains(TURN_MARKER)
    }

    pub fn parse_line(&mut self, line: &str) -> Vec<TranscriptFact> {
        let Ok(entry) = serde_json::from_str::<Value>(line) else {
            return Vec::new();
        };
        match entry.get("type").and_then(Value::as_str) {
            Some("user") => {
                if let Some(prompt_id) = str_at(&entry, "/promptId") {
                    self.prompt_id = Some(prompt_id);
                }
                Vec::new()
            }
            Some("assistant") => self
                .call_fact(&entry)
                .map(TranscriptFact::Call)
                .into_iter()
                .collect(),
            Some("system") => self
                .turn_fact(&entry)
                .map(TranscriptFact::Turn)
                .into_iter()
                .collect(),
            _ => Vec::new(),
        }
    }

    fn call_fact(&self, entry: &Value) -> Option<CallFact> {
        let request_id = str_at(entry, "/requestId")?;
        let usage = entry.pointer("/message/usage")?;
        Some(CallFact {
            request_id,
            message_id: str_at(entry, "/message/id"),
            uuid: str_at(entry, "/uuid"),
            parent_uuid: str_at(entry, "/parentUuid"),
            prompt_id: self.prompt_id.clone(),
            model: str_at(entry, "/message/model"),
            advisor_model: str_at(entry, "/advisorModel"),
            effort: str_at(entry, "/effort"),
            turn_effort: str_at(entry, "/perTurnEffort"),
            speed: str_at(entry, "/message/usage/speed"),
            stop_reason: str_at(entry, "/message/stop_reason"),
            input_tokens: u64_at(entry, "/message/usage/input_tokens"),
            output_tokens: u64_at(entry, "/message/usage/output_tokens"),
            cache_read_tokens: u64_at(entry, "/message/usage/cache_read_input_tokens"),
            cache_write_tokens: u64_at(entry, "/message/usage/cache_creation_input_tokens"),
            cache_write_5m_tokens: u64_at(
                entry,
                "/message/usage/cache_creation/ephemeral_5m_input_tokens",
            ),
            cache_write_1h_tokens: u64_at(
                entry,
                "/message/usage/cache_creation/ephemeral_1h_input_tokens",
            ),
            thinking_tokens: u64_at(
                entry,
                "/message/usage/output_tokens_details/thinking_tokens",
            ),
            iterations_json: usage.get("iterations").map(Value::to_string),
            timestamp: str_at(entry, "/timestamp").and_then(|ts| parse_rfc3339_secs(&ts)),
            thinking_ms: u64_at(entry, "/thinkingDurationMs"),
            tool_use_ids: tool_use_ids(entry),
            advisor_uses: advisor_uses(usage),
        })
    }

    fn turn_fact(&self, entry: &Value) -> Option<TurnFact> {
        if entry.get("subtype").and_then(Value::as_str) != Some("turn_duration") {
            return None;
        }
        Some(TurnFact {
            prompt_id: self.prompt_id.clone()?,
            duration_ms: u64_at(entry, "/durationMs"),
            message_count: u64_at(entry, "/messageCount"),
            timestamp: str_at(entry, "/timestamp").and_then(|ts| parse_rfc3339_secs(&ts)),
        })
    }
}

fn str_at(value: &Value, pointer: &str) -> Option<String> {
    value
        .pointer(pointer)
        .and_then(Value::as_str)
        .map(str::to_string)
}

fn u64_at(value: &Value, pointer: &str) -> Option<u64> {
    value.pointer(pointer).and_then(Value::as_u64)
}

fn tool_use_ids(entry: &Value) -> Vec<String> {
    entry
        .pointer("/message/content")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|block| block.get("type").and_then(Value::as_str) == Some("tool_use"))
        .filter_map(|block| block.get("id").and_then(Value::as_str).map(str::to_string))
        .collect()
}

fn advisor_uses(usage: &Value) -> Vec<AdvisorFact> {
    let count = |item: &Value, field: &str| item.get(field).and_then(Value::as_u64).unwrap_or(0);
    usage
        .get("iterations")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|item| item.get("type").and_then(Value::as_str) == Some("advisor_message"))
        .filter_map(|item| {
            let model = item.get("model").and_then(Value::as_str)?;
            (!model.is_empty()).then(|| (model.to_string(), item))
        })
        .enumerate()
        .map(|(index, (model, item))| AdvisorFact {
            index: index as u32,
            model,
            input_tokens: count(item, "input_tokens"),
            output_tokens: count(item, "output_tokens"),
            cache_read_tokens: count(item, "cache_read_input_tokens"),
            cache_write_tokens: count(item, "cache_creation_input_tokens"),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn user_line(prompt_id: &str) -> String {
        json!({
            "type": "user",
            "promptId": prompt_id,
            "uuid": "u-1",
            "message": {"role": "user", "content": "hi"}
        })
        .to_string()
    }

    fn assistant_line(request_id: &str, content: Value, extra: Value) -> String {
        let mut entry = json!({
            "type": "assistant",
            "uuid": "a-1",
            "parentUuid": "u-1",
            "requestId": request_id,
            "timestamp": "2026-01-01T00:00:10.000Z",
            "advisorModel": "advisor-model",
            "effort": "medium",
            "perTurnEffort": "high",
            "message": {
                "id": "msg-1",
                "model": "main-model",
                "stop_reason": "tool_use",
                "content": content,
                "usage": {
                    "input_tokens": 2,
                    "output_tokens": 286,
                    "cache_read_input_tokens": 36109,
                    "cache_creation_input_tokens": 500,
                    "cache_creation": {
                        "ephemeral_5m_input_tokens": 400,
                        "ephemeral_1h_input_tokens": 100
                    },
                    "output_tokens_details": {"thinking_tokens": 120},
                    "speed": "standard",
                    "iterations": [{"type": "message"}]
                }
            }
        });
        for (key, value) in extra.as_object().unwrap() {
            entry[key] = value.clone();
        }
        entry.to_string()
    }

    fn only_call(facts: Vec<TranscriptFact>) -> CallFact {
        match facts.into_iter().next() {
            Some(TranscriptFact::Call(call)) => call,
            other => panic!("expected a call fact, got {other:?}"),
        }
    }

    #[test]
    fn extracts_every_call_field_from_an_assistant_line() {
        let mut parser = ClaudeFactsParser::default();
        parser.parse_line(&user_line("prompt-1"));
        let line = assistant_line(
            "req-1",
            json!([{"type": "tool_use", "id": "toolu_1", "name": "Edit"}]),
            json!({"thinkingDurationMs": 4200}),
        );

        let call = only_call(parser.parse_line(&line));

        assert_eq!(call.request_id, "req-1");
        assert_eq!(call.message_id.as_deref(), Some("msg-1"));
        assert_eq!(call.uuid.as_deref(), Some("a-1"));
        assert_eq!(call.parent_uuid.as_deref(), Some("u-1"));
        assert_eq!(call.prompt_id.as_deref(), Some("prompt-1"));
        assert_eq!(call.model.as_deref(), Some("main-model"));
        assert_eq!(call.advisor_model.as_deref(), Some("advisor-model"));
        assert_eq!(call.effort.as_deref(), Some("medium"));
        assert_eq!(call.turn_effort.as_deref(), Some("high"));
        assert_eq!(call.speed.as_deref(), Some("standard"));
        assert_eq!(call.stop_reason.as_deref(), Some("tool_use"));
        assert_eq!(call.input_tokens, Some(2));
        assert_eq!(call.output_tokens, Some(286));
        assert_eq!(call.cache_read_tokens, Some(36109));
        assert_eq!(call.cache_write_tokens, Some(500));
        assert_eq!(call.cache_write_5m_tokens, Some(400));
        assert_eq!(call.cache_write_1h_tokens, Some(100));
        assert_eq!(call.thinking_tokens, Some(120));
        assert_eq!(
            call.iterations_json.as_deref(),
            Some(r#"[{"type":"message"}]"#)
        );
        assert_eq!(call.timestamp, Some(1_767_225_610));
        assert_eq!(call.thinking_ms, Some(4200));
        assert_eq!(call.tool_use_ids, vec!["toolu_1".to_string()]);
        assert!(call.advisor_uses.is_empty());
    }

    #[test]
    fn lines_of_one_call_share_a_request_id_and_split_their_content() {
        let mut parser = ClaudeFactsParser::default();
        let thinking = assistant_line(
            "req-1",
            json!([{"type": "thinking"}]),
            json!({"thinkingDurationMs": 900}),
        );
        let tool_use = assistant_line(
            "req-1",
            json!([{"type": "tool_use", "id": "toolu_1"}]),
            json!({}),
        );

        let first = only_call(parser.parse_line(&thinking));
        let second = only_call(parser.parse_line(&tool_use));

        assert_eq!(first.request_id, second.request_id);
        assert_eq!(first.output_tokens, second.output_tokens);
        assert_eq!(first.thinking_ms, Some(900));
        assert_eq!(second.thinking_ms, None);
        assert!(first.tool_use_ids.is_empty());
        assert_eq!(second.tool_use_ids, vec!["toolu_1".to_string()]);
    }

    #[test]
    fn collects_multiple_tool_use_ids_and_ignores_other_blocks() {
        let mut parser = ClaudeFactsParser::default();
        let line = assistant_line(
            "req-1",
            json!([
                {"type": "text", "text": "ok"},
                {"type": "tool_use", "id": "toolu_1"},
                {"type": "tool_use", "id": "toolu_2"}
            ]),
            json!({}),
        );

        let call = only_call(parser.parse_line(&line));

        assert_eq!(
            call.tool_use_ids,
            vec!["toolu_1".to_string(), "toolu_2".to_string()]
        );
    }

    #[test]
    fn numbers_advisor_uses_in_order_and_skips_plain_iterations() {
        let mut parser = ClaudeFactsParser::default();
        let mut entry: Value =
            serde_json::from_str(&assistant_line("req-1", json!([]), json!({}))).unwrap();
        entry["message"]["usage"]["iterations"] = json!([
            {"type": "message", "input_tokens": 1},
            {"type": "advisor_message", "model": "adv-a", "input_tokens": 10, "output_tokens": 2,
             "cache_read_input_tokens": 3, "cache_creation_input_tokens": 4},
            {"type": "advisor_message", "model": "adv-b", "input_tokens": 20}
        ]);

        let call = only_call(parser.parse_line(&entry.to_string()));

        assert_eq!(
            call.advisor_uses,
            vec![
                AdvisorFact {
                    index: 0,
                    model: "adv-a".to_string(),
                    input_tokens: 10,
                    output_tokens: 2,
                    cache_read_tokens: 3,
                    cache_write_tokens: 4,
                },
                AdvisorFact {
                    index: 1,
                    model: "adv-b".to_string(),
                    input_tokens: 20,
                    output_tokens: 0,
                    cache_read_tokens: 0,
                    cache_write_tokens: 0,
                },
            ]
        );
    }

    #[test]
    fn drops_advisor_items_without_a_model() {
        let mut parser = ClaudeFactsParser::default();
        let mut entry: Value =
            serde_json::from_str(&assistant_line("req-1", json!([]), json!({}))).unwrap();
        entry["message"]["usage"]["iterations"] = json!([
            {"type": "advisor_message", "input_tokens": 5},
            {"type": "advisor_message", "model": "", "input_tokens": 6},
            {"type": "advisor_message", "model": "adv-a", "input_tokens": 7}
        ]);

        let call = only_call(parser.parse_line(&entry.to_string()));

        assert_eq!(call.advisor_uses.len(), 1);
        assert_eq!(call.advisor_uses[0].index, 0);
        assert_eq!(call.advisor_uses[0].input_tokens, 7);
    }

    #[test]
    fn missing_optional_fields_become_none() {
        let mut parser = ClaudeFactsParser::default();
        let line = json!({
            "type": "assistant",
            "requestId": "req-1",
            "message": {"usage": {"input_tokens": 1, "output_tokens": 2}}
        })
        .to_string();

        let call = only_call(parser.parse_line(&line));

        assert_eq!(call.request_id, "req-1");
        assert_eq!(call.prompt_id, None);
        assert_eq!(call.model, None);
        assert_eq!(call.cache_read_tokens, None);
        assert_eq!(call.cache_write_1h_tokens, None);
        assert_eq!(call.thinking_tokens, None);
        assert_eq!(call.timestamp, None);
        assert!(call.tool_use_ids.is_empty());
    }

    #[test]
    fn assistant_lines_without_a_usable_request_id_yield_nothing() {
        let mut parser = ClaudeFactsParser::default();
        let no_request = json!({"type": "assistant", "message": {"usage": {}}}).to_string();
        let null_request =
            json!({"type": "assistant", "requestId": null, "message": {"usage": {}}}).to_string();
        let no_usage =
            json!({"type": "assistant", "requestId": "req-1", "message": {}}).to_string();

        assert!(parser.parse_line(&no_request).is_empty());
        assert!(parser.parse_line(&null_request).is_empty());
        assert!(parser.parse_line(&no_usage).is_empty());
    }

    #[test]
    fn invalid_json_and_unrelated_entry_types_yield_nothing() {
        let mut parser = ClaudeFactsParser::default();

        assert!(parser.parse_line("not json").is_empty());
        assert!(
            parser
                .parse_line(r#"{"type":"mode","mode":"normal"}"#)
                .is_empty()
        );
        assert!(
            parser
                .parse_line(r#"{"type":"attachment","requestId":"x"}"#)
                .is_empty()
        );
    }

    #[test]
    fn latest_user_prompt_id_is_attached_to_later_calls() {
        let mut parser = ClaudeFactsParser::default();
        let line = assistant_line("req-1", json!([]), json!({}));

        parser.parse_line(&user_line("prompt-1"));
        let first = only_call(parser.parse_line(&line));
        parser.parse_line(&user_line("prompt-2"));
        let second = only_call(parser.parse_line(&line));

        assert_eq!(first.prompt_id.as_deref(), Some("prompt-1"));
        assert_eq!(second.prompt_id.as_deref(), Some("prompt-2"));
    }

    #[test]
    fn turn_duration_is_keyed_by_the_current_prompt() {
        let mut parser = ClaudeFactsParser::default();
        parser.parse_line(&user_line("prompt-1"));
        let line = json!({
            "type": "system",
            "subtype": "turn_duration",
            "durationMs": 61000,
            "messageCount": 14,
            "timestamp": "2026-01-01T00:01:00.000Z"
        })
        .to_string();

        let facts = parser.parse_line(&line);

        assert_eq!(
            facts,
            vec![TranscriptFact::Turn(TurnFact {
                prompt_id: "prompt-1".to_string(),
                duration_ms: Some(61000),
                message_count: Some(14),
                timestamp: Some(1_767_225_660),
            })]
        );
    }

    #[test]
    fn turn_duration_without_a_prompt_or_other_subtypes_yield_nothing() {
        let mut parser = ClaudeFactsParser::default();
        let turn =
            json!({"type": "system", "subtype": "turn_duration", "durationMs": 1}).to_string();
        let hook = json!({"type": "system", "subtype": "stop_hook_summary"}).to_string();

        assert!(parser.parse_line(&turn).is_empty());
        parser.parse_line(&user_line("prompt-1"));
        assert!(parser.parse_line(&hook).is_empty());
    }

    #[test]
    fn wants_line_matches_calls_prompts_and_turn_durations_only() {
        let parser = ClaudeFactsParser::default();

        assert!(parser.wants_line(&assistant_line("req-1", json!([]), json!({}))));
        assert!(parser.wants_line(&user_line("prompt-1")));
        assert!(parser.wants_line(r#"{"type":"system","subtype":"turn_duration"}"#));
        assert!(!parser.wants_line(r#"{"type":"mode","mode":"normal"}"#));
    }
}
