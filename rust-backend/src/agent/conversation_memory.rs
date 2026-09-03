use std::collections::HashSet;

use serde_json::{Map, Value};

use crate::domain::{Message, SessionStateData};

const RECENT_MESSAGE_LIMIT: usize = 12;
const MESSAGE_COMPACT_LIMIT: usize = 180;
const SUMMARY_LIMIT: usize = 6_000;
const SUMMARY_EDGE_LIMIT: usize = 3_000;

#[derive(Clone, Debug)]
pub(crate) struct ConversationMemory {
    pub(crate) recent_messages: Vec<Message>,
    pub(crate) durable_summary: Option<String>,
    pub(crate) confirmed_facts: Map<String, Value>,
}

impl ConversationMemory {
    pub(crate) fn from_messages(
        messages: &[Message],
        state: &SessionStateData,
        current_message: Option<&str>,
    ) -> Self {
        let history = without_repeated_current_message(messages, current_message);
        let recent_start = history.len().saturating_sub(RECENT_MESSAGE_LIMIT);
        let recent_messages = history[recent_start..].to_vec();
        let compacted_history = compact_history(&history[..recent_start]);

        let mut seen = HashSet::new();
        let mut summary_parts = Vec::new();
        for value in [
            state
                .extra
                .get("conversation_memory_summary")
                .and_then(Value::as_str),
            state.writing_context.context_summary.as_deref(),
            compacted_history.as_deref(),
        ]
        .into_iter()
        .flatten()
        {
            let value = value.trim();
            if !value.is_empty() && seen.insert(value.to_owned()) {
                summary_parts.push(value);
            }
        }

        let durable_summary =
            (!summary_parts.is_empty()).then(|| retain_summary_edges(&summary_parts.join("\n")));

        Self {
            recent_messages,
            durable_summary,
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
    let should_drop_last = current_message
        .filter(|message| !message.is_empty())
        .zip(messages.last())
        .is_some_and(|(current, last)| {
            last.role == "user" && last.content.trim() == current.trim()
        });
    if should_drop_last {
        &messages[..messages.len() - 1]
    } else {
        messages
    }
}

fn compact_history(messages: &[Message]) -> Option<String> {
    let lines = messages
        .iter()
        .filter_map(|message| {
            let content = message.content.trim().replace('\n', " ");
            if content.is_empty() {
                return None;
            }
            let role = if message.role == "user" {
                "学生"
            } else {
                "学伴"
            };
            Some(format!(
                "{role}: {}",
                content
                    .chars()
                    .take(MESSAGE_COMPACT_LIMIT)
                    .collect::<String>()
            ))
        })
        .collect::<Vec<_>>();
    (!lines.is_empty()).then(|| retain_summary_edges(&lines.join("\n")))
}

fn retain_summary_edges(summary: &str) -> String {
    if summary.chars().count() <= SUMMARY_LIMIT {
        return summary.to_owned();
    }
    let first = summary.chars().take(SUMMARY_EDGE_LIMIT).collect::<String>();
    let last = summary
        .chars()
        .rev()
        .take(SUMMARY_EDGE_LIMIT)
        .collect::<String>()
        .chars()
        .rev()
        .collect::<String>();
    format!("{first}\n…\n{last}")
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
    fn keeps_twelve_recent_messages_and_compacts_older_turns() {
        let messages = (0..24)
            .map(|index| {
                let content = if index == 0 {
                    "最早确认：研究对象是大学生网络社交。".to_owned()
                } else {
                    format!("第 {index} 轮内容")
                };
                message(if index % 2 == 0 { "user" } else { "assistant" }, content)
            })
            .collect::<Vec<_>>();

        let memory =
            ConversationMemory::from_messages(&messages, &SessionStateData::default(), None);

        assert_eq!(memory.recent_messages.len(), 12);
        assert_eq!(memory.recent_messages[0].content, "第 12 轮内容");
        assert_eq!(memory.recent_messages[11].content, "第 23 轮内容");
        assert!(
            memory
                .durable_summary
                .as_deref()
                .unwrap_or_default()
                .contains("学生: 最早确认：研究对象是大学生网络社交。")
        );
    }

    #[test]
    fn removes_only_the_current_user_message_from_history() {
        let messages = vec![
            message("user", "前一个问题"),
            message("assistant", "前一个回答"),
            message("user", "  本轮输入  "),
        ];

        let memory = ConversationMemory::from_messages(
            &messages,
            &SessionStateData::default(),
            Some("本轮输入"),
        );

        assert_eq!(memory.recent_messages.len(), 2);
        assert_eq!(memory.recent_messages[0].content, "前一个问题");
        assert_eq!(memory.recent_messages[1].content, "前一个回答");
    }

    #[test]
    fn compacts_each_older_message_to_180_characters_and_one_line() {
        let long = format!("{}\n{}", "甲".repeat(100), "乙".repeat(120));
        let mut messages = vec![message("user", long)];
        messages.extend((0..12).map(|index| message("assistant", format!("近期 {index}"))));

        let memory =
            ConversationMemory::from_messages(&messages, &SessionStateData::default(), None);
        let compacted = memory.durable_summary.unwrap();
        let first_line = compacted.lines().next().unwrap();

        assert_eq!(first_line.chars().count(), 184); // "学生: " + 180 个字符
        assert_eq!(compacted.lines().count(), 1);
        assert!(compacted.contains(&format!("{} {}", "甲".repeat(100), "乙".repeat(79))));
    }

    #[test]
    fn merges_existing_summaries_without_duplicate_sections() {
        let mut state = SessionStateData::default();
        state.extra.insert(
            "conversation_memory_summary".to_owned(),
            json!("已确认：分工不均"),
        );
        state.writing_context.context_summary = Some("已确认：分工不均".to_owned());

        let memory = ConversationMemory::from_messages(&[], &state, None);

        assert_eq!(memory.durable_summary.as_deref(), Some("已确认：分工不均"));
    }

    #[test]
    fn exposes_non_empty_confirmed_writing_facts() {
        let mut state = SessionStateData::default();
        state.writing_context.topic = Some("小组合作".to_owned());
        state.writing_context.motivation = Some("分工总是不均".to_owned());
        state.writing_context.research_question = Some("为什么搭便车会持续？".to_owned());

        let memory = ConversationMemory::from_messages(&[], &state, None);

        assert_eq!(memory.confirmed_facts.len(), 3);
        assert_eq!(memory.confirmed_facts["topic"], json!("小组合作"));
        assert_eq!(
            memory.confirmed_facts["research_question"],
            json!("为什么搭便车会持续？")
        );
        assert!(!memory.confirmed_facts.contains_key("core_claim"));
    }

    #[test]
    fn caps_durable_summary_at_6003_characters_and_keeps_both_ends() {
        let mut state = SessionStateData::default();
        state.extra.insert(
            "conversation_memory_summary".to_owned(),
            json!(format!("START{}END", "中".repeat(7_000))),
        );

        let memory = ConversationMemory::from_messages(&[], &state, None);
        let summary = memory.durable_summary.unwrap();

        assert_eq!(summary.chars().count(), 6_003);
        assert!(summary.starts_with("START"));
        assert!(summary.contains("\n…\n"));
        assert!(summary.ends_with("END"));
    }
}
