//! `search_vesc_knowledge` — search the embedded firmware and package knowledge index.

use serde::{Deserialize, Serialize};
use vesc_knowledge_index::{Category, search_knowledge};

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SearchVescKnowledgeParams {
    /// Free-text query matched against entry names, keywords, and summaries.
    pub query: String,
    /// Optional category filter (`firmware_api`, `lispbm`, `package_build`, etc.).
    #[serde(default)]
    pub category: Option<String>,
    /// Maximum number of hits to return (default 10).
    #[serde(default = "default_search_limit")]
    pub limit: usize,
}

const fn default_search_limit() -> usize {
    10
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema, PartialEq, Eq)]
pub struct SearchVescKnowledgeSource {
    pub repo: String,
    pub path: String,
    pub line: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema, PartialEq, Eq)]
pub struct SearchVescKnowledgeResult {
    pub id: String,
    pub name: String,
    pub category: String,
    pub summary: String,
    pub source: SearchVescKnowledgeSource,
    pub score: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema, PartialEq, Eq)]
pub struct SearchVescKnowledgeResponse {
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub results: Vec<SearchVescKnowledgeResult>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

fn parse_category(raw: Option<&str>) -> Result<Option<Category>, String> {
    raw.map(|name| {
        let value = serde_json::Value::String(name.to_string());
        serde_json::from_value(value).map_err(|_| format!("unsupported category {name:?}"))
    })
    .transpose()
}

#[must_use]
pub fn search_vesc_knowledge_tool(
    params: &SearchVescKnowledgeParams,
) -> SearchVescKnowledgeResponse {
    let limit = if params.limit == 0 {
        default_search_limit()
    } else {
        params.limit
    };

    match parse_category(params.category.as_deref()) {
        Ok(category) => SearchVescKnowledgeResponse {
            ok: true,
            results: search_knowledge(&params.query, category, limit)
                .into_iter()
                .map(|hit| SearchVescKnowledgeResult {
                    id: hit.id,
                    name: hit.name,
                    category: category_label(hit.category).into(),
                    summary: hit.summary,
                    source: SearchVescKnowledgeSource {
                        repo: hit.source.repo,
                        path: hit.source.path,
                        line: hit.source.line,
                    },
                    score: hit.score,
                })
                .collect(),
            error: None,
        },
        Err(error) => SearchVescKnowledgeResponse {
            ok: false,
            results: Vec::new(),
            error: Some(error),
        },
    }
}

const fn category_label(category: Category) -> &'static str {
    match category {
        Category::FirmwareApi => "firmware_api",
        Category::Lispbm => "lispbm",
        Category::PackageBuild => "package_build",
        Category::RefloatCommand => "refloat_command",
        Category::NativeLibAbi => "native_lib_abi",
    }
}

/// Serialize a tool response as JSON text for rmcp handlers.
#[must_use]
pub fn search_vesc_knowledge_json(params: &SearchVescKnowledgeParams) -> String {
    let response = search_vesc_knowledge_tool(params);
    serde_json::to_string(&response)
        .unwrap_or_else(|_| r#"{"ok":false,"error":"serialization failed"}"#.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invalid_category_returns_error_response() {
        let resp = search_vesc_knowledge_tool(&SearchVescKnowledgeParams {
            query: "nvm".into(),
            category: Some("not_a_category".into()),
            limit: 10,
        });
        assert!(!resp.ok);
        assert!(resp.error.is_some());
        assert!(resp.results.is_empty());
    }

    #[test]
    fn zero_limit_uses_default() {
        let resp = search_vesc_knowledge_tool(&SearchVescKnowledgeParams {
            query: "pkg".into(),
            category: None,
            limit: 0,
        });
        assert!(resp.ok);
        assert!(!resp.results.is_empty());
    }

    #[test]
    fn category_label_maps_firmware_api() {
        assert_eq!(
            category_label(vesc_knowledge_index::Category::FirmwareApi),
            "firmware_api"
        );
    }
}
