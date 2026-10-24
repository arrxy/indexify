use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PassageMeta {
    pub id: u32,
    pub source: PathBuf,
    pub format: String,
    pub location: String,
    pub heading: String,
    pub page: Option<u32>,
    pub line: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImportIssue {
    pub source: PathBuf,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Manifest {
    pub version: u32,
    pub name: String,
    pub source: PathBuf,
    pub built_at: u64,
    pub files: usize,
    pub passages: usize,
    pub skipped: usize,
    pub issues: Vec<ImportIssue>,
    pub index_bytes: u64,
}

#[derive(Debug, Clone)]
pub struct Progress {
    pub stage: String,
    pub detail: String,
    pub done: usize,
    pub total: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct Hit {
    pub rank: usize,
    pub score: f64,
    #[serde(flatten)]
    pub passage: PassageMeta,
    pub text: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct Found {
    pub query: String,
    pub mode: String,
    pub matched_passages: u64,
    pub elapsed_ms: u128,
    pub hits: Vec<Hit>,
}
