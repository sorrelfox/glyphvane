// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Spell check for the whole application: the dictionaries, the user's
//! words and the words ignored this session, as a GPUI global.
//!
//! - **Dictionaries** ([`gv_spell::Library`]): the bundled `en_US`, plus
//!   `.aff`/`.dic` pairs in the `dictionaries` folder of the config dir.
//!   They load on first use, off the UI thread, once per run.
//! - **Words:** *Add to dictionary* writes `dictionaries/words.txt` in the
//!   config dir; *Add to vault dictionary* writes `words.txt` in the
//!   vault's folder of the app data dir (`vaults/<vault id>/`). Both are
//!   plain text, one word per line, and are read again when the vault
//!   changes. Without locations (tests, no home folder) they are kept in
//!   memory. *Ignore* lasts for the session.
//! - **Languages:** the `spelling.languages` setting (global or the
//!   vault's), unless a document's front matter names its own (`lang`).
//!   A word is correct if any of them accepts it. Languages without a
//!   dictionary are left out; with none at all nothing is checked.
//!
//! Every change of the words or the spelling settings bumps
//! [`SpellService::revision`]; editors observe this global and check
//! again (only the blocks whose results depended on what changed are new
//! to the block cache, but a new revision is a new cache key, so they are
//! all checked again, in the background).

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;

use gpui::{App, BorrowAppContext as _, Global};
use gv_spell::{Checker, Library, Rules, WordList, WordSet, document_languages};
use gv_syntax::SyntaxTree;
use gv_text::TextSnapshot;

use crate::settings::AppSettings;

/// The name of a word list file.
const WORDS_FILE: &str = "words.txt";

/// What a background check needs to build a document's checker.
#[derive(Debug, Clone)]
pub(crate) struct SpellSetup {
    pub library: Arc<Library>,
    pub languages: Vec<String>,
    pub words: Vec<Arc<WordSet>>,
    pub rules: Rules,
    pub revision: u64,
}

impl SpellSetup {
    /// The checker of the document `tree` was parsed from: its own
    /// languages, else the setting's. Loads dictionaries: not on the UI
    /// thread.
    pub fn checker(&self, tree: &SyntaxTree, text: &TextSnapshot) -> Checker {
        let tags = document_languages(tree, text).unwrap_or_else(|| self.languages.clone());
        let mut backends = Vec::new();
        let mut seen: Vec<String> = Vec::new();
        for tag in &tags {
            let Some(id) = self.library.resolve(tag) else {
                tracing::debug!("spell check: no dictionary for `{tag}`");
                continue;
            };
            if seen.contains(&id) {
                continue;
            }
            match self.library.backend(&id) {
                Ok(backend) => backends.push(backend),
                Err(err) => tracing::warn!("spell check: {id}: {err}"),
            }
            seen.push(id);
        }
        Checker::new(backends, self.words.clone(), self.rules, self.revision)
    }
}

/// The application's spell-check state.
#[derive(Debug)]
pub struct SpellService {
    library: Arc<Library>,
    user: WordList,
    /// The vault id and its list.
    vault: Option<(String, WordList)>,
    ignored: WordSet,
    /// Snapshots of the three sets above, for checkers.
    sets: Vec<Arc<WordSet>>,
    revision: u64,
    /// The spelling settings last seen.
    seen: Option<gv_settings::Spelling>,
}

/// The global holding the service. Reading it never notifies; changing
/// the words notifies its observers (the editors).
#[derive(Debug, Clone)]
pub struct SpellGlobal(Rc<RefCell<SpellService>>);

impl Global for SpellGlobal {}

fn load_list(path: Option<PathBuf>) -> WordList {
    let Some(path) = path else {
        return WordList::in_memory();
    };
    WordList::load(&path).unwrap_or_else(|err| {
        tracing::warn!("spell check: cannot read {}: {err}", path.display());
        WordList::in_memory()
    })
}

impl SpellService {
    fn new(cx: &App) -> Self {
        let locations = cx
            .try_global::<AppSettings>()
            .and_then(|s| s.locations().cloned());
        let folder = locations.as_ref().map(|l| l.dictionaries_dir());
        let user = load_list(folder.as_ref().map(|f| f.join(WORDS_FILE)));
        let mut service = Self {
            library: Arc::new(Library::new(folder)),
            user,
            vault: None,
            ignored: WordSet::default(),
            sets: Vec::new(),
            revision: 0,
            seen: None,
        };
        service.rebuild_sets();
        service
    }

