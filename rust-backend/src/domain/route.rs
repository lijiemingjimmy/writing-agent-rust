use serde::{Deserialize, Deserializer, Serialize, Serializer};

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum WritingStage {
    Unknown(String),
    Topic,
    Literature,
    ResearchQuestion,
    Theory,
    Method,
    DraftArgument,
    CoursePolicy,
    AcademicNorm,
}

impl WritingStage {
    pub fn as_str(&self) -> &str {
        match self {
            Self::Unknown(value) => value,
            Self::Topic => "topic",
            Self::Literature => "literature",
            Self::ResearchQuestion => "research_question",
            Self::Theory => "theory",
            Self::Method => "method",
            Self::DraftArgument => "draft_argument",
            Self::CoursePolicy => "course_policy",
            Self::AcademicNorm => "academic_norm",
        }
    }

    pub fn is_unknown(&self) -> bool {
        matches!(self, Self::Unknown(_))
    }
}

impl Default for WritingStage {
    fn default() -> Self {
        Self::Unknown("unknown".to_owned())
    }
}

impl From<&str> for WritingStage {
    fn from(value: &str) -> Self {
        match value {
            "topic" => Self::Topic,
            "literature" => Self::Literature,
            "research_question" => Self::ResearchQuestion,
            "theory" => Self::Theory,
            "method" => Self::Method,
            "draft_argument" => Self::DraftArgument,
            "course_policy" => Self::CoursePolicy,
            "academic_norm" => Self::AcademicNorm,
            other => Self::Unknown(other.to_owned()),
        }
    }
}

impl Serialize for WritingStage {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for WritingStage {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        String::deserialize(deserializer).map(|value| Self::from(value.as_str()))
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RiskLevel {
    DirectOk,
    NeedsSocratic,
    NeedsContext,
    GhostwritingRisk,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct RouteDecision {
    pub stage: WritingStage,
    pub intent: String,
    pub risk: RiskLevel,
    pub target_skill: Option<String>,
    pub confidence: f32,
    pub reason: String,
    pub required_context: Vec<String>,
    pub needs_socratic: bool,
}
