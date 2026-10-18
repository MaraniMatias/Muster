//! What a Markdown document asks to be seen, and what is said when it cannot be.
//!
//! A `![alt](assets/diagram.png)` is the one thing the toolkit's own Markdown
//! parser cannot answer: it turns the URL into an image source and leaves it to
//! GPUI, which reads a relative path against the *process* rather than against
//! the document that wrote it. In a checkout that is never the directory the
//! picture is in, so a README's own images arrived as an empty box with nothing
//! where they were.
//!
//! This module claims the image nodes the toolkit is not already better at, and
//! answers each one in one of two ways:
//!
//! - a name that resolves inside the checkout is the picture: read on a worker
//!   through `core::files`, off the descriptor that walk validated, and drawn
//!   from the bytes that came back. The picture is therefore the file that was
//!   checked, not whatever sits at the same name a moment later — which is the
//!   whole point of not handing a validated path to something that reopens it;
//! - anything else — a video, an audio file, a link out of the checkout, a
//!   picture that is not there, one too large to hold, one whose bytes are not a
//!   picture — is said in words: the alt text the author wrote, or
//!   `Video: assets/demo.mp4` when they wrote none.
//!
//! The words are not a second path. They are the node's own text, which is what
//! the toolkit draws for an inline object it cannot draw: so the line stands in
//! the picture's place until the bytes arrive, and stands there for good if they
//! never do or turn out not to be a picture, and it is what a selection of the
//! reading copies.
//!
//! Nothing here reads the filesystem on the thread that draws. [`parse`] only
//! works out the name a URL spells and records that the name is wanted; the read
//! is a worker task started from the frame that owns the reading, and the parse
//! itself is the toolkit's, which is free to run on a worker of its own.
use std::{
    collections::HashMap,
    sync::{Arc, Mutex, MutexGuard, PoisonError, Weak},
};

use gpui_kit::component::text::{
    markdown_ast::Node, InlineElement, InlineRenderContext, MarkdownNode, MarkdownParseContext,
    MarkdownPlugin,
};
use gpui_kit::{
    div, img, px, App, Image, ImageFormat, ImageSource, InteractiveElement, IntoElement, ObjectFit,
    ParentElement, Pixels, SharedString, Styled, StyledImage, TestSupportExt, WeakEntity, Window,
};

use crate::app::TerminalWindow;
use muster_core::workspace::{Checkout, Repo};

/// The name every node this module answers carries, and the prefix of the id the
/// picture it draws is found by.
const NAME: &str = "document-media";

/// The widest and the tallest a picture is drawn, in logical pixels.
///
/// The toolkit measures an inline element against `MaxContent` rather than
/// against the pane it is drawn in, and an element that has been measured cannot
/// be scaled afterwards, so nothing else here would stop a four-thousand-pixel
/// screenshot from running off the side of the reading pane. The picture keeps
/// its own proportions on the way down: an image element derives its missing side
/// from the one this bounds.
const MAX_IMAGE_EDGE: Pixels = px(720.);

/// The air above and below a picture, a video or a file's name.
///
/// The toolkit measures an inline element as a root box, and a root box has no
/// parent to apply a margin for it, so the space has to be inside the box for
/// the words around it to stand off from it. A picture that runs straight into
/// the line above it and the line below it is what this is the answer to.
const MEDIA_GAP: Pixels = px(8.);

/// How much one document may hold in prepared pictures at once.
///
/// `core::files` bounds a single resource on its way in; this bounds what stays
/// resident, because a document can name as many pictures as it likes and every
/// one of them is kept for as long as it is on screen. Four resources' worth is
/// enough for the diagrams a document is actually about, and it stops the
/// retained set from growing with the length of the file. Past it a picture is
/// said in words like any other one that cannot be shown.
const MAX_DOCUMENT_MEDIA_BYTES: usize = 4 * muster_core::files::MAX_RESOURCE_BYTES;

