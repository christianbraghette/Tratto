# InkNote

[![License: GPL v3+](https://img.shields.io/badge/license-GPL--3.0--or--later-blue.svg)](LICENSE)
[![Commercial license available](https://img.shields.io/badge/commercial%20license-available-green.svg)](#license)
![Rust 1.85+](https://img.shields.io/badge/rust-1.85%2B-orange.svg)

InkNote is a minimal, cross-platform note-taking app for handwriting, built with
Rust, [egui](https://github.com/emilk/egui) and [wgpu](https://wgpu.rs).
You write with a pen on paged paper and keep notes in notebooks. Notes save
automatically, and you can export any note as a vector PDF.

It is designed for low-latency inking. Strokes are smoothed, anti-aliased on
the GPU and, optionally, predicted slightly ahead of the pen tip.

- [Features](#features)
- [Getting Started](#getting-started)
- [Using InkNote](#using-inknote)
- [Your Data](#your-data)
- [Configuration](#configuration)
- [Project Structure](#project-structure)
- [Contributing](#contributing)
- [Acknowledgements](#acknowledgements)
- [License](#license)

## Features

**Writing**
- Pen and highlighter with configurable colors and widths
- Pressure-sensitive strokes, with smoothing and optional prediction of the pen tip
- Stroke eraser, and lasso selection to move or delete ink
- Text boxes for typed notes
- Native pen support on Wayland (pressure, eraser end, side button) and Windows (eraser end, side button);
  on X11, a pen side button reported as the middle mouse button also switches to the eraser

**Pages**
- A3, A4, A5, Letter, Legal or custom paper sizes, in portrait or landscape
- Blank, ruled or grid paper with adjustable line spacing
- A page panel with previews for navigating, reordering, inserting and deleting pages
- Zoom presets, plus zooming with Ctrl + wheel or a touchpad pinch
- Smooth scrolling: wheel notches glide and speed up when you spin the wheel
  quickly, a pan flicked on the grey area keeps moving, and page jumps ease
  into place

**Interface**
- VS Code-style title bar with File, Edit, View, Page and Help menus (or the system title bar, if you prefer)
- Context menu on right-click, adapted to what is under the pointer
- Icon buttons, with names and shortcuts in tooltips
- Controls in notches attached to the canvas edges; resizable page and notebook panels
- Rename the note from the title bar and the notebook from the notebook panel

**Organisation**
- A library of notebooks (with colour covers), sections and quick notes
- Recent notes; rename, duplicate, move and delete notes
- Import existing `.ink` files
- Deleted items go to a trash folder instead of being erased

**Safety and export**
- Autosave a moment after every change, with atomic writes and periodic `.bak` backups
- Undo and redo
- Vector PDF export, with or without the ruled lines or grid

## Getting Started

### Requirements

- [Rust](https://www.rust-lang.org/tools/install) 1.85 or newer (Rust 2024 edition)
- A GPU and driver supported by wgpu (Vulkan, Metal, DirectX 12 or OpenGL)
- **Linux**: the Wayland and xkbcommon development packages, for example
  `sudo apt install libwayland-dev libxkbcommon-dev` on Debian/Ubuntu.
  InkNote runs on both Wayland and X11, but native pen pressure needs Wayland.

### Build and run

```sh
git clone https://github.com/christianbraghette/inklav.git
cd inklav
cargo run --release
```

The optimized executable is written to `target/release/inknote` (`inknote.exe` on Windows).

### Tests

```sh
cargo test --workspace
```

Use `--workspace`: a plain `cargo test` only runs the tests of the UI crate.

## Using InkNote

### Library

InkNote opens on the library screen. From there you can:

- create a **quick note** (**+ New note**) or a **notebook** (**+ New notebook**)
- open a notebook to see its **sections** (**+ Section**) and the notes inside them
- rename, duplicate, move or delete a note from its menu
- **Import** an existing `.ink` file into the library
- choose a different **Library folder**, or **Refresh** after an external sync tool changes it

### Window and menus

Like VS Code, InkNote draws its own title bar. It holds the menus, the
window title and the minimize, maximize and close buttons. Drag the title bar
to move the window, double-click it to maximize, and drag the window edges to
resize.

| Menu | Contents |
| --- | --- |
| **File** | New note, new notebook, import, save, export as PDF, close note, library folder, settings, exit |
| **Edit** | Undo, redo, select all on page, delete selection, deselect |
| **View** | Custom title bar, zoom in / out, actual size, fit width / page, toolbar position, full screen |
| **Page** | Insert a page before / after the current one, delete it, paper style, page setup |
| **Help** | Keyboard shortcuts, about InkNote |

If you prefer the system title bar, turn off **View > Custom title bar**. The
menus then appear below it.

### Editor

All editor controls sit in notches attached to the edges of the canvas. All
buttons are icons; hover over one to see its name.

| Where | Buttons |
| --- | --- |
| Top-centre notch (or bottom, see **View**) | Pen, Highlighter, Eraser, Select, Text, the active tool's colours and size, Undo, Redo |
| Top-left notch | **Pages** (page panel) |
| Bottom-left notch | **Settings** (settings panel) |
| Top-right notch | **Library** (toggle the notebook panel) |

If the window is too narrow for the tool notch to fit between the corner
notches, it moves to the other edge.

The corner notches grow into side panels. Both panels can be resized by
dragging their inner edge, and both have a context menu (right-click):

- **Left panel**: a strip along its left edge joins the two left notches,
  with **Pages** at the top and **Settings** at the bottom, in the same places
  as their notches. Next to it, the panel shows either the page previews (drag
  to reorder; right-click a page to go to it, insert a page before or after
  it, or delete it) or the settings. Click the other button to switch, or the
  highlighted one to close the panel. Each view keeps its own width. The page
  list follows the page you are on, and the dashed tile at its end adds a page.
- **Notebook panel** (right): the sections and notes of the open note's
  notebook, with the open note highlighted. Click a note to switch to it (the
  current one is saved first). The header is the notebook's name: click it to
  rename the notebook. Right-click a note to open, rename, duplicate or delete
  it, a section to add a note to it, rename or delete it, and the empty space
  to add a note or a section. A quick note (outside any notebook) shows the
  quick notes instead. Each group shows how many notes it holds; the dashed
  tile adds a note next to the open one. The button next to **Library** opens
  the full library screen.

The note's title is shown in the middle of the title bar. Click it to rename
the note; Enter or clicking elsewhere saves, Escape cancels.

To pan, drag on the grey area around the pages, or use the scrollbars, the
mouse wheel or a touchpad. Flick the drag and the view keeps gliding.

The settings include zoom, paper size and orientation, paper style and
line spacing, pen prediction, PDF export options and
**Save now**. Notes save automatically, so you don't need to save before
closing. **File > Close note** (`Ctrl+W`) saves the note and goes back to the
library screen.

### Keyboard shortcuts

| Shortcut | Action |
| --- | --- |
| `Ctrl+N` | New note (in the same notebook or section) |
| `Ctrl+S` | Save now |
| `Ctrl+E` | Export the note as PDF |
| `Ctrl+W` | Close the note and go back to the library screen |
| `Ctrl+Z` | Undo |
| `Ctrl+Y` or `Ctrl+Shift+Z` | Redo |
| `Ctrl+A` | Select everything on the current page |
| `Delete` or `Backspace` | Delete the selection |
| `Escape` | Clear the selection, or close the context menu |
| `Ctrl+Plus` / `Ctrl+Minus` | Zoom in / out |
| `Ctrl+0` | Actual size (100 %) |
| `Ctrl` + mouse wheel, or pinch | Zoom around the pointer |
| `Page Up` / `Page Down` | Scroll by one screen (animated) |
| `Home` / `End` | Go to the first / last page |
| `F11` | Toggle fullscreen |
| `Ctrl+Q` | Exit |

On macOS, use `Cmd` instead of `Ctrl`. The list is also under **Help >
Keyboard shortcuts**.

### PDF export

`Ctrl+E` (or **Export PDF** in the settings) writes `<note name>.pdf` next to
the note file. Each note page becomes one PDF page. Ink is exported as
vector paths that follow the same smoothed curves as on screen.

## Your Data

Notes are plain files in a plain folder, so they work with any backup or
sync tool, and one damaged note cannot affect the others. The default library
folder is `Documents/InkNote` in your home folder:

```text
InkNote/
├── Quick note.ink          quick notes
├── My notebook/            a notebook
│   ├── .notebook           cover colour
│   ├── Section 1/          a section
│   │   └── Some note.ink
│   └── Loose note.ink      notes outside any section ("General")
└── .trash/                 deleted items
```

- **Format**: `.ink` is a versioned binary format. It stores a small header
  (title, dates, preview) followed by the document (pages, strokes, text
  boxes, paper settings). Files from older versions still open.
- **Safety**: every save writes a temporary file and then renames it over the
  note, so a crash cannot leave a half-written note. The previous version is
  copied to `<note>.ink.bak` at most every ten minutes.
- **Deleting** moves items to `.trash/` inside the library folder. To
  recover something, move it back by hand.

UI preferences (toolbar position, tool styles, zoom, library folder) are stored
separately by eframe in the platform's application-data folder.

## Configuration

By default InkNote presents frames without waiting for vsync, for the lowest
latency. These environment variables change rendering:

| Variable | Effect |
| --- | --- |
| `INKNOTE_VSYNC=1` | Enable vsync: may save power, but adds latency |
| `INKNOTE_MSAA=4` | Enable 4× MSAA (default `1`, off). Strokes are already anti-aliased, so this is rarely needed. |

```sh
# Linux / macOS
INKNOTE_VSYNC=1 cargo run --release
```

```powershell
# Windows PowerShell
$env:INKNOTE_VSYNC = "1"; cargo run --release
```

## Project Structure

The repository is a Cargo workspace that separates the backend from the UI:

```text
crates/
├── core/   inknote-core: backend library, no UI dependencies
│           model, history (undo/redo), stroke_builder, stroke_path,
│           storage (.ink files), library (notebooks/notes), pdf_export
└── ui/     inknote-ui: the `inknote` executable (egui + wgpu)
            app (editor, library, menus), chrome (title bar), theme (design
            system), view, render, stroke_mesh, toolbar, kinetic (scrolling),
            scrollbar, settings, input (Wayland tablet / Windows pen)
```

`inknote-core` depends only on `emath` (the geometry types egui is built on),
`serde`, `bincode` and `flate2`. It can be tested and reused without a window
or GPU. The UI crate depends on the core, never the other way around.

## Contributing

Contributions are welcome! Please read [CONTRIBUTING.md](CONTRIBUTING.md)
first. Because InkNote is dual-licensed, contributors sign the
[Contributor License Agreement](CLA.md) once, in their first pull request. You
keep the copyright in your work.

## Acknowledgements

- Interface font: [Inter](https://rsms.me/inter/) by The Inter Project Authors
  (SIL Open Font License 1.1), bundled in `crates/ui/assets/fonts` with a small
  modification described there.
- Icons: [Phosphor Icons](https://phosphoricons.com) through the
  [egui-phosphor](https://github.com/amPerl/egui-phosphor) crate (MIT license).

## License

Copyright © 2026 Christian Braghette.

InkNote is **dual-licensed**:

1. **Open source: GNU GPL v3 or later.**
   You can use, study, modify and redistribute InkNote under the terms of the
   [GNU General Public License](LICENSE), version 3 or (at your option) any
   later version. If you distribute InkNote or a work based on it, you must
   release it under the same license and make the complete source code
   available.

2. **Commercial license.**
   If you want to ship InkNote, or code derived from it, in a product without
   the GPL obligations (for example in closed-source software), you can buy a
   commercial license. Write to
   [christianbraghette@gmail.com](mailto:christianbraghette@gmail.com).

InkNote is distributed in the hope that it will be useful, but WITHOUT ANY
WARRANTY; without even the implied warranty of MERCHANTABILITY or FITNESS FOR
A PARTICULAR PURPOSE. See the [LICENSE](LICENSE) for details.
