//! Типизированные идентификаторы сущностей (UUID внутри).
//!
//! Newtype-обёртки не дают перепутать `ExchangeId` с `BodyId` и остаются
//! прозрачными для serde (сериализуются как строка UUID).

use serde::{Deserialize, Serialize};

macro_rules! define_id {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(
            Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize,
        )]
        #[serde(transparent)]
        pub struct $name(uuid::Uuid);

        impl $name {
            /// Сгенерировать новый случайный идентификатор.
            pub fn new() -> Self {
                Self(uuid::Uuid::new_v4())
            }

            /// Внутренний UUID.
            pub fn as_uuid(&self) -> uuid::Uuid {
                self.0
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }

        impl From<uuid::Uuid> for $name {
            fn from(value: uuid::Uuid) -> Self {
                Self(value)
            }
        }

        impl From<$name> for uuid::Uuid {
            fn from(value: $name) -> Self {
                value.0
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                write!(f, "{}", self.0)
            }
        }
    };
}

define_id!(
    /// Идентификатор HTTP-обмена (request/response пара).
    ExchangeId
);
define_id!(
    /// Идентификатор клиентского соединения (TCP).
    ConnectionId
);
define_id!(
    /// Идентификатор захваченного тела.
    BodyId
);
define_id!(
    /// Идентификатор правила из RuleSet.
    RuleId
);
define_id!(
    /// Идентификатор припаркованного брейкпоинта.
    InterceptId
);
define_id!(
    /// Идентификатор сохранённой сессии.
    SessionId
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_roundtrip_through_serde() {
        let id = ExchangeId::new();
        let json = serde_json::to_string(&id).expect("serialize");
        assert!(json.starts_with('"') && json.ends_with('"'));
        let back: ExchangeId = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(id, back);
    }

    #[test]
    fn ids_from_uuid_and_back() {
        let uuid = uuid::Uuid::new_v4();
        let id: BodyId = uuid.into();
        assert_eq!(uuid::Uuid::from(id), uuid);
    }

    #[test]
    fn new_ids_are_unique() {
        assert_ne!(SessionId::new(), SessionId::new());
    }
}
