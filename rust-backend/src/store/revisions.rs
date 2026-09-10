use serde_json::{Value, json};

use super::sessions::TransferMessage;
use crate::{
    AppError,
    domain::SessionStateData,
    skills::{SkillRegistry, SlotFiller},
};

/// Old exports lack checkpoints. Replay only retained text and recorded routing
/// metadata through the existing deterministic context/slot extractors. Never
/// use the source session's latest state or call a model to reconstruct history.
pub(super) fn restore_legacy_context(
    messages: &[TransferMessage],
    registry: &SkillRegistry,
) -> Result<Value, AppError> {
    let mut state = SessionStateData::default();
    let filler = SlotFiller::new();
    for message in messages {
        let meta = &message.metadata_json;
        let skill = meta
            .get("selected_skill")
            .or_else(|| meta.get("skill_id"))
            .or_else(|| {
                meta.get("student_progress")
                    .and_then(|p| p.get("current_skill"))
            })
            .and_then(Value::as_str)
            .map(str::to_owned)
            .or_else(|| {
                state
                    .extra
                    .get("current_skill")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            });
        if message.role == "user"
            && meta.get("action").and_then(Value::as_str) != Some("synthesize")
        {
            if let Some(definition) = skill.as_deref().and_then(|id| registry.get(id)) {
                if state.extra.get("current_skill").and_then(Value::as_str)
                    != Some(definition.id.as_str())
                {
                    state.extra.remove("collected_slots");
                    state.extra.remove("awaiting_slots");
                }
                let collected = filler.fill(definition, &state, &message.content);
                state.update_from_user(&message.content, skill.as_deref(), &collected);
                state
                    .extra
                    .insert("collected_slots".to_owned(), json!(collected));
            } else {
                state.apply_user_message(&message.content);
            }
        }
        if let Some(skill) = skill.as_deref() {
            state.task_type = Some(skill.to_owned());
            state.extra.insert("current_skill".to_owned(), json!(skill));
        }
        if message.role == "assistant" {
            if let Some(awaiting) = meta.get("awaiting_slots").filter(|value| value.is_array()) {
                state
                    .extra
                    .insert("awaiting_slots".to_owned(), awaiting.clone());
            }
            let mut value =
                serde_json::to_value(&state).map_err(|e| AppError::CorruptData(e.to_string()))?;
            if let Some(progress) = meta.get("student_progress").and_then(Value::as_object) {
                if !value["writing_context"].is_object() {
                    value["writing_context"] = json!({});
                }
                for key in [
                    "topic",
                    "context_summary",
                    "research_question",
                    "selected_path",
                    "choice_reason",
                    "socratic_rounds",
                    "thinking_stage",
                    "thinking_task",
                    "stage",
                    "pending_questions",
                ] {
                    if let Some(field) = progress.get(key).filter(|v| !v.is_null()) {
                        value["writing_context"][key] = field.clone();
                    }
                }
            }
            state = SessionStateData::from_legacy_json(value)?;
            // Candidate parsing depends on the stage recorded for this reply,
            // not the stage that preceded it.
            state.update_after_reply(&message.content, skill.as_deref());
            if let Some(branch) = meta.get("branch").filter(|value| value.is_object()) {
                state
                    .extra
                    .insert("branch_control".to_owned(), branch.clone());
                if branch.get("active_skill").is_some_and(Value::is_null) {
                    state.extra.remove("current_skill");
                    state.task_type = None;
                }
            }
        }
    }
    serde_json::to_value(state).map_err(|e| AppError::CorruptData(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_candidate_answer_keeps_options_for_the_revised_short_selection() {
        let messages = vec![TransferMessage {
            id: "1".repeat(32),
            session_id: "2".repeat(32),
            role: "assistant".to_owned(),
            content: "请选择一个候选方向：\n1. 沉默成本方向\n2. 责任边界方向\n3. 评分制度方向"
                .to_owned(),
            metadata_json: json!({"skill_id":"socratic_review", "student_progress":{"thinking_stage":"candidate_paths","topic":"小组合作"}}),
            created_at: None,
        }];
        let value = restore_legacy_context(&messages, &SkillRegistry::default()).unwrap();
        let mut state = SessionStateData::from_legacy_json(value).unwrap();
        assert_eq!(state.writing_context.candidate_paths.len(), 3);
        state.apply_user_message("2");
        assert_eq!(
            state.writing_context.selected_path.as_deref(),
            Some("责任边界方向")
        );
    }
}
