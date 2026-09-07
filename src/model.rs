use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
pub struct Symbol {
    pub name: String,
    pub qualname: String,
    pub kind: String,
    pub start: usize,
    pub end: usize,
    pub sig: String,
    pub snippet: String,
    #[serde(default)]
    pub snippet_truncated: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq, Hash)]
pub struct Edge {
    pub src_name: Option<String>,
    pub dst_name: String,
    pub kind: String,
    pub line: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
pub struct Hit {
    pub path: String,
    pub start: usize,
    pub end: usize,
    pub symbol: Option<String>,
    pub kind: String,
    pub sig: String,
    pub snippet: String,
    #[serde(default, skip_serializing_if = "is_false")]
    pub snippet_truncated: bool,
    pub score: f64,
    pub why: String,
}

fn is_false(value: &bool) -> bool {
    !value
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
pub struct Envelope {
    pub hits: Vec<Hit>,
    pub tokens: usize,
    pub freshness_ms: u128,
    pub coverage: String,
    pub hint: Option<String>,
}

impl Envelope {
    pub fn empty(coverage: &str, hint: Option<String>) -> Self {
        Self {
            hits: Vec::new(),
            tokens: 0,
            freshness_ms: 0,
            coverage: coverage.to_owned(),
            hint,
        }
    }
}
