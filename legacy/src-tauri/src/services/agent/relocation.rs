use std::{
    cmp::Reverse,
    path::{Path, PathBuf},
};

/// Canonical checkout roots are resolved once per relocation snapshot, then reused for all targets.
pub(super) struct CheckoutIndex {
    roots: Vec<PathBuf>,
}

impl CheckoutIndex {
    pub(super) fn new(checkouts: &[(String, PathBuf)]) -> Self {
        let mut roots: Vec<_> = checkouts
            .iter()
            .filter_map(|(_, path)| path.canonicalize().ok())
            .collect();
        roots.sort_by_key(|path| Reverse(path.components().count()));
        Self { roots }
    }

    /// Deepest canonical root wins; duplicate roots remain ambiguous. A nested unregistered Git
    /// checkout is never attributed to its registered parent.
    pub(super) fn work_directory(&self, targets: &[PathBuf]) -> Option<PathBuf> {
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

/// Inspect completed tool calls only; prompts and command text never establish a filesystem target.
pub(super) fn latest_work(
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

// lean-ctx: explicit execution/mutation targets only; extend when a verified tool contract exposes one.
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
    use super::CheckoutIndex;
    use std::fs;

    #[test]
    fn checkout_index_handles_nested_unregistered_late_and_deleted_targets() {
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

    #[cfg(unix)]
    #[test]
    fn checkout_index_keeps_snapshot_canonical_identity_and_duplicate_roots_ambiguous() {
        use std::os::unix::fs::symlink;
        let temp = tempfile::tempdir().unwrap();
        let first = temp.path().join("first");
        let second = temp.path().join("second");
        fs::create_dir(&first).unwrap();
        fs::create_dir(&second).unwrap();
        fs::write(first.join("file"), "").unwrap();
        fs::write(second.join("file"), "").unwrap();
        let alias = temp.path().join("checkout");
        symlink(&first, &alias).unwrap();
        let snapshot = CheckoutIndex::new(&[("alias".into(), alias.clone())]);
        fs::remove_file(&alias).unwrap();
        symlink(&second, &alias).unwrap();
        let first = first.canonicalize().unwrap();
        let second = second.canonicalize().unwrap();
        assert_eq!(
            snapshot.work_directory(&[first.join("file")]),
            Some(first.clone())
        );
        assert_eq!(snapshot.work_directory(&[second.join("file")]), None);

        fs::remove_file(&alias).unwrap();
        symlink(&first, &alias).unwrap();
        let duplicate =
            CheckoutIndex::new(&[("first".into(), first.clone()), ("alias".into(), alias)]);
        assert_eq!(duplicate.work_directory(&[first.join("file")]), None);
    }
}
