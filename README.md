# InkNote

InkNote is a minimal, cross-platform handwriting note-taking app built with Rust, egui, and wgpu. It provides a paged paper canvas for handwriting and typed notes, with pen input, editing tools, undo/redo, native tablet support, and vector PDF export.

## Features

- Pen and highlighter tools with configurable colors and stroke widths
- Pressure-aware pen input, smoothed strokes, and optional pen-tip prediction
- Stroke eraser and lasso selection for moving or deleting ink
- Text boxes for typed notes
- Multiple pages with A3, A4, A5, Letter, Legal, and custom page sizes
- Blank, ruled, and grid paper styles
- Page previews, navigation, reordering, insertion, and deletion
- Zoom presets and zooming with the mouse wheel or a touchpad pinch
- Undo and redo
- Native tablet input support on Wayland and Windows
- Save and reopen `.ink` documents; export pages as vector PDF

## Requirements

- Rust toolchain with Cargo (Rust 1.85 or newer for the Rust 2024 edition)
- A graphics-capable desktop environment supported by wgpu

On Linux, the application uses eframe's Wayland and X11 backends. Native tablet input is available when running under Wayland; mouse input remains available on other supported desktop backends. On Windows, pen side-button and eraser-end input are supported where available.

## Build and Run

Run from the project directory:

```sh
cargo run
```

Build an optimized executable with:

```sh
cargo build --release
```

Run the unit tests with:

```sh
cargo test
```

## Using InkNote

The floating toolbar provides the Pen, Highlighter, Eraser, Select, and Text tools, along with undo/redo, the page preview toggle, and Settings.

The Settings window lets you choose the toolbar position, zoom, paper size and dimensions, page style and line spacing, save/load paths, and PDF export options. The default document path is `notes.ink`; the default PDF path is `notes.pdf`. If the configured `.ink` file exists when InkNote starts, it is loaded automatically. Use Settings to change either path. Save and Open operate on the configured document path, and New starts a blank document.

### Keyboard shortcuts

| Shortcut | Action |
| --- | --- |
| `Ctrl+Z` | Undo |
| `Ctrl+Y` or `Ctrl+Shift+Z` | Redo |
| `Ctrl+S` | Save to the configured `.ink` path |
| `Ctrl+E` | Export to the configured PDF path |
| `Delete` or `Backspace` | Delete the current selection |
| `Escape` | Clear the current selection |
| `Page Up` / `Page Down` | Scroll between pages |
| `F11` | Toggle fullscreen |
| `Ctrl` + mouse wheel, or pinch | Zoom around the pointer |

### Files and export

InkNote stores documents in its versioned binary `.ink` format. This format preserves pages, strokes, text boxes, and paper settings; version 2 documents are migrated when loaded. PDF export creates one PDF page per note page and preserves vector paths. Ruled or grid lines can be included or omitted in the export settings.

## Rendering Options

By default, InkNote favors low-latency presentation without waiting for vertical synchronization. Set these environment variables before launching to change that behavior:

| Variable | Effect |
| --- | --- |
| `INKNOTE_VSYNC=1` | Enable vsync; may reduce power use while adding presentation latency |
| `INKNOTE_MSAA=4` | Enable 4x multisample anti-aliasing; the default is 1 (off) |

For example, on PowerShell:

```powershell
$env:INKNOTE_VSYNC = "1"
cargo run
```