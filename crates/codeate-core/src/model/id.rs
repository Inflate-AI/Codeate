use crate::hash::hash_bytes;

/// A stable, language-independent identifier for a canonical-model entity.
///
/// Derived deterministically from the entity's kind, declaring location, and
/// name (plus an optional disambiguator for overloads/duplicates) — the same
/// logical entity always produces the same `EntityId`, on any run, on any
/// platform.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct EntityId(String);

impl EntityId {
    pub fn new(kind: &str, location: &str, name: &str, disambiguator: Option<&str>) -> Self {
        let seed = format!(
            "{kind}\u{0}{location}\u{0}{name}\u{0}{}",
            disambiguator.unwrap_or("")
        );
        Self(hash_bytes(seed.as_bytes()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_inputs_produce_the_same_id() {
        let a = EntityId::new("Function", "src/main.rs", "main", None);
        let b = EntityId::new("Function", "src/main.rs", "main", None);
        assert_eq!(a, b);
    }

    #[test]
    fn different_kind_produces_a_different_id() {
        let a = EntityId::new("Function", "src/main.rs", "Point", None);
        let b = EntityId::new("Struct", "src/main.rs", "Point", None);
        assert_ne!(a, b);
    }

    #[test]
    fn different_location_produces_a_different_id() {
        let a = EntityId::new("Function", "src/a.rs", "run", None);
        let b = EntityId::new("Function", "src/b.rs", "run", None);
        assert_ne!(a, b);
    }

    #[test]
    fn different_name_produces_a_different_id() {
        let a = EntityId::new("Function", "src/main.rs", "run", None);
        let b = EntityId::new("Function", "src/main.rs", "walk", None);
        assert_ne!(a, b);
    }

    #[test]
    fn different_disambiguator_produces_a_different_id() {
        let a = EntityId::new("Method", "src/lib.rs", "new", Some("u32"));
        let b = EntityId::new("Method", "src/lib.rs", "new", Some("u64"));
        assert_ne!(a, b);
    }

    #[test]
    fn id_has_a_stable_string_representation() {
        let id = EntityId::new("Function", "src/main.rs", "main", None);
        assert_eq!(id.as_str().len(), 64);
        assert!(id.as_str().chars().all(|c| c.is_ascii_hexdigit()));
    }
}
