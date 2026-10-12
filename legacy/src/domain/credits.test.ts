import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import { ACKNOWLEDGEMENT, CREDITS, REPOSITORY } from "./credits";

/**
 * Read rather than imported: an attribution is only a claim, and a claim checked against the same
 * module that makes it proves nothing. What has to agree with this list is the manifest the build
 * actually resolves against, so that is what is read off disk.
 */
const manifest = JSON.parse(
  readFileSync(join(dirname(fileURLToPath(import.meta.url)), "..", "..", "package.json"), "utf8"),
) as { dependencies: Record<string, string> };

describe("the credits the About section draws", () => {
  it("names a licence for everything it lists", () => {
    // The whole point of a credit is the licence: a name with no terms beside it says who made
    // something and nothing about whether it may be here.
    for (const group of CREDITS) {
      expect(group.title.length, group.title).toBeGreaterThan(0);
      expect(group.entries.length, group.title).toBeGreaterThan(0);
      for (const entry of group.entries) {
        expect(entry.name.length, entry.name).toBeGreaterThan(0);
        expect(entry.role.length, entry.name).toBeGreaterThan(0);
        expect(entry.license.trim(), entry.name).not.toBe("");
      }
    }
    expect(ACKNOWLEDGEMENT.trim()).not.toBe("");
  });

  it("names no two of the same project twice", () => {
    const names = CREDITS.flatMap((group) => group.entries.map((entry) => entry.name));
    expect(new Set(names).size).toBe(names.length);
  });

  it("credits only packages this build actually depends on", () => {
    expect(manifest.dependencies, "package.json has no runtime dependencies to check against").not.toEqual({});
    // The list is hand-written, so the failure it cannot catch on its own is drift: a dependency
    // swapped for another leaves the credit naming something that no longer ships. Naming the
    // package on the entry is what lets this fail instead.
    for (const group of CREDITS) {
      for (const entry of group.entries) {
        if (!entry.package) continue;
        expect(manifest.dependencies[entry.package], entry.name).toBeDefined();
      }
    }
  });

  it("names the repository and the licence of this app itself", () => {
    // The two facts this section is opened to answer, and the ones the credits list cannot answer
    // for itself: what this is, where it came from, and what it is licensed under.
    expect(REPOSITORY).toMatch(/^https:\/\/github\.com\//);
    expect(ACKNOWLEDGEMENT).toContain("MIT");
  });

  it("credits every bundled asset that carries a licence of its own", () => {
    // The three copied works, named in the sources they are copied into. Each of them ships inside
    // the binary, so each is a condition of the licence rather than a link somebody can follow.
    const bundled = CREDITS.flatMap((group) => group.entries.map((entry) => entry.name));
    for (const asset of ["Catppuccin", "Lucide", "Zed", "Fira Code Nerd Font Mono"]) {
      expect(bundled, asset).toContain(asset);
    }
  });
});
