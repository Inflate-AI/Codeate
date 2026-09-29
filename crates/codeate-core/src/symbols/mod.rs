pub mod defined_in;
pub mod scope;
pub mod table;

pub use defined_in::defined_in_edges;
pub use scope::ScopeTree;
pub use table::{build_symbol_table, merge_symbol_tables, SymbolTable, SymbolTableEntry};
