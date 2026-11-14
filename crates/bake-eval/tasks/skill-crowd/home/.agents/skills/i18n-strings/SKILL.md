---
name: i18n-strings
description: "Add or change user-facing text so it can be translated - message keys, ICU plural and select syntax, placeholders instead of string concatenation, context notes for translators, and keeping every locale file in sync. Use when adding UI text, fixing a string that reads wrong in another language, handling plurals or genders, formatting dates, numbers, or currencies for a locale, or when asked about internationalization, localization, i18n, or l10n."
---

# Translatable strings

- Keys are `<screen>.<element>.<purpose>`; add them to `locales/en.json` only.
- Use ICU syntax for plurals: `{count, plural, one {# item} other {# items}}`.
- Never concatenate translated fragments; pass placeholders.
- Add a `_note` for any key whose meaning is unclear without the screen.
