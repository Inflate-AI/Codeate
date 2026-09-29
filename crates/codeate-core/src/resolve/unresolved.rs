use crate::model::{SymbolEntity, SymbolKind};

/// A canonical placeholder symbol for a reference/call that couldn't be
/// resolved to a real declaration -- so the edge pointing at it is still a
/// real, present edge, never a silently dropped reference.
///
/// `reason` is a short tag describing *why* resolution failed (e.g.
/// `"unknown-local"`, `"external-path"`, `"unsupported-form"`); `name` is
/// the raw referenced name/path text. The reason is folded into the
/// symbol's own name so it stays visible on the symbol itself, not just an
/// opaque id.
pub fn unresolved_symbol(reason: &str, name: &str) -> SymbolEntity {
    SymbolEntity::new(SymbolKind::Symbol, "unresolved", format!("{reason}:{name}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_reason_and_name_produce_the_same_id() {
        let a = unresolved_symbol("unknown-local", "foo");
        let b = unresolved_symbol("unknown-local", "foo");
        assert_eq!(a.id, b.id);
    }

    #[test]
    fn different_reasons_produce_different_ids() {
        let a = unresolved_symbol("unknown-local", "foo");
        let b = unresolved_symbol("external-path", "foo");
        assert_ne!(a.id, b.id);
    }

    #[test]
    fn different_names_produce_different_ids() {
        let a = unresolved_symbol("unknown-local", "foo");
        let b = unresolved_symbol("unknown-local", "bar");
        assert_ne!(a.id, b.id);
    }

    #[test]
    fn the_reason_is_visible_on_the_symbol_itself() {
        let symbol = unresolved_symbol("external-path", "std::process::exit");
        assert_eq!(symbol.name, "external-path:std::process::exit");
    }
}