/// How many reads one document may have open at once.
///
/// The retained bound above is what a document *keeps*; this is what it may be
/// holding at once while it reads. A document of one mebibyte can name hundreds
/// of thousands of pictures, so draining the whole list into one frame started a
/// read for each and let as many as completed between two passes of the main
/// loop sit in memory at the same time — nothing the retained share counted.
///
/// The same number as that share, deliberately: a document never has more
/// encoded bytes on the way to it than it is allowed to hold afterwards, so what
/// is being read is bounded by `MAX_DOCUMENT_MEDIA_READS *
/// MAX_RESOURCE_BYTES` — the same 64 MiB. Slots are held by reads rather than by
/// pictures, and are given back by both answers, so the pictures a document is
/// waiting on are read in the order frames come, and a name that is not there
/// never holds up the ones behind it.
const MAX_DOCUMENT_MEDIA_READS: usize = 4;

/// The pictures one document has named, and what has come back for them.
///
/// Owned by the document view, not by the window: a different document, or
/// closing this one, drops the bytes with it. A read still in flight holds this
/// weakly, so a picture that arrives after its document is gone updates nothing
/// and keeps nothing alive.
pub(crate) struct MediaStore(Mutex<Pictures>);

#[derive(Default)]
struct Pictures {
    /// What each name has reached, by checkout-relative name.
    by_name: HashMap<String, What>,
    /// What the pictures held here are worth together.
    bytes: usize,
    /// How many reads this document has started. A name is asked once, so a
    /// document that is drawn over and over over one picture is one number.
    reads: usize,
    /// How many of those reads have not come back yet. What a document is
    /// holding right now, rather than what it keeps: a read in flight owns its
    /// own buffer whether or not the picture it is fetching is ever kept.
    in_flight: usize,
}

/// What one name has reached.
enum What {
    /// A parse named it and no read has been started for it yet.
    Wanted,
    /// A read has been started for it. What it brought back is the picture, or
    /// there is none: a name that is not a picture, a read that was refused, a
    /// document with no room left for it. Not asked again — a second read would
    /// be a second answer to a question that has already been answered, and a
    /// document that links to a picture that is not there must not ask for it on
    /// every frame.
    Asked(Option<Arc<Image>>),
}

impl MediaStore {
    /// The store one document's pictures live in.
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self(Mutex::default()))
    }

    fn lock(&self) -> MutexGuard<'_, Pictures> {
        // A panic while holding this loses what was being written down, not the
        // document: the names are asked again by the next parse.
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Record that this document names that picture, once per name.
    fn want(&self, name: &str) {
        self.lock()
            .by_name
            .entry(name.to_owned())
            .or_insert(What::Wanted);
    }

    /// The names a parse has asked for that no read has been started for, and
    /// that there is a slot to start one in.
    ///
    /// Each name leaves `Wanted` as it is taken, so a name that is read once is
    /// read once: a document is drawn over and over, and every one of those
    /// frames asks this question. What a frame may take is also what this
    /// document has not already got open — a name that is left `Wanted` costs
    /// one entry in `by_name` and nothing else, and the frame after a read comes
    /// back asks again and takes what the slot that read freed.
    fn take_wanted(&self) -> Vec<String> {
        let mut pictures = self.lock();
        let room = MAX_DOCUMENT_MEDIA_READS.saturating_sub(pictures.in_flight);
        let mut wanted = Vec::new();
        for (name, what) in &mut pictures.by_name {
            if wanted.len() == room {
                break;
            }
            let What::Wanted = what else {
                continue;
            };
            *what = What::Asked(None);
            wanted.push(name.clone());
        }
        pictures.reads += wanted.len();
        pictures.in_flight += wanted.len();
        wanted
    }

    /// How many reads this document has started.
    ///
    /// The number a regression looks at to say that drawing a document again does
    /// not read it again.
    #[cfg(test)]
    pub(crate) fn reads(&self) -> usize {
        self.lock().reads
    }

    /// The picture a name has, or nothing while it is still on its way.
    pub(crate) fn image(&self, name: &str) -> Option<Arc<Image>> {
        match self.lock().by_name.get(name) {
            Some(What::Asked(Some(image))) => Some(image.clone()),
            _ => None,
        }
    }

    /// What a read came back with, and the slot it was holding.
    ///
    /// Both answers come through here, because the slot belongs to the read: a
    /// name that is not a picture, a file that is not there and a document with
    /// no room left all leave the slot free for the pictures behind them, and
    /// the one that brings bytes holds them within the document's share.
    fn finish(&self, name: &str, result: Result<Arc<Image>, String>) {
        let mut pictures = self.lock();
        pictures.in_flight = pictures.in_flight.saturating_sub(1);
        let Ok(image) = result else {
            return;
        };
        if pictures.bytes + image.bytes.len() > MAX_DOCUMENT_MEDIA_BYTES {
            log::warn!(
                "{name} would take this document past the {} MiB it holds in pictures, so it is said in words",
                MAX_DOCUMENT_MEDIA_BYTES / (1024 * 1024)
            );
            pictures.by_name.insert(name.to_owned(), What::Asked(None));
            return;
        }
        pictures.bytes += image.bytes.len();
        pictures
            .by_name
            .insert(name.to_owned(), What::Asked(Some(image)));
    }
}

