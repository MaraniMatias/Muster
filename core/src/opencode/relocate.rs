//! Where a session is working, and which checkout that work belongs to.
//!
//! A session does not have to be *in* a checkout to be working in it: OpenCode's own
//! directory stays where it was started while the work moves, so the work itself is what
//! has to be read to know where a session now is.
//!
//! Two rules keep that reading honest:
//!
//! * Only **completed** tool work counts. A prompt says what somebody intends to do and
//!   command text says what a shell was told; neither is a fact about the filesystem.
//!   A failed or truncated call did not do what it said it would.
//! * A target is only attributed to a checkout this app has registered, and only when
//!   exactly one of them could be meant. The deepest registered root wins, a nested Git
//!   checkout this app does not know about is never folded into its parent, and two roots
//!   that canonicalize to the same directory are ambiguous rather than one of them.

use std::{
    cmp::Reverse,
    path::{Path, PathBuf},
};

/// The registered checkouts, resolved once per snapshot.
pub struct CheckoutIndex {
    /// Canonical roots, deepest first, so the first match is the deepest one.
    roots: Vec<PathBuf>,
}

impl CheckoutIndex {
    /// The checkouts this snapshot knows, from their roots.
    pub fn new(checkouts: &[(String, PathBuf)]) -> Self {
        let mut roots: Vec<_> = checkouts
            .iter()
            .filter_map(|(_, path)| path.canonicalize().ok())
            .collect();
        roots.sort_by_key(|path| Reverse(path.components().count()));
        Self { roots }
    }

    /// The checkout root `targets` belong to, when exactly one of them can.
    ///
    /// Every target has to resolve to the same root: work in two worktrees is work in
    /// neither, and a session that is in two places at once has no place.
    pub fn work_directory(&self, targets: &[PathBuf]) -> Option<PathBuf> {
        let mut directory = None;
        for target in targets {
            if !target.is_absolute() {
                return None;
            }
            let target = canonical_work_target(target)?;
            let mut candidates = self.roots.iter().filter(|path| target.starts_with(path));
            let best = candidates.next()?;
            if target
                .ancestors()
                .take_while(|path| *path != best)
                .any(|path| path.join(".git").exists())
                || candidates.next() == Some(best)
                || directory.as_ref().is_some_and(|previous| previous != best)
            {
                return None;
            }
            directory = Some(best.clone());
        }
        directory
    }
}

/// The completed work of a session, and when it happened.
///
/// `None` when the session has no attributable work: a session that has not finished
/// anything has not moved.
pub fn latest_work(
    messages: &[serde_json::Value],
    after: i64,
    origin: &Path,
) -> Option<(i64, Vec<PathBuf>)> {
    let mut latest: Option<(i64, Vec<PathBuf>)> = None;
    for tool in messages
        .iter()
        .filter(|message| message["type"] == "assistant")
        .flat_map(|message| message["content"].as_array().into_iter().flatten())
    {
        let completed = tool["time"]["completed"].as_i64().unwrap_or(0);
        if tool["type"] != "tool"
            || tool["state"]["status"] != "completed"
            || tool["state"]["metadata"]["error"] == true
            || tool["state"]["metadata"]["truncated"] == true
            || completed <= after
        {
            continue;
        }
        let mut targets = work_targets(
            tool["name"].as_str().unwrap_or(""),
            &tool["state"]["input"],
            origin,
        );
        if tool["name"] == "execute" {
            for call in tool["state"]["metadata"]["toolCalls"]
                .as_array()
                .into_iter()
                .flatten()
            {
                if call["status"] == "completed" {
                    targets.extend(work_targets(
                        call["tool"].as_str().unwrap_or(""),
                        &call["input"],
                        origin,
                    ));
                }
            }
        }
        if targets.is_empty() {
            continue;
        }
        match &mut latest {
            Some((time, paths)) if *time == completed => paths.extend(targets),
            Some((time, _)) if *time > completed => {}
            _ => latest = Some((completed, targets)),
        }
    }
    latest
}

/// The filesystem a tool call says it touched.
///
/// Only the tools that say where they work are read. A tool whose input carries no path
/// says nothing about where it happened.
fn work_targets(name: &str, input: &serde_json::Value, origin: &Path) -> Vec<PathBuf> {
    let resolve = |value: &str| {
        (!value.is_empty()).then(|| {
            let path = PathBuf::from(value);
            if path.is_absolute() || !matches!(name, "edit" | "write" | "patch") {
                path
            } else {
                origin.join(path)
            }
        })
    };
    let field = match name {
        "shell" => "workdir",
        "lean-ctx.ctx_shell" => "cwd",
        "edit" | "write" | "lean-ctx.ctx_patch" => "path",
        "patch" => {
            return input["patchText"]
                .as_str()
                .into_iter()
                .flat_map(str::lines)
                .filter_map(|line| {
                    [
                        "*** Add File: ",
                        "*** Update File: ",
                        "*** Delete File: ",
                        "*** Move to: ",
                    ]
                    .into_iter()
                    .find_map(|prefix| line.strip_prefix(prefix))
                })
                .filter_map(resolve)
                .collect();
        }
        _ => return Vec::new(),
    };
    let mut paths: Vec<_> = input[field]
        .as_str()
        .and_then(resolve)
        .into_iter()
        .collect();
    if name == "lean-ctx.ctx_patch" {
        if let Some(ops) = input["ops"].as_array() {
            paths.extend(
                ops.iter()
                    .filter_map(|op| op["path"].as_str())
                    .filter_map(resolve),
            );
        }
    }
    paths
}

