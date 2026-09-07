use uuid::Uuid;

use crate::AppError;

macro_rules! legacy_id {
    ($name:ident) => {
        #[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
        pub struct $name(Uuid);

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }

        impl $name {
            pub fn new() -> Self {
                Self(Uuid::new_v4())
            }

            pub fn parse_legacy(value: &str) -> Result<Self, AppError> {
                if value.len() != 32
                    || !value
                        .bytes()
                        .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
                {
                    return Err(AppError::CorruptData(format!(
                        "expected a 32-character lowercase hexadecimal UUID, got {value:?}"
                    )));
                }

                Uuid::parse_str(value).map(Self).map_err(|error| {
                    AppError::CorruptData(format!("invalid legacy UUID {value:?}: {error}"))
                })
            }

            pub fn to_legacy_hex(self) -> String {
                self.0.simple().to_string()
            }
        }
    };
}

legacy_id!(SessionId);
legacy_id!(RunId);
legacy_id!(MessageId);
legacy_id!(DocumentId);
legacy_id!(DocumentChunkId);
legacy_id!(SkillEventId);
