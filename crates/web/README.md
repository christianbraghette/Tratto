# Tratto web reader

Opens Tratto notes (`.ink`) and PDFs in the browser. It is the app's own code
compiled to WebAssembly:

- notes are read by `tratto-core`'s `storage` (snapshot and journal), as the
  app opens them; a PDF becomes a note written on it, as the app imports one
- pages are painted on a canvas in the app's order and colours, with the
  same smoothed stroke curve (`stroke_path`) the app draws and exports;
  highlighters use the darken blend, as in the app
- PDF pages are rendered by PDFium, the same engine the app uses, here as a
  WASM build
- "Export PDF" is the app's vector export (`pdf_export`), with the PDF a note
  is written on placed under its pages

Files are read in the browser and never uploaded. Read only: nothing is
saved back to the note.

Text boxes are drawn from their stored lines (their text as shown, wrapped
as in the app) without their Markdown styles, which need the app's text
layout. PDF exports made here have the same limitation.

## Build

```sh
rustup target add wasm32-unknown-unknown
cargo install wasm-bindgen-cli --version 0.2.129 --locked   # the version in Cargo.lock
scripts/build-web.sh --serve
```

Then open <http://localhost:8000>. On Windows, run it from PowerShell with
`bash scripts/build-web.sh --serve` (Git Bash's `bash`), or from Git Bash.

To test a build again without rebuilding, serve `crates/web/dist/` with any
web server, for example (from the repository root):

```sh
python -m http.server 8000 --directory crates/web/dist
```

Opening `index.html` straight from the disk (`file://`) does not work: the
browser refuses to load its modules and fonts from there.

`crates/web/dist/` is a static site you can put on any web server:

```text
dist/
├── index.html, main.js   the page (from www/)
├── pkg/                  tratto_web.js + tratto_web_bg.wasm (wasm-bindgen)
├── pdfium/               pdfium.js + pdfium.wasm
├── fonts/                Inter, for text boxes (from crates/ui/assets)
└── licenses/
```

`?file=<url>` opens a file from a URL on load. The server hosting that file
must allow requests from the reader's origin (CORS).

## PDFium

The script fetches PDFium's WASM build into `crates/web/pdfium/` (not in git)
from [pdfium-lib](https://github.com/paulocoutinhox/pdfium-lib) (MIT),
release **7902**, along with PDFium's license. pdfium-lib has no WASM build
of chromium/7881, the release the desktop app uses, so this is the closest
newer one. Its API covers what `pdfium-render`'s `pdfium_7881` feature binds.
pdfium-lib is used instead of
[pdfium-binaries](https://github.com/bblanchon/pdfium-binaries) because its
WASM heap can grow, so large PDFs open (see `pdfium-render`'s README).

The page starts PDFium and binds it with `initialize_pdfium_render` (see
`www/main.js`). On WebAssembly, `tratto-core`'s `pdf_pages` runs PDFium on the
calling thread instead of a worker thread (the browser gives it no other).
Without PDFium, notes still open: the PDF pages under them show as grey
placeholders, with a notice explaining why.

## API

`src/lib.rs` exports one class, `Reader`:

```js
import init, { Reader, initialize_pdfium_render } from "./pkg/tratto_web.js";

const reader = new Reader(bytes, "Lecture.ink"); // Uint8Array, file name
reader.title; reader.isNote; reader.pageCount;
reader.pageWidth; reader.pageHeight;             // CSS pixels (1/96 in)
const missing = reader.render(page, canvas, devicePixelRatio * zoom);
const pdf = reader.exportPdf();                  // Uint8Array
```

`render` sizes the canvas to the page and returns a message describing
anything it could not show, or `undefined` when everything was drawn.

## Tests

```sh
cargo test -p tratto-web
```
