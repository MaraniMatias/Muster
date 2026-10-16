# Local compatibility patch

Source: `konomanoasa-tree-sitter-gitignore` 0.3.1, crates.io archive SHA-256
`060cb9f087373dda1b2899edb48edbdb004d7c212f1fddb2c42f977a321fd91c`, upstream
commit `212d4972cb53fa64a422049818459189af710994`.

The parser, scanner, bindings, and highlight query are unchanged. The local Cargo
manifest narrows the build dependency from `cc = "1.4"` to `cc = "~1.2.1"`,
which is the version already selected by the workspace's SQL grammar. The
upstream-only test target and its `tree-sitter 0.27` dev dependency are omitted:
they conflict with the app's single linked Tree-Sitter ABI and are not part of
the runtime grammar.

The archive also carried the upstream README, its own `Cargo.lock`,
`Cargo.toml.orig` and `.cargo_vcs_info.json`. None of them is read by the build,
and the commit they record is the one written above, so they are not vendored.
Everything the grammar needs to compile is here, and the whole directory is
hash-pinned by `scripts/patched-gitignore.mjs`.
