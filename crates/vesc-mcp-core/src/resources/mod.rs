//! MCP resource URI scheme, metadata, and registry.
//!
//! URI schemes (see epic `br-mcp-resources-9at`):
//! - `vesc://catalog/{kind}/{id}` — static catalog-backed resources
//! - `vescpkg://fixture/{name}/manifest` — in-repo fixture manifests
//! - `vescpkg://manifest/{path}` — dynamic manifests (sandboxed at read time)

mod abi;
mod catalog;
mod manifest;
mod refloat_command;
mod r#static;
mod uri;

pub use abi::{
    AbiResourceHandler, MINIMAL_TEST_PACKAGE_ABI_URI, read_abi_resource, register_abi_resources,
};
pub use catalog::{
    BuildFlowDoc, BuildRecipeResourceHandler, POC_RUST_PACKER_URI, REFLOAT_VESC_TOOL_URI,
    load_build_flow, read_build_recipe, register_build_recipe_resources,
};
pub use manifest::{
    ManifestResourceHandler, POC_NATIVE_LIB_MANIFEST_URI, REFLOAT_MINIMAL_MANIFEST_URI,
    read_manifest, register_manifest_resources,
};
pub use refloat_command::{
    REALTIME_DATA_COMMAND_URI, RefloatCommandResourceHandler, read_refloat_command,
    refloat_command_uri, register_refloat_command_resources,
};
pub use r#static::{
    DocTopicResourceHandler, LISP_IMPORTS_URI, PKGDESC_DIALECTS_URI, VESC_C_IF_URI, read_doc_topic,
    register_doc_topic_resources,
};
pub use uri::{
    CatalogResourceUri, FixtureManifestUri, ManifestResourceUri, ParsedResourceUri,
    ResourceUriError, parse_resource_uri,
};

use std::collections::BTreeMap;
use std::path::PathBuf;

use rmcp::model::{RawResource, RawResourceTemplate};

/// Metadata for a statically registered MCP resource.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResourceMeta {
    pub uri: String,
    pub name: String,
    pub description: Option<String>,
    pub mime_type: String,
}

impl ResourceMeta {
    #[must_use]
    pub fn to_mcp_resource(&self) -> RawResource {
        let mut resource = RawResource::new(&self.uri, &self.name).with_mime_type(&self.mime_type);
        if let Some(description) = &self.description {
            resource = resource.with_description(description);
        }
        resource
    }
}

/// Read handler for a registered resource URI (rmcp 1.8 `resources/read` parity).
///
/// Implementations are wired by later resource tasks; the registry only routes by URI.
pub trait ResourceReadHandler: Send + Sync {
    /// Returns true when this handler serves the parsed URI variant.
    fn matches(&self, uri: &ParsedResourceUri) -> bool;

    /// Read resource body bytes. Errors propagate as MCP resource-not-found or I/O failures.
    ///
    /// # Errors
    ///
    /// Returns [`ResourceReadError`] when the resource is missing or cannot be read.
    fn read(&self, uri: &ParsedResourceUri) -> Result<String, ResourceReadError>;
}

/// Errors while reading a resource body.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ResourceReadError {
    #[error("resource not found: {uri}")]
    NotFound { uri: String },

    #[error("read failed for {uri}: {message}")]
    ReadFailed { uri: String, message: String },
}

/// Registry of static MCP resources plus URI validation for dynamic templates.
#[derive(Default)]
pub struct ResourceRegistry {
    static_resources: BTreeMap<String, ResourceMeta>,
    handlers: Vec<Box<dyn ResourceReadHandler>>,
}

impl std::fmt::Debug for ResourceRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ResourceRegistry")
            .field("static_resources", &self.static_resources)
            .field("handlers", &self.handlers.len())
            .finish()
    }
}

impl ResourceRegistry {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Parse and validate a resource URI against the vesc/vescpkg schemes.
    ///
    /// # Errors
    ///
    /// Returns [`ResourceUriError`] when the URI is malformed or uses an unsupported scheme.
    pub fn validate_uri(&self, uri: &str) -> Result<ParsedResourceUri, ResourceUriError> {
        let _ = self;
        parse_resource_uri(uri)
    }

    /// Register a static resource. URIs must parse successfully and be unique.
    ///
    /// # Errors
    ///
    /// Returns [`ResourceRegistryError`] when the URI is invalid or already registered.
    pub fn register(&mut self, meta: ResourceMeta) -> Result<(), ResourceRegistryError> {
        let ResourceMeta {
            uri,
            name,
            description,
            mime_type,
        } = meta;
        parse_resource_uri(&uri).map_err(|source| ResourceRegistryError::InvalidUri {
            uri: uri.clone(),
            source,
        })?;
        if self.static_resources.contains_key(&uri) {
            return Err(ResourceRegistryError::DuplicateUri { uri });
        }
        self.static_resources.insert(
            uri.clone(),
            ResourceMeta {
                uri,
                name,
                description,
                mime_type,
            },
        );
        Ok(())
    }

