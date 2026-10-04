//! Renders benchmark results into the Markdown tables `docs/benchmarks.md`
//! embeds -- one shared formatter for both the performance table and the
//! coverage/depth table, so the two read as one consistent document.

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct BenchResult {
    pub language: String,
    pub tool: String,
    pub cold_seconds: f64,
    pub incremental_seconds: f64,
    pub peak_memory_mb: f64,
    pub deterministic: bool,
}

/// Renders a GitHub-flavored Markdown table. An empty slice still renders
/// the header row -- a table with no rows is still a valid, readable table,
/// not an error.
pub fn format_results_table(results: &[BenchResult]) -> String {
    let mut out = String::new();
    out.push_str(
        "| Language | Tool | Cold (s) | Incremental (s) | Peak Memory (MB) | Deterministic |\n",
    );
    out.push_str("|---|---|---|---|---|---|\n");
    for r in results {
        out.push_str(&format!(
            "| {} | {} | {:.2} | {:.2} | {:.1} | {} |\n",
            r.language,
            r.tool,
            r.cold_seconds,
            r.incremental_seconds,
            r.peak_memory_mb,
            if r.deterministic { "yes" } else { "no" }
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_results_renders_only_the_header() {
        let table = format_results_table(&[]);
        assert_eq!(
            table,
            "| Language | Tool | Cold (s) | Incremental (s) | Peak Memory (MB) | Deterministic |\n\
             |---|---|---|---|---|---|\n"
        );
    }

    #[test]
    fn renders_one_row_with_exact_formatting() {
        let results = vec![BenchResult {
            language: "rust".to_string(),
            tool: "codeate".to_string(),
            cold_seconds: 1.5,
            incremental_seconds: 0.25,
            peak_memory_mb: 128.0,
            deterministic: true,
        }];
        let table = format_results_table(&results);
        assert_eq!(
            table,
            "| Language | Tool | Cold (s) | Incremental (s) | Peak Memory (MB) | Deterministic |\n\
             |---|---|---|---|---|---|\n\
             | rust | codeate | 1.50 | 0.25 | 128.0 | yes |\n"
        );
    }

    #[test]
    fn a_non_deterministic_result_renders_as_no() {
        let results = vec![BenchResult {
            language: "go".to_string(),
            tool: "codeate".to_string(),
            cold_seconds: 2.0,
            incremental_seconds: 0.5,
            peak_memory_mb: 64.0,
            deterministic: false,
        }];
        let table = format_results_table(&results);
        assert!(table.contains("| no |"));
    }
}
