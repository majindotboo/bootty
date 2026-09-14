# Localization

`bootty-ui::i18n::Localizer` owns application messages. The native host publishes
its accepted locale to GPUI Kit with `set_locale`; the component library owns its
own translations. The config owner persists the top-level `locale` value.

`crates/bootty-ui/locales/en.ftl` contains extracted English settings, commands and
common UI messages. Settings metadata supplies English fallback for newly added
schema rows. `localize_settings` translates only presentation fields, after stable
section IDs and dependent rows have been resolved. User values, choice tokens,
paths, diagnostics and theme names are not translated.

Use complete Fluent messages with named variables for inserted values. Plural
variants use the language's CLDR rules, including when a missing translation falls
back to English. See [Fluent selectors](https://projectfluent.org/fluent/guide/selectors.html).
Keep Fluent's bidirectional isolation enabled. The `en-XA` transform changes message
text and expands vowels; it never transforms interpolated variables.

For a reviewed language, add its FTL file beside `en.ftl` and select that resource
in `Localizer::new`, calling `with_translation(language, Some(source))`. Missing
messages and formatting errors fall back individually; syntax errors reject the
catalog. Register any language-region fallback explicitly when adding those
catalogs. No external translation files or scripts are executed.

Static message keys describe meaning (`common-save`). Schema presentation keys use
`setting-<path>-label` and `setting-<path>-help`: dots become `--`, colons become
`---`. For example, `session.bell` uses `setting-session--bell-label`. Choice keys
add `-choice-<token>-label` or `-description`. Command text uses
`command-<action>-title` and `-description`. The action and persisted value never
change. Native panel features can reuse `Localizer`; custom Lua/Luau execution
remains unsupported.

Translate before filtering selectable command text, retain English/action keywords,
and preserve typed Cancel buttons rather than deriving cancel behavior from their
translated labels. Toolbars wrap longer labels. The settings navigation and content
already use scalable widths and scrolling. Minimum-size, large-text and CJK visual
acceptance is required before shipping a reviewed translation.

Coverage currently includes the shared settings projection, native command palette,
terminal find controls, Files/Changes/Document toolbars, native menus and command
completion. Remaining product dialogs and OS-owned messages are not yet a complete
localized UI. New messages should enter this catalog rather than a new string
registry.