/// The reading of one document's pictures: the checkout they may be reached
/// through, the directory their relative names are written against, and the
/// store the bytes arrive in.
///
/// The store is borrowed, not owned: the document owns it, and the plugin
/// reaches it through the node it answers, so nothing that outlives the
/// document — this plugin, a node a parse left behind, a read still in flight —
/// can keep the bytes of a picture nobody is showing.
pub(crate) struct DocumentMedia {
    repo: Repo,
    checkout: Checkout,
    /// The document's own checkout-relative directory, empty for a document at
    /// the root of the checkout.
    directory: String,
    store: Weak<MediaStore>,
}

impl DocumentMedia {
    /// The media of the document at checkout-relative `document`.
    pub(crate) fn new(
        repo: Repo,
        checkout: Checkout,
        document: &str,
        store: Weak<MediaStore>,
    ) -> Self {
        let directory = document
            .rsplit_once('/')
            .map_or(String::new(), |(directory, _)| directory.to_owned());
        Self {
            repo,
            checkout,
            directory,
            store,
        }
    }

    /// The checkout-relative name of the picture a URL names, or nothing when it
    /// names no picture inside the checkout.
    ///
    /// Purely a question about the name: `..` walked here rather than handed on,
    /// a scheme and a backslash refused, and a name that climbs past the root of
    /// the checkout refused. Whether there is a file there, and what is in it, is
    /// asked of a worker by [`DocumentMedia::prepare`].
    fn picture(&self, url: &str) -> Option<String> {
        if kind(url) != Kind::Picture {
            return None;
        }
        checkout_relative(&self.directory, url)
    }

    /// Start a read for every picture the last parse asked for.
    ///
    /// Drained here, from the frame that owns the reading, rather than from
    /// [`parse`]: the parse may be running on a worker and has no application to
    /// start work with, and the names it produces are only known once it has
    /// run. A name is asked once, so re-rendering a document, scrolling it or
    /// drawing it again starts no further reads.
    pub(crate) fn prepare(&self, view: WeakEntity<TerminalWindow>, cx: &mut App) {
        let Some(store) = self.store.upgrade() else {
            return;
        };
        let wanted = store.take_wanted();
        if wanted.is_empty() {
            return;
        }
        let store = Arc::downgrade(&store);
        for name in wanted {
            let reading = name.clone();
            let worker = cx.background_executor().spawn({
                let (repo, checkout) = (self.repo.clone(), self.checkout.clone());
                async move { picture(&repo, &checkout, &reading) }
            });
            let (store, view) = (store.clone(), view.clone());
            cx.spawn(async move |cx| {
                let result = worker.await;
                // Nothing to publish into means the document this picture was
                // read for has gone: its store went with it, so a read that
                // arrives late keeps nothing alive and asks for no frame.
                let Some(store) = store.upgrade() else {
                    return;
                };
                store.finish(&name, result);
                let _ = view.update(cx, |_, cx| cx.notify());
            })
            .detach();
        }
    }
}

