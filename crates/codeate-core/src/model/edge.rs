use crate::model::id::EntityId;

/// The closed set of relationship edge types the canonical model recognizes,
/// per the README's graph schema. No catch-all "other" variant — every
/// relationship a later phase produces must be one of these 14 kinds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EdgeKind {
    Contains,
    Imports,
    Exports,
    Declares,
    References,
    Calls,
    Extends,
    Implements,
    Overrides,
    Instantiates,
    Returns,
    Accepts,
    DependsOn,
    DefinedIn,
}

impl EdgeKind {
    /// The README's canonical SCREAMING_SNAKE_CASE name, used as the stable
    /// on-disk/serialized form later phases (e.g. the Phase 10 state store)
    /// rely on.
    pub fn as_str(&self) -> &'static str {
        match self {
            EdgeKind::Contains => "CONTAINS",
            EdgeKind::Imports => "IMPORTS",
            EdgeKind::Exports => "EXPORTS",
            EdgeKind::Declares => "DECLARES",
            EdgeKind::References => "REFERENCES",
            EdgeKind::Calls => "CALLS",
            EdgeKind::Extends => "EXTENDS",
            EdgeKind::Implements => "IMPLEMENTS",
            EdgeKind::Overrides => "OVERRIDES",
            EdgeKind::Instantiates => "INSTANTIATES",
            EdgeKind::Returns => "RETURNS",
            EdgeKind::Accepts => "ACCEPTS",
            EdgeKind::DependsOn => "DEPENDS_ON",
            EdgeKind::DefinedIn => "DEFINED_IN",
        }
    }

    /// Parse the canonical SCREAMING_SNAKE_CASE form back into an `EdgeKind`.
    /// Returns `None` for anything outside the closed set.
    pub fn parse(s: &str) -> Option<EdgeKind> {
        match s {
            "CONTAINS" => Some(EdgeKind::Contains),
            "IMPORTS" => Some(EdgeKind::Imports),
            "EXPORTS" => Some(EdgeKind::Exports),
            "DECLARES" => Some(EdgeKind::Declares),
            "REFERENCES" => Some(EdgeKind::References),
            "CALLS" => Some(EdgeKind::Calls),
            "EXTENDS" => Some(EdgeKind::Extends),
            "IMPLEMENTS" => Some(EdgeKind::Implements),
            "OVERRIDES" => Some(EdgeKind::Overrides),
            "INSTANTIATES" => Some(EdgeKind::Instantiates),
            "RETURNS" => Some(EdgeKind::Returns),
            "ACCEPTS" => Some(EdgeKind::Accepts),
            "DEPENDS_ON" => Some(EdgeKind::DependsOn),
            "DEFINED_IN" => Some(EdgeKind::DefinedIn),
            _ => None,
        }
    }
}

/// A directed relationship between two canonical-model entities.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Edge {
    pub from: EntityId,
    pub to: EntityId,
    pub kind: EdgeKind,
}

impl Edge {
    pub fn new(from: EntityId, to: EntityId, kind: EdgeKind) -> Self {
        Self { from, to, kind }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL_KINDS: [EdgeKind; 14] = [
        EdgeKind::Contains,
        EdgeKind::Imports,
        EdgeKind::Exports,
        EdgeKind::Declares,
        EdgeKind::References,
        EdgeKind::Calls,
        EdgeKind::Extends,
        EdgeKind::Implements,
        EdgeKind::Overrides,
        EdgeKind::Instantiates,
        EdgeKind::Returns,
        EdgeKind::Accepts,
        EdgeKind::DependsOn,
        EdgeKind::DefinedIn,
    ];

    // Exhaustive match with no wildcard arm: only compiles if every EdgeKind
    // variant is handled, keeping the enum a genuinely closed set.
    fn exhaustive_name(kind: EdgeKind) -> &'static str {
        match kind {
            EdgeKind::Contains => "CONTAINS",
            EdgeKind::Imports => "IMPORTS",
            EdgeKind::Exports => "EXPORTS",
            EdgeKind::Declares => "DECLARES",
            EdgeKind::References => "REFERENCES",
            EdgeKind::Calls => "CALLS",
            EdgeKind::Extends => "EXTENDS",
            EdgeKind::Implements => "IMPLEMENTS",
            EdgeKind::Overrides => "OVERRIDES",
            EdgeKind::Instantiates => "INSTANTIATES",
            EdgeKind::Returns => "RETURNS",
            EdgeKind::Accepts => "ACCEPTS",
            EdgeKind::DependsOn => "DEPENDS_ON",
            EdgeKind::DefinedIn => "DEFINED_IN",
        }
    }

    #[test]
    fn every_kind_matches_its_exhaustive_name() {
        for kind in ALL_KINDS {
            assert_eq!(exhaustive_name(kind), kind.as_str());
        }
    }

    #[test]
    fn every_kind_round_trips_through_its_string_form() {
        for kind in ALL_KINDS {
            assert_eq!(EdgeKind::parse(kind.as_str()), Some(kind));
        }
    }

    #[test]
    fn parsing_an_unrecognized_string_returns_none() {
        assert_eq!(EdgeKind::parse("FROBNICATES"), None);
    }

    #[test]
    fn edge_carries_its_endpoints_and_kind() {
        let from = EntityId::new("Class", "src/lib.rs", "Dog", None);
        let to = EntityId::new("Interface", "src/lib.rs", "Animal", None);
        let edge = Edge::new(from.clone(), to.clone(), EdgeKind::Implements);

        assert_eq!(edge.from, from);
        assert_eq!(edge.to, to);
        assert_eq!(edge.kind, EdgeKind::Implements);
    }

    #[test]
    fn edges_with_different_kinds_are_not_equal() {
        let from = EntityId::new("Class", "src/lib.rs", "Dog", None);
        let to = EntityId::new("Interface", "src/lib.rs", "Animal", None);
        let a = Edge::new(from.clone(), to.clone(), EdgeKind::Implements);
        let b = Edge::new(from, to, EdgeKind::Extends);

        assert_ne!(a, b);
    }
}
