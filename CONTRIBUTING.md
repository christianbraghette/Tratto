# Contributing to Tratto

Thanks for helping out! Bug reports, ideas and pull requests are all welcome.

## Contributor License Agreement

Tratto is dual-licensed: it is free software under the AGPL-3.0-or-later and
is also offered under commercial licenses for use in projects that do not
follow the AGPL. Code contributed under the AGPL alone could not go into the
commercial version, so every contribution must be covered by the
[Contributor License Agreement](CLA.md) (CLA).

- You keep the copyright in your work.
- You allow the maintainer to distribute your contribution under the AGPL and
  under commercial licenses.
- You confirm that you have the right to contribute it (CLA, Section 5).
- In return, everything accepted into the project stays available as free
  software (CLA, Section 4).

The [Contributing](README.md#contributing) section of the README explains this
in more detail.

A bot checks the CLA on every pull request. Sign once by commenting the
sentence it asks for (see [How to sign](CLA.md#how-to-sign)). Pull requests
without a signed CLA cannot be merged.

## Reporting issues

Please include:

- your operating system and, on Linux, Wayland or X11
- the input device (mouse, pen or tablet model)
- steps to reproduce, and what you expected to happen instead
- if possible, a small `.ink` file that shows the problem

## Development

The repository is a Cargo workspace. The layout is described under *Project
Structure* in the [README](README.md#project-structure). The main rule is that
`tratto-core` must not depend on egui, eframe or any other UI or windowing
crate. Code that needs those belongs in `tratto-ui`.

Before opening a pull request:

```sh
cargo fmt --all
cargo clippy --workspace --all-targets
cargo test --workspace
cargo build --release
```

Guidelines:

- Keep pull requests focused: one fix or feature per pull request.
- Take colours, corner radii, spacing and text sizes from `crates/ui/src/theme.rs`
  instead of hard-coding them, so the interface stays consistent. Add a token
  there if none fits.
- Add unit tests for new behaviour in `tratto-core`.
- Keep the `.ink` format backwards-compatible. Its parts are bincode, which
  is positional: any change to `Document`, `NoteMeta` or the journal `Delta`
  (even an added field) changes the format. Bump `FORMAT_VERSION` in
  `storage.rs` and keep reading version 6 files.
- Start every new source file with the license header:

  ```rust
  // SPDX-FileCopyrightText: 2026 Christian Braghette
  // SPDX-License-Identifier: AGPL-3.0-or-later
  ```

  You may add your own `SPDX-FileCopyrightText` line for files you write or
  substantially change.
