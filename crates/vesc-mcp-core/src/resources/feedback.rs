use crate::{
    resources::{ParsedResourceUri, ResourceReadError, ResourceReadHandler},
    tools::knowledge_feedback::FeedbackStore,
};

/// Reads persisted model notes and corrections by stable resource URI.
#[derive(Debug, Clone)]
pub struct FeedbackResourceHandler {
    store: FeedbackStore,
}

impl FeedbackResourceHandler {
    #[must_use]
    pub const fn new(store: FeedbackStore) -> Self {
        Self { store }
    }
}

impl ResourceReadHandler for FeedbackResourceHandler {
    fn matches(&self, uri: &ParsedResourceUri) -> bool {
        matches!(uri, ParsedResourceUri::KnowledgeFeedback(_))
    }

    fn read(&self, uri: &ParsedResourceUri) -> Result<String, ResourceReadError> {
        let ParsedResourceUri::KnowledgeFeedback(feedback) = uri else {
            return Err(ResourceReadError::NotFound { uri: uri.to_uri() });
        };
        let record = self
            .store
            .get(&feedback.id)
            .map_err(|error| ResourceReadError::ReadFailed {
                uri: uri.to_uri(),
                message: error.to_string(),
            })?
            .ok_or_else(|| ResourceReadError::NotFound { uri: uri.to_uri() })?;
        serde_json::to_string_pretty(&record).map_err(|error| ResourceReadError::ReadFailed {
            uri: uri.to_uri(),
            message: error.to_string(),
        })
    }
}
