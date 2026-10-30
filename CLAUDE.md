# CLAUDE.md

Tratto is a low-latency handwriting and PDF annotation app for pen and tablet
(Windows, macOS, Linux), written in Rust with egui 0.37 + wgpu. User-facing
behaviour, data layout and env vars are documented in [README.md](README.md);
contribution rules in [CONTRIBUTING.md](CONTRIBUTING.md). This file covers what
you need to work on the code.

## Commands

```sh
cargo run --release                     # the app (default member is crates/ui)
cargo test --workspace                  # ALL tests: plain `cargo test` only runs tratto-ui's
cargo test -p tratto-core               # backend tests only (fast, no GPU/window)
cargo fmt --all                         # rustfmt.toml: max_width = 120, use_small_heuristics = "Max"
cargo clippy --workspace --all-targets
```

- Rust 1.95+, edition 2024, workspace resolver 3.
- Dev profile is `opt-level = 1` for our crates and `3` for dependencies.
- Ignored benchmarks: `cargo test --release -p tratto-ui bench -- --ignored --nocapture`
  ([bench.rs](crates/ui/src/bench.rs)), and
  `TRATTO_BENCH_PDF=<file> cargo test -p tratto-core --release bench_tiles -- --ignored --nocapture`.
- GPU tests (e.g. in [stroke_gpu.rs](crates/ui/src/stroke_gpu.rs)) and PDFium tests
  skip themselves (pass early) when no adapter / no PDFium library is available.
- **PDFium** is loaded at runtime and is not in git. Fetch it into
  `crates/ui/pdfium/<platform>/` as described in
  [crates/ui/pdfium/README.md](crates/ui/pdfium/README.md) (release chromium/7881).
  [build.rs](crates/ui/build.rs) copies it next to the executable; `TRATTO_PDFIUM=<path>`
  overrides. Without it the app builds and runs but cannot show or import PDFs.
- Debug env vars: `TRATTO_DEBUG_PEN=1` (pen button codes), plus the runtime ones in the
  README (`TRATTO_FPS`, `TRATTO_VSYNC`, `TRATTO_MSAA`, `WGPU_POWER_PREF`, `WGPU_BACKEND`).
- Releases: pushing a `v*` tag runs [.github/workflows/release.yml](.github/workflows/release.yml),
  which uses [scripts/package.sh](scripts/package.sh). Tag suffix picks the updater channel
  (none: stable, `-rc`/`-beta`: testing, `-alpha`: unstable). `TRATTO_VERSION` (from the tag)
  and `TRATTO_TARGET` are baked in by build.rs for the updater.

## Architecture

Two crates; the dependency only goes one way:

- **`crates/core` (`tratto-core`)**: no UI. Depends only on `emath`, `serde`, `bincode`,
  `flate2`, `pdfium-render`, `image`. **Never add egui/eframe/winit/wgpu or any windowing
  crate here**; that code belongs in `tratto-ui`. New core behaviour gets unit tests.
- **`crates/ui` (`tratto-ui`)**: the `tratto` binary. `main.rs` parses CLI args
  (`[--reader] [PATH]`, `--register-pdf`, `--unregister-pdf`) and sets up eframe/wgpu.

Each module starts with a `//!` doc comment listing its submodules and responsibilities:
read [crates/core/src/lib.rs](crates/core/src/lib.rs), [crates/ui/src/main.rs](crates/ui/src/main.rs)
and [crates/ui/src/app/mod.rs](crates/ui/src/app/mod.rs) first for the map.

### Core concepts

- **Model** ([model.rs](crates/core/src/model.rs)): `Document` holds pages, `Stroke`s
  (points shared as `Arc<[Point]>`, so cloning a document is cheap), `TextBox`es, images
  and paper settings. Ids come from `doc.alloc_id()`. There is also an "infinite page"
  mode (`model::CANVAS_SIZE`).
- **Coordinate spaces** ([view.rs](crates/ui/src/view.rs)): *page space* (what strokes
  store; 1 unit = 1 CSS px at 100%, A4 = 794x1123), *content space* (all pages stacked,
  scroll offset lives here), *screen space* (egui points). Store ink in page space only.
- **Undo/redo** ([history.rs](crates/core/src/history.rs)): action based, never clones
  the document. Each stack entry is the `Action` that reverses a change; applying it returns
  its own inverse. Group multi-part edits in `Action::Batch`. New editing operations need
  an `Action` (or a composition of existing ones), not a snapshot.