/// What one picture is, read off the checkout on a worker.
///
/// The bytes come back, never a name: a path handed on would be re-opened by
/// whatever drew it, and the walk that validated it has stopped vouching for it
/// the moment it returns.
fn picture(repo: &Repo, checkout: &Checkout, name: &str) -> Result<Arc<Image>, String> {
    let bytes = muster_core::files::read_resource(repo, checkout, name)?;
    let Some(format) = picture_format(name, &bytes) else {
        return Err(format!(
            "{name} does not hold a picture this window can draw"
        ));
    };
    Ok(Arc::new(Image::from_bytes(format, bytes)))
}

/// The format the toolkit should decode these bytes as, or nothing when they are
/// not a picture it draws.
///
/// The same formats [`Kind::Picture`] names, decided by the header rather than
/// by the extension, because the toolkit decodes by the format it is handed and
/// a name is not a format: a `.png` holding a JPEG is a JPEG. SVG is the one
/// taken by name — it is markup, and there is no fixed first byte to look for.
fn picture_format(name: &str, bytes: &[u8]) -> Option<ImageFormat> {
    match bytes {
        [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, ..] => Some(ImageFormat::Png),
        [0xff, 0xd8, 0xff, ..] => Some(ImageFormat::Jpeg),
        [b'G', b'I', b'F', b'8', b'7' | b'9', b'a', ..] => Some(ImageFormat::Gif),
        // `RIFF`, the four-byte size, then the form at its fixed offset 8.
        [b'R', b'I', b'F', b'F', _, _, _, _, b'W', b'E', b'B', b'P', ..] => Some(ImageFormat::Webp),
        [b'B', b'M', ..] => Some(ImageFormat::Bmp),
        [0x00, 0x00, 0x01, 0x00, ..] => Some(ImageFormat::Ico),
        [b'I', b'I', 0x2a, 0x00, ..] | [b'M', b'M', 0x00, 0x2a, ..] => Some(ImageFormat::Tiff),
        [b'P', b'1'..=b'6', ..] => Some(ImageFormat::Pnm),
        _ if extension(name).as_deref() == Some("svg") => Some(ImageFormat::Svg),
        _ => None,
    }
}

/// What a parse leaves in a node: the name the picture has inside the checkout,
/// and where its bytes arrive.
///
/// The store is held weakly on purpose: the document owns it, so a node that
/// outlives the document — the toolkit releases a text view's state in its own
/// time — cannot keep the bytes of a picture nobody is showing.
struct LocalPicture {
    /// Nothing when the URL names no picture inside this checkout: a video, an
    /// audio file, a name that reaches out of it, a URL that names nothing.
    name: Option<String>,
    store: Weak<MediaStore>,
}

impl MarkdownPlugin for DocumentMedia {
    fn name(&self) -> &str {
        NAME
    }

    /// A document's own image node, answered here rather than by the toolkit.
    ///
    /// Names the picture and nothing else: no path is resolved against the disk
    /// here, because this runs wherever the toolkit chose to parse — on the UI
    /// thread for a document small enough to parse in one go, and on a worker
    /// for one that is not.
    fn parse(&self, node: &Node, _cx: &MarkdownParseContext<'_>) -> Option<MarkdownNode> {
        let Node::Image(image) = node else {
            return None;
        };
        if !answers(&image.url) {
            return None;
        }
        let label = label(&image.url, &image.alt);
        let name = self.picture(&image.url);
        if let (Some(store), Some(name)) = (self.store.upgrade(), name.as_deref()) {
            store.want(name);
        }
        Some(
            MarkdownNode::new(
                NAME,
                LocalPicture {
                    name,
                    store: self.store.clone(),
                },
            )
            .text(label.clone())
            .accessibility_label(label),
        )
    }

