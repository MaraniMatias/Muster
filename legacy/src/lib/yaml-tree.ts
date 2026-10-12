export interface YamlTreeNode {
  key: string | null;
  value: string | null;
  children: YamlTreeNode[];
}

interface StackEntry {
  indent: number;
  node: YamlTreeNode;
}

function mapEntry(text: string): { key: string; value: string | null } | null {
  const separator = text.indexOf(":");
  if (separator < 0) return null;
  const key = text.slice(0, separator).trim();
  if (!key) return null;
  const value = text.slice(separator + 1).trim();
  return { key, value: value || null };
}

/**
 * Reads the indentation-shaped part of YAML that is useful in a metadata viewer.
 * Values that need YAML's full grammar stay as written; mappings and sequence items still form a
 * useful expandable tree without bringing a parser into the preview bundle.
 */
export function parseYamlTree(source: string): YamlTreeNode[] {
  const root: YamlTreeNode = { key: null, value: null, children: [] };
  const stack: StackEntry[] = [{ indent: -1, node: root }];

  for (const line of source.split(/\r?\n/)) {
    if (!line.trim() || line.trimStart().startsWith("#")) continue;
    const indent = line.length - line.trimStart().length;
    const text = line.trim();
    while (stack.length > 1 && indent <= (stack.at(-1)?.indent ?? -1)) stack.pop();
    const parent = stack.at(-1)?.node ?? root;
    const sequence = /^-\s*(.*)$/.exec(text);

    if (sequence) {
      const item = sequence[1] ?? "";
      const node: YamlTreeNode = { key: null, value: item || null, children: [] };
      parent.children.push(node);
      stack.push({ indent, node });
      const entry = mapEntry(item);
      if (entry) {
        node.value = null;
        const child: YamlTreeNode = { key: entry.key, value: entry.value, children: [] };
        node.children.push(child);
        if (entry.value === null) stack.push({ indent: indent + 1, node: child });
      }
      continue;
    }

    const entry = mapEntry(text);
    const node: YamlTreeNode = entry
      ? { key: entry.key, value: entry.value, children: [] }
      : { key: null, value: text, children: [] };
    parent.children.push(node);
    if (entry?.value === null) stack.push({ indent, node });
  }

  return root.children;
}

function escapeHtml(value: string): string {
  return value.replace(/[&<>"']/g, (character) => {
    switch (character) {
      case "&":
        return "&amp;";
      case "<":
        return "&lt;";
      case ">":
        return "&gt;";
      case '"':
        return "&quot;";
      default:
        return "&#39;";
    }
  });
}

function renderNode(node: YamlTreeNode): string {
  const label = escapeHtml(node.key ?? "-");
  const value = node.value === null ? "" : `<span class="markdown-yaml-value">${escapeHtml(node.value)}</span>`;
  const separator = node.key === null ? "" : `<span class="markdown-yaml-separator">:</span>`;
  if (node.children.length === 0) {
    return `<div class="markdown-yaml-leaf"><span class="markdown-yaml-key">${label}</span>${separator}${value}</div>`;
  }
  return `<details class="markdown-yaml-branch" open><summary>${label}${separator}${value}</summary><div class="markdown-yaml-children">${node.children.map(renderNode).join("")}</div></details>`;
}

/** Renders escaped YAML tree markup; values from the checkout never become markup. */
export function renderYamlTree(source: string): string {
  const nodes = parseYamlTree(source);
  return nodes.length === 0
    ? '<p class="markdown-yaml-empty">Empty metadata.</p>'
    : `<div class="markdown-yaml-tree">${nodes.map(renderNode).join("")}</div>`;
}
