//! The pictures a document admits, and when the application may forget them.
//!
//! The toolkit decodes a picture through the application's asset system, and
//! that system keeps what it decoded: an entry for a picture stays in the
//! application's cache until something calls `remove_asset` for it. A window
//! that reads a document, replaces it and reads another would therefore leave
//! a decoded picture behind for every document it ever showed, and nothing in
//! the window would ever say so.
//!
//! So a document admits the pictures it shows, and gives them up when it goes.
//! The claim is what says a picture is still wanted.
//!
//! Two pictures with the same bytes are one entry — the toolkit's cache is
//! keyed by the bytes, not by the file they came from — so a claim is counted
//! rather than owned: the entry is retired when the last claim on it is given
//! up, and not before. Giving up one document's picture therefore never pulls
//! it out from under another document, or another window, that is still
//! showing the same bytes.
use std::collections::HashMap;
use std::sync::Arc;

use gpui_kit::{App, Global, Image};

/// The pictures one document has admitted, and the claims it holds on them.
///
/// The claims are counted because two holders inside one document may hold the
/// same bytes — a drawing and a picture the document names inline — and the
/// entry they share may only be retired when neither of them wants it.
pub(crate) struct DocumentAssets {
    /// The pictures this document has claimed, by the id of their bytes, and
    /// how many claims this document holds on each of them.
    held: HashMap<u64, (usize, Arc<Image>)>,
}

impl DocumentAssets {
    /// The ledger of a document that has admitted nothing yet.
    pub(crate) fn new() -> Self {
        Self {
            held: HashMap::new(),
        }
    }

    /// Admit a picture into this document's lifecycle.
    ///
    /// The claim is what keeps the application's entry for these bytes alive.
    pub(crate) fn admit(&mut self, image: Arc<Image>, cx: &mut App) {
        let id = image.id();
        {
            let claims = cx.default_global::<Claims>();
            *claims.by_picture.entry(id).or_insert(0) += 1;
        }
        self.held
            .entry(id)
            .and_modify(|(count, _)| *count += 1)
            .or_insert((1, image));
    }

    /// Give up one claim on a picture.
    ///
    /// The application's entry goes only when the last claim on these bytes is
    /// given up: another document, or another window, may be showing them.
    pub(crate) fn release(&mut self, id: u64, cx: &mut App) {
        let Some(entry) = self.held.get_mut(&id) else {
            return;
        };
        let remaining = entry.0.saturating_sub(1);
        entry.0 = remaining;
        if !give_up(cx, id, 1) {
            // Somebody else is still showing these bytes.
            return;
        }
        if remaining == 0 {
            if let Some((_, image)) = self.held.remove(&id) {
                image.remove_asset(cx);
            }
        }
    }

    /// Give up every claim this document holds.
    ///
    /// This is what a replaced or closed document does with its pictures: the
    /// decoded ones leave the application's cache, unless another document is
    /// still showing the same bytes.
    pub(crate) fn release_all(&mut self, cx: &mut App) {
        for (id, (count, image)) in std::mem::take(&mut self.held) {
            if give_up(cx, id, count) {
                image.remove_asset(cx);
            }
        }
    }

    /// How many claims this document holds, on how many pictures.
    ///
    /// What a test reads to say that a picture which arrived late left no
    /// claim behind.
    #[cfg(test)]
    pub(crate) fn claims(&self) -> usize {
        self.held.values().map(|(count, _)| *count).sum()
    }
}

/// How many claims are held on each picture the application has decoded.
///
/// The count is the application's own because the cache is the application's:
/// two windows showing the same picture share one entry, and that entry may
/// only be retired when neither of them wants it any more.
#[derive(Default)]
struct Claims {
    by_picture: HashMap<u64, usize>,
}

impl Global for Claims {}

/// Give back `count` claims on a picture, and say whether the last one went.
///
/// The answer is what decides whether the application's entry is retired: an
/// entry another document is still reading stays, and only the last claim
/// takes it away.
fn give_up(cx: &mut App, id: u64, count: usize) -> bool {
    let claims = cx.default_global::<Claims>();
    let held = claims.by_picture.get(&id).copied().unwrap_or(0);
    let remaining = held.saturating_sub(count);
    if remaining == 0 {
        claims.by_picture.remove(&id);
    } else {
        claims.by_picture.insert(id, remaining);
    }
    remaining == 0
}
