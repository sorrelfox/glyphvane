// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Showing an edit made from the Reading view (a task checkbox written
//! back): the document is built again from the editor's text off the UI
//! thread while the old one stays on screen, then shown where the view
//! was, with the folds of its `<details>` kept.

use std::sync::Arc;

use gpui::{AppContext as _, Context};

use super::links::Resolver;
use super::{LoadStatus, Reader, index_key, load};

impl Reader {
    /// Shows `text`, the shown document after an edit, in place: the view
    /// keeps its document and scroll position until the new build is in.
    pub fn show_edited(&mut self, text: Arc<str>, cx: &mut Context<'_, Self>) {
        let Some(path) = self.path.clone() else {
            return;
        };
        if self.status != LoadStatus::Ready {
            self.open_text(path, text, None, cx);
            return;
        }
        let revision = {
            let mut st = self.state.borrow_mut();
            st.revision += 1;
            st.revision
        };
        self.live.forget();
        let resolver = Resolver::statuses(&path, self.index.as_ref(), self.profile.id);
        self.statuses_from = index_key(self.index.as_ref());
        let options = self.build_options();
        let profile = Arc::clone(&self.profile);
        self._load = Some(cx.spawn(async move |this, cx| {
            let doc = cx
                .background_spawn(async move {
                    load::load_text(&path, text, &resolver, options, &profile)
                })
                .await;
            this.update(cx, |reader, cx| {
                let mut st = reader.state.borrow_mut();
                if st.revision != revision {
                    return;
                }
                // The same blocks before and after a one-character edit:
                // the anchor's item keeps its id.
                let anchor = st.engine.anchor();
                let folds = std::mem::take(&mut st.cache.folds);
                st.install(doc);
                st.cache.folds = folds;
                if let Some(anchor) = anchor
                    && let Some(ix) = st.doc.as_ref().and_then(|d| d.tree.index_of(anchor.id))
                {
                    st.engine.jump_to(ix, anchor.offset);
                }
                drop(st);
                reader._load = None;
                cx.notify();
            })
            .ok();
        }));
    }
}
