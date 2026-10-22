//! What a terminal line offers under the pointer: a page to open, or a file to open.
//!
//! Legacy drew these as links. Hovering a path underlined it, and ctrl+click opened
//! it: a path in the preview, a page in the browser. This is the half that decides
//! *what* a line offers, and it decides it from the text alone -- whether a run of
//! characters names a file, names a page, or is a word in a log.
//!
//! Nothing here asks the disk. A candidate is a candidate: whether the file is
//! there is the checkout's own answer, and it is asked where the checkout is known.
//! Keeping the guess apart from the fact is what lets the first be a pure function
//! of one line of text.
//!
//! The rules are Legacy's, because they are the ones that made a build log
//! readable: a name with a separator or an extension in it is a path wherever it
//! appears, a bare name is a path only on a listing (`ls` prints `LICENSE` with no
//! directory on it), a location is a path without its `:42:10`, and a sentence's
//! punctuation is not part of the name it stands around.

use super::CellSnapshot;

/// What a terminal line offers at one cell.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LinkTarget {
    /// A web address: the browser's to open.
    Url(String),
    /// A path, as the terminal printed it.
    Path(String),
}

/// One grid row as text, with the character each cell begins at.
///
/// A cell is not a character: a wide glyph takes two cells and a combining mark
/// shares one, so the column a click landed on and the character it landed on are
/// two different numbers. This carries both, because a link is a range of
/// characters and the pointer arrives as a column.
pub struct RowText {
    text: Vec<char>,
    starts: Vec<usize>,
}

impl RowText {
    /// The text of one visible row.
    pub fn of(cells: &[CellSnapshot]) -> Self {
        let mut text = Vec::new();
        let mut starts = Vec::with_capacity(cells.len());
        for cell in cells {
            if cell.width == 0 {
                // The second half of a wide glyph holds no character of its own,
                // and a click on it is a click on the glyph.
                starts.push(text.len().saturating_sub(1));
                continue;
            }
            starts.push(text.len());
            text.push(cell.c);
            text.extend(cell.zerowidth.iter().copied());
        }
        // The padding to the right of the last glyph is not part of the line, and
        // a path that ran into it was never one. Trailing spaces only: a space
        // inside a quoted path is part of the name.
        while text.last() == Some(&' ') {
            text.pop();
        }
        Self { text, starts }
    }

    /// What the line offers under the cell at `column`.
    pub fn link_at(&self, column: usize) -> Option<LinkTarget> {
        let index = *self.starts.get(column)?;
        let line: String = self.text.iter().collect();
        candidates(&line)
            .into_iter()
            .find(|candidate| candidate.start <= index && index < candidate.end)
            .map(|candidate| candidate.target)
    }
}

/// One run of the line that names something.
struct Candidate {
    /// The character the name begins at.
    start: usize,
    /// The character just past the name.
    end: usize,
    target: LinkTarget,
}

/// Every run of the line that names a page or a file, in the order the line prints them.
fn candidates(line: &str) -> Vec<Candidate> {
    let chars: Vec<char> = line.chars().collect();
    let mut found = Vec::new();
    for (start, end) in spans(&chars) {
        let Some((body, offset)) = trimmed(&chars[start..end]) else {
            continue;
        };
        let start = start + offset;
        // A page is a page: the address on screen is the address the browser is
        // asked for, and nothing is asked about the disk.
        if let Some(url) = web_url(&chars[start..end], &body) {
            found.push(Candidate {
                start,
                end: start + url.chars().count(),
                target: LinkTarget::Url(url),
            });
            continue;
        }
        let path = without_location(&body);
        if path == "." || path == ".." || !could_name_file(&path, line) {
            continue;
        }
        found.push(Candidate {
            start,
            end: start + path.chars().count(),
            target: LinkTarget::Path(path),
        });
    }
    found.sort_by_key(|candidate| candidate.start);
    found
}

/// The runs of the line a path or a page could be.
///
/// A quoted run is a name even when it has a space in it, so it is read first and
/// the unquoted pass steps over it: a quote is not part of the unquoted rule and so
/// a quoted path would otherwise be found twice.
fn spans(chars: &[char]) -> Vec<(usize, usize)> {
    let mut spans = Vec::new();
    let mut quoted = vec![false; chars.len()];
    let mut index = 0;
    while index < chars.len() {
        let quote = chars[index];
        if quote == '"' || quote == '\'' {
            if let Some(close) = (index + 1..chars.len()).find(|at| chars[*at] == quote) {
                // The name is what is between the quotes.
                spans.push((index + 1, close));
                for mark in quoted.iter_mut().take(close + 1).skip(index) {
                    *mark = true;
                }
                index = close + 1;
                continue;
            }
        }
        index += 1;
    }
    // The unquoted runs: everything that is not whitespace and not a quote.
    let mut start = None;
    for (index, character) in chars.iter().enumerate() {
        let boundary = quoted[index]
            || character.is_whitespace()
            || matches!(character, '"' | '\'' | '`' | '<' | '>');
        if boundary {
            if let Some(begin) = start.take() {
                spans.push((begin, index));
            }
        } else if start.is_none() {
            start = Some(index);
        }
    }
    if let Some(begin) = start {
        spans.push((begin, chars.len()));
    }
    spans.sort_by_key(|(start, _)| *start);
    spans
}

