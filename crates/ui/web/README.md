# Tratto in the browser

The desktop app's UI crate built for the web: the same `InkApp`, run by
eframe's web runner in a canvas, drawn with WebGPU (WebGL 2 where the
browser has no WebGPU). Started by `src/web.rs`.

A work in progress: the library and the editor work, with the notes kept
in the browser (see *Storage*); what the desktop asks the system for is
still missing (see *Not there yet*).

## Build and run

```sh
scripts/build-web.sh          # once: fetches PDFium's WASM build (crates/web/pdfium/)
scripts/build-web-app.sh --serve
```

Then open <http://localhost:8001>. On Windows, run the scripts with `bash`
(Git Bash). The build needs the same tools as the web reader: the
`wasm32-unknown-unknown` target and `wasm-bindgen-cli` (see
[crates/web/README.md](../../web/README.md)).

## What works

Tested in Edge (WebGPU):

- the editor's UI: title bar, menus, side panel, toolbar
- pen and highlighter strokes, through the GPU stroke pipeline
  (`stroke_gpu`); the highlighter's darken blend keeps the ink under it
- pen pressure from the browser's Pointer Events (`input/web_pen.rs`);
  eframe takes a pen for a mouse, the pressure comes from there
- the library: a quick note created, written in, saved, and opened again
  with its strokes after the page was reloaded

Built but not tested yet: the eraser, selection, text boxes, undo, the pen's
eraser end and side button, WebGL 2 (browsers without WebGPU), and settings
kept in the browser's local storage by eframe.

## Not there yet

| What | Why | Next step |
|---|---|---|
| File dialogs, import, export | `rfd` is async-only on the web | browser file pickers and downloads |
| Clipboard for pictures, pasting text | `arboard` is desktop-only | the browser's clipboard API |
| Printing | system print dialogs | export a PDF and print it from the browser |
| Background work (picture decoding, search, saving) | no threads | runs inline for now; Web Workers later |
| Updates, file associations, window frame | desktop features | not needed: a web app is as new as its page |

The wasm file is about 10 MB before `wasm-opt`.

## Storage

Notebooks stay folders and notes stay `.ink` files, in the browser's
private storage for the page's site (the origin private file system,
OPFS). The page can only reach it asynchronously, and the app's file code is
synchronous, so:

- the app's file system is `tratto_core::fs`: `std::fs` on the desktop, a
  tree in memory on the web (`fs/memory.rs`), with the same functions;
- before the app starts, `src/web_storage.rs` reads the whole storage into
  that tree; afterwards it writes back what changed, every second and when
  the page is hidden.

Things to know:

- **Per browser and per site.** Notes made in one browser (or at another
  address) are not in another. Moving notes in and out needs import and
  export (not there yet).
- **The last second.** A change made just before the tab is closed may not
  reach the storage.
- **Whole files.** A note appended to is written again whole; a large note
  written on a PDF costs that much every save.
- **Kept, mostly.** The page asks the browser to keep the storage
  (`navigator.storage.persist()`); a browser may still clear it if the user
  clears the site's data.
- **Everything in memory.** The whole library is read at start: fine for
  notes, heavy for many large PDFs. Later: read files when opened.
