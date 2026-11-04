//! Translations of the egui interface.
//!
//! Texts are written in English in the code and passed through [`tr`] (fixed text) or [`trf`]
//! (text with `{}` placeholders, filled in order). Each other language has its catalog in a
//! file of its own (`i18n/it.rs`…); a text missing from a catalog is shown in English. English
//! is the language until the user chooses another, or the system one (View > Preferences).
//! Tests check that every text passed to `tr`/`trf` in the interface is in every catalog.

mod de;
mod es;
mod fr;
mod it;
mod zh;

use std::collections::HashMap;
use std::fmt::Display;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU8, Ordering};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lang {
    English,
    Italian,
    German,
    French,
    Spanish,
    Chinese,
}

impl Lang {
    const ALL: [Lang; 6] = [
        Lang::English,
        Lang::Italian,
        Lang::German,
        Lang::French,
        Lang::Spanish,
        Lang::Chinese,
    ];

    /// The setting value and the ISO 639-1 code of the language.
    fn code(self) -> &'static str {
        match self {
            Lang::English => "en",
            Lang::Italian => "it",
            Lang::German => "de",
            Lang::French => "fr",
            Lang::Spanish => "es",
            Lang::Chinese => "zh",
        }
    }

    /// English → this language; `None` for English.
    fn catalog(self) -> Option<&'static [(&'static str, &'static str)]> {
        match self {
            Lang::English => None,
            Lang::Italian => Some(it::CATALOG),
            Lang::German => Some(de::CATALOG),
            Lang::French => Some(fr::CATALOG),
            Lang::Spanish => Some(es::CATALOG),
            Lang::Chinese => Some(zh::CATALOG),
        }
    }
}

/// The setting that follows the language of the system.
pub const SYSTEM_LANGUAGE: &str = "system";

/// Language choices of the Preferences: (setting value, name in its own language). The
/// system choice is named in the current language.
pub const CHOICES: [(&str, &str); 7] = [
    (SYSTEM_LANGUAGE, "System language"),
    ("en", "English"),
    ("it", "Italiano"),
    ("de", "Deutsch"),
    ("fr", "Français"),
    ("es", "Español"),
    ("zh", "中文"),
];

static CURRENT: AtomicU8 = AtomicU8::new(0);

/// Applies the language setting: a language code, [`SYSTEM_LANGUAGE`], or anything else
/// (no choice made yet) for English.
pub fn set_language(setting: &str) {
    CURRENT.store(language_of(setting) as u8, Ordering::Relaxed);
}

fn language_of(setting: &str) -> Lang {
    if setting == SYSTEM_LANGUAGE {
        system_language()
    } else {
        from_code(setting).unwrap_or(Lang::English)
    }
}

pub fn current() -> Lang {
    let n = CURRENT.load(Ordering::Relaxed) as usize;
    Lang::ALL.get(n).copied().unwrap_or(Lang::English)
}

fn from_code(code: &str) -> Option<Lang> {
    Lang::ALL.into_iter().find(|l| l.code() == code)
}

/// The language of the system if Horama has it, English otherwise ("de-CH" → German).
fn system_language() -> Lang {
    sys_locale::get_locale()
        .and_then(|l| from_code(l.to_lowercase().get(..2)?))
        .unwrap_or(Lang::English)
}

fn translations(lang: Lang) -> Option<&'static HashMap<&'static str, &'static str>> {
    static MAPS: [OnceLock<HashMap<&'static str, &'static str>>; 6] =
        [const { OnceLock::new() }; 6];
    let catalog = lang.catalog()?;
    Some(MAPS[lang as usize].get_or_init(|| catalog.iter().copied().collect()))
}

/// The text in the current language.
pub fn tr(en: &str) -> &str {
    translations(current())
        .and_then(|map| map.get(en).copied())
        .unwrap_or(en)
}