    /// Lookup static resource metadata by exact URI.
    #[must_use]
    pub fn lookup(&self, uri: &str) -> Option<&ResourceMeta> {
        self.static_resources.get(uri)
    }

    /// List registered static resources in URI order.
    #[must_use]
    pub fn list_static(&self) -> Vec<&ResourceMeta> {
        self.static_resources.values().collect()
    }

    /// Convert registered static resources to rmcp list payload entries.
    #[must_use]
    pub fn list_mcp_resources(&self) -> Vec<RawResource> {
        self.static_resources
            .values()
            .map(ResourceMeta::to_mcp_resource)
            .collect()
    }

    /// Register a read handler for dynamic resource bodies.
    pub fn register_handler(&mut self, handler: impl ResourceReadHandler + 'static) {
        self.handlers.push(Box::new(handler));
    }

    /// Read a resource body by URI, dispatching to the first matching handler.
    ///
    /// # Errors
    ///
    /// Returns [`ResourceReadError`] when the URI is invalid, unhandled, or read fails.
    pub fn read(&self, uri: &str) -> Result<String, ResourceReadError> {
        let parsed = parse_resource_uri(uri).map_err(|err| ResourceReadError::NotFound {
            uri: format!("{uri}: {err}"),
        })?;
        for handler in &self.handlers {
            if handler.matches(&parsed) {
                return handler.read(&parsed);
            }
        }
        Err(ResourceReadError::NotFound { uri: uri.into() })
    }

    /// List MCP resource templates served by registered handlers.
    #[must_use]
    pub fn list_mcp_templates(&self) -> Vec<RawResourceTemplate> {
        let probe = ParsedResourceUri::DynamicManifest(ManifestResourceUri { path: "_".into() });
        if self.handlers.iter().any(|handler| handler.matches(&probe)) {
            vec![Self::manifest_template()]
        } else {
            vec![]
        }
    }

    /// Registry preloaded with build-recipe and manifest resources plus read handlers.
    ///
    /// # Errors
    ///
    /// Returns [`ResourceRegistryError`] when static resource registration fails.
    pub fn with_defaults() -> Result<Self, ResourceRegistryError> {
        let mut registry = Self::new();
        let catalog_root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../catalog");
        register_build_recipe_resources(&mut registry)?;
        register_doc_topic_resources(&mut registry)?;
        register_abi_resources(&mut registry)?;
        register_refloat_command_resources(&mut registry, &catalog_root)?;
        register_manifest_resources(&mut registry)?;
        registry.register_handler(BuildRecipeResourceHandler::new());
        registry.register_handler(DocTopicResourceHandler::new());
        registry.register_handler(AbiResourceHandler::new());
        registry.register_handler(RefloatCommandResourceHandler::new());
        registry.register_handler(ManifestResourceHandler::from_config());
        Ok(registry)
    }

    /// MCP resource template for sandboxed live manifest reads.
    #[must_use]
    pub fn manifest_template() -> RawResourceTemplate {
        RawResourceTemplate::new("vescpkg://manifest/{path}", "vescpkg manifest")
            .with_description("Parsed pkgdesc for a package root under configured sandbox paths")
            .with_mime_type("application/json")
    }
}

/// Registry mutation errors.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ResourceRegistryError {
    #[error("duplicate resource URI: {uri}")]
    DuplicateUri { uri: String },

    #[error("invalid resource URI {uri}: {source}")]
    InvalidUri {
        uri: String,
        #[source]
        source: ResourceUriError,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resource_meta_converts_to_mcp_resource() {
        let meta = ResourceMeta {
            uri: "vesc://catalog/abi/minimal-test-package".into(),
            name: "minimal test package ABI".into(),
            description: Some("JSON ABI inventory".into()),
            mime_type: "application/json".into(),
        };
        let mcp = meta.to_mcp_resource();
        assert_eq!(mcp.uri, meta.uri);
        assert_eq!(mcp.name, meta.name);
        assert_eq!(mcp.mime_type.as_deref(), Some("application/json"));
        assert_eq!(mcp.description.as_deref(), Some("JSON ABI inventory"));
    }

    #[test]
    fn register_rejects_duplicate_uri() {
        let mut registry = ResourceRegistry::new();
        let meta = ResourceMeta {
            uri: "vesc://catalog/build-recipe/poc-rust-packer".into(),
            name: "poc".into(),
            description: None,
            mime_type: "text/markdown".into(),
        };
        registry.register(meta.clone()).expect("first register");
        let err = registry.register(meta).expect_err("duplicate");
        assert!(matches!(err, ResourceRegistryError::DuplicateUri { .. }));
    }
}