    /// The picture, or nothing at all — which is the toolkit's own answer for an
    /// The picture, or the words that stand in for one.
    ///
    /// Both answers are one box with the same air around it, so a video and a
    /// file's name stand where a picture stands and neither crowds the text
    /// beside it.
    ///
    /// The element draws the line too, for the one failure the bytes can
    /// still fail at: a file whose header says it is a picture and whose rest
    /// is not one decodes to nothing, and a blank box with the alt text nowhere
    /// in sight is a worse answer than the words.
    fn render_inline(
        &self,
        node: &MarkdownNode,
        _context: &InlineRenderContext,
        _window: &mut Window,
        _cx: &mut App,
    ) -> Option<InlineElement> {
        let picture = node.data::<LocalPicture>()?;
        // The source offset, because a document may name the same picture twice
        // and two elements with one id are one element as far as a test is
        // concerned.
        let id = format!(
            "{NAME}-{}",
            node.source_range().map_or(0, |range| range.start)
        );
        let said: SharedString = node.as_text().into();
        // The source offset again, so the line a picture that will not decode
        // falls back to is as findable as the picture it stands in for.
        let said_id = format!(
            "{NAME}-said-{}",
            node.source_range().map_or(0, |range| range.start)
        );
        // A name that is not a picture has no image to look up and says so by
        // being absent, rather than by answering nothing at all: the words are
        // drawn here too, so they stand where a picture stands.
        let image = picture
            .store
            .upgrade()
            .zip(picture.name.as_deref())
            .and_then(|(store, name)| store.image(name));
        let body = match image {
            // The bytes themselves, which the toolkit decodes on its own worker
            // through its asset system. A pathname here would be reopened by the
            // image loader, which is the check/use boundary this exists to
            // remove.
            //
            // This box carries the picture's own id and the words carry the
            // other one, so the id a document's offset names is there exactly
            // when the bytes are: a name that was refused, or has not come back
            // yet, is the words and nothing else.
            Some(image) => div()
                .id(id)
                .test_support()
                .py(MEDIA_GAP)
                .child(
                    img(ImageSource::Image(image))
                        .object_fit(ObjectFit::Contain)
                        .max_w(MAX_IMAGE_EDGE)
                        .max_h(MAX_IMAGE_EDGE)
                        .with_fallback(move || {
                            div()
                                .id(said_id.clone())
                                .test_support()
                                .child(said.clone())
                                .into_any_element()
                        }),
                )
                .into_any_element(),
            // Nothing to draw: the words are the drawing, which is what the
            // toolkit's own answer for an inline object it cannot draw says.
            None => div()
                .id(said_id)
                .test_support()
                .py(MEDIA_GAP)
                .child(said)
                .into_any_element(),
        };
        Some(InlineElement::new(body))
    }
}

/// What a URL's own name says it is.
///
/// The extension is the only thing a document gives to go on, and it is enough
/// twice over: it decides whether there is a picture to draw at all, and it
/// names the line that stands in for one when there is not.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Picture,
    Video,
    Audio,
    File,
}

impl Kind {
    /// The word the line names this with.
    fn as_text(self) -> &'static str {
        match self {
            Kind::Picture => "Image",
            Kind::Video => "Video",
            Kind::Audio => "Audio",
            Kind::File => "File",
        }
    }
}

fn kind(url: &str) -> Kind {
    match extension(url).as_deref().unwrap_or_default() {
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "svg" | "bmp" | "ico" | "tif" | "tiff"
        | "pnm" => Kind::Picture,
        "mp4" | "m4v" | "mov" | "mkv" | "webm" | "avi" => Kind::Video,
        "mp3" | "wav" | "m4a" | "ogg" | "oga" | "flac" => Kind::Audio,
        _ => Kind::File,
    }
}

/// The lowercased extension a URL's own name carries, without the dot.
///
/// The query and the fragment are not part of a name, and neither is anything
/// before the last `/`.
fn extension(url: &str) -> Option<String> {
    let name = url.split(['?', '#']).next()?.rsplit('/').next()?;
    name.rsplit_once('.')
        .map(|(_, extension)| extension.to_ascii_lowercase())
}

/// Whether this module answers a URL the toolkit is not already better at.
///
/// Two answers are left to the toolkit: a picture written into the document as a
/// `data:` URL, which it decodes where it stands, and a picture on the network,
/// which it fetches. A video on the network is not a picture, and is answered
/// here rather than offered to a decoder that cannot draw one.
fn answers(url: &str) -> bool {
    match scheme(url).as_deref() {
        Some("data") => !url.starts_with("data:image/"),
        Some("http") | Some("https") => matches!(kind(url), Kind::Video | Kind::Audio),
        Some(_) | None => true,
    }
}

