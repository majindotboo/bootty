//! App-owned messages. Machine identifiers, paths and backend diagnostics are never translated.
use anyhow::{Result, anyhow};
use fluent_bundle::{FluentArgs, FluentResource, concurrent::FluentBundle};
use std::{
    borrow::Cow,
    sync::{Arc, OnceLock},
};
use unic_langid::LanguageIdentifier;

mod settings;
pub use settings::localize_settings;

pub const ENGLISH: &str = include_str!("../locales/en.ftl");

#[derive(Clone)]
pub struct Localizer {
    requested: String,
    english: Arc<FluentBundle<FluentResource>>,
    translated: Option<Arc<FluentBundle<FluentResource>>>,
    pseudo: bool,
}

impl Localizer {
    /// Load the bundled catalog for a requested locale.
    ///
    /// # Errors
    /// Returns an error if the bundled catalog is malformed or contains duplicate messages.
    pub fn new(locale: &str) -> Result<Self> {
        Self::with_translation(locale, None)
    }

    /// A native feature can supply a reviewed catalog without changing message callers.
    /// Missing messages and translation formatting errors fall back individually to English.
    ///
    /// # Errors
    /// Rejects malformed catalogs and duplicate message definitions.
    pub fn with_translation(locale: &str, source: Option<&str>) -> Result<Self> {
        let english_language = "en".parse::<LanguageIdentifier>()?;
        let language = locale
            .parse::<LanguageIdentifier>()
            .unwrap_or_else(|_| english_language.clone());
        let pseudo = language.to_string().eq_ignore_ascii_case("en-XA");
        let english = bundle(english_language, ENGLISH, pseudo)?;
        let translated = source
            .map(|source| bundle(language, source, false))
            .transpose()?;
        Ok(Self {
            requested: locale.to_owned(),
            english: Arc::new(english),
            translated: translated.map(Arc::new),
            pseudo,
        })
    }

    #[must_use]
    pub fn locale(&self) -> &str {
        &self.requested
    }

    /// Resolve a complete message, including its parameters and CLDR plural variants.
    #[must_use]
    pub fn message(&self, key: &str, args: Option<&FluentArgs<'_>>) -> String {
        self.lookup(key, args).unwrap_or_else(|| key.to_owned())
    }

    /// Schema-owned English copy stays with its schema. Only presentation uses these stable keys.
    #[must_use]
    pub fn text(&self, key: &str, english: &str) -> String {
        self.lookup(key, None).unwrap_or_else(|| {
            if self.pseudo {
                pseudo_text(english).into_owned()
            } else {
                english.to_owned()
            }
        })
    }

    fn lookup(&self, key: &str, args: Option<&FluentArgs<'_>>) -> Option<String> {
        self.translated
            .iter()
            .chain(std::iter::once(&self.english))
            .find_map(|bundle| {
                let message = bundle.get_message(key)?;
                let pattern = message.value()?;
                let mut errors = Vec::new();
                let result = bundle
                    .format_pattern(pattern, args, &mut errors)
                    .into_owned();
                errors.is_empty().then_some(result)
            })
    }
}

fn bundle(
    locale: LanguageIdentifier,
    source: &str,
    pseudo: bool,
) -> Result<FluentBundle<FluentResource>> {
    let resource = FluentResource::try_new(source.to_owned())
        .map_err(|(_, errors)| anyhow!("invalid translation catalog: {errors:?}"))?;
    let mut bundle = FluentBundle::new_concurrent(vec![locale]);
    bundle
        .add_resource(resource)
        .map_err(|errors| anyhow!("duplicate translation messages: {errors:?}"))?;
    if pseudo {
        bundle.set_transform(Some(pseudo_text));
    }
    Ok(bundle)
}

fn pseudo_text(text: &str) -> Cow<'_, str> {
    if text.trim().is_empty() {
        return Cow::Borrowed(text);
    }
    let mut output = String::from("［");
    for character in text.chars() {
        output.push_str(match character {
            'a' => "áá",
            'e' => "ëë",
            'i' => "ïï",
            'o' => "öö",
            'u' => "üü",
            'A' => "ÁÁ",
            'E' => "ËË",
            'I' => "ÏÏ",
            'O' => "ÖÖ",
            'U' => "ÜÜ",
            _ => {
                output.push(character);
                continue;
            }
        });
    }
    output.push('］');
    Cow::Owned(output)
}

/// Dots and colons remain distinct in keys; translating a label never changes its action ID.
#[must_use]
pub fn presentation_key(prefix: &str, id: &str, field: &str) -> String {
    format!(
        "{prefix}-{}-{field}",
        id.replace('.', "--").replace(':', "---")
    )
}

struct UiLocale(Localizer);
impl gpui_kit::Global for UiLocale {}

pub(crate) fn publish(localizer: &Localizer, cx: &mut gpui_kit::App) {
    if cx
        .try_global::<UiLocale>()
        .is_some_and(|current| current.0.locale() == localizer.locale())
    {
        return;
    }
    let language = localizer
        .locale()
        .parse::<LanguageIdentifier>()
        .map_or_else(|_| "en".to_owned(), |language| language.to_string());
    gpui_kit::component::set_locale(if localizer.pseudo { "en" } else { &language });
    cx.set_global(UiLocale(localizer.clone()));
    cx.refresh_windows();
}

fn ui(cx: &gpui_kit::App) -> Option<&Localizer> {
    static ENGLISH_UI: OnceLock<Option<Localizer>> = OnceLock::new();
    cx.try_global::<UiLocale>()
        .map(|locale| &locale.0)
        .or_else(|| {
            ENGLISH_UI
                .get_or_init(|| match Localizer::new("en") {
                    Ok(localizer) => Some(localizer),
                    Err(error) => {
                        eprintln!("load English UI catalog: {error:#}");
                        None
                    }
                })
                .as_ref()
        })
}

pub(crate) fn t(cx: &gpui_kit::App, key: &str) -> String {
    ui(cx).map_or_else(|| key.to_owned(), |localizer| localizer.message(key, None))
}
