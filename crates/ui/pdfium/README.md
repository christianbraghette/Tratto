# PDFium

Writing on PDFs uses [PDFium](https://pdfium.googlesource.com/pdfium/), the
PDF engine of Chrome (BSD-3-Clause / Apache-2.0), through the
[`pdfium-render`](https://github.com/ajrcarey/pdfium-render) crate. The
library is loaded at runtime, so it is not in git: fetch the prebuilt binary
for your platform from
[pdfium-binaries](https://github.com/bblanchon/pdfium-binaries) (MIT), release
**chromium/7881** (the version `pdfium-render`'s `pdfium_latest` feature is
built for), and unpack it into a folder named after the archive:

```sh
cd crates/ui/pdfium
mkdir win-x64 && curl -L https://github.com/bblanchon/pdfium-binaries/releases/download/chromium%2F7881/pdfium-win-x64.tgz | tar -xz -C win-x64
```

| Platform | Archive | Library |
|---|---|---|
| Windows x64 | `pdfium-win-x64.tgz` | `win-x64/bin/pdfium.dll` |
| Windows ARM64 | `pdfium-win-arm64.tgz` | `win-arm64/bin/pdfium.dll` |
| Linux x64 | `pdfium-linux-x64.tgz` | `linux-x64/lib/libpdfium.so` |
| Linux ARM64 | `pdfium-linux-arm64.tgz` | `linux-arm64/lib/libpdfium.so` |
| macOS Apple silicon | `pdfium-mac-arm64.tgz` | `mac-arm64/lib/libpdfium.dylib` |
| macOS Intel | `pdfium-mac-x64.tgz` | `mac-x64/lib/libpdfium.dylib` |

Only the library and the `LICENSE` / `licenses/` files are needed; the
headers and import libraries can be deleted.

`build.rs` copies the library next to the built executable (`target/debug`,
`target/release`), where the app looks for it; a release ships it next to
`tratto.exe` together with its license files. `TRATTO_PDFIUM=<path>`
points the app at another copy. Without the library the app works as
before, only PDFs cannot be imported or shown.
