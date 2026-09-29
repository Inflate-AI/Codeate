/// The language a file is classified as. Detection always yields a
/// `LanguageId` — an unrecognized file is `Unknown`, never silently dropped.
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum LanguageId {
    Known(String),
    Unknown,
}

impl LanguageId {
    pub fn known(name: impl Into<String>) -> Self {
        Self::Known(name.into())
    }
}

/// A language a `LanguageRegistry` can recognize: its id, the file
/// extensions that identify it, and the shebang interpreters that identify
/// it in extensionless scripts. Later language adapters (Phase 4+) register
/// their language through this same shape.
#[derive(Debug, Clone)]
pub struct LanguageDescriptor {
    pub id: LanguageId,
    pub extensions: Vec<String>,
    pub shebang_interpreters: Vec<String>,
}

impl LanguageDescriptor {
    pub fn new(id: LanguageId) -> Self {
        Self {
            id,
            extensions: Vec::new(),
            shebang_interpreters: Vec::new(),
        }
    }

    pub fn with_extensions<I, S>(mut self, extensions: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.extensions = extensions.into_iter().map(Into::into).collect();
        self
    }

    pub fn with_shebang_interpreters<I, S>(mut self, interpreters: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.shebang_interpreters = interpreters.into_iter().map(Into::into).collect();
        self
    }
}

/// The pluggable set of languages a detector can recognize.
#[derive(Debug, Clone, Default)]
pub struct LanguageRegistry {
    descriptors: Vec<LanguageDescriptor>,
}

impl LanguageRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&mut self, descriptor: LanguageDescriptor) {
        self.descriptors.push(descriptor);
    }

    pub fn by_extension(&self, extension: &str) -> Option<&LanguageId> {
        let extension = extension.to_lowercase();
        self.descriptors
            .iter()
            .find(|d| d.extensions.iter().any(|e| e.to_lowercase() == extension))
            .map(|d| &d.id)
    }

    pub fn by_shebang_interpreter(&self, interpreter: &str) -> Option<&LanguageId> {
        self.descriptors
            .iter()
            .find(|d| d.shebang_interpreters.iter().any(|i| i == interpreter))
            .map(|d| &d.id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registering_a_descriptor_makes_it_findable_by_extension() {
        let mut registry = LanguageRegistry::new();
        registry
            .register(LanguageDescriptor::new(LanguageId::known("rust")).with_extensions(["rs"]));

        assert_eq!(
            registry.by_extension("rs"),
            Some(&LanguageId::known("rust"))
        );
    }

    #[test]
    fn extension_lookup_is_case_insensitive() {
        let mut registry = LanguageRegistry::new();
        registry
            .register(LanguageDescriptor::new(LanguageId::known("rust")).with_extensions(["rs"]));

        assert_eq!(
            registry.by_extension("RS"),
            Some(&LanguageId::known("rust"))
        );
    }

    #[test]
    fn unregistered_extension_is_not_found() {
        let registry = LanguageRegistry::new();
        assert_eq!(registry.by_extension("rs"), None);
    }

    #[test]
    fn registering_a_descriptor_makes_it_findable_by_shebang_interpreter() {
        let mut registry = LanguageRegistry::new();
        registry.register(
            LanguageDescriptor::new(LanguageId::known("python"))
                .with_extensions(["py"])
                .with_shebang_interpreters(["python", "python3"]),
        );

        assert_eq!(
            registry.by_shebang_interpreter("python3"),
            Some(&LanguageId::known("python"))
        );
    }

    #[test]
    fn unregistered_shebang_interpreter_is_not_found() {
        let registry = LanguageRegistry::new();
        assert_eq!(registry.by_shebang_interpreter("python3"), None);
    }
}
