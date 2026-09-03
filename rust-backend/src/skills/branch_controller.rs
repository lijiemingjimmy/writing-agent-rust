use serde_json::{Value, json};

use crate::domain::{RouteInput, SessionStateData};

use super::{SkillRegistry, SkillRouter};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BranchResolution {
    pub skill_id: Option<String>,
    pub mode: &'static str,
    pub needs_target: bool,
    pub switched: bool,
}

impl BranchResolution {
    pub fn metadata(&self) -> Value {
        json!({
            "mode": self.mode,
            "active_skill": self.skill_id,
            "needs_target": self.needs_target,
            "switched": self.switched,
        })
    }
}

pub struct BranchController {
    router: SkillRouter,
    registry: SkillRegistry,
}

impl BranchController {
    pub fn new(registry: SkillRegistry) -> Self {
        Self {
            router: SkillRouter::new(registry.clone()),
            registry,
        }
    }

    pub fn resolve(
        &self,
        message: &str,
        state: &mut SessionStateData,
        context_text: &str,
    ) -> BranchResolution {
        let mut control = normalize_control(state);
        let explicit = is_switch_request(message);
        if explicit {
            if requests_ordinary_chat(message) {
                return lock(state, &mut control, None, message, true);
            }
            let target = switch_target(message);
            if target.is_empty() {
                return wait_for_target(state, control);
            }
            if let Some(skill_id) = self.route_unlocked(&target, state, context_text) {
                return lock(state, &mut control, Some(skill_id), message, true);
            }
            return wait_for_target(state, control);
        }

        if control["mode"] == "locked" {
            let active = control
                .get("active_skill")
                .and_then(Value::as_str)
                .map(str::to_owned);
            if active
                .as_deref()
                .is_none_or(|skill| self.registry.get(skill).is_some())
            {
                set_current_skill(state, active.as_deref());
                return BranchResolution {
                    skill_id: active,
                    mode: "locked",
                    needs_target: false,
                    switched: false,
                };
            }
            control["mode"] = json!("unresolved");
            control["active_skill"] = Value::Null;
        }

        if let Some(skill_id) = self.route_unlocked(message, state, context_text) {
            let switched = control["mode"] == "awaiting_switch";
            return lock(state, &mut control, Some(skill_id), message, switched);
        }
        if control["mode"] == "awaiting_switch" {
            return wait_for_target(state, control);
        }
        control["mode"] = json!("unresolved");
        control["active_skill"] = Value::Null;
        state.extra.insert("branch_control".to_owned(), control);
        set_current_skill(state, None);
        BranchResolution {
            skill_id: None,
            mode: "unresolved",
            needs_target: false,
            switched: false,
        }
    }

    fn route_unlocked(
        &self,
        message: &str,
        state: &SessionStateData,
        context_text: &str,
    ) -> Option<String> {
        let input = RouteInput::new(message)
            .with_writing_context(
                serde_json::to_value(&state.writing_context).unwrap_or_else(|_| json!({})),
            )
            .with_context_text(context_text);
        self.router.route(&input).target_skill
    }
}

fn normalize_control(state: &mut SessionStateData) -> Value {
    if let Some(existing) = state.extra.get("branch_control").and_then(Value::as_object) {
        let mode = existing.get("mode").and_then(Value::as_str);
        if matches!(mode, Some("unresolved" | "locked" | "awaiting_switch")) {
            let mut control = Value::Object(existing.clone());
            if !control["history"].is_array() {
                control["history"] = json!([]);
            }
            if control.get("active_skill").is_none() {
                control["active_skill"] = state
                    .extra
                    .get("current_skill")
                    .cloned()
                    .unwrap_or(Value::Null);
            }
            state
                .extra
                .insert("branch_control".to_owned(), control.clone());
            return control;
        }
    }
    let active = state
        .extra
        .get("current_skill")
        .cloned()
        .unwrap_or(Value::Null);
    let control = json!({
        "mode": if active.is_null() { "unresolved" } else { "locked" },
        "active_skill": active,
        "history": [],
    });
    state
        .extra
        .insert("branch_control".to_owned(), control.clone());
    control
}

fn wait_for_target(state: &mut SessionStateData, mut control: Value) -> BranchResolution {
    control["mode"] = json!("awaiting_switch");
    state.extra.insert("branch_control".to_owned(), control);
    BranchResolution {
        skill_id: None,
        mode: "awaiting_switch",
        needs_target: true,
        switched: false,
    }
}

fn lock(
    state: &mut SessionStateData,
    control: &mut Value,
    skill_id: Option<String>,
    message: &str,
    switched: bool,
) -> BranchResolution {
    let previous = control.get("active_skill").cloned().unwrap_or(Value::Null);
    control["mode"] = json!("locked");
    control["active_skill"] = skill_id.clone().map(Value::String).unwrap_or(Value::Null);
    if switched {
        let history = control["history"]
            .as_array_mut()
            .expect("normalized history");
        history.push(json!({"from_skill": previous, "to_skill": skill_id, "message": message}));
        if history.len() > 20 {
            history.drain(..history.len() - 20);
        }
    }
    state
        .extra
        .insert("branch_control".to_owned(), control.clone());
    set_current_skill(state, skill_id.as_deref());
    BranchResolution {
        skill_id,
        mode: "locked",
        needs_target: false,
        switched,
    }
}

fn set_current_skill(state: &mut SessionStateData, skill_id: Option<&str>) {
    match skill_id {
        Some(skill) => {
            state.task_type = Some(skill.to_owned());
            state
                .extra
                .insert("current_skill".to_owned(), Value::String(skill.to_owned()));
        }
        None => {
            state.task_type = None;
            state.extra.insert("current_skill".to_owned(), Value::Null);
        }
    }
}

fn is_switch_request(message: &str) -> bool {
    ["我要切换分支", "切换分支", "切换到"]
        .iter()
        .any(|phrase| message.trim().contains(phrase))
}

fn requests_ordinary_chat(message: &str) -> bool {
    is_switch_request(message)
        && ["普通聊天", "自然聊天", "自由聊天", "闲聊"]
            .iter()
            .any(|target| message.contains(target))
}

fn switch_target(message: &str) -> String {
    let mut target = message.trim().to_owned();
    for phrase in ["我要切换分支", "切换分支", "切换到"] {
        target = target.replace(phrase, " ");
    }
    target
        .trim_start_matches(|c: char| "，,。:：;； ".contains(c))
        .trim_start_matches("我想")
        .trim_start_matches("我要")
        .trim_start_matches('要')
        .trim_start_matches('想')
        .trim_matches(|c: char| " ，,。:：;；".contains(c))
        .to_owned()
}
