# Muster — presentation script

Every claim here has a `file:line` behind it; nothing in this file asserts something the code
does not.

Use the 30-second version for an introduction and the slides for the pitch. The demo script is the
part that actually convinces.

---

## The 30-second version

> Muster is a desktop workspace for people running several coding agents in parallel. It treats each
> Git worktree as a first-class checkout with its own files, diffs and terminals, so you can try the
> code, review what changed, and leave the feedback on the exact diff line it belongs to — then send
> it back to the agent, or export it as Markdown. It doesn't run models. You start the agents in its
> terminals.

## Six slides

### 1. The problem

**Point:** Parallel agents fragment the context, not just the work.

**Say:** When three agents work at once you get three directories, three sets of terminals and three
sets of changes. To review one of them you `cd`, find the right terminal, find the diff, and copy a
thought from your head into a comment box on some web page. None of that is the work. The work is
deciding whether the change is right.

**Say if pressed:** The README's own framing is that Muster groups worktrees "by checkout in one
window, so you can try the code, review the diff, and send feedback without switching between
directories and copying comments from a browser."

### 2. The checkout is the unit

**Point:** The repo root and each worktree are objects with their own everything.

**Show:** The sidebar — repo group, worktree rows, terminal rows under each.

**Say:** A checkout owns its files, its terminals, its diff state and its review notes. Create a
worktree from the dialog and the name slugs into a branch; archive it later and its files survive;
delete it and the branch goes too, with a warning listing the dirty files and the unmerged commits
before you confirm. Worktrees you create outside Muster show up on their own.

**Evidence:** `app/src/worktree_dialog.rs`, `core/src/navigation/worktrees.rs`.

### 3. The review loop

**Point:** A note belongs to a line, not to a paragraph.

**Show:** Open **Changes**, click the `+` widget on a diff line, drag the range, write. Then show the
note card moving through `draft → sent → resolved`.

**Say:** Notes anchor to a file, a side and a range, in the GitLab style — the first `+` fixes the
end, a second one above sets the start. Each note keeps a hash of the line it was written about, so
when the agent changes that line the note flags itself **outdated** instead of quietly pointing at
something else. Outdated notes are held back from a send unless you tick the box to include them.

**Land it with:** resolving a note is only offered when the diff can prove what it refers to. When
it cannot, it says so rather than guessing.

**Evidence:** `app/src/review.rs`, `core/src/review.rs`.

### 4. Sending it back — and not losing it

**Point:** Delivery is reconciled, not assumed.

**Show:** Send a round to an OpenCode session. Then kill and restart the app and show the round
still accounted for.

**Say:** Before the message goes out, Muster writes a round marker into the prompt and marks the
notes as sent in the database. If anything dies in between, the next connection asks the session's own
transcript whether that marker ever arrived — and either confirms the round or requeues it. A crash
can neither fake a delivery nor hide one. The export alternative is Markdown, and it is compatible
with CodeReview.nvim.

**Honesty beat, and it strengthens the slide:** Muster never starts or stops an OpenCode service. It
finds the one you already run, refuses any address that isn't loopback before it sends the password,
and refuses to answer with anything that belongs to a different worktree.

**Evidence:** `core/src/opencode/rounds.rs`, `core/src/opencode.rs`.

### 5. It's a product, not a prototype

**Point:** This ships.

**Say:** Version 0.20.1, on macOS Apple Silicon and Linux on amd64 and arm64, released through
GitHub. Roughly 1,550 automated tests, weighted to the hard parts — the Git service has 111 and the
agent bridge 86. No telemetry, no cloud, no account.

**Then the part most demos skip:** the release pipeline cannot publish until CI has passed against
that exact commit and all three targets build, and it refuses to re-tag a different commit. The
dependency tree is pinned and audited, including a vendored security backport whose entire tree is
hash-checked. Nothing ships without that gate.

**Evidence:** `scripts/release.sh`, `scripts/audit-rust.mjs`, `.github/workflows/linux-release.yml`.

### 6. Where it stops

**Point:** Draw the boundary yourself; it reads as confidence.

**Say:** Muster is not an IDE — the editor is for quick changes. It is not an LLM harness: it calls
no models and does not manage how agents run. It is not a GitHub client: notes go to OpenCode or to a
Markdown file. Git itself is read-only apart from creating and removing worktrees; it does not stage,
commit or push, and that is deliberate. OpenCode is the only harness with native integration; Codex,
Claude Code and `pi` are specified in `docs/todo/` and not built. Any other CLI agent still works in
the terminals, and Markdown export still carries its notes.

---

## Live demo, five minutes

1. **Open a repo with real worktrees.** Show the sidebar structure and the per-checkout `+N/-N`
   badges. _30 s_
2. **Create a worktree** from the dialog. Point at the auto-slugged branch name and the start point.
   _30 s_
3. **Open a terminal**, start an agent. Show the sidebar row pick up its state — working, awaiting
   reply. _45 s_
4. **Drag that terminal to another worktree.** Process and scrollback survive; the optional `cd`
   writes into the idle shell. _45 s_ — this is the moment people go "oh".
5. **Open Changes**, add a note on a range, resolve another, mark one outdated by touching the file.
   _60 s_
6. **Send the round** to the session. Show the round counter. Restart the app and show the round is
   still accounted for. _60 s_
7. **Export the same notes as Markdown.** _15 s_

If the demo dies, the fallback is the screenshot set plus `README.md` — but note that
`docs/screanshot/` is currently untracked and `docs/demo-video/` is gitignored. Neither is in a
clone. Commit them before you rely on either.

---

## Questions you should expect

**"How is this different from just running `git worktree` in a terminal?"**
The worktree commands exist, but the grouping does not: which terminal belongs to which worktree, what
changed in each one, and what you already said about those lines. None of that survives a `cd`.

**"What if the agent moves the code while I'm reviewing?"**
That is what the `outdated` flag is for. The note keeps a hash of the line it was written about, so a
moved line surfaces as outdated instead of silently describing something else. The round that follows
holds outdated notes back unless you explicitly include them.

**"Does it send my code anywhere?"**
Muster has no telemetry and no cloud. The only outbound requests go to the OpenCode service running on
your own machine — and Muster refuses any address that is not loopback before authenticating. What
your agent sends to its model provider is your agent's decision, not Muster's.

**"Does it commit my code?"**
No. Git is read-only apart from worktree lifecycle. There is no stage, commit, stash, fetch or push.
Muster reads your repository; it does not write to it.

**"What happens to my data if I upgrade?"**
At `0.x` a breaking change is made in place. The recent Marvis → Muster rename moved the bundle
identifier, the paths and the database name, and no migration runs in either direction: an old
database is not read and the workspace starts empty. That is the stated policy in `AGENTS.md`, and it
stops being acceptable at 1.0.

**"Why only OpenCode?"**
Because it is the one with a local API that can be queried and prompted. Codex, Claude Code and `pi`
are written up in `docs/todo/` with the exact protocol each needs, and none of them are built. The
Markdown export is the path that keeps every other agent usable today.

---

## Do not say

- That Codex, Claude Code or `pi` are supported. They are specs.
- That it commits, stages, pushes or manages branches. It does not.
- That terminals split into panes. The model exists; the UI cannot reach it.
- That you can stop an agent from the app. The command is implemented and nothing calls it.
- That upgrading preserves your workspace. It does not, at `0.x`.
- Anything about the screenshots or demo video existing in the repository.
