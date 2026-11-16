// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Hunspell dictionaries: decoding, the bundled `en_US`, and the library
//! of what can be loaded.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, PoisonError, RwLock};

use crate::backend::{SpellBackend, SpellbookBackend};
use crate::language::{normalize_tag, primary};

/// A dictionary built into the application.
#[derive(Debug)]
pub struct Bundled {
    /// The language id (`en_US`).
    pub language: &'static str,
    pub aff: &'static [u8],
    pub dic: &'static [u8],
    /// The dictionary's licence and credits, shipped unmodified.
    pub readme: &'static str,
}

/// The bundled dictionaries: SCOWL `en_US` 2026.02.25 (size 60), from
/// `hunspell-en_US-2026.02.25.zip` of github.com/en-wl/wordlist release
/// `rel-2026.02.25`.
pub static BUNDLED: &[Bundled] = &[Bundled {
    language: "en_US",
    aff: include_bytes!("../dictionaries/en_US/en_US.aff"),
    dic: include_bytes!("../dictionaries/en_US/en_US.dic"),
    readme: include_str!("../dictionaries/en_US/README_en_US.txt"),
}];

/// Why a dictionary could not be loaded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DictionaryError {
    /// The `.aff`'s `SET` names an encoding we do not know.
    UnknownEncoding(String),
    /// The files do not decode in their encoding.
    Malformed(String),
    /// spellbook rejected the files.
    Parse(String),
    /// A file could not be read.
    Io(String),
    /// No dictionary for this language.
    NotFound(String),
}

impl std::fmt::Display for DictionaryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownEncoding(label) => write!(
                f,
                "the dictionary's encoding `{label}` (its SET line) is not supported"
            ),
            Self::Malformed(encoding) => {
                write!(f, "the dictionary is not valid {encoding} text")
            }
            Self::Parse(e) => write!(f, "the dictionary could not be read: {e}"),
            Self::Io(e) => write!(f, "{e}"),
            Self::NotFound(language) => write!(f, "no dictionary for `{language}`"),
        }
    }
}

impl std::error::Error for DictionaryError {}

/// The label of the `SET` line of an `.aff` file, if it has one.
fn set_label(aff: &[u8]) -> Option<String> {
    aff.split(|&b| b == b'\n').find_map(|line| {
        let line = line.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(line);
        let rest = line.strip_prefix(b"SET")?;
        let label = String::from_utf8_lossy(rest).trim().to_owned();
        (!label.is_empty() && rest.first().is_some_and(u8::is_ascii_whitespace)).then_some(label)
    })
}

/// The encoding Hunspell's label names. Hunspell writes `ISO8859-1` where
/// the WHATWG label is `iso-8859-1`.
fn encoding_for(label: &str) -> Option<&'static encoding_rs::Encoding> {
    encoding_rs::Encoding::for_label(label.as_bytes()).or_else(|| {
        let lower = label.to_ascii_lowercase();
        let fixed = lower
            .strip_prefix("iso8859")
            .map(|rest| format!("iso-8859{rest}"))
            .or_else(|| {
                lower
                    .strip_prefix("microsoft-cp")
                    .map(|n| format!("windows-{n}"))
            })?;
        encoding_rs::Encoding::for_label(fixed.as_bytes())
    })
}

/// Decodes an `.aff`/`.dic` pair from the encoding the `.aff`'s `SET` line
/// names (UTF-8 without one), dropping byte-order marks.
pub fn decode_hunspell(aff: &[u8], dic: &[u8]) -> Result<(String, String), DictionaryError> {
    let encoding = match set_label(aff) {
        Some(label) => encoding_for(&label).ok_or(DictionaryError::UnknownEncoding(label))?,
        None => encoding_rs::UTF_8,
    };
    let decode = |bytes: &[u8]| {
        let (text, _, errors) = encoding.decode(bytes);
        if errors {
            Err(DictionaryError::Malformed(encoding.name().to_owned()))
        } else {
            Ok(text.trim_start_matches('\u{FEFF}').to_owned())
        }
    };
    Ok((decode(aff)?, decode(dic)?))
}

/// Loads a dictionary from its files' bytes.
fn load(language: &str, aff: &[u8], dic: &[u8]) -> Result<SpellbookBackend, DictionaryError> {
    let (aff, dic) = decode_hunspell(aff, dic)?;
    let dictionary = std::panic::catch_unwind(|| spellbook::Dictionary::new(&aff, &dic))
        .map_err(|_| DictionaryError::Parse("the parser failed".to_owned()))?
        .map_err(|e| DictionaryError::Parse(e.to_string()))?;
    Ok(SpellbookBackend::new(language, Arc::new(dictionary)))
}

