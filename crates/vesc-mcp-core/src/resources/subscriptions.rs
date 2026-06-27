//! MCP resource subscription tracking (`resources/subscribe`, `resources/unsubscribe`).

use std::collections::HashSet;
use std::sync::RwLock;

/// In-memory set of resource URIs subscribed by MCP clients.
#[derive(Debug, Default)]
pub struct ResourceSubscriptions {
    uris: RwLock<HashSet<String>>,
}

impl ResourceSubscriptions {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Record a subscription. Returns `true` when the URI was newly added.
    ///
    /// # Panics
    ///
    /// Panics if the subscription lock is poisoned.
    pub fn subscribe(&self, uri: impl Into<String>) -> bool {
        self.uris
            .write()
            .expect("resource subscription lock")
            .insert(uri.into())
    }

    /// Remove a subscription. Returns `true` when the URI was present.
    ///
    /// # Panics
    ///
    /// Panics if the subscription lock is poisoned.
    pub fn unsubscribe(&self, uri: &str) -> bool {
        self.uris
            .write()
            .expect("resource subscription lock")
            .remove(uri)
    }

    /// Returns whether the URI is currently subscribed.
    ///
    /// # Panics
    ///
    /// Panics if the subscription lock is poisoned.
    #[must_use]
    pub fn is_subscribed(&self, uri: &str) -> bool {
        self.uris
            .read()
            .expect("resource subscription lock")
            .contains(uri)
    }

    /// List subscribed URIs in sorted order.
    ///
    /// # Panics
    ///
    /// Panics if the subscription lock is poisoned.
    #[must_use]
    pub fn subscribed_uris(&self) -> Vec<String> {
        let mut uris: Vec<_> = self
            .uris
            .read()
            .expect("resource subscription lock")
            .iter()
            .cloned()
            .collect();
        uris.sort();
        uris
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subscribe_tracks_and_deduplicates_uri() {
        let subs = ResourceSubscriptions::new();
        assert!(subs.subscribe("vesc://catalog/abi/minimal-test-package"));
        assert!(!subs.subscribe("vesc://catalog/abi/minimal-test-package"));
        assert!(subs.is_subscribed("vesc://catalog/abi/minimal-test-package"));
    }

    #[test]
    fn unsubscribe_removes_tracked_uri() {
        let subs = ResourceSubscriptions::new();
        subs.subscribe("vescpkg://fixture/refloat-minimal/manifest");
        assert!(subs.unsubscribe("vescpkg://fixture/refloat-minimal/manifest"));
        assert!(!subs.is_subscribed("vescpkg://fixture/refloat-minimal/manifest"));
    }
}
