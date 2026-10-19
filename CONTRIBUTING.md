# Contributing to InkNote

Thanks for helping out! Bug reports, ideas and pull requests are all welcome.

## Contributor License Agreement

InkNote is dual-licensed: it is free software under the GPL-3.0-or-later and
is also offered under commercial licenses. So every contribution must be
covered by the [Contributor License Agreement](CLA.md) (CLA).

- You keep the copyright in your work.
- You allow the maintainer to distribute your contribution under the GPL and
  under commercial licenses.
- In return, everything accepted into the project stays available as free
  software (CLA, Section 4).

Sign once by adding the statement from [How to sign](CLA.md#how-to-sign)
to your first pull request. Pull requests without a signed CLA cannot be
merged.

## Reporting issues

Please include:

- your operating system and, on Linux, Wayland or X11
- the input device (mouse, pen or tablet model)
- steps to reproduce, and what you expected to happen instead
- if possible, a small `.ink` file that shows the problem

## Development

The repository is a Cargo workspace. The layout is described under *Project
Structure* in the [README](README.md#project-structure). The main rule is that
`inknote-core` must not depend on egui, eframe or any other UI or windowing
crate. Code that needs those belongs in `inknote-ui`.

Before opening a pull request:

```sh
cargo fmt --all
cargo test --workspace
cargo build --release
```

Guidelines:

- Keep pull requests focused: one fix or feature per pull request.
- Add unit tests for new behaviour in `inknote-core`.
- Keep the `.ink` format backwards-compatible. If the format has to change,
  bump the version in `storage.rs` and make sure older files still load.
- Start every new source file with the license header:

  ```rust
  // SPDX-FileCopyrightText: 2026 Christian Braghette
  // SPDX-License-Identifier: GPL-3.0-or-later
  ```

  You may add your own `SPDX-FileCopyrightText` line for files you write or
  substantially change.
