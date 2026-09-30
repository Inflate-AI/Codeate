pub mod go;
pub mod javascript;
pub mod python;
pub mod rust;

use std::collections::HashMap;

pub use go::GoAdapter;
pub use javascript::JavaScriptAdapter;
pub use python::PythonAdapter;
pub use rust::RustAdapter;

use crate::adapter::DynLanguageAdapter;
use crate::language::LanguageId;

/// The registry `pipeline::run_pipeline` dispatches through: every language
/// this build has an adapter for, keyed by the `LanguageId` `detect.rs`
/// classifies a file as. A file whose language has no entry here is skipped
/// by the pipeline (not an error -- the same "unsupported language" stance
/// Phase 2's detection already takes for `LanguageId::Unknown`).
pub fn default_adapters() -> HashMap<LanguageId, Box<dyn DynLanguageAdapter>> {
    let mut adapters: HashMap<LanguageId, Box<dyn DynLanguageAdapter>> = HashMap::new();
    adapters.insert(LanguageId::known("rust"), Box::new(RustAdapter));
    adapters.insert(LanguageId::known("python"), Box::new(PythonAdapter));
    adapters.insert(LanguageId::known("go"), Box::new(GoAdapter));
    adapters.insert(LanguageId::known("javascript"), Box::new(JavaScriptAdapter));
    adapters
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_adapters_registers_rust() {
        let adapters = default_adapters();
        assert!(adapters.contains_key(&LanguageId::known("rust")));
    }

    #[test]
    fn default_adapters_registers_python() {
        let adapters = default_adapters();
        assert!(adapters.contains_key(&LanguageId::known("python")));
    }

    #[test]
    fn default_adapters_registers_go() {
        let adapters = default_adapters();
        assert!(adapters.contains_key(&LanguageId::known("go")));
    }

    #[test]
    fn default_adapters_registers_javascript() {
        let adapters = default_adapters();
        assert!(adapters.contains_key(&LanguageId::known("javascript")));
    }

    #[test]
    fn default_adapters_has_no_entry_for_an_unknown_language() {
        let adapters = default_adapters();
        assert!(!adapters.contains_key(&LanguageId::known("cobol")));
    }
}
