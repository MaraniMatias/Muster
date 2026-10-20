# Plan: seguridad de dependencias y toolchain

**Estado:** plan aprobado; pendiente de implementación.

Este plan estaba escrito para el árbol de Tauri + Vue, que ya no está en este
repositorio. Sus pasos de pnpm, Vitest, Vite, ESLint, SVGO y `rusqlite` se
ejecutaron allí y no tienen a qué aplicarse acá: este árbol es Rust, con un
workspace de dos crates y unos scripts en Node. Lo que sigue es lo que todavía
corresponde a este árbol.

## Alcance

Las acciones de GitHub y los permisos con los que corren.

## 1. Fijar las acciones de GitHub por SHA

SHA-pinear cada `uses:` de `.github/workflows/ci.yml` y
`.github/workflows/linux-release.yml`, manteniendo un comentario con el tag
legible. Hoy los workflows usan tags mutables (`actions/checkout@v7`,
`actions/setup-node@v7`, `swatinem/rust-cache@v2`, `actions/upload-artifact@v7`,
`actions/download-artifact@v7`), que cualquiera puede mover.

## 2. Permisos mínimos

Declarar `permissions: contents: read` en los jobs de `ci.yml`, y
`persist-credentials: false` en el checkout de todo job que no escriba.

## Verificación y aceptación

- Ningún `uses:` de los workflows apunta a un tag en vez de a un SHA.
- Cada job de CI declara `permissions: contents: read`, y ningún checkout deja
  credenciales persistidas donde no hacen falta.

## Lo que este plan ya no cubre

- **La auditoría de Rust ya está hecha.** `scripts/audit-rust.mjs` consulta OSV
  sobre el grafo bloqueado y falla cerrado ante un paquete de ruta que no puede
  fijar por hash; `cargo audit` no se usa en este árbol.
- **El toolchain ya está fijado** en `rust-toolchain.toml` (1.97.1), y los
  perfiles y el `[patch.crates-io]` viven en el manifiesto raíz del workspace.
- **El lockfile de Rust está versionado** y los scripts de Node no instalan
  nada: `package.json` ya no existe en este árbol.

## Referencias de implementación

- GitHub Actions: fijar una acción a un SHA — <https://docs.github.com/actions/security-guides/security-hardening-for-github-actions#using-third-party-actions>
