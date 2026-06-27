//! Resource URI parsing for `vesc://` and `vescpkg://` schemes.

use std::fmt;

/// Parsed resource URI variants.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParsedResourceUri {
    Catalog(CatalogResourceUri),
    FixtureManifest(FixtureManifestUri),
    DynamicManifest(ManifestResourceUri),
}

/// `vesc://catalog/{kind}/{id}` — first path segment is kind, remainder is id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogResourceUri {
    pub kind: String,
    pub id: String,
}

impl CatalogResourceUri {
    #[must_use]
    pub fn to_uri(&self) -> String {
        format!("vesc://catalog/{}/{}", self.kind, self.id)
    }
}

/// `vescpkg://fixture/{name}/manifest`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FixtureManifestUri {
    pub name: String,
}

impl FixtureManifestUri {
    #[must_use]
    pub fn to_uri(&self) -> String {
        format!("vescpkg://fixture/{}/manifest", self.name)
    }
}

/// `vescpkg://manifest/{path}` — path is relative to configured sandbox roots.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManifestResourceUri {
    pub path: String,
}

impl ManifestResourceUri {
    #[must_use]
    pub fn to_uri(&self) -> String {
        format!("vescpkg://manifest/{}", self.path)
    }
}

/// URI parse/validation failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResourceUriError {
    pub uri: String,
    pub reason: String,
}

impl ResourceUriError {
    fn malformed(uri: impl Into<String>, reason: impl Into<String>) -> Self {
        Self {
            uri: uri.into(),
            reason: reason.into(),
        }
    }
}

impl fmt::Display for ResourceUriError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "malformed resource URI {}: {}", self.uri, self.reason)
    }
}

impl std::error::Error for ResourceUriError {}

impl ParsedResourceUri {
    #[must_use]
    pub fn to_uri(&self) -> String {
        match self {
            Self::Catalog(catalog) => catalog.to_uri(),
            Self::FixtureManifest(fixture) => fixture.to_uri(),
            Self::DynamicManifest(manifest) => manifest.to_uri(),
        }
    }
}

/// Parse a resource URI string into a typed variant.
///
/// # Errors
///
/// Returns [`ResourceUriError`] when the URI is empty, malformed, or uses an unsupported scheme.
pub fn parse_resource_uri(uri: &str) -> Result<ParsedResourceUri, ResourceUriError> {
    let uri = uri.trim();
    if uri.is_empty() {
        return Err(ResourceUriError::malformed(uri, "URI must not be empty"));
    }

    let Some((scheme, rest)) = uri.split_once("://") else {
        return Err(ResourceUriError::malformed(
            uri,
            "URI must include a scheme followed by ://",
        ));
    };

    match scheme {
        "vesc" => parse_vesc_uri(uri, rest),
        "vescpkg" => parse_vescpkg_uri(uri, rest),
        _ => Err(ResourceUriError::malformed(
            uri,
            format!("unsupported scheme {scheme:?}; expected vesc or vescpkg"),
        )),
    }
}

fn parse_vesc_uri(full: &str, rest: &str) -> Result<ParsedResourceUri, ResourceUriError> {
    let Some((authority, path)) = rest.split_once('/') else {
        return Err(ResourceUriError::malformed(
            full,
            "vesc URI must include an authority and path",
        ));
    };

    if authority != "catalog" {
        return Err(ResourceUriError::malformed(
            full,
            format!("vesc authority must be catalog, got {authority:?}"),
        ));
    }

    let Some((kind, id)) = path.split_once('/') else {
        return Err(ResourceUriError::malformed(
            full,
            "catalog URI must include kind and id segments",
        ));
    };

    if kind.is_empty() || id.is_empty() {
        return Err(ResourceUriError::malformed(
            full,
            "catalog kind and id must not be empty",
        ));
    }

    Ok(ParsedResourceUri::Catalog(CatalogResourceUri {
        kind: kind.into(),
        id: id.into(),
    }))
}

fn parse_vescpkg_uri(full: &str, rest: &str) -> Result<ParsedResourceUri, ResourceUriError> {
    let Some((authority, path)) = rest.split_once('/') else {
        return Err(ResourceUriError::malformed(
            full,
            "vescpkg URI must include an authority and path",
        ));
    };

    match authority {
        "fixture" => parse_fixture_manifest_uri(full, path),
        "manifest" => parse_dynamic_manifest_uri(full, path),
        other => Err(ResourceUriError::malformed(
            full,
            format!("unsupported vescpkg authority {other:?}; expected fixture or manifest"),
        )),
    }
}

fn parse_fixture_manifest_uri(
    full: &str,
    path: &str,
) -> Result<ParsedResourceUri, ResourceUriError> {
    let Some((name, tail)) = path.split_once('/') else {
        return Err(ResourceUriError::malformed(
            full,
            "fixture URI must match vescpkg://fixture/<name>/manifest",
        ));
    };

    if name.is_empty() {
        return Err(ResourceUriError::malformed(
            full,
            "fixture name must not be empty",
        ));
    }

    if tail != "manifest" {
        return Err(ResourceUriError::malformed(
            full,
            "fixture URI path must end with /manifest",
        ));
    }

    Ok(ParsedResourceUri::FixtureManifest(FixtureManifestUri {
        name: name.into(),
    }))
}

fn parse_dynamic_manifest_uri(
    full: &str,
    path: &str,
) -> Result<ParsedResourceUri, ResourceUriError> {
    if path.is_empty() {
        return Err(ResourceUriError::malformed(
            full,
            "manifest URI must include a non-empty path",
        ));
    }

    Ok(ParsedResourceUri::DynamicManifest(ManifestResourceUri {
        path: path.into(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_uri_round_trips() {
        let parsed = parse_resource_uri("vesc://catalog/commands/refloat/balance").unwrap();
        assert_eq!(parsed.to_uri(), "vesc://catalog/commands/refloat/balance");
    }

    #[test]
    fn fixture_manifest_uri_round_trips() {
        let parsed =
            parse_resource_uri("vescpkg://fixture/poc-native-lib-minimal/manifest").unwrap();
        assert_eq!(
            parsed.to_uri(),
            "vescpkg://fixture/poc-native-lib-minimal/manifest"
        );
    }
}
