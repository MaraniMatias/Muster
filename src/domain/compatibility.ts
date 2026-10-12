import type { ReviewTarget } from "./review";

/** Installed coding-agent binaries available as review destinations. */
export interface CompatibilitySnapshot {
  piInstalled: boolean;
  openCodeInstalled: boolean;
}

/** Available review destinations depend on installed executables, not live sessions. */
export function availableReviewTargets(snapshot: CompatibilitySnapshot | null): ReviewTarget[] {
  return [
    "markdown",
    ...(snapshot?.openCodeInstalled ? (["opencode"] as const) : []),
    ...(snapshot?.piInstalled ? (["pi"] as const) : []),
  ];
}
