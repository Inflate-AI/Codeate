#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("{0}")]
    Message(String),
}

pub type Result<T> = std::result::Result<T, Error>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn message_variant_displays_its_text_verbatim() {
        let err = Error::Message("bad repository config".to_string());
        assert_eq!(err.to_string(), "bad repository config");
    }

    #[test]
    fn io_variant_wraps_and_displays_the_source_error() {
        let io_err = std::io::Error::new(std::io::ErrorKind::NotFound, "missing file");
        let err = Error::Io(io_err);
        assert_eq!(err.to_string(), "io error: missing file");
    }

    #[test]
    fn io_error_converts_via_from() {
        let io_err = std::io::Error::new(std::io::ErrorKind::PermissionDenied, "no access");
        let err: Error = io_err.into();
        assert!(matches!(err, Error::Io(_)));
    }

    #[test]
    fn error_implements_std_error_trait() {
        fn assert_is_std_error<E: std::error::Error>() {}
        assert_is_std_error::<Error>();
    }

    #[test]
    fn error_debug_is_not_empty() {
        let err = Error::Message("x".to_string());
        let debug_str = format!("{:?}", err);
        assert!(!debug_str.is_empty());
    }
}