/// The scheme a URL names, lowercased, or nothing when it names none.
///
/// A scheme only, never the first segment of a path: `assets/a:b.png` is a name,
/// and a Windows drive letter is a name this refuses rather than reaches.
fn scheme(url: &str) -> Option<String> {
    let (scheme, _) = url.split_once(':')?;
    (!scheme.is_empty() && !scheme.contains('/')).then(|| scheme.to_ascii_lowercase())
}

/// The line to draw in place of a picture that cannot be drawn.
///
/// The alt text is the author's own sentence for it and is used whenever there is
/// one; otherwise the kind and the URL say what the document put there, so a
/// video or a picture from another machine is never a blank line.
fn label(url: &str, alt: &str) -> String {
    let alt = alt.trim();
    if alt.is_empty() {
        format!("{}: {url}", kind(url).as_text())
    } else {
        alt.to_owned()
    }
}

/// The checkout-relative path a URL in a document spells, or nothing when it
/// spells none.
///
/// A relative URL is written against the document's own directory and its `..` is
/// walked here rather than handed on: `core::files` refuses a path carrying one,
/// which is the right answer for a name a person typed into a tree and the wrong
/// one for a document that may legitimately reach a sibling directory. A name
/// that walks past the root of the checkout is no name at all.
fn checkout_relative(directory: &str, url: &str) -> Option<String> {
    // The query and the fragment of a URL are not part of a file's name.
    let url = url.split(['?', '#']).next()?;
    // A scheme is not a path into a checkout: `file:///etc/passwd` and
    // `javascript:alert(1)` are both names this walk cannot have.
    if scheme(url).is_some() {
        return None;
    }
    // A backslash is not a separator in a URL, and Legacy refused one here too.
    if url.contains('\\') {
        return None;
    }
    let name = percent_decode(url)?;
    if name.is_empty() || name.starts_with('/') {
        return None;
    }
    let mut parts: Vec<&str> = directory
        .split('/')
        .filter(|part| !part.is_empty())
        .collect();
    for part in name.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            _ => parts.push(part),
        }
    }
    (!parts.is_empty()).then(|| parts.join("/"))
}

