use serde_json::{Map, Value, json};

use crate::{
    agent::RunContext,
    domain::{Message, SessionStateData},
    llm::{ModelMessage, ModelRequest},
};

pub(crate) const DEFAULT_CONTEXT_MAX_CHARS: usize = 24_000;
pub(crate) const DEFAULT_RECENT_CHARS: usize = 12_000;
const SUMMARY_LIMIT: usize = 6_000;

#[derive(Clone, Debug)]
pub(crate) struct ConversationMemory {
    pub(crate) recent_messages: Vec<Message>,
    pub(crate) durable_summary: Option<String>,
    pub(crate) confirmed_facts: Map<String, Value>,
}

impl ConversationMemory {
    pub(crate) async fn build(
        context: &RunContext,
        messages: &[Message],
        state: &mut SessionStateData,
        current_message: Option<&str>,
        max_chars: usize,
        recent_chars: usize,
    ) -> Self {
        let history = without_repeated_current_message(messages, current_message);
        let total_chars = history
            .iter()
            .map(|message| message.content.chars().count())
            .sum::<usize>()
            + current_message
                .map(str::chars)
                .map(Iterator::count)
                .unwrap_or(0);
        let first_user_message = history
            .iter()
            .find(|message| message.role == "user")
            .map(|message| message.content.clone())
            .or_else(|| current_message.map(str::to_owned));

        if total_chars <= max_chars {
            return Self {
                recent_messages: history.to_vec(),
                durable_summary: existing_summary(state),
                confirmed_facts: confirmed_facts(state),
            };
        }

        let latest_chars = current_message
            .map(str::chars)
            .map(Iterator::count)
            .unwrap_or(0);
        let split_at = recent_start(history, recent_chars.saturating_sub(latest_chars));
        let mut recent_messages = history[split_at..].to_vec();
        if let Some(first) = first_user_message.as_deref()
            && !recent_messages
                .iter()
                .any(|message| message.role == "user" && message.content == first)
            && let Some(original) = history
                .iter()
                .find(|message| message.role == "user" && message.content == first)
        {
            let mut retained_first = original.clone();
            retained_first.content = format!("[First User Message]\n{first}");
            recent_messages.insert(0, retained_first);
        }
        let memory = state
            .extra
            .get("conversation_memory")
            .and_then(Value::as_object);
        let previous_summary = memory
            .and_then(|value| value.get("summary"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .trim()
            .to_owned();
        let covered_count = memory
            .and_then(|value| value.get("covered_message_count"))
            .and_then(Value::as_u64)
            .unwrap_or(0)
            .min(split_at as u64) as usize;
        let newly_covered = &history[covered_count..split_at];
        let summary = if newly_covered.is_empty() && !previous_summary.is_empty() {
            previous_summary
        } else {
            summarize(context, &previous_summary, newly_covered, state).await
        };
        state.extra.insert(
            "conversation_memory".to_owned(),
            json!({
                "summary": summary,
                "covered_message_count": split_at,
                "first_user_message": first_user_message,
            }),
        );
        state.extra.insert(
            "conversation_memory_summary".to_owned(),
            Value::String(summary.clone()),
        );
        Self {
            recent_messages,
            durable_summary: (!summary.is_empty()).then_some(summary),
            confirmed_facts: confirmed_facts(state),
        }
    }

    #[cfg(test)]
    fn from_messages_with_limits(
        messages: &[Message],
        state: &SessionStateData,
        current_message: Option<&str>,
        max_chars: usize,
        recent_chars: usize,
    ) -> Self {
        let history = without_repeated_current_message(messages, current_message);
        let total = history
            .iter()
            .map(|message| message.content.chars().count())
            .sum::<usize>()
            + current_message
                .map(str::chars)
                .map(Iterator::count)
                .unwrap_or(0);
        if total <= max_chars {
            return Self {
                recent_messages: history.to_vec(),
                durable_summary: existing_summary(state),
                confirmed_facts: confirmed_facts(state),
            };
        }
        let current_chars = current_message
            .map(str::chars)
            .map(Iterator::count)
            .unwrap_or(0);
        let split_at = recent_start(history, recent_chars.saturating_sub(current_chars));
        let summary = extractive_summary(
            existing_summary(state).as_deref().unwrap_or_default(),
            &history[..split_at],
            state,
        );
        let mut recent_messages = history[split_at..].to_vec();
        if let Some(original) = history.iter().find(|message| message.role == "user")
            && !recent_messages
                .iter()
                .any(|message| message.id == original.id)
        {
            let mut retained_first = original.clone();
            retained_first.content = format!("[First User Message]\n{}", original.content);
            recent_messages.insert(0, retained_first);
        }
        Self {
            recent_messages,
            durable_summary: (!summary.is_empty()).then_some(summary),
            confirmed_facts: confirmed_facts(state),
        }
    }

    pub(crate) fn refresh_confirmed_facts(&mut self, state: &SessionStateData) {
        self.confirmed_facts = confirmed_facts(state);
    }
}

fn without_repeated_current_message<'a>(
    messages: &'a [Message],
    current_message: Option<&str>,
) -> &'a [Message] {
    let repeated = current_message
        .filter(|message| !message.is_empty())
        .zip(messages.last())
        .is_some_and(|(current, last)| {
            last.role == "user" && last.content.trim() == current.trim()
        });
    if repeated {
        &messages[..messages.len() - 1]
    } else {
        messages
    }
}

