# Native release safety

## Supported targets

- macOS 15+, Apple Silicon: ad-hoc signed `.app` in a versioned ZIP, not notarized.
- Ubuntu 24.04, x86_64: `.deb`. No native ARM64 Linux or AppImage promise yet.

The macOS floor is one decision in three places: `LSMinimumSystemVersion 15.0`
in the bundle's `Info.plist`, `MACOSX_DEPLOYMENT_TARGET` forced to 15.0 in
`.cargo/config.toml`, and the minimum macOS version the packaged binary states
in its own Mach-O load commands. All three are the same version rather than a
range: the floor is what this build claims to need, so a binary stating an
older one is refused as much as one stating a newer one.

Existing native candidates may contain only macOS; this workflow adds the Linux
package to future releases. It never changes an existing release or version.

## Local checks

Use Rust/rustup 1.97.1 and Node 22. On Ubuntu install:

```sh
sudo apt-get update
sudo apt-get install -y libvulkan-dev libx11-dev libxcb1-dev \
  libxkbcommon-dev libxkbcommon-x11-dev libfreetype6-dev libfontconfig1-dev dpkg-dev zip unzip \
  build-essential
scripts/release.sh build
scripts/package-linux-deb.sh
```

`build-essential` is the C compiler: the tree-sitter grammars are C sources,
and the crate that builds them says so with `cc`.

`release.sh check` runs fmt, clippy, workspace tests, release-script tests and the
RustSec audit. `build` adds the locked release build. On macOS, `release` also
assembles `dist/Muster.app`; these local commands never tag or publish.

The package derives shared-library dependencies from the ELF with
`dpkg-shlibdeps`. Git, `xdg-utils` and the dynamically loaded Vulkan loader are
explicit runtime dependencies. An appropriate Vulkan driver must be available on
the desktop. A Cargo version such as `1.0.0-rc.3` becomes Debian `1.0.0~rc.3`,
which upgrades correctly to `1.0.0` without implying that final release is approved.
Finalization keeps `-rc.3` in the public download filename because GitHub rewrites
`~` in asset names; checksums refer to the public filenames, not the intermediate
Debian package filename. The package bytes and internal version are unchanged.

## Validate without publishing

`.github/workflows/linux-release.yml` is named **Native release**. After the new
workflow has been merged to the default branch, it can also be dispatched using
an implementation branch as `--ref` for validation. All jobs build the exact
40-character commit SHA requested, not a moving branch tip.

```sh
gh workflow run linux-release.yml --ref main \
  -f sha=<full-commit-sha> -f mode=validate
```

Both platforms run the checks and create their bundles. Linux installs and launches
the `.deb` in a fresh Ubuntu 24.04 container, with no development libraries, as an
unprivileged user with an isolated HOME. Xvfb and Mesa lavapipe provide the test
display and software Vulkan driver. The bounded check requires an application
window and a process that remains alive; it is **not** a pixel-render or full
interaction test. The ordinary pull-request CI exercises Linux packaging and
this runtime check too, before this manual workflow is available on `main`.

A read-only finalization job checks source SHA, exact file lists, checksums,
Debian version/architecture/payload and macOS bundle version/architecture. It
also reads the minimum macOS version out of the packaged arm64 binary's own
Mach-O load commands and refuses a bundle that declares a floor other than
15.0, a binary that states no floor, a binary that states a floor other than
15.0 -- older or newer -- and a binary whose own load commands disagree about
it. It retains one `native-release-<sha>` artifact with the `.deb`, ZIP,
`SHA256SUMS` and `SOURCE_SHA`. Validation never creates a tag or release.

Before publishing, inspect that artifact and manually verify real desktop
rendering, terminal input/process shutdown, clipboard and links. Real GPU/IME,
Wayland and broader distribution acceptance are not implied by the Xvfb check.

## Publish

Prepare a new version in `app/Cargo.toml` and `Cargo.lock`, update the changelog,
and merge it to `main`. Do not reuse `v1.0.0-rc.2` or move an existing tag.

```sh
gh workflow run linux-release.yml --ref main \
  -f sha=<full-commit-sha> -f mode=publish -f tag=v1.0.0-rc.3
```

Publication must be dispatched from `main`, for a commit on `main`, and the new
tag must equal the manifest/lock version. Supported policy is `v0.minor.patch`
or `v1.0.0-rc.N`; final `v1.0.0` remains a separate deliberate decision.

Publish mode builds and validates both platforms in the same run, then downloads
the final artifact without rebuilding it. Only this last job has write permission;
it never checks out or executes source. It rechecks checksums and the source SHA,
refuses existing tags (including a concurrent creation), creates an annotated tag,
uploads a draft release and exposes it only after the uploads succeed. RCs are
prereleases and never replace Latest. Network errors fail closed.

If upload/publication fails after tag creation, stop: do not delete/move the tag
or rebuild under it. Recover the exact retained final artifact from that run,
verify its checksums/source and finish the draft/upload manually. Expired or
missing artifacts require investigation, not an automatic tagged-version rebuild.
