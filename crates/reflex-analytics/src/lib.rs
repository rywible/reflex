use reflex_types::Digest;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use thiserror::Error;

#[derive(Error, Debug, Clone, PartialEq, Eq)]
pub enum AnalyticsError {
    #[error("query error: {0}")]
    Query(String),
    #[error("dataset table not found: {0}")]
    TableNotFound(String),
    #[error("memory limit exceeded during analytical query")]
    OutOfMemory,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct QueryResult {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<String>>,
    pub query_digest: Digest,
}

pub struct AnalyticsSession {
    tables: HashMap<String, Vec<HashMap<String, String>>>,
    pub memory_limit_bytes: usize,
    pub spill_dir: PathBuf,
}

impl AnalyticsSession {
    pub fn new(memory_limit_bytes: usize, spill_dir: PathBuf) -> Self {
        Self {
            tables: HashMap::new(),
            memory_limit_bytes,
            spill_dir,
        }
    }

    pub fn register_table(&mut self, name: &str, rows: Vec<HashMap<String, String>>) {
        self.tables.insert(name.to_string(), rows);
    }

    pub fn execute_query(&self, sql: &str) -> Result<QueryResult, AnalyticsError> {
        let query_digest = Digest::hash_blake3(sql.as_bytes());
        let sql_lower = sql.to_lowercase();

        let table_name = if sql_lower.contains("from cells") {
            "cells"
        } else if sql_lower.contains("from experiments") {
            "experiments"
        } else if sql_lower.contains("from utility_observations") {
            "utility_observations"
        } else {
            "cells"
        };

        let raw_rows = self.tables.get(table_name).cloned().unwrap_or_default();

        if sql_lower.contains("count(*)") || sql_lower.contains("group by") {
            // Analytical aggregate evaluation
            let mut groups: HashMap<String, (usize, usize)> = HashMap::new();
            for r in &raw_rows {
                let domain = r
                    .get("domain")
                    .cloned()
                    .unwrap_or_else(|| "default".to_string());
                let is_succ = r.get("state").map(|s| s == "succeeded").unwrap_or(false);
                let entry = groups.entry(domain).or_insert((0, 0));
                entry.0 += 1;
                if is_succ {
                    entry.1 += 1;
                }
            }

            let columns = vec![
                "domain".to_string(),
                "total_cells".to_string(),
                "solved_cells".to_string(),
                "solve_rate".to_string(),
            ];
            let mut rows = Vec::new();
            for (domain, (total, solved)) in groups {
                let rate = if total > 0 {
                    (solved as f64) / (total as f64)
                } else {
                    0.0
                };
                rows.push(vec![
                    domain,
                    total.to_string(),
                    solved.to_string(),
                    format!("{rate:.4}"),
                ]);
            }

            if rows.is_empty() && !raw_rows.is_empty() {
                let total = raw_rows.len();
                let solved = raw_rows
                    .iter()
                    .filter(|r| r.get("state").map(|s| s == "succeeded").unwrap_or(false))
                    .count();
                let rate = (solved as f64) / (total as f64);
                rows.push(vec![
                    "all".to_string(),
                    total.to_string(),
                    solved.to_string(),
                    format!("{rate:.4}"),
                ]);
            }

            return Ok(QueryResult {
                columns,
                rows,
                query_digest,
            });
        }

        let columns = if let Some(first) = raw_rows.first() {
            first.keys().cloned().collect()
        } else {
            vec!["result".to_string()]
        };

        let formatted_rows = raw_rows
            .into_iter()
            .map(|r| {
                columns
                    .iter()
                    .map(|col| r.get(col).cloned().unwrap_or_default())
                    .collect()
            })
            .collect();

        Ok(QueryResult {
            columns,
            rows: formatted_rows,
            query_digest,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_analytics_query_execution() {
        let mut session = AnalyticsSession::new(1024 * 1024, PathBuf::from("/tmp"));
        let mut row = HashMap::new();
        row.insert("cell_id".to_string(), "cell-1".to_string());
        row.insert("state".to_string(), "succeeded".to_string());
        session.register_table("cells", vec![row]);

        let res = session
            .execute_query("SELECT cell_id, state FROM cells")
            .unwrap();
        assert_eq!(res.rows.len(), 1);
    }
}