/// The text in the current language, with each `{}` replaced by the next argument.
pub fn trf(en: &str, args: &[&dyn Display]) -> String {
    let mut out = String::new();
    let mut args = args.iter();
    let mut parts = tr(en).split("{}");
    if let Some(first) = parts.next() {
        out.push_str(first);
    }
    for part in parts {
        if let Some(a) = args.next() {
            out.push_str(&a.to_string());
        }
        out.push_str(part);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    /// Texts passed to `tr("…")` / `trf("…"` in the interface sources.
    fn texts_in(source: &str) -> Vec<String> {
        let mut out = Vec::new();
        for call in ["tr(\"", "trf(\""] {
            let mut rest = source;
            while let Some(i) = rest.find(call) {
                // `ctr(` or `attr(` are not calls of ours.
                let before = rest[..i].chars().last();
                rest = &rest[i + call.len()..];
                if before
                    .map(|c| c.is_alphanumeric() || c == '_')
                    .unwrap_or(false)
                {
                    continue;
                }
                let mut text = String::new();
                let mut chars = rest.chars();
                while let Some(c) = chars.next() {
                    match c {
                        '\\' => {
                            if let Some(n) = chars.next() {
                                text.push(n);
                            }
                        }
                        '"' => break,
                        c => text.push(c),
                    }
                }
                out.push(text);
            }
        }
        out
    }

    #[test]
    fn every_interface_text_has_its_translation() {
        let sources = [
            include_str!("mod.rs"),
            include_str!("filters.rs"),
            include_str!("files.rs"),
            include_str!("profiles.rs"),
            include_str!("watch.rs"),
            include_str!("edit.rs"),
            include_str!("compare.rs"),
            include_str!("shortcuts.rs"),
            include_str!("tools.rs"),
            include_str!("toolbar.rs"),
            include_str!("prefs.rs"),
            include_str!("display.rs"),
            include_str!("view_rotation.rs"),
            include_str!("overwrite.rs"),
        ];
        let texts: Vec<String> = sources.iter().flat_map(|s| texts_in(s)).collect();
        for lang in Lang::ALL {
            let Some(map) = translations(lang) else {
                continue;
            };
            let missing: Vec<&String> = texts
                .iter()
                .filter(|t| !map.contains_key(t.as_str()))
                .collect();
            assert!(missing.is_empty(), "no {:?} text for: {:?}", lang, missing);
        }
    }

    #[test]
    fn catalogs_have_the_same_texts_once_and_keep_placeholders() {
        let english: HashSet<&str> = it::CATALOG.iter().map(|(en, _)| *en).collect();
        for lang in Lang::ALL {
            let Some(catalog) = lang.catalog() else {
                continue;
            };
            let mut seen = HashSet::new();
            for (en, text) in catalog {
                assert!(seen.insert(*en), "twice in {:?}: {}", lang, en);
                assert!(!text.trim().is_empty(), "empty in {:?}: {}", lang, en);
                assert_eq!(
                    en.matches("{}").count(),
                    text.matches("{}").count(),
                    "{:?}: {}",
                    lang,
                    en
                );
            }
            let extra: Vec<&&str> = seen.difference(&english).collect();
            let missing: Vec<&&str> = english.difference(&seen).collect();
            assert!(
                extra.is_empty() && missing.is_empty(),
                "{:?}: extra {:?}, missing {:?}",
                lang,
                extra,
                missing
            );
        }
    }

    #[test]
    fn english_until_another_language_is_chosen() {
        for (setting, lang) in [
            ("", Lang::English),
            ("it", Lang::Italian),
            ("zh", Lang::Chinese),
            ("xx", Lang::English),
        ] {
            assert_eq!(language_of(setting), lang, "{:?}", setting);
        }
    }

    #[test]
    fn placeholders_are_filled_in_order() {
        CURRENT.store(Lang::Italian as u8, Ordering::Relaxed);
        assert_eq!(
            trf("Cannot display {}: {}", &[&"a.jpg", &"bad"]),
            "Impossibile mostrare a.jpg: bad"
        );
        assert_eq!(tr("not in the catalog"), "not in the catalog");
        CURRENT.store(Lang::English as u8, Ordering::Relaxed);
        assert_eq!(trf("{} images shown", &[&3]), "3 images shown");
    }
}