fn recent_start(messages: &[Message], budget: usize) -> usize {
    let mut chars = 0usize;
    let mut start = messages.len();
    for index in (0..messages.len()).rev() {
        let item_chars = messages[index].content.chars().count();
        if start < messages.len() && chars.saturating_add(item_chars) > budget {
            break;
        }
        start = index;
        chars = chars.saturating_add(item_chars);
    }
    start
}

async fn summarize(
    context: &RunContext,
    previous_summary: &str,
    newly_covered: &[Message],
    state: &SessionStateData,
) -> String {
    let transcript = newly_covered
        .iter()
        .filter(|message| matches!(message.role.as_str(), "user" | "assistant"))
        .map(|message| {
            format!(
                "{}：{}",
                if message.role == "user" {
                    "用户"
                } else {
                    "助手"
                },
                message.content
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    let request = ModelRequest {
        messages: vec![
            ModelMessage::system(
                "压缩同一会话的早期内容。只保留用户目标、已确认事实、重要选择、未解决问题和分支变化；不得编造。输出简洁中文摘要。",
            ),
            ModelMessage::user(format!(
                "[已有摘要]\n{}\n\n[写作上下文]\n{}\n\n[新增旧消息]\n{}",
                if previous_summary.is_empty() {
                    "无"
                } else {
                    previous_summary
                },
                serde_json::to_string(&state.writing_context).unwrap_or_else(|_| "{}".to_owned()),
                transcript,
            )),
        ],
        temperature: Some(0.0),
    };
    match context.call_model("summarize_conversation", request).await {
        Ok(response) if !response.content.trim().is_empty() => {
            truncate(response.content.trim(), SUMMARY_LIMIT)
        }
        _ => extractive_summary(previous_summary, newly_covered, state),
    }
}

fn extractive_summary(
    previous_summary: &str,
    newly_covered: &[Message],
    state: &SessionStateData,
) -> String {
    let mut parts = Vec::new();
    if !previous_summary.trim().is_empty() {
        parts.push(previous_summary.trim().to_owned());
    }
    if let Some(summary) = state
        .writing_context
        .context_summary
        .as_deref()
        .filter(|value| !value.trim().is_empty())
    {
        parts.push(summary.trim().to_owned());
    }
    parts.extend(newly_covered.iter().filter_map(|message| {
        if !matches!(message.role.as_str(), "user" | "assistant")
            || message.content.trim().is_empty()
        {
            return None;
        }
        Some(format!(
            "{}：{}",
            if message.role == "user" {
                "用户"
            } else {
                "助手"
            },
            truncate(message.content.trim(), 240)
        ))
    }));
    truncate(&parts.join("\n"), SUMMARY_LIMIT)
}

fn existing_summary(state: &SessionStateData) -> Option<String> {
    state
        .extra
        .get("conversation_memory")
        .and_then(|value| value.get("summary"))
        .and_then(Value::as_str)
        .or_else(|| {
            state
                .extra
                .get("conversation_memory_summary")
                .and_then(Value::as_str)
        })
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

fn truncate(value: &str, limit: usize) -> String {
    value.chars().take(limit).collect()
}

fn confirmed_facts(state: &SessionStateData) -> Map<String, Value> {
    let writing = &state.writing_context;
    let mut facts = Map::new();
    for (key, value) in [
        ("topic", writing.topic.as_ref()),
        ("initial_idea", writing.initial_idea.as_ref()),
        ("motivation", writing.motivation.as_ref()),
        ("observed_scene", writing.observed_scene.as_ref()),
        ("confusion_point", writing.confusion_point.as_ref()),
        ("selected_path", writing.selected_path.as_ref()),
        ("choice_reason", writing.choice_reason.as_ref()),
        ("research_question", writing.research_question.as_ref()),
        ("core_claim", writing.core_claim.as_ref()),
    ] {
        if let Some(value) = value.filter(|value| !value.is_empty()) {
            facts.insert(key.to_owned(), Value::String(value.clone()));
        }
    }
    facts
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use crate::domain::{Message, MessageId, SessionId, SessionStateData};

    use super::ConversationMemory;

    fn message(role: &str, content: impl Into<String>) -> Message {
        Message {
            id: MessageId::new(),
            session_id: SessionId::new(),
            role: role.to_owned(),
            content: content.into(),
            metadata_json: json!({}),
            created_at: None,
        }
    }

    #[test]
    fn keeps_the_whole_session_until_the_character_threshold() {
        let messages = (0..30)
            .map(|index| {
                message(
                    if index % 2 == 0 { "user" } else { "assistant" },
                    format!("第 {index} 轮"),
                )
            })
            .collect::<Vec<_>>();
        let memory = ConversationMemory::from_messages_with_limits(
            &messages,
            &SessionStateData::default(),
            None,
            24_000,
            12_000,
        );
        assert_eq!(memory.recent_messages.len(), 30);
        assert!(memory.durable_summary.is_none());
    }

    #[test]
    fn compression_uses_a_recent_character_budget_and_summarizes_older_turns() {
        let messages = (0..8)
            .map(|index| {
                message(
                    if index % 2 == 0 { "user" } else { "assistant" },
                    format!("{index}{}", "甲".repeat(900)),
                )
            })
            .collect::<Vec<_>>();
        let memory = ConversationMemory::from_messages_with_limits(
            &messages,
            &SessionStateData::default(),
            None,
            4_000,
            2_000,
        );
        assert_eq!(memory.recent_messages.len(), 3);
        assert!(
            memory
                .durable_summary
                .as_deref()
                .unwrap()
                .contains("用户：0")
        );
        assert!(memory.recent_messages[1].content.starts_with('6'));
    }

    #[test]
    fn removes_only_the_current_user_message_from_history() {
        let messages = vec![
            message("user", "前一个问题"),
            message("assistant", "前一个回答"),
            message("user", "  本轮输入  "),
        ];
        let memory = ConversationMemory::from_messages_with_limits(
            &messages,
            &SessionStateData::default(),
            Some("本轮输入"),
            24_000,
            12_000,
        );
        assert_eq!(memory.recent_messages.len(), 2);
    }

    #[test]
    fn exposes_non_empty_confirmed_writing_facts() {
        let mut state = SessionStateData::default();
        state.writing_context.topic = Some("小组合作".to_owned());
        state.writing_context.motivation = Some("分工总是不均".to_owned());
        let memory =
            ConversationMemory::from_messages_with_limits(&[], &state, None, 24_000, 12_000);
        assert_eq!(memory.confirmed_facts.len(), 2);
    }
}
