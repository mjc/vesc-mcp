//! Resource URI parsing for `vesc://` and `vescpkg://` schemes.

use std::fmt;

/// Parsed resource URI variants.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParsedResourceUri {
    Catalog(CatalogResourceUri),
    RefloatCommand(RefloatCommandUri),
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

/// `vesc://catalog/commands/refloat/{command}` — refloat command doc resources.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RefloatCommandUri {
    pub command: String,
}

impl RefloatCommandUri {
    #[must_use]
    pub fn to_uri(&self) -> String {
        format!("vesc://catalog/commands/refloat/{}", self.command)
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
        format!("vescpkg://manifest/{}", encode_manifest_path(&self.path))
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
            Self::RefloatCommand(command) => command.to_uri(),
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

    if kind == "commands" {
        if let Some(command) = id
            .strip_prefix("refloat/")
            .filter(|name| !name.is_empty() && !name.contains('/'))
        {
            return Ok(ParsedResourceUri::RefloatCommand(RefloatCommandUri {
                command: command.into(),
            }));
        }
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

    let decoded =
        decode_manifest_path(path).map_err(|reason| ResourceUriError::malformed(full, reason))?;

    Ok(ParsedResourceUri::DynamicManifest(ManifestResourceUri {
        path: decoded,
    }))
}

const HEX: &[u8; 16] = b"0123456789ABCDEF";

const fn is_unreserved(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~')
}

/// Percent-encode a manifest path for `vescpkg://manifest/{path}` URIs (RFC 3986).
///
/// Relative paths preserve `/` segment separators. Absolute paths encode every `/` as `%2F`.
#[must_use]
pub fn encode_manifest_path(path: &str) -> String {
    if path.is_empty() {
        return String::new();
    }

    if path.starts_with('/') {
        return encode_bytes(path.as_bytes());
    }

    path.split('/')
        .map(|segment| encode_bytes(segment.as_bytes()))
        .collect::<Vec<_>>()
        .join("/")
}

/// Decode a percent-encoded manifest path from a URI.
///
/// # Errors
///
/// Returns a reason string when `%` sequences are truncated or use invalid hex digits.
pub fn decode_manifest_path(encoded: &str) -> Result<String, String> {
    let mut out = String::with_capacity(encoded.len());
    let bytes = encoded.as_bytes();
    let mut index = 0;

    while index < bytes.len() {
        if bytes[index] == b'%' {
            let hex = bytes
                .get(index + 1..index + 3)
                .ok_or_else(|| "truncated percent-encoding".to_string())?;
            let high = hex_digit(hex[0])?;
            let low = hex_digit(hex[1])?;
            out.push(char::from((high << 4) | low));
            index += 3;
            continue;
        }

        out.push(char::from(bytes[index]));
        index += 1;
    }

    Ok(out)
}

fn encode_bytes(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len());
    for &byte in bytes {
        if is_unreserved(byte) {
            out.push(char::from(byte));
        } else {
            out.push('%');
            out.push(char::from(HEX[(byte >> 4) as usize]));
            out.push(char::from(HEX[(byte & 0x0f) as usize]));
        }
    }
    out
}

fn hex_digit(byte: u8) -> Result<u8, String> {
    match byte {
        b'0'..=b'9' => Ok(byte - b'0'),
        b'a'..=b'f' => Ok(byte - b'a' + 10),
        b'A'..=b'F' => Ok(byte - b'A' + 10),
        _ => Err(format!("invalid percent-encoding byte {byte:?}")),
    }
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
    fn refloat_command_uri_round_trips() {
        let parsed = parse_resource_uri("vesc://catalog/commands/refloat/REALTIME_DATA").unwrap();
        assert_eq!(
            parsed,
            ParsedResourceUri::RefloatCommand(RefloatCommandUri {
                command: "REALTIME_DATA".into(),
            })
        );
        assert_eq!(
            parsed.to_uri(),
            "vesc://catalog/commands/refloat/REALTIME_DATA"
        );
    }

    #[test]
    fn catalog_doc_uri_keeps_multi_segment_id() {
        let parsed = parse_resource_uri("vesc://catalog/doc/topic/pkgdesc_dialects").unwrap();
        assert_eq!(
            parsed,
            ParsedResourceUri::Catalog(CatalogResourceUri {
                kind: "doc".into(),
                id: "topic/pkgdesc_dialects".into(),
            })
        );
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

    #[test]
    fn manifest_path_encode_decode_round_trips_relative_with_spaces() {
        let path = "my package/pkgdesc.qml";
        let encoded = encode_manifest_path(path);
        assert_eq!(encoded, "my%20package/pkgdesc.qml");
        assert_eq!(decode_manifest_path(&encoded).unwrap(), path);

        let uri = format!("vescpkg://manifest/{encoded}");
        let parsed = parse_resource_uri(&uri).unwrap();
        assert_eq!(
            parsed,
            ParsedResourceUri::DynamicManifest(ManifestResourceUri { path: path.into() })
        );
        assert_eq!(parsed.to_uri(), uri);
    }

    #[test]
    fn manifest_path_encode_decode_round_trips_absolute() {
        let path = "/tmp/foo bar/pkgdesc.qml";
        let encoded = encode_manifest_path(path);
        assert_eq!(encoded, "%2Ftmp%2Ffoo%20bar%2Fpkgdesc.qml");
        assert_eq!(decode_manifest_path(&encoded).unwrap(), path);

        let uri = format!("vescpkg://manifest/{encoded}");
        let parsed = parse_resource_uri(&uri).unwrap();
        assert_eq!(
            parsed,
            ParsedResourceUri::DynamicManifest(ManifestResourceUri { path: path.into() })
        );
        assert_eq!(parsed.to_uri(), uri);
    }

    #[test]
    fn manifest_path_decode_rejects_truncated_percent() {
        let err = decode_manifest_path("foo%2").unwrap_err();
        assert!(err.contains("truncated"));
    }
}
