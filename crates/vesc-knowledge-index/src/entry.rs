use serde::{Deserialize, Serialize};

/// Knowledge index category aligned with `catalog/priorities.json`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Category {
    FirmwareApi,
    Lispbm,
    PackageBuild,
    RefloatCommand,
    PocAbi,
}

/// Source attribution for an indexed artifact.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceRef {
    pub repo: String,
    pub path: String,
    pub line: u32,
}

/// One searchable knowledge index entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IndexEntry {
    pub id: String,
    pub name: String,
    pub category: Category,
    pub summary: String,
    pub source: SourceRef,
    pub keywords: Vec<String>,
}