- **Storage** ([storage.rs](crates/core/src/storage.rs), [journal.rs](crates/core/src/journal.rs)):
  `.ink` = magic + version + two meta slots + raw PDF bytes + bincode `Document` snapshot +
  append-only journal of `Delta` records, all CRC-checked. Autosave appends a record; once the
  journal is big the whole file is rewritten atomically (temp + rename, `.ink.bak` at most every
  10 min). The library screen reads only the meta slot.
- **Library** ([library.rs](crates/core/src/library.rs)): notebooks/sections/notes are plain
  folders and files under the library root; deletes go to `.trash/`.
- **Strokes**: [stroke_builder.rs](crates/core/src/stroke_builder.rs) (jitter filter, smoothing,
  prediction) → [stroke_path.rs](crates/core/src/stroke_path.rs) (centreline shared by screen and
  PDF export) → [stroke_mesh.rs](crates/ui/src/stroke_mesh.rs) (AA tessellation) →
  [stroke_gpu.rs](crates/ui/src/stroke_gpu.rs) (meshes kept on the GPU, custom wgpu pipelines).
- **PDF**: [pdf_pages.rs](crates/core/src/pdf_pages.rs) reads/rasterises via PDFium;
  [pdf_view.rs](crates/ui/src/pdf_view.rs) renders tiles in the background;
  [pdf_export.rs](crates/core/src/pdf_export.rs) is a hand-written vector PDF writer.

### UI

- `InkApp` in [app/mod.rs](crates/ui/src/app/mod.rs) holds all state; `eframe::App::ui`
  runs a frame and the `app/*` submodules implement features (canvas, ink, selection,
  menus, panels, dialogs). [app/menus.rs](crates/ui/src/app/menus.rs) owns shortcuts and
  the context menu.
- Anything slow runs off the UI thread and reports back through channels: saving
  ([saver.rs](crates/ui/src/saver.rs), one ordered queue; call `Saver::wait` before closing
  notes or moving/renaming files), file dialogs, library scans, PDF tiles, picture decoding,
  updates. Don't block a frame on I/O.
- Platform code is behind `cfg`: [input/](crates/ui/src/input/) (Wayland tablet and drop,
  Windows pen buttons, multi-finger gestures), [win_frame.rs](crates/ui/src/win_frame.rs),
  [print.rs](crates/ui/src/print.rs), [file_assoc.rs](crates/ui/src/file_assoc.rs),
  [desktop.rs](crates/ui/src/desktop.rs). Non-native platforms get a stub that returns `None`.
- **Design system** ([theme.rs](crates/ui/src/theme.rs)): take every colour, radius, spacing
  and text size from `theme::{color, radius, space, ...}`; add a token there if none fits.
  Never hard-code them in widgets.
- UI preferences persist via eframe storage ([settings.rs](crates/ui/src/settings.rs)).

## Rules that are easy to break

- **`.ink` compatibility**: bincode is positional, so *any* change to `Document`, `NoteMeta`
  or journal `Delta` (even adding a field) changes the format. Bump `FORMAT_VERSION` in
  storage.rs (currently 9), keep the readers for older versions (6, 7, 8) and add a test.
- **Escape**: one press does one thing. Anything that closes itself on Escape without consuming
  it (egui popups, the context menu, library dialogs, focused text fields) owns it for the frame
  (`InkApp::escape_for_overlay`); other handlers must check that flag and use `consume_key`, in
  the priority order documented on the field.
- **License header** on every new source file:
  ```rust
  // SPDX-FileCopyrightText: 2026 Christian Braghette
  // SPDX-License-Identifier: AGPL-3.0-or-later
  ```
- Dependencies: the project keeps default features off and comments *why* each crate and
  feature is needed in Cargo.toml; keep doing that, and match egui/winit versions already
  in the tree (e.g. one `wayland-client`).
- Settled decisions, don't undo them without asking: vsync off by default with a fps cap;
  battery saver at 20%; no graphics-backend setting (DX12 by default on Windows, override
  only via `WGPU_BACKEND`); PDF export currently turns non-Latin-1 text into `?` with a
  warning (embedded fonts are planned).

## Style

- Comments and docs are plain, full-sentence English with British spelling ("colour",
  "centreline"), explaining *why*. Modules open with a `//!` overview; keep it in sync
  when adding submodules.
- Tests live in `#[cfg(test)] mod tests` at the bottom of each file.
- Commit messages: Conventional-ish prefixes (`feat:`, `fix:`, `refactor:`) with a lower-case
  summary. Work happens on `dev`; PRs target `main`. Contributions need the CLA (bot-checked).
