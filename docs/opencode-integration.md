# OpenCode integration inventory

Source snapshot: `206fd33` (`v0.21.0`). This inventory describes existing integration,
not proposed features. User-facing behavior and internal capabilities are listed separately.

## Sidebar and terminals

1. Detect OpenCode as the terminal's foreground program.
2. Use an agent icon instead of the ordinary terminal icon.
3. Display the OpenCode session title as the terminal row's name.
4. Display the active agent's name, such as `plan`, `build`, or `coder`.
5. Read agent colors from OpenCode and use valid hex colors for the row's icon.
6. Show an animated working indicator while a turn is running.
7. Show a question icon and waiting state for pending questions or permissions.
8. Show a failed-turn state in the error color.
9. Show an idle state for an identified session that is not working or awaiting a reply.
10. Show elapsed time since the identified session's last update.
11. Keep terminal rows updated in worktrees that are not currently selected.
12. Distinguish missing session matches (`sin sesión`) from ambiguous matches
    (`varias con este nombre`) without borrowing another session's state.
13. Match OpenCode's truncated terminal titles when their prefix identifies exactly one session.

Terminal-to-session association is inferred from the foreground process and the terminal
title. It is not a direct mapping provided by OpenCode. A pending state that cannot be read
is shown as unavailable, not as confirmed idle. Agent colors apply while idle, working, and
waiting; failed turns retain the error color.

Sources:
[Sidebar](../src/components/Sidebar.vue),
[agent domain](../src/domain/agent.ts),
[session presentation](../src/presentation/agent-sessions.ts).

## Worktrees and terminal movement

14. Discover worktrees created outside Muster, including those created by OpenCode.
15. Detect an OpenCode session working in another worktree through session location and
    tool activity, including shell working directories and edit/write/patch paths.
16. Automatically move the associated terminal row under the destination worktree when
    exactly one terminal can be identified for the session.
17. Preserve the terminal process and scrollback during movement instead of restarting OpenCode.
18. Follow the moved terminal in the main view and restore focus when that terminal was visible.
19. Preserve newer user navigation rather than overwrite it with a late movement result.
20. Configure automatic following as `off`, `cd`, `agent`, or `both`.
21. Manually move a terminal containing OpenCode between worktrees without forcing the
    running OpenCode process to change its working directory.
22. Refuse worktree removal while active agent sessions remain and identify those sessions.

Creating a worktree alone does not move a terminal. Muster must also detect that the associated
session is working there. Automatic following requires the agent-following setting and an
unambiguous terminal match. Shell-directory following is separate from agent relocation.

Sources:
[app](../src/App.vue),
[terminal movement](../src/presentation/terminal-movement-controller.ts),
[relocation analysis](../src-tauri/src/services/agent/relocation.rs),
[worktree service](../src-tauri/src/services/worktree.rs).

## Review delivery

23. Choose OpenCode as the review-note destination instead of Markdown export.
24. Select the destination session from the diff surface.
25. Search destination sessions when more than five are available.
26. Default to the most recently updated session unless the user selects another.
27. Create a review session automatically when no destination session exists.
28. Send selected notes as one message with file, line, and review context.
29. Exclude outdated notes unless the user explicitly includes them.
30. Detect a busy destination and offer Send now, Queue, or Not now.
31. Automatically flush queued reviews when a completed turn is observed.
32. Persist review rounds and their queued message text across restarts.
33. Reconcile interrupted delivery by looking for the round's marker in the session transcript,
    confirming delivery or requeueing as appropriate.
34. Count unfinished rounds and update them when the turn is observed to finish.
35. Validate that the destination session belongs to the intended checkout.
36. Persist the review destination preference per checkout.

Delivery confirmation, turn completion, and resolution of individual review notes are distinct.
A completed turn does not by itself prove that the agent addressed each note.

Sources:
[diff UI](../src/components/FileDiff.vue),
[app](../src/App.vue),
[review-round service](../src-tauri/src/services/review_round.rs).

## Internal connection and synchronization capabilities

37. Discover an already-running local OpenCode service from its registration file.
38. Authenticate with the registered service and require API major version `2`.
39. Reject non-loopback endpoints before sending credentials.
40. Consume live events, including tool calls, questions, and permission requests.
41. Poll running sessions and pending replies, including turns started directly in the TUI.
42. Reconnect after an event-stream interruption and retry discovery when the service becomes available.
43. Read the agent catalog, including names and colors.
44. Read session metadata, including model, agent, turn outcome, and parent/subagent relationship.
45. Guard against stale replies and checkout changes so data or writes do not target the wrong location.

These are supporting capabilities, not necessarily dedicated UI controls. In particular,
reading model or parent-session metadata does not provide a model selector or a subagent tree.

Sources:
[service discovery](../src-tauri/src/services/opencode.rs),
[agent bridge](../src-tauri/src/services/agent.rs),
[IPC commands](../src-tauri/src/commands/agent.rs),
[session presentation](../src/presentation/agent-sessions.ts).

## Not currently integrated

- Answering questions or approving permissions from Muster: the sidebar indicates waiting,
  but does not provide a reply form.
- Displaying question content in a dedicated Muster panel.
- Native operating-system notifications for questions or completed turns.
- Automatically starting the OpenCode executable or its service.
- Cancelling a running turn from Muster's UI. The internal `agent_stop` operation disconnects
  the bridge; it does not cancel OpenCode's work or shut down its external service.
- Creating or managing subagents from Muster, or displaying a dedicated subagent tree.
- Selecting the OpenCode agent or model through a Muster control.

## Verification scope

Inventory compiled from the repository's implementation. This document does not claim a new
live end-to-end verification against a running OpenCode service.
