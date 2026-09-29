use crate::adapter::LanguageAdapter;
use crate::error::{Error, Result};
use crate::model::{Edge, SyntaxEntity};

/// The Rust language adapter: parses Rust source via `syn` and maps it onto
/// the canonical syntax model.
pub struct RustAdapter;

impl LanguageAdapter for RustAdapter {
    type Ast = syn::File;

    fn parse(&self, source: &str) -> Result<Self::Ast> {
        syn::parse_file(source)
            .map_err(|err| Error::Message(format!("failed to parse Rust source: {err}")))
    }

    fn extract(&self, _ast: &Self::Ast, _location: &str) -> (Vec<SyntaxEntity>, Vec<Edge>) {
        // Declaration and body extraction land in the next two tasks.
        (Vec::new(), Vec::new())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_valid_rust_source() {
        let adapter = RustAdapter;
        let result = adapter.parse("fn main() {}");
        assert!(result.is_ok());
    }

    #[test]
    fn parsing_invalid_syntax_is_a_clear_error() {
        let adapter = RustAdapter;
        let result = adapter.parse("fn main( {{{ not rust");

        let err = result.expect_err("invalid syntax should error");
        assert!(matches!(err, Error::Message(_)));
        assert!(err.to_string().contains("failed to parse Rust source"));
    }
}