/// The name a run holds, without the punctuation a sentence put around it.
///
/// The offset is how far into the run the name begins: only the leading
/// punctuation is trimmed from the front, and the trailing trimmer shortens the
/// name rather than moving where it begins.
fn trimmed(run: &[char]) -> Option<(String, usize)> {
    let offset = run
        .iter()
        .take_while(|character| matches!(character, '[' | '(' | '<' | '\'' | '"' | '`'))
        .count();
    let body: String = run[offset..].iter().collect();
    let body = body
        .trim_end_matches(|character| {
            matches!(
                character,
                '.' | ',' | ';' | ':' | '!' | '?' | ')' | ']' | '}' | '\'' | '"' | '`'
            )
        })
        .to_owned();
    if body.is_empty() {
        return None;
    }
    Some((body, offset))
}

/// The page a run names, when it names one.
///
/// A parenthesis is legal inside a URL and common in one -- every article with a
/// qualifier in its title carries one -- so trimming it off hands back an address
/// that is not the one on screen, and the browser opens a page that does not
/// exist. A close with nothing open in front of it is the sentence's, and stays
/// trimmed.
fn web_url(run: &[char], body: &str) -> Option<String> {
    let restored = match run.last() {
        Some(')') if has_unclosed_parenthesis(body) => format!("{body})"),
        _ => body.to_owned(),
    };
    let lower = restored.to_ascii_lowercase();
    if !(lower.starts_with("http://") || lower.starts_with("https://")) {
        return None;
    }
    // The opener refuses a control character and a space, so an address holding
    // either is not a page that could be opened.
    if !restored
        .chars()
        .all(|character| !character.is_whitespace() && !character.is_control())
    {
        return None;
    }
    Some(restored)
}

/// Whether a run still holds a `(` that the trailing trimmer closed on its behalf.
fn has_unclosed_parenthesis(body: &str) -> bool {
    let mut open = 0;
    for character in body.chars() {
        if character == '(' {
            open += 1;
        } else if character == ')' {
            open -= 1;
        }
    }
    open > 0
}

/// Whether a name could be a file, judged by its shape and by the line it sits on.
///
/// A name with a separator or an extension in it is one wherever it appears: that
/// is what a compiler prints and what a path-shaped thing in a log always looks
/// like.
///
/// A bare name is a different question, and the answer belongs to the line rather
/// than to the word. `ls` prints `LICENSE` and `Makefile` -- half a listing carries
/// neither a separator nor an extension -- but a build log is mostly prose, and
/// every word of it would cost a lookup that finds nothing. A listing is told
/// apart from a sentence by what separates its parts: names stand alone on their
/// line under `ls -1`, and `ls` sets them in columns more than one space apart, and
/// a sentence does neither.
fn could_name_file(name: &str, line: &str) -> bool {
    let length = name.chars().count();
    if !(3..=4096).contains(&length) {
        return false;
    }
    // A URL is a link, not a file: `https://host/a.ts` names nothing on this disk.
    if has_scheme(name) {
        return false;
    }
    // An argument is not a name, however the line is set: `--verbose` and `-la` are
    // what a command was given rather than what it printed.
    if name.starts_with('-') {
        return false;
    }
    // Nothing but punctuation names nothing at all: `=>`, `1/7]`, `...`.
    if !name.chars().any(char::is_alphanumeric) {
        return false;
    }
    let last = name.rsplit('/').next().unwrap_or(name);
    name.contains('/') || last.contains('.') || is_listing(line)
}

/// Whether a name carries a scheme, which is what makes it something else.
fn has_scheme(name: &str) -> bool {
    let Some(colon) = name.find(':') else {
        return false;
    };
    let Some(scheme) = name.get(..colon) else {
        return false;
    };
    let mut characters = scheme.chars();
    match characters.next() {
        Some(first) if first.is_ascii_alphabetic() => {}
        _ => return false,
    }
    if !characters
        .all(|character| character.is_ascii_alphanumeric() || matches!(character, '+' | '.' | '-'))
    {
        return false;
    }
    name[colon..].starts_with("://")
}

/// Whether a line is a listing rather than a sentence.
///
/// Both halves of what `ls` prints land here: one name on a line of its own, and a
/// row of columns set more than one space apart, which is how a terminal aligns
/// them and how no sentence does it.
fn is_listing(line: &str) -> bool {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return false;
    }
    !trimmed.chars().any(char::is_whitespace) || has_column_gap(trimmed)
}

