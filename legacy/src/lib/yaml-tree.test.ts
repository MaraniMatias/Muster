import { describe, expect, it } from "vitest";
import { parseYamlTree, renderYamlTree } from "./yaml-tree";

describe("YAML tree", () => {
  it("keeps nested mappings and sequence items in their indentation tree", () => {
    expect(
      parseYamlTree(
        [
          "title: Example",
          "author:",
          "  name: Ada",
          "tags:",
          "  - api",
          "  - docs",
          "items:",
          "  - name: first",
          "    enabled: true",
        ].join("\n"),
      ),
    ).toEqual([
      { key: "title", value: "Example", children: [] },
      { key: "author", value: null, children: [{ key: "name", value: "Ada", children: [] }] },
      {
        key: "tags",
        value: null,
        children: [
          { key: null, value: "api", children: [] },
          { key: null, value: "docs", children: [] },
        ],
      },
      {
        key: "items",
        value: null,
        children: [
          {
            key: null,
            value: null,
            children: [
              { key: "name", value: "first", children: [] },
              { key: "enabled", value: "true", children: [] },
            ],
          },
        ],
      },
    ]);
  });

  it("does not let YAML values become HTML", () => {
    expect(renderYamlTree("title: <script>alert(1)</script>")).not.toContain("<script>");
  });
});
