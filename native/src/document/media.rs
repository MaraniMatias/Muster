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
//! - a name that resolves inside the checkout is the picture, loaded by GPUI from
//!   the canonical path the same walk that decides what may be opened produced;
//! - anything else — a video, an audio file, a link out of the checkout, a
//!   picture that is not there — is said in words: the alt text the author wrote,
//!   or `Video: assets/demo.mp4` when they wrote none.
//!
//! The words are not a second path. They are the node's own text, which is what
//! the toolkit draws for an inline object it cannot draw: so the line stands in
//! the picture's place while the picture is loading, and stands there for good
//! if it never arrives, and it is what a selection of the reading copies.
use std::path::PathBuf;

use gpui_kit::component::text::{
    markdown_ast::Node, InlineElement, InlineRenderContext, MarkdownNode, MarkdownParseContext,
    MarkdownPlugin,
};
use gpui_kit::{
    img, px, App, ImageSource, InteractiveElement, ObjectFit, Pixels, Styled, StyledImage,
    TestSupportExt, Window,
};

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

/// The reading of one document's pictures: the checkout they may be reached
/// through, and the directory their relative names are written against.
pub(crate) struct DocumentMedia {
    repo: Repo,
    checkout: Checkout,
    /// The document's own checkout-relative directory, empty for a document at
    /// the root of the checkout.
    directory: String,
}

impl DocumentMedia {
    /// The media of the document at checkout-relative `document`.
    pub(crate) fn new(repo: Repo, checkout: Checkout, document: &str) -> Self {
        let directory = document
            .rsplit_once('/')
            .map_or(String::new(), |(directory, _)| directory.to_owned());
        Self {
            repo,
            checkout,
            directory,
        }
    }

    /// The canonical path of the picture a URL names, when it names one that is
    /// inside the checkout.
    ///
    /// Whether it does is `core::files::external_path`'s question, asked through
    /// the same walk that decides what may be opened elsewhere: `..`, an
    /// absolute path, `.git` and a link out of the checkout are all refused
    /// there, and a refused picture is a picture said in words rather than a
    /// path read.
    fn picture(&self, url: &str) -> Option<PathBuf> {
        if kind(url) != Kind::Picture {
            return None;
        }
        let relative = checkout_relative(&self.directory, url)?;
        muster_core::files::external_path(&self.repo, &self.checkout, &relative).ok()
    }
}

impl MarkdownPlugin for DocumentMedia {
    fn name(&self) -> &str {
        NAME
    }

    /// A document's own image node, answered here rather than by the toolkit.
    fn parse(&self, node: &Node, _cx: &MarkdownParseContext<'_>) -> Option<MarkdownNode> {
        let Node::Image(image) = node else {
            return None;
        };
        if !answers(&image.url) {
            return None;
        }
        let label = label(&image.url, &image.alt);
        Some(
            MarkdownNode::new(NAME, self.picture(&image.url))
                .text(label.clone())
                .accessibility_label(label),
        )
    }

    /// The picture, or nothing at all — which is the toolkit's own answer for an
    /// inline object it cannot draw, and is why the line [`parse`] wrote is what
    /// a reader sees until the picture arrives.
    fn render_inline(
        &self,
        node: &MarkdownNode,
        _context: &InlineRenderContext,
        _window: &mut Window,
        _cx: &mut App,
    ) -> Option<InlineElement> {
        let picture = node.data::<Option<PathBuf>>()?.as_ref()?;
        // The source offset, because a document may name the same picture twice
        // and two elements with one id are one element as far as a test is
        // concerned.
        let id = format!(
            "{NAME}-{}",
            node.source_range().map_or(0, |range| range.start)
        );
        Some(InlineElement::new(
            img(ImageSource::from(picture.clone()))
                .object_fit(ObjectFit::Contain)
                .max_w(MAX_IMAGE_EDGE)
                .max_h(MAX_IMAGE_EDGE)
                .id(id)
                .test_support(),
        ))
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
    let name = url
        .split(['?', '#'])
        .next()
        .unwrap_or(url)
        .rsplit('/')
        .next()
        .unwrap_or(url);
    let extension = name
        .rsplit_once('.')
        .map_or_else(String::new, |(_, extension)| extension.to_ascii_lowercase());
    match extension.as_str() {
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "svg" | "bmp" | "ico" | "tif" | "tiff"
        | "pnm" => Kind::Picture,
        "mp4" | "m4v" | "mov" | "mkv" | "webm" | "avi" => Kind::Video,
        "mp3" | "wav" | "m4a" | "ogg" | "oga" | "flac" => Kind::Audio,
        _ => Kind::File,
    }
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
}