/// Whether the line sets two things more than one space apart.
fn has_column_gap(line: &str) -> bool {
    let characters: Vec<char> = line.chars().collect();
    let mut index = 0;
    while index < characters.len() {
        if characters[index] != ' ' {
            index += 1;
            continue;
        }
        let start = index;
        while index < characters.len() && characters[index] == ' ' {
            index += 1;
        }
        if index - start >= 2 && start > 0 && index < characters.len() {
            return true;
        }
    }
    false
}

/// The file a name is about, without the place in it that a location carries.
///
/// `foo.rs:42:10` is a location, and the file is `foo.rs`.
fn without_location(name: &str) -> String {
    let Some(colon) = name.rfind(':') else {
        return name.to_owned();
    };
    let tail = &name[colon + 1..];
    if tail.is_empty() || !tail.chars().all(|character| character.is_ascii_digit()) {
        return name.to_owned();
    }
    let head = &name[..colon];
    // A second place in the same file: `foo.rs:42:10`.
    match head.rfind(':') {
        Some(colon) => {
            let middle = &head[colon + 1..];
            if !middle.is_empty() && middle.chars().all(|character| character.is_ascii_digit()) {
                head[..colon].to_owned()
            } else {
                head.to_owned()
            }
        }
        None => head.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The text of one row, as the grid would hold it.
    fn row(text: &str) -> Vec<CellSnapshot> {
        text.chars()
            .map(|character| CellSnapshot {
                c: character,
                zerowidth: Vec::new(),
                width: 1,
                fg: super::super::ColorSnapshot::Default,
                bg: super::super::ColorSnapshot::Default,
                bold: false,
                dim: false,
                italic: false,
                underline: false,
                strikethrough: false,
                inverse: false,
            })
            .collect()
    }

    /// What the line offers at the character under the pointer.
    fn link_at(line: &str, column: usize) -> Option<LinkTarget> {
        RowText::of(&row(line)).link_at(column)
    }

    #[test]
    fn a_page_is_offered_where_the_address_is() {
        let line = "curl https://www.example.com/ now";
        assert_eq!(
            link_at(line, 6),
            Some(LinkTarget::Url("https://www.example.com/".into()))
        );
        // The space around it is not part of the address.
        assert_eq!(link_at(line, 4), None);
        assert_eq!(link_at(line, 29), None);
    }

    #[test]
    fn a_path_is_offered_where_the_name_is() {
        let line = "error in src/lib/foo.ts:42:10";
        assert_eq!(
            link_at(line, 10),
            Some(LinkTarget::Path("src/lib/foo.ts".into()))
        );
        // The place in the file is not part of the name.
        assert_eq!(link_at(line, 23), None);
    }

    #[test]
    fn a_sentence_is_not_a_listing_of_files() {
        // A word in a log is not a path: a lookup for it finds nothing, and a
        // build log is mostly prose.
        assert_eq!(link_at("the build failed", 4), None);
        assert_eq!(
            link_at("could not read src/lib/missing.ts", 15),
            Some(LinkTarget::Path("src/lib/missing.ts".into()))
        );
    }

    #[test]
    fn a_listing_offers_its_bare_names() {
        // `ls` prints `LICENSE` with no directory on it, and it names the file
        // where the shell is sitting.
        assert_eq!(
            link_at("LICENSE", 2),
            Some(LinkTarget::Path("LICENSE".into()))
        );
        assert_eq!(
            link_at("LICENSE   README.md", 12),
            Some(LinkTarget::Path("README.md".into()))
        );
    }

    #[test]
    fn a_sentences_punctuation_is_not_part_of_the_name() {
        assert_eq!(
            link_at("see src/lib/foo.rs, then stop", 6),
            Some(LinkTarget::Path("src/lib/foo.rs".into()))
        );
        assert_eq!(
            link_at("(src/lib/foo.rs)", 2),
            Some(LinkTarget::Path("src/lib/foo.rs".into()))
        );
    }

    #[test]
    fn a_quoted_name_is_a_name_even_with_a_space_in_it() {
        assert_eq!(
            link_at("could not read \"my notes.txt\"", 16),
            Some(LinkTarget::Path("my notes.txt".into()))
        );
    }

    #[test]
    fn a_scheme_is_a_page_and_not_a_file() {
        // The opener refuses everything outside `http` and `https`, so a token
        // naming an application is not underlined.
        assert_eq!(link_at("file:///etc/passwd", 6), None);
        assert_eq!(link_at("vscode://file/tmp/x", 9), None);
    }

    #[test]
    fn an_argument_is_not_a_name() {
        assert_eq!(link_at("cargo build --verbose", 13), None);
    }
}