    /// The service, created on first use.
    pub fn shared(cx: &mut App) -> Rc<RefCell<Self>> {
        if let Some(global) = cx.try_global::<SpellGlobal>() {
            return Rc::clone(&global.0);
        }
        let service = Rc::new(RefCell::new(Self::new(cx)));
        cx.set_global(SpellGlobal(Rc::clone(&service)));
        service
    }

    /// Tells the editors that the words changed.
    fn changed(cx: &mut App) {
        if cx.has_global::<SpellGlobal>() {
            cx.update_global::<SpellGlobal, _>(|_, _| {});
        }
    }

    fn rebuild_sets(&mut self) {
        let vault = self
            .vault
            .as_ref()
            .map(|(_, list)| WordSet::new(list.words()))
            .unwrap_or_default();
        self.sets = vec![
            Arc::new(WordSet::new(self.user.words())),
            Arc::new(vault),
            Arc::new(self.ignored.clone()),
        ];
        self.revision += 1;
    }

    /// Changes with every change of the words, the vault or the settings.
    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// The dictionaries.
    pub fn library(&self) -> &Arc<Library> {
        &self.library
    }

    /// Follows the vault in use and the spelling settings; returns the
    /// setup for a check, or `None` when spell check is off.
    pub(crate) fn setup(cx: &mut App) -> Option<SpellSetup> {
        let (spelling, vault, locations) = {
            let settings = cx.global::<AppSettings>();
            (
                settings.settings().spelling.clone(),
                settings.vault().map(|v| v.id().to_owned()),
                settings.locations().cloned(),
            )
        };
        let shared = Self::shared(cx);
        let mut this = shared.borrow_mut();
        if this.vault.as_ref().map(|(id, _)| id) != vault.as_ref() {
            this.vault = vault.map(|id| {
                let path = locations
                    .as_ref()
                    .and_then(|l| l.vault_data_dir(&id).ok())
                    .map(|d| d.join(WORDS_FILE));
                (id, load_list(path))
            });
            this.rebuild_sets();
        }
        if this.seen.as_ref() != Some(&spelling) {
            this.seen = Some(spelling.clone());
            this.revision += 1;
        }
        spelling.enabled.then(|| SpellSetup {
            library: Arc::clone(&this.library),
            languages: spelling.languages.clone(),
            words: this.sets.clone(),
            rules: Rules {
                skip_words_with_digits: spelling.skip_words_with_digits,
                skip_all_caps: spelling.skip_all_caps,
            },
            revision: this.revision,
        })
    }

    /// Whether a vault (with a dictionary of its own) is in use.
    pub fn in_vault(cx: &App) -> bool {
        cx.try_global::<AppSettings>()
            .is_some_and(|s| s.vault().is_some())
    }

    /// Adds `word` to the user's dictionary (or the vault's) and tells the
    /// editors.
    pub fn add_word(cx: &mut App, word: &str, vault: bool) {
        // Make sure the vault's list is the current vault's.
        let _ = Self::setup(cx);
        let shared = Self::shared(cx);
        {
            let mut this = shared.borrow_mut();
            let this = &mut *this;
            let list = if vault {
                match this.vault.as_mut() {
                    Some((_, list)) => list,
                    None => return,
                }
            } else {
                &mut this.user
            };
            match list.add(word) {
                Ok(true) => this.rebuild_sets(),
                Ok(false) => return,
                Err(err) => {
                    tracing::warn!("spell check: cannot save the word list: {err}");
                    // Accepted for this session all the same.
                    this.ignored.insert(word);
                    this.rebuild_sets();
                }
            }
        }
        Self::changed(cx);
    }

    /// Accepts `word` everywhere for the rest of the session.
    pub fn ignore_word(cx: &mut App, word: &str) {
        let shared = Self::shared(cx);
        let inserted = {
            let mut this = shared.borrow_mut();
            let inserted = this.ignored.insert(word);
            if inserted {
                this.rebuild_sets();
            }
            inserted
        };
        if inserted {
            Self::changed(cx);
        }
    }

    /// The user's words, the vault's and those ignored, for tests and the
    /// settings window.
    pub fn words(&self) -> (Vec<String>, Vec<String>, usize) {
        (
            self.user.words().map(str::to_owned).collect(),
            self.vault
                .as_ref()
                .map(|(_, l)| l.words().map(str::to_owned).collect())
                .unwrap_or_default(),
            self.ignored.len(),
        )
    }
}