/// The bundled dictionaries, loaded at most once per process.
fn bundled_backend(bundled: &'static Bundled) -> Result<Arc<dyn SpellBackend>, DictionaryError> {
    type Loaded = Result<SpellbookBackend, DictionaryError>;
    static LOADED: OnceLock<Vec<OnceLock<Loaded>>> = OnceLock::new();
    let slots = LOADED.get_or_init(|| BUNDLED.iter().map(|_| OnceLock::new()).collect());
    let index = BUNDLED
        .iter()
        .position(|b| std::ptr::eq(b, bundled))
        .ok_or_else(|| DictionaryError::NotFound(bundled.language.to_owned()))?;
    let loaded = slots[index].get_or_init(|| load(bundled.language, bundled.aff, bundled.dic));
    loaded.clone().map(|b| Arc::new(b) as Arc<dyn SpellBackend>)
}

/// A dictionary loaded, or why it could not be.
type Loaded = Result<Arc<dyn SpellBackend>, DictionaryError>;

/// Where a language's dictionary comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DictionarySource {
    Bundled(&'static str),
    /// An `.aff`/`.dic` pair in the user's dictionary folder.
    Files {
        aff: PathBuf,
        dic: PathBuf,
    },
}

/// The dictionaries that can be loaded, and those loaded so far.
///
/// User pairs (`<language>.aff` with `<language>.dic`) in the folder given
/// to [`Library::new`] add languages, or replace a bundled one of the same
/// name. Loading happens on first use, off the UI thread.
#[derive(Debug)]
pub struct Library {
    folder: Option<PathBuf>,
    sources: RwLock<Vec<(String, DictionarySource)>>,
    loaded: Mutex<HashMap<String, Loaded>>,
}

impl Library {
    /// The bundled dictionaries and the pairs found in `folder`.
    pub fn new(folder: Option<PathBuf>) -> Self {
        let library = Self {
            folder,
            sources: RwLock::new(Vec::new()),
            loaded: Mutex::new(HashMap::new()),
        };
        library.rescan();
        library
    }

    /// Looks for dictionaries again (files added or removed) and forgets
    /// the loaded ones.
    pub fn rescan(&self) {
        let mut sources: Vec<(String, DictionarySource)> = BUNDLED
            .iter()
            .map(|b| (b.language.to_owned(), DictionarySource::Bundled(b.language)))
            .collect();
        if let Some(folder) = &self.folder {
            for (language, aff, dic) in scan(folder) {
                sources.retain(|(l, _)| *l != language);
                sources.push((language, DictionarySource::Files { aff, dic }));
            }
        }
        sources.sort_by(|a, b| a.0.cmp(&b.0));
        *self.sources.write().unwrap_or_else(PoisonError::into_inner) = sources;
        self.loaded
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clear();
    }

    /// The folder of user dictionaries.
    pub fn folder(&self) -> Option<&Path> {
        self.folder.as_deref()
    }

    /// Every language with a dictionary, sorted.
    pub fn languages(&self) -> Vec<String> {
        self.sources
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .map(|(l, _)| l.clone())
            .collect()
    }

    /// The dictionary a language tag means: `en-gb`, `en_GB` and `EN_gb`
    /// name `en_GB`; `en_GB` also takes `en_GB-ise`; a bare `en` takes the
    /// bundled `en_US`, else `xx_XX`, else the first `xx_…`.
    pub fn resolve(&self, tag: &str) -> Option<String> {
        let tag = normalize_tag(tag)?;
        let sources = self.sources.read().unwrap_or_else(PoisonError::into_inner);
        let ids: Vec<&str> = sources.iter().map(|(l, _)| l.as_str()).collect();
        if let Some(id) = ids.iter().find(|id| id.eq_ignore_ascii_case(&tag)) {
            return Some((*id).to_owned());
        }
        let lower = tag.to_ascii_lowercase();
        if tag.contains('_')
            && let Some(id) = ids.iter().find(|id| {
                let id = id.to_ascii_lowercase();
                id.strip_prefix(&lower)
                    .is_some_and(|rest| rest.starts_with(['-', '_']))
            })
        {
            return Some((*id).to_owned());
        }
        let language = primary(&tag).to_ascii_lowercase();
        let same: Vec<&str> = ids
            .iter()
            .copied()
            .filter(|id| primary(id).eq_ignore_ascii_case(&language))
            .collect();
        let bundled = same.iter().find(|id| {
            sources
                .iter()
                .any(|(l, s)| l == *id && matches!(s, DictionarySource::Bundled(_)))
        });
        let doubled = format!("{language}_{}", language.to_ascii_uppercase());
        bundled
            .or_else(|| same.iter().find(|id| **id == doubled))
            .or_else(|| same.first())
            .map(|id| (*id).to_owned())
    }

