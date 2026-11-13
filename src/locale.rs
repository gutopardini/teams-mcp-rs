use fluent_bundle::{FluentBundle, FluentResource};
use unic_langid::LanguageIdentifier;

const EN_US: &str = include_str!("../locales/en-US.ftl");
const CATALOGS: &[(&str, &str)] = &[("en-US", EN_US)];

/// English is always the fallback; adding another catalog does not alter policy.
pub fn effective_language(language: &str) -> &'static str {
    CATALOGS
        .iter()
        .find(|(name, _)| *name == language)
        .map(|(name, _)| *name)
        .unwrap_or("en-US")
}

pub fn text_for(language: &str, key: &str) -> String {
    let language = effective_language(language);
    let catalog = CATALOGS
        .iter()
        .find(|(name, _)| *name == language)
        .map(|(_, value)| *value)
        .unwrap_or(EN_US);
    find_text(language, catalog, key)
        .or_else(|| {
            (language != "en-US")
                .then(|| find_text("en-US", EN_US, key))
                .flatten()
        })
        .unwrap_or_else(|| key.to_owned())
}

pub fn text(key: &str) -> String {
    text_for("en-US", key)
}

fn find_text(language: &str, catalog: &str, key: &str) -> Option<String> {
    let locale: LanguageIdentifier = language.parse().expect("valid built-in locale");
    let resource = FluentResource::try_new(catalog.to_owned()).expect("valid built-in catalog");
    let mut bundle = FluentBundle::new(vec![locale]);
    bundle.add_resource(resource).expect("unique catalog keys");
    bundle
        .get_message(key)
        .and_then(|message| message.value())
        .map(|pattern| {
            let mut errors = Vec::new();
            bundle
                .format_pattern(pattern, None, &mut errors)
                .into_owned()
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::AppError;

    #[test]
    fn error_messages_have_english_catalog_entries() {
        let errors = [
            AppError::Forbidden,
            AppError::InvalidInput,
            AppError::InvalidCursor,
            AppError::ResponseTooLarge,
            AppError::AuthenticationRequired,
            AppError::AuthenticationFailed,
            AppError::IncompatibleClient,
            AppError::AuthenticationCancelled,
            AppError::BrowserUnavailable,
            AppError::AccountMismatch,
            AppError::CredentialStore,
            AppError::UnsupportedPlatform,
            AppError::Network,
            AppError::Upstream(403),
            AppError::InvalidResponse,
            AppError::InvalidConfig,
            AppError::NoResources,
            AppError::Protocol,
        ];
        for error in errors {
            assert_ne!(text(error.message_key()), error.message_key());
        }
    }

    #[test]
    fn unavailable_locales_fall_back_to_english() {
        assert_eq!(effective_language("en-US"), "en-US");
        assert_eq!(effective_language("zz-ZZ"), "en-US");
        assert_eq!(
            text_for("zz-ZZ", "error-forbidden"),
            text("error-forbidden")
        );
    }
}
