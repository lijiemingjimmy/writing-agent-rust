use std::{
    collections::{BTreeMap, HashSet},
    fs,
    path::{Component, Path, PathBuf},
};

use serde::Deserialize;
use serde_json::{Map, Value};

use crate::AppError;

#[derive(Clone, Debug, Deserialize)]
pub struct SkillDefinition {
    pub id: String,
    pub name: String,
    pub description: String,
    #[serde(default)]
    pub mode_aliases: Vec<String>,
    #[serde(default)]
    pub trigger_keywords: Vec<String>,
    #[serde(default)]
    pub required_slots: Vec<String>,
    #[serde(default)]
    pub slot_questions: Option<BTreeMap<String, String>>,
    #[serde(default)]
    pub corpus_paths: Vec<String>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Clone, Debug)]
pub struct GlobalPolicy {
    pub id: String,
    pub data: Map<String, Value>,
}

#[derive(Clone, Debug, Default)]
pub struct SkillRegistry {
    skills: BTreeMap<String, SkillDefinition>,
    policies: Vec<GlobalPolicy>,
    project_root: PathBuf,
}

#[derive(Clone, Debug)]
pub(crate) struct ValidatedCorpusScope {
    pub(crate) root: PathBuf,
    pub(crate) patterns: Vec<PathBuf>,
}

impl SkillRegistry {
    pub fn load(root: &Path) -> Result<Self, AppError> {
        if !root.is_dir() {
            return Err(AppError::InvalidConfig(format!(
                "skills directory not found: {}",
                root.display()
            )));
        }
        let root = root.canonicalize()?;

        let project_root = root.parent().ok_or_else(|| {
            AppError::InvalidConfig(format!(
                "skills directory has no project root: {}",
                root.display()
            ))
        })?;
        let project_root = project_root.to_path_buf();
        let mut yaml_paths = Vec::new();
        collect_yaml_paths(&root, &mut yaml_paths)?;
        yaml_paths.sort();

        let mut registry = Self {
            project_root: project_root.clone(),
            ..Self::default()
        };
        let mut ids = HashSet::new();
        for path in yaml_paths {
            let data: Value = serde_yaml::from_str(&fs::read_to_string(&path)?)
                .map_err(|error| AppError::InvalidConfig(format!("{}: {error}", path.display())))?;
            let object = data.as_object().ok_or_else(|| {
                AppError::InvalidConfig(format!("{}: YAML root must be a mapping", path.display()))
            })?;

            if object
                .get("always_on")
                .and_then(Value::as_bool)
                .unwrap_or(false)
            {
                let id = required_string(object, "id", &path)?;
                registry.policies.push(GlobalPolicy {
                    id,
                    data: object.clone(),
                });
                continue;
            }

            let skill: SkillDefinition = serde_json::from_value(data)
                .map_err(|error| AppError::InvalidConfig(format!("{}: {error}", path.display())))?;
            validate_skill(&skill, &path, &project_root)?;
            if !ids.insert(skill.id.clone()) {
                return Err(AppError::InvalidConfig(format!(
                    "{}: duplicate skill id: {}",
                    path.display(),
                    skill.id
                )));
            }
            registry.skills.insert(skill.id.clone(), skill);
        }

        Ok(registry)
    }

    pub fn get(&self, id: &str) -> Option<&SkillDefinition> {
        self.skills.get(id)
    }

    pub fn all(&self) -> impl ExactSizeIterator<Item = &SkillDefinition> {
        self.skills.values()
    }

    pub fn policies(&self) -> &[GlobalPolicy] {
        &self.policies
    }

    pub(crate) fn corpus_scope_for(&self, skill_id: &str) -> Option<ValidatedCorpusScope> {
        let skill = self.get(skill_id)?;
        Some(ValidatedCorpusScope {
            root: self.project_root.clone(),
            patterns: skill
                .corpus_paths
                .iter()
                .map(|path| self.project_root.join(path))
                .collect(),
        })
    }
}

fn collect_yaml_paths(root: &Path, paths: &mut Vec<PathBuf>) -> Result<(), AppError> {
    for entry in fs::read_dir(root)? {
        let path = entry?.path();
        if path.is_dir() {
            collect_yaml_paths(&path, paths)?;
        } else if path
            .extension()
            .is_some_and(|extension| extension == "yaml")
        {
            paths.push(path);
        }
    }
    Ok(())
}

fn required_string(
    object: &Map<String, Value>,
    field: &str,
    path: &Path,
) -> Result<String, AppError> {
    object
        .get(field)
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(ToOwned::to_owned)
        .ok_or_else(|| {
            AppError::InvalidConfig(format!(
                "{}: {field} must be a non-empty string",
                path.display()
            ))
        })
}

fn validate_skill(
    skill: &SkillDefinition,
    source: &Path,
    project_root: &Path,
) -> Result<(), AppError> {
    if skill.id.trim().is_empty()
        || skill.name.trim().is_empty()
        || skill.description.trim().is_empty()
    {
        return Err(AppError::InvalidConfig(format!(
            "{}: id, name, and description must be non-empty",
            source.display()
        )));
    }
    if skill
        .trigger_keywords
        .iter()
        .all(|trigger| trigger.trim().is_empty())
    {
        return Err(AppError::InvalidConfig(format!(
            "{}: trigger_keywords must contain a non-empty trigger",
            source.display()
        )));
    }
    for slot in &skill.required_slots {
        let question = skill
            .slot_questions
            .as_ref()
            .and_then(|questions| questions.get(slot));
        if question.is_none_or(|question| question.trim().is_empty()) {
            return Err(AppError::InvalidConfig(format!(
                "{}: required slot {slot:?} has no question",
                source.display()
            )));
        }
    }
    for corpus_path in &skill.corpus_paths {
        validate_corpus_path(corpus_path, source, project_root)?;
    }
    Ok(())
}

fn validate_corpus_path(pattern: &str, source: &Path, project_root: &Path) -> Result<(), AppError> {
    let candidate = Path::new(pattern);
    if pattern.trim().is_empty()
        || candidate.is_absolute()
        || candidate
            .components()
            .any(|component| matches!(component, Component::ParentDir))
    {
        return Err(AppError::InvalidConfig(format!(
            "{}: corpus path escapes project root: {pattern}",
            source.display()
        )));
    }

    let stable_prefix = candidate.components().take_while(|component| {
        !component
            .as_os_str()
            .to_string_lossy()
            .contains(['*', '?', '[', '{'])
    });
    let prefix = stable_prefix.fold(project_root.to_path_buf(), |path, component| {
        path.join(component.as_os_str())
    });
    if prefix.exists() {
        validate_existing_target(&prefix, source, project_root)?;
    }
    Ok(())
}

fn validate_existing_target(
    target: &Path,
    source: &Path,
    project_root: &Path,
) -> Result<(), AppError> {
    let canonical = target.canonicalize()?;
    if !canonical.starts_with(project_root) {
        return Err(AppError::InvalidConfig(format!(
            "{}: corpus path escapes project root: {}",
            source.display(),
            target.display()
        )));
    }
    if canonical.is_dir() {
        for entry in fs::read_dir(canonical)? {
            validate_existing_target(&entry?.path(), source, project_root)?;
        }
    }
    Ok(())
}
