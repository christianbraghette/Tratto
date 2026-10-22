# Tratto

[![License: AGPL v3+](https://img.shields.io/badge/license-AGPL--3.0--or--later-blue.svg)](LICENSE)
[![Commercial license available](https://img.shields.io/badge/commercial%20license-available-green.svg)](#license)
![Rust 1.95+](https://img.shields.io/badge/rust-1.95%2B-orange.svg)

**Tratto** (Italian for *stroke*) is a fast, distraction-free handwriting app
for pen and tablet: take notes on paper that feels like paper, read PDFs and
write directly on them, and keep everything in plain files on your own disk.
It runs natively on Windows, macOS and Linux, built with Rust,
[egui](https://github.com/emilk/egui) and [wgpu](https://wgpu.rs).

- **Ink that keeps up with your hand.** Tratto is designed for low latency:
  strokes are pressure-sensitive, smoothed, anti-aliased on the GPU and,
  optionally, predicted slightly ahead of the pen tip.
- **Notes and PDFs in one place.** Write on blank, ruled or grid pages in any
  paper size, or open a PDF to read it with its table of contents and links,
  and import it to annotate it.
- **Organised, and never lost.** Notebooks with sections, quick notes and
  recent notes; every change is saved automatically with atomic writes,
  backups and a trash folder.
- **Yours to take anywhere.** Export any note as a crisp vector PDF. There is
  no account and no cloud: your library is an ordinary folder.

- [Tratto](#tratto)
  - [Features](#features)
  - [Getting Started](#getting-started)
    - [Requirements](#requirements)
    - [Build and run](#build-and-run)
    - [Tests](#tests)
  - [Using Tratto](#using-tratto)
    - [Library](#library)
    - [Window and menus](#window-and-menus)
    - [Editor](#editor)
    - [Settings](#settings)
    - [Keyboard shortcuts](#keyboard-shortcuts)
    - [Reading PDFs](#reading-pdfs)
    - [PDF export](#pdf-export)
    - [Printing](#printing)
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
- Eraser that rubs out only the ink under it, or whole strokes
- Lasso or rectangle selection to move or delete ink, text boxes and pictures;
  drag a corner of the selection to resize it, the handle above it to rotate
  ink, and pick a colour in the toolbar to recolour it
- Cut, copy and paste what is selected, within a note or into another one;
  other apps get the copied text, or a picture of the copied ink
- Text boxes for typed notes; while typing, drag a side of the box to change
  its width, or the knob above it to move it
- A pointer tool to move around a note as in the PDF reader: drag to scroll,
  tap to turn pages or follow the links of the PDF it is written on;
  its laser pointer, to present, leaves a fading red trail where you drag
- Native pen support on Wayland (pressure, eraser end, side button) and Windows (eraser end, side button);
  on X11, a pen side button reported as the middle mouse button also switches to the eraser

**Pages**
- A3, A4, A5, Letter, Legal or custom paper sizes, in portrait or landscape
- Blank, ruled or grid paper with adjustable line spacing
- A sidebar (left, or right for left-handed writers) with the notebook, page previews for navigating, reordering, inserting and deleting pages, the PDF's contents and search
- Search like VS Code, in the open note, its section or the whole notebook (`Ctrl+F`, `Ctrl+Shift+F`)
- Zoom presets, plus zooming with Ctrl + wheel or a touchpad pinch
- Smooth scrolling: wheel notches glide and speed up when you spin the wheel
  quickly, a pan flicked on the grey area keeps moving, and page jumps ease
  into place

**Interface**
- VS Code-style title bar with File, Edit, View, Page and Help menus (or the system title bar, if you prefer)
- Dark or light interface, or the same as the system (Settings > Appearance)
- Context menu on right-click, adapted to what is under the pointer
- Icon buttons, with names and shortcuts in tooltips
- Tools in a notch attached to the canvas edge, views in an always-visible sidebar strip; each sidebar view resizable
- Rename the note from the title bar and the notebook from the sidebar

**Organisation**
- A library of notebooks (with colour covers), sections and quick notes
- Recent notes; rename, duplicate, move, copy and paste, and delete notes
- Import existing `.ink` files
- Read PDFs without importing them: page turning, the PDF's table of
  contents and links, recently read PDFs in the library; import one to write on it
- Deleted items go to a trash folder instead of being erased

**Safety and export**
- Autosave a moment after every change, with atomic writes and periodic `.bak` backups
- Undo and redo
- Vector PDF export, with or without the ruled lines or grid
- Printing, with the same choices and preview, through the system's print dialog
- Save a page (with its paper, rules and PDF page) or a selection as a PNG picture

## Install

Download the file for your system from the
[latest release](https://github.com/christianbraghette/Tratto/releases):

| System | File | |
|---|---|---|
| Windows | `Tratto-x64.msi` | Installs for your user only, no administrator rights needed. |
| macOS (Apple silicon) | `Tratto-arm64.dmg` | Open it and drag Tratto into Applications. |
| Linux (x86-64) | `Tratto-x86_64.AppImage` | Make it executable (`chmod +x`) and run it. |

The `tratto-<target>.zip` files hold the portable app: unpack and run.
Tratto keeps itself up to date (menu > Check for updates...).

## Getting Started

### Requirements

- [Rust](https://www.rust-lang.org/tools/install) 1.95 or newer (Rust 2024 edition)
- A GPU and driver supported by wgpu (Vulkan, Metal, DirectX 12 or OpenGL)
- **Linux**: the Wayland and xkbcommon development packages, for example
  `sudo apt install libwayland-dev libxkbcommon-dev` on Debian/Ubuntu.
  Tratto runs on both Wayland and X11, but native pen pressure needs Wayland.

### Build and run

```sh
git clone https://github.com/christianbraghette/Tratto.git
cd Tratto
cargo run --release
```

The optimized executable is written to `target/release/tratto` (`tratto.exe` on Windows).

### Tests

```sh
cargo test --workspace
```

Use `--workspace`: a plain `cargo test` only runs the tests of the UI crate.

## Using Tratto

### Library

Tratto opens on the library screen. From there you can:

- create a **quick note** (**+ New note**) or a **notebook** (**+ New notebook**)
- open a notebook to see its **sections** (**+ Section**) and the notes inside them
- rename, duplicate, move or delete a note from its menu; **Copy** or **Cut**
  it there, then **Paste note here** on a notebook or section (or `Ctrl+V` in the
  notebook or section shown)
- **Import** an existing `.ink` file into the library
- choose a different **Library folder**, or **Refresh** after an external sync tool changes it

### Window and menus

Like VS Code, Tratto draws its own title bar. It holds the menus, the
window title and the minimize, maximize and close buttons. Drag the title bar
to move the window, double-click it to maximize, and drag the window edges to
resize.

| Menu | Contents |
| --- | --- |
| **File** | New note, new notebook, import, open a PDF to read, write on a PDF, save, export as PDF, print, close note, library folder, settings, exit |
| **Edit** | Undo, redo, cut, copy, paste, select all on page, delete selection, deselect |
| **View** | zoom in / out, actual size, fit width / page, toolbar position, full screen, present |
| **Page** | Insert a page before / after the current one, delete it, copy, cut or paste a page, save it as PNG, paper style, page setup |
| **Help** | Keyboard shortcuts, about Tratto |

### Editor

The tools sit in a notch attached to the edge of the canvas, the views in
the sidebar's strip along the window edge. All buttons are icons; hover over
one to see its name.

| Where | Buttons |
| --- | --- |
| Top-centre notch (or bottom, see **View**) | Pointer, Pen, Highlighter, Eraser, Select, Text, the active tool's colours and size (for the pointer: contents, Back and the pages), Undo, Redo, zoom out / zoom level / zoom in |
| Sidebar strip | **Notebook**, **Pages**, **Contents** (for a note written on a PDF), **Search**, **Note settings** (paper, pages, export) |

The sidebar is on the left by default. Left-handed writers can move it to the
right, out from under their hand (**View > Sidebar on the right**, or
**Settings > Appearance > Sidebar**).

If the window is too narrow for the tool notch to fit at its edge, it moves
to the other edge.

The sidebar works like the one in VS Code: its strip is always there, with a
button per view. Click a button to show that view next to the strip, or the
highlighted one to hide it again. In full screen the tool notch stays and
the sidebar waits behind a small notch in its top corner: click it to
show the sidebar, and the highlighted view to hide it again. The title bar
slides in from the top edge. On a touch screen, swipe down from the top edge
for the title bar, or in from the sidebar's edge for the sidebar.

Each view keeps its own width (drag its inner edge), and has a context menu
(right-click):

- **Notebook**: the sections and notes of the open note's notebook, with the
  open note highlighted. Click a note to switch to it (the current one is
  saved first). The header is the notebook's name: click it to rename the
  notebook. Right-click a note to open, rename, duplicate or delete it, a
  section to add a note to it, rename or delete it, and the empty space to add
  a note or a section. A quick note (outside any notebook) shows the quick
  notes instead. Each group shows how many notes it holds; the dashed tile
  adds a note next to the open one. Notes are listed in the order they were
  created; drag a note within its group to reorder it (the order is saved in a
  hidden `.order` file in the folder, and the library screen uses it too). The
  button in the header opens the full library screen.
- **Pages**: the page previews (drag to reorder; right-click a page to go to
  it, insert a page before or after it, copy or cut it, paste copied pages
  after it, or delete it). The list follows the page you are on, and the
  dashed tile at its end adds a page.
- **Search** (`Ctrl+F` for the note, `Ctrl+Shift+F` for the notebook): one
  field, and radio buttons for where to search: the open **Note**, its
  **Section** (for a note directly in the notebook, the notes of "General")
  or its whole **Notebook** (for a quick note, the quick notes). It searches
  the typed text and the text of the PDF a note is written on, as you type;
  handwriting is not searchable. While the view is shown, the open note's
  matches are highlighted on its pages, the current one stronger, and Enter
  or `F3` goes to the next one, Shift+Enter or `Shift+F3` to the previous.
  The note lists its matches by page. A section or notebook lists them by
  note, each with its page and the words around it; click a note's header to
  fold it, and a result to open its note at that match (the open note is
  saved first). The open note is searched as it is now, saved or not; the
  refresh button searches again after you change other notes. Text selected
  when you press `Ctrl+F` becomes the query.
- **Note settings**: see **Settings** below.

The note's title is shown in the middle of the title bar. Click it to rename
the note; Enter or clicking elsewhere saves, Escape cancels.

To pan, drag on the grey area around the pages, or use the scrollbars, the
mouse wheel or a touchpad. Flick the drag and the view keeps gliding.

**View > Present** (`F5`) shows the note or the PDF to an audience: full
screen with the page alone, fitted whole (no sidebar or title bar). Turn the
pages with the arrow keys, `Space`, `Page Up` / `Page Down`, the wheel or a
tap on either side of the page; nothing is written. A small bar, on the
toolbar's edge, slides in when the pointer touches that edge of the screen
(or on a swipe in from it): the laser pointer, Back, the pages, the zoom and
**End presentation**. Presenting starts at **Fit page**; zooming there
(also with `Ctrl` + wheel or a pinch) leaves the note's own zoom as it was. `L` turns the laser on or off, and `Escape`, `F5` or the
right-click menu end it too.

### Settings

There are two kinds of settings:

- **Note settings** (the gear button in the sidebar's strip) are
  stored in the note: paper size and orientation, paper style and line
  spacing, and the pages. They also show when the note was created and
  modified and where it is, with **Save now** and **Export PDF**.
- **Settings** (**File > Settings**, `Ctrl+,`, also on the library screen)
  apply to every note: interface size, title bar, toolbar position and sidebar side; smooth
  scrolling and scroll speed; pen tip prediction, pressure sensitivity, the
  pen side button and the eraser size; the paper of new notes; the autosave
  delay; the library folder and the number of recent notes. **Reset to defaults** restores them (the library
  folder and the panels are kept).

Notes save automatically a moment after each change (see **Settings**), so you
don't need to save before closing. **File > Close note** (`Ctrl+W`) saves the
note and goes back to the library screen.

### Keyboard shortcuts

| Shortcut | Action |
| --- | --- |
| `Ctrl+N` | New note (in the same notebook or section) |
| `Ctrl+O` | Open a PDF to read |
| `Ctrl+S` | Save now |
| `Ctrl+E` | Export the note as PDF (opens the export dialog) |
| `Ctrl+P` | Print (opens the print dialog; a PDF in the reader prints as it is) |
| `Ctrl+W` | Close the note and go back to the library screen |
| `Ctrl+Z` | Undo |
| `Ctrl+Y` or `Ctrl+Shift+Z` | Redo |
| `Ctrl+A` | Select everything on the current page |
| `Ctrl+X` / `Ctrl+C` | Cut / copy the selection (or copy the selected text) |
| `Ctrl+V` | Paste: at the pointer when it is over a page, else on the current page |
| `Delete` or `Backspace` | Delete the selection |
| `Escape` | Clear the selection, or close the context menu |
| `Ctrl+F` | Search the note |
| `F3` / `Shift+F3` | Next / previous match |
| `Ctrl+Shift+F` | Search the notebook |
| `Ctrl+Plus` / `Ctrl+Minus` | Zoom in / out |
| `Ctrl+0` | Actual size (100 %) |
| `Ctrl` + mouse wheel, or pinch | Zoom around the pointer |
| `Page Up` / `Page Down` | Scroll by one screen (animated) |
| `Home` / `End` | Go to the first / last page |
| `F11` | Toggle fullscreen |
| `F5` / `Escape` | Present / end presenting |
| `L` | Laser pointer, while presenting |
| `Ctrl+,` | Settings |
| `Ctrl+Q` | Exit |

On macOS, use `Cmd` instead of `Ctrl`. The list is also under **Help >
Keyboard shortcuts**.

### Reading PDFs

**File > Open PDF...** (`Ctrl+O`), **Read a PDF** on the library's home
screen, or dropping a PDF onto the window opens it to read, as it is: nothing
can be written on it, and it is not copied into the library.

- Drag anywhere (pen, finger or mouse) to scroll. Tap the left or right
  quarter of the page, or press `Left` / `Right`, `Space` / `Shift+Space` or
  `Page Up` / `Page Down`, to turn the page. A page taller than the window is
  shown in steps first, so nothing is skipped.
- **Contents** (in the sidebar) shows the PDF's outline; a click goes
  there, and the part being read is marked.
- The **laser pointer** in the bar, to present: with it on, drag on the page
  to leave a fading red trail (dragging no longer scrolls; a finger still
  does).
- Links in the PDF can be tapped: to another page, or a web address (opened
  in the browser). **Back** (`Alt+Left` or `Backspace`) returns to where you
  were.
- **Import** turns the PDF into a note to write on, in the notebook or
  section open in the library, and opens it at the same page.

Recently read PDFs are listed on the library's home screen and reopen where
you left them.

In a note, the **Pointer** tool (first in the toolbar) moves around the same
way: drag to scroll, tap or use the keys above to turn pages, and, on a note
written on a PDF, follow its links and open its contents. It writes nothing;
pick a pen or another tool to write again.

### PDF export

`Ctrl+E` (**File > Export as PDF**, or **Export PDF** in the note settings)
opens the export dialog, with a live preview of the exported pages:

- **Export**: this note (all pages, the current one, or a range), every note of
  its section, or a selection of the notebook's notes. Several notes go into
  one PDF, in the notebook view's order, with a bookmark per note
- **Content**: ruled lines and grid (not over a PDF's pages), highlighter
  strokes, text boxes, and for notes written on a PDF its pages under the ink
- **Ink colours**: as drawn, black (highlighter in grey) or grayscale
- **Page size**: the note's own, or a standard size (A4, Letter, ...) with each
  page scaled to fit and centred, with an adjustable margin
- **Document**: the PDF's title and author
- **File**: where to write it (next to the note, named after its title, by
  default) and whether to open it afterwards

The dialog remembers your choices, except what to export, the title and the
file.
Each exported note page becomes one PDF page. Ink is exported as vector paths
that follow the same smoothed curves as on screen.

### Printing

`Ctrl+P` (**File > Print...**) opens the same dialog as the export, to print:
what to print (this note, its pages, its section or selected notes), the
content and the ink colours, with the preview. **Print...** then opens the
system's print dialog, to choose the printer and the copies. Each page is
scaled to fit the printer's paper and centred, and turned a quarter if it lies
the other way round than the paper. A PDF open in the reader goes straight to
the system's print dialog, all of it.

- **Windows**: the system's Print dialog; Tratto draws the pages for the
  printer at its resolution (up to 300 dpi).
- **Linux**: the desktop's print dialog (through the desktop portal, as on
  GNOME and KDE); without one, the default printer through `lp`.
- **macOS**: the PDF opens in Preview, to print from there (`Cmd+P`).

## Your Data

Notes are plain files in a plain folder, so they work with any backup or
sync tool, and one damaged note cannot affect the others. The default library
folder is `Documents/Tratto` in your home folder:

```text
Tratto/
├── Quick note.ink          quick notes
├── My notebook/            a notebook
│   ├── .notebook           cover colour
│   ├── .order              manual order of the notes (if you reordered them)
│   ├── Section 1/          a section
│   │   └── Some note.ink
│   └── Loose note.ink      notes outside any section ("General")
└── .trash/                 deleted items
```

- **Format**: `.ink` is a versioned binary format. It stores a small header
  (title, dates, preview), the PDF the note is written on (if any), the
  document (pages, strokes, text boxes, paper settings) and a journal of the
  changes made since. Notes made by pre-beta builds of Tratto (format
  versions before 6) are not supported.
- **Safety**: an autosave only appends what changed to the journal, so it
  stays small however large the note or its PDF is. Every journal entry and
  header carries a checksum, so a crash while saving loses at most that last
  save. Once the journal has grown large, the whole note is written again:
  to a temporary file, then renamed over the note, with the previous version
  copied to `<note>.ink.bak` at most every ten minutes.
- **Deleting** moves items to `.trash/` inside the library folder. To
  recover something, move it back by hand.

UI preferences (toolbar position, tool styles, zoom, library folder) are stored
separately by eframe in the platform's application-data folder.

## Configuration

By default Tratto presents frames without waiting for vsync, for the lowest
latency, capped at 120 fps so the GPU does not render frames that are never
shown. Both are set in **File > Settings > Display**, and apply at once. The
**battery saver** there turns vsync on to save power: by default when the
computer runs on battery at 20% or less (or whenever it is on battery,
always, or never). These environment variables override
the settings:

| Variable | Effect |
| --- | --- |
| `TRATTO_FPS=144` | Frame rate cap without vsync, instead of the setting (`0` removes the cap) |
| `TRATTO_VSYNC=1` | Force vsync on: may save power, but adds latency |
| `TRATTO_MSAA=4` | Enable 4× MSAA (default `1`, off). Strokes are already anti-aliased, so this is rarely needed. |
| `WGPU_POWER_PREF=high` | Use the discrete GPU on a laptop with two. By default Tratto uses the integrated one, which saves battery. |

```sh
# Linux / macOS
TRATTO_VSYNC=1 cargo run --release
```

```powershell
# Windows PowerShell
$env:TRATTO_VSYNC = "1"; cargo run --release
```

## Project Structure

The repository is a Cargo workspace that separates the backend from the UI:

```text
crates/
├── core/   tratto-core: backend library, no UI dependencies
│           model, history (undo/redo), stroke_builder, stroke_path,
│           storage + journal (.ink files), library (notebooks/notes),
│           pdf_export, pdf_pages (PDFium)
└── ui/     tratto-ui: the `tratto` executable (egui + wgpu)
            app (editor, library, menus), chrome (title bar), theme (design
            system), view, render, stroke_mesh, stroke_gpu (strokes kept on
            the GPU), pdf_view, toolbar, kinetic (scrolling), scrollbar,
            settings, saver (background saves), update, input (Wayland
            tablet / Windows pen)
```

`tratto-core` depends only on `emath` (the geometry types egui is built on),
`serde`, `bincode`, `flate2` and `pdfium-render`. It can be tested and reused
without a window or GPU. The UI crate depends on the core, never the other way
around. PDFium itself is loaded at runtime: see
[crates/ui/pdfium/README.md](crates/ui/pdfium/README.md) to fetch it for
development builds.

## Contributing

Contributions are welcome! Please read [CONTRIBUTING.md](CONTRIBUTING.md)
first.

Before your first pull request can be merged, you sign the
[Contributor License Agreement](CLA.md) (CLA). A bot comments on your pull
request with a link to it; you sign by replying with one sentence. You sign
once, and it covers all your later contributions.

**Why a CLA?** Tratto is [dual-licensed](#license). Without a CLA, your code
would reach the project under the AGPL only, and it could never be part of the
commercially licensed version. Every outside contribution would split the
codebase in two. The CLA gives the maintainer the extra permission needed to
ship your contribution under both licenses.

**What it means for you:**

- **You keep the copyright** in your work. The CLA is a license, not a
  transfer: you can still use, publish and relicense your own code however you
  like.
- **You allow** the maintainer to distribute your contribution under the AGPL
  *and* under commercial licenses ([CLA, Section 2](CLA.md#2-copyright-license)).
- **You confirm** that the work is yours to give: you wrote it, or you have
  the right to submit it, and your employer agrees if they own what you write
  ([Section 5](CLA.md#5-your-representations)).
- **In return**, everything accepted into Tratto stays available to everyone
  as free software, for as long as the project is distributed
  ([Section 4](CLA.md#4-commitment-to-keep-the-project-free-software)). Your
  contribution can never become closed-source only.

## Acknowledgements

- Interface font: [Inter](https://rsms.me/inter/) by The Inter Project Authors
  (SIL Open Font License 1.1), bundled in `crates/ui/assets/fonts` with a small
  modification described there.
- Icons: [Phosphor Icons](https://phosphoricons.com) through the
  [egui-phosphor](https://github.com/amPerl/egui-phosphor) crate (MIT license).

## License

Copyright © 2026 Christian Braghette.

Tratto is **dual-licensed**:

1. **Open source: GNU AGPL v3 or later.**
   You can use, study, modify and redistribute Tratto under the terms of the
   [GNU Affero General Public License](LICENSE), version 3 or (at your option)
   any later version. This is free for everyone, as long as you keep the
   license's conditions:
   - If you distribute Tratto or a work based on it, you must release the
     whole work under the AGPL and provide its complete source code.
   - If you modify Tratto and let people use it over a network (for example
     as a hosted service), you must offer those users the source code of your
     modified version too.

2. **Commercial license.**
   If you want to use Tratto's code in a project without those obligations,
   for example in a closed-source product, or in a service whose source you
   do not want to publish, you need a commercial license. Write to
   [info@tratto.page](mailto:info@tratto.page).

In short: using Tratto to take your own notes, at home or at work, is free.
Building it into a product or service that you do not release under the AGPL
requires a commercial license.

Tratto is distributed in the hope that it will be useful, but WITHOUT ANY
WARRANTY; without even the implied warranty of MERCHANTABILITY or FITNESS FOR
A PARTICULAR PURPOSE. See the [LICENSE](LICENSE) for details.