/// The percent-escapes in a URL decoded, or nothing when it is not a name.
///
/// A document spells a file the way a browser would, so a name with a space in
/// it arrives as `%20`. An escape that is not two hex digits is a name this
/// refuses rather than guesses at.
fn percent_decode(value: &str) -> Option<String> {
    if !value.contains('%') {
        return Some(value.to_owned());
    }
    let bytes = value.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut at = 0;
    while at < bytes.len() {
        if bytes[at] == b'%' {
            decoded.push(u8::from_str_radix(value.get(at + 1..at + 3)?, 16).ok()?);
            at += 3;
        } else {
            decoded.push(bytes[at]);
            at += 1;
        }
    }
    String::from_utf8(decoded).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_name_is_written_against_the_documents_own_directory() {
        for (directory, url, expected) in [
            ("", "assets/diagram.png", "assets/diagram.png"),
            ("", "./diagram.png", "diagram.png"),
            ("docs", "diagram.png", "docs/diagram.png"),
            (
                "docs/guide",
                "../assets/diagram.png",
                "docs/assets/diagram.png",
            ),
            (
                "docs",
                "./assets/../diagram.png?raw=1#top",
                "docs/diagram.png",
            ),
            ("docs", "my%20diagram.png", "docs/my diagram.png"),
            ("docs/café", "diagram.png", "docs/café/diagram.png"),
        ] {
            assert_eq!(
                checkout_relative(directory, url).as_deref(),
                Some(expected),
                "{directory} + {url}"
            );
        }
    }

    #[test]
    fn a_name_that_reaches_out_of_the_checkout_is_no_name() {
        for (directory, url) in [
            ("", "/etc/passwd"),
            ("", "../outside.png"),
            ("docs", "../../outside.png"),
            // A Windows spelling is not a URL, and a scheme is not a path.
            ("", r"C:\Windows\win.ini"),
            ("", r"assets\diagram.png"),
            ("", "file:///etc/passwd"),
            ("", ""),
            // An escape this does not read is a name it will not invent.
            ("", "diagram%zz.png"),
            ("", "diagram%2.png"),
        ] {
            assert_eq!(
                checkout_relative(directory, url),
                None,
                "{directory} + {url} is not a name in the checkout"
            );
        }
    }

    #[test]
    fn the_alt_text_is_the_line_when_there_is_one() {
        assert_eq!(label("assets/demo.mp4", "The demo"), "The demo");
        assert_eq!(label("assets/demo.mp4", "  "), "Video: assets/demo.mp4");
        assert_eq!(
            label("docs/notes.pdf", ""),
            "File: docs/notes.pdf",
            "a document that names no picture still says what it named"
        );
        assert_eq!(
            label("../outside.png", ""),
            "Image: ../outside.png",
            "a picture that reaches out of the checkout is still a picture"
        );
        assert_eq!(label("assets/clip.MP4", ""), "Video: assets/clip.MP4");
        assert_eq!(label("assets/logo.svg", ""), "Image: assets/logo.svg");
    }

    /// The two answers the toolkit keeps, because it draws both and this cannot.
    #[test]
    fn a_picture_the_toolkit_draws_is_left_to_the_toolkit() {
        for url in [
            "data:image/png;base64,iVBORw0KGgo=",
            "https://example.com/logo.png",
            "HTTPS://example.com/photo.JPEG",
        ] {
            assert!(!answers(url), "{url} is the toolkit's own picture");
        }
        // A video is not a picture wherever it lives, and neither is anything a
        // document has no business reaching through.
        for url in [
            "https://example.com/demo.mp4",
            "http://example.com/talk.mkv",
            "data:video/mp4;base64,AAAA",
            "file:///etc/passwd",
            "javascript:alert(1)",
            "assets/demo.mp4",
            "diagram.png",
            "/etc/passwd",
            "assets/a:b.png",
        ] {
            assert!(answers(url), "{url} is answered in words");
        }
    }

    /// What the toolkit is handed is bytes with the format those bytes say they
    /// are, so a name that lies does not decide what is drawn.
    #[test]
    fn the_bytes_decide_the_format_not_the_name() {
        let png = [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, 0, 0];
        let jpeg = [0xff, 0xd8, 0xff, 0xe0, 0, 0];
        let riff = [
            b'R', b'I', b'F', b'F', 4, 0, 0, 0, b'W', b'E', b'B', b'P', 0, 0,
        ];
        let svg = b"<svg xmlns=\"http://www.w3.org/2000/svg\"></svg>";
        assert_eq!(picture_format("a.png", &png), Some(ImageFormat::Png));
        assert_eq!(
            picture_format("a.png", &jpeg),
            Some(ImageFormat::Jpeg),
            "a JPEG named .png is decoded as the JPEG it is"
        );
        assert_eq!(picture_format("logo", &riff), Some(ImageFormat::Webp));
        assert_eq!(
            picture_format("logo.svg", svg),
            Some(ImageFormat::Svg),
            "markup has no fixed header, so the name is all there is"
        );
        for refused in [
            // The same bytes with a name of their own: not a picture.
            ("notes.png", b"".as_slice()),
            (
                "notes.png",
                b"version https://git-lfs.github.com/spec/v1\n".as_slice(),
            ),
            // A truncated header is not a match.
            ("a.png", &png[..4][..]),
            // `WEBP` at the end of anything is not a WebP.
            ("a.webp", &riff[..11][..]),
        ] {
            assert_eq!(
                picture_format(refused.0, refused.1),
                None,
                "{refused:?} is not a picture this window draws"
            );
        }
    }

    /// One document holds so many pictures, and no more: past the share a
    /// picture is refused rather than kept alongside the rest of them.
    #[test]
    fn a_document_holds_a_bounded_number_of_picture_bytes() {
        let store = MediaStore::new();
        // One picture of half the share, held under two names: the budget counts
        // what the document holds, so the second name cannot have it too.
        let half = MAX_DOCUMENT_MEDIA_BYTES / 2 + 1;
        let shared = Arc::new(Image::from_bytes(ImageFormat::Png, vec![0; half]));
        store.finish("first.png", Ok(shared.clone()));
        assert_eq!(
            store.image("first.png").map(|image| image.bytes.len()),
            Some(half)
        );
        store.finish("second.png", Ok(shared));
        assert!(
            store.image("second.png").is_none(),
            "the second picture would take the document past its share"
        );
        store.finish(
            "third.png",
            Ok(Arc::new(Image::from_bytes(ImageFormat::Png, vec![0; 1]))),
        );
        assert_eq!(
            store.image("third.png").map(|image| image.bytes.len()),
            Some(1),
            "a picture the document still has room for is held, refusal or not"
        );
        assert_eq!(
            store.image("first.png").map(|image| image.bytes.len()),
            Some(half),
            "the pictures already held are untouched by a refusal"
        );
    }

    /// A document that names more pictures than it has slots reads them a few
    /// at a time, and reads every one of them once.
    ///
    /// The list is drained a frame at a time rather than all at once, because a
    /// document of one mebibyte can name more pictures than any bound on bytes
    /// can account for while they are still being read. What has to stay true is
    /// that a frame never starts more reads than the document has slots, that a
    /// slot is given back by a picture *and* by a refusal, and that a name left
    /// waiting is still asked.
    #[test]
    fn a_document_reads_only_as_many_pictures_as_it_has_slots_open() {
        const NAMED: usize = MAX_DOCUMENT_MEDIA_READS * 5;
        let store = MediaStore::new();
        for index in 0..NAMED {
            store.want(&format!("picture-{index}.png"));
        }

        let mut started: Vec<String> = Vec::new();
        let mut open: Vec<String> = Vec::new();
        while started.len() < NAMED {
            let round = store.take_wanted();
            assert!(
                round.len() <= MAX_DOCUMENT_MEDIA_READS - open.len(),
                "a frame started {} reads with {} of {} slots already open",
                round.len(),
                open.len(),
                MAX_DOCUMENT_MEDIA_READS
            );
            if open.is_empty() {
                assert_eq!(
                    round.len(),
                    MAX_DOCUMENT_MEDIA_READS,
                    "a document with every slot free fills them all"
                );
            }
            started.extend(round.iter().cloned());
            open.extend(round.iter().cloned());

            // One read comes back per frame: half of them a picture, half of
            // them the refusal a name that is not there gives, which frees the
            // slot exactly as the picture does.
            let name = open.remove(0);
            let attempt = started.iter().position(|one| *one == name).unwrap();
            if attempt % 2 == 0 {
                store.finish(&name, Err(format!("{name} is not there")));
                assert!(
                    store.image(&name).is_none(),
                    "a refused read holds nothing and says so"
                );
            } else {
                store.finish(&name, Ok(picture_of_a_pixel()));
            }
        }

        // The reads the last round started still come back, and give back the
        // last of the slots on the way.
        for name in open {
            store.finish(&name, Err(format!("{name} is not there")));
        }

        let mut asked = started.clone();
        asked.sort();
        let mut named: Vec<String> = (0..NAMED)
            .map(|index| format!("picture-{index}.png"))
            .collect();
        named.sort();
        assert_eq!(asked, named, "every name the document named was asked once");
        assert_eq!(
            started
                .iter()
                .collect::<std::collections::HashSet<_>>()
                .len(),
            started.len(),
            "and none of them was asked twice"
        );
        assert_eq!(store.reads(), NAMED);
        assert!(
            store.take_wanted().is_empty(),
            "nothing is left waiting once every name has been read"
        );
    }

    /// A picture's worth of bytes, small enough that twenty of them say nothing
    /// about the budget the other test holds: what is being checked here is the
    /// slot, not the picture.
    fn picture_of_a_pixel() -> Arc<Image> {
        Arc::new(Image::from_bytes(ImageFormat::Png, vec![0; 1]))
    }
}