    /// The dictionary of `language` (an id from [`Library::languages`]),
    /// loaded on first use.
    pub fn backend(&self, language: &str) -> Result<Arc<dyn SpellBackend>, DictionaryError> {
        if let Some(done) = self
            .loaded
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(language)
        {
            return done.clone();
        }
        let source = self
            .sources
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .find(|(l, _)| l == language)
            .map(|(_, s)| s.clone());
        // Loaded without the lock held: a large dictionary takes a while.
        let result = match source {
            None => Err(DictionaryError::NotFound(language.to_owned())),
            Some(DictionarySource::Bundled(id)) => BUNDLED
                .iter()
                .find(|b| b.language == id)
                .ok_or_else(|| DictionaryError::NotFound(id.to_owned()))
                .and_then(bundled_backend),
            Some(DictionarySource::Files { aff, dic }) => {
                let read = |p: &Path| {
                    std::fs::read(p)
                        .map_err(|e| DictionaryError::Io(format!("{}: {e}", p.display())))
                };
                read(&aff).and_then(|aff| {
                    let dic = read(&dic)?;
                    load(language, &aff, &dic).map(|b| Arc::new(b) as Arc<dyn SpellBackend>)
                })
            }
        };
        self.loaded
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(language.to_owned(), result.clone());
        result
    }
}

/// The `.aff`/`.dic` pairs in `folder`: language (the file stem), paths.
fn scan(folder: &Path) -> Vec<(String, PathBuf, PathBuf)> {
    let Ok(entries) = std::fs::read_dir(folder) else {
        return Vec::new();
    };
    let mut pairs: Vec<(String, PathBuf, PathBuf)> = entries
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("aff")))
        .filter_map(|aff| {
            let stem = aff.file_stem()?.to_str()?.to_owned();
            let dic = aff.with_extension("dic");
            dic.is_file().then_some((stem, aff, dic))
        })
        .collect();
    pairs.sort();
    pairs
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_labels_pick_the_encoding() {
        let aff = b"# comment\nSET ISO8859-1\nTRY abc\n";
        let dic = b"1\nStra\xDFe\n";
        let (_, dic) = decode_hunspell(aff, dic).expect("decodes");
        assert_eq!(dic, "1\nStra\u{DF}e\n");
        let koi = b"SET KOI8-R\n";
        let (_, dic) = decode_hunspell(koi, b"1\n\xF7\xCF\xC4\xC1\n").expect("decodes");
        assert_eq!(dic, "1\n\u{412}\u{43E}\u{434}\u{430}\n");
        let err = decode_hunspell(b"SET NOSUCH-9\n", b"").expect_err("unknown");
        assert_eq!(err, DictionaryError::UnknownEncoding("NOSUCH-9".to_owned()));
        assert!(err.to_string().contains("NOSUCH-9"));
        // No SET: UTF-8, BOM dropped.
        let (aff, _) = decode_hunspell(b"\xEF\xBB\xBFTRY a\n", b"").expect("decodes");
        assert_eq!(aff, "TRY a\n");
    }

    #[test]
    fn the_bundled_dictionary_loads() {
        let library = Library::new(None);
        assert_eq!(library.languages(), ["en_US"]);
        let en = library.backend("en_US").expect("loads");
        assert!(en.check("color"));
        assert!(!en.check("colour"));
        assert!(en.check("can't"));
        assert!(en.check("can\u{2019}t"));
    }

    #[test]
    fn user_pairs_add_languages_and_tags_resolve() {
        let dir = tempfile::tempdir().expect("temp dir");
        let write = |name: &str, text: &[u8]| std::fs::write(dir.path().join(name), text);
        write("de_DE.aff", b"SET ISO8859-1\n").expect("write");
        write("de_DE.dic", b"1\nStra\xDFe\n").expect("write");
        write("en_GB-ise.aff", b"SET UTF-8\n").expect("write");
        write("en_GB-ise.dic", b"1\ncolour\n").expect("write");
        // An .aff without its .dic is not a dictionary.
        write("fr_FR.aff", b"").expect("write");
        let library = Library::new(Some(dir.path().to_owned()));
        assert_eq!(library.languages(), ["de_DE", "en_GB-ise", "en_US"]);
        assert_eq!(library.resolve("de").as_deref(), Some("de_DE"));
        assert_eq!(library.resolve("en-gb").as_deref(), Some("en_GB-ise"));
        assert_eq!(library.resolve("en").as_deref(), Some("en_US"));
        assert_eq!(library.resolve("EN_us").as_deref(), Some("en_US"));
        assert_eq!(library.resolve("fr"), None);
        let de = library.backend("de_DE").expect("loads");
        assert!(de.check("Stra\u{DF}e"));
        let gb = library.backend("en_GB-ise").expect("loads");
        assert!(gb.check("colour"));
        assert!(matches!(
            library.backend("fr_FR"),
            Err(DictionaryError::NotFound(_))
        ));
    }
}
