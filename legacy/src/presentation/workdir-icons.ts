import type { Component } from "vue";
import {
  CircleQuestionMark as CircleQuestionMarkIcon,
  Folder as FolderIcon,
  FolderGit2 as FolderGit2Icon,
  FolderX as FolderXIcon,
  GitBranch as GitBranchIcon,
  House as HouseIcon,
  LoaderCircle as LoaderCircleIcon,
  Sparkles as SparklesIcon,
  SquareTerminal as SquareTerminalIcon,
} from "@lucide/vue";
import type { WorkdirIconKind } from "../domain/workspace";

/**
 * The glyph each role of a checkout wears, and nothing else: the shape of an icon is a drawing
 * decision, so the map lives here with the rest of the drawing and the rule that picks a key out
 * of it lives in the domain, where `workdirIconKind` is written.
 */
export const WORKDIR_ICONS: Record<WorkdirIconKind, Component> = {
  /** A repo root: a folder that is also a Git directory. */
  git: FolderGit2Icon,
  /** A worktree is a branch checked out in a directory of its own, not a fork of anything. */
  worktree: GitBranchIcon,
  folder: FolderIcon,
  home: HouseIcon,
  /** The directory is gone: a folder with a cross, painted in the disabled colour by the row. */
  missing: FolderXIcon,
  terminal: SquareTerminalIcon,
  /**
   * An agent, which is what the row already says in its name and icon colour. The glyph is the only
   * place the row names the program, so it replaces a prefix in the text rather than joining it.
   */
  agent: SparklesIcon,
  /**
   * A turn that stopped to ask something. A question is not the agent at work and not the agent at
   * rest: it is the one state where the shape itself has to say what happened, because a colour
   * cannot — and what it wears is the agent's own colour, which names *who* is asking.
   */
  asking: CircleQuestionMarkIcon,
  /**
   * A turn that is running. A spinner rather than the agent's sparkles, because it is the one state
   * that moves: "a turn is open somewhere" is the one thing a static list cannot say about itself.
   */
  working: LoaderCircleIcon,
};