/// A target as the filesystem sees it.
///
/// A file that was deleted since the call still names where the work was: its parent
/// still exists, and the leaf is the name of the thing that was there.
fn canonical_work_target(target: &Path) -> Option<PathBuf> {
    match target.canonicalize() {
        Ok(target) => Some(target),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let leaf = match target.components().next_back()? {
                std::path::Component::Normal(leaf) => leaf,
                _ => return None,
            };
            Some(target.parent()?.canonicalize().ok()?.join(leaf))
        }
        Err(_) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn the_deepest_registered_root_wins_and_a_nested_one_this_app_does_not_know_blocks_it() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("root");
        let nested = root.join("nested");
        fs::create_dir_all(nested.join(".git")).unwrap();
        fs::write(root.join("file"), "").unwrap();
        fs::write(nested.join("file"), "").unwrap();
        let root = root.canonicalize().unwrap();
        let nested = nested.canonicalize().unwrap();

        let parent_only = CheckoutIndex::new(&[("root".into(), root.clone())]);
        assert_eq!(
            parent_only.work_directory(&[root.join("file")]),
            Some(root.clone())
        );
        assert_eq!(parent_only.work_directory(&[nested.join("file")]), None);
        assert_eq!(
            parent_only.work_directory(&[root.join("deleted")]),
            Some(root.clone())
        );

        let registered = CheckoutIndex::new(&[
            ("root".into(), root.clone()),
            ("nested".into(), nested.clone()),
        ]);
        assert_eq!(
            registered.work_directory(&[nested.join("file")]),
            Some(nested)
        );
    }

    #[test]
    fn a_relative_target_is_refused_and_two_roots_that_are_one_directory_are_ambiguous() {
        let temp = tempfile::tempdir().unwrap();
        let first = temp.path().join("first");
        fs::create_dir(&first).unwrap();
        fs::write(first.join("file"), "").unwrap();
        let first = first.canonicalize().unwrap();

        let index = CheckoutIndex::new(&[("first".into(), first.clone())]);
        assert_eq!(index.work_directory(&[PathBuf::from("relative")]), None);

        let duplicate = CheckoutIndex::new(&[
            ("first".into(), first.clone()),
            ("again".into(), first.clone()),
        ]);
        assert_eq!(duplicate.work_directory(&[first.join("file")]), None);
    }

    #[test]
    fn two_targets_in_two_worktrees_name_neither_of_them() {
        let temp = tempfile::tempdir().unwrap();
        let first = temp.path().join("first");
        let second = temp.path().join("second");
        fs::create_dir_all(&first).unwrap();
        fs::create_dir_all(&second).unwrap();
        fs::write(first.join("file"), "").unwrap();
        fs::write(second.join("file"), "").unwrap();
        let index = CheckoutIndex::new(&[
            ("first".into(), first.canonicalize().unwrap()),
            ("second".into(), second.canonicalize().unwrap()),
        ]);
        assert_eq!(
            index.work_directory(&[
                first.canonicalize().unwrap().join("file"),
                second.canonicalize().unwrap().join("file")
            ]),
            None
        );
    }

    /// Only a call that finished, and that did not fail, says where the work happened.
    #[test]
    fn only_completed_work_places_a_session() {
        let origin = Path::new("/tmp/project");
        let messages = vec![serde_json::json!({
            "type": "assistant",
            "content": [
                {
                    "type": "tool",
                    "name": "shell",
                    "time": { "completed": 100 },
                    "state": {
                        "status": "completed",
                        "input": { "workdir": "/tmp/other" }
                    }
                }
            ]
        })];
        assert_eq!(
            latest_work(&messages, 0, origin),
            Some((100, vec![PathBuf::from("/tmp/other")]))
        );

        // Older than the cursor: already accounted for.
        assert_eq!(latest_work(&messages, 100, origin), None);

        let failed = vec![serde_json::json!({
            "type": "assistant",
            "content": [
                {
                    "type": "tool",
                    "name": "shell",
                    "time": { "completed": 100 },
                    "state": {
                        "status": "error",
                        "input": { "workdir": "/tmp/other" }
                    }
                }
            ]
        })];
        assert_eq!(latest_work(&failed, 0, origin), None);

        // A prompt is not work.
        let prompt = vec![serde_json::json!({
            "type": "user",
            "content": [{ "type": "text", "text": "go to /tmp/other" }]
        })];
        assert_eq!(latest_work(&prompt, 0, origin), None);
    }

    #[test]
    fn relative_editing_tool_paths_are_read_against_the_session_own_directory() {
        let origin = Path::new("/tmp/project");
        for (name, input) in [
            ("edit", serde_json::json!({"path": "src/main.rs"})),
            ("write", serde_json::json!({"path": "src/main.rs"})),
            (
                "patch",
                serde_json::json!({"patchText": "*** Update File: src/main.rs"}),
            ),
        ] {
            let messages = vec![serde_json::json!({
                "type": "assistant", "content": [{
                    "type": "tool", "name": name, "time": {"completed": 5},
                    "state": {"status": "completed", "input": input}
                }]
            })];
            let relative = "src/main.rs";
            assert_eq!(
                latest_work(&messages, 0, origin),
                Some((5, vec![origin.join(relative)])),
                "{name}"
            );
        }
    }

    #[test]
    fn a_relative_edit_path_is_read_against_the_session_own_directory() {
        let origin = Path::new("/tmp/project");
        let messages = vec![serde_json::json!({
            "type": "assistant",
            "content": [
                {
                    "type": "tool",
                    "name": "edit",
                    "time": { "completed": 5 },
                    "state": {
                        "status": "completed",
                        "input": { "path": "src/main.rs" }
                    }
                }
            ]
        })];
        assert_eq!(
            latest_work(&messages, 0, origin),
            Some((5, vec![PathBuf::from("/tmp/project/src/main.rs")]))
        );
    }
}
