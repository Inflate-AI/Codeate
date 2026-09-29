pub mod structural;
pub mod unresolved;

pub use structural::resolve_depends_on;
pub use unresolved::unresolved_symbol;
