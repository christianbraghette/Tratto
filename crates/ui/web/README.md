# Tratto in the browser (spike)

The desktop app's UI crate built for the web: the same `InkApp`, run by
eframe's web runner in a canvas, drawn with WebGPU (WebGL 2 where the
browser has no WebGPU). Started by `src/web.rs`.

This is a spike. It opens the editor on a new note kept in memory: nothing
is saved, and a page reload starts over.

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

Built but not tested yet: the eraser, selection, text boxes, undo, the pen's
eraser end and side button, WebGL 2 (browsers without WebGPU), and settings
kept in the browser's local storage by eframe.

## Not there yet

| What | Why | Next step |
|---|---|---|
| Library, opening and saving notes | notebooks are folders and files | storage behind an interface; the browser's private file system (OPFS) |
| File dialogs, import, export | `rfd` is async-only on the web | browser file pickers and downloads |
| Clipboard for pictures, pasting text | `arboard` is desktop-only | the browser's clipboard API |
| Printing | system print dialogs | export a PDF and print it from the browser |
| Background work (picture decoding, search, saving) | no threads | runs inline for now; Web Workers later |
| Updates, file associations, window frame | desktop features | not needed: a web app is as new as its page |

The wasm file is about 10 MB before `wasm-opt`.
