// SPDX-FileCopyrightText: 2026 Christian Braghette
// SPDX-License-Identifier: AGPL-3.0-or-later

// The reader's page: opens a file, lays its pages out and has the WASM
// module (`Reader`, see ../src/lib.rs) paint those in view.
//
// PDFium (pdfium/pdfium.js and .wasm, see ../README.md) is bound once at
// start: without it notes still open, only the PDF pages under them (and
// PDFs) cannot be shown.

import init, { Reader, initialize_pdfium_render } from "./pkg/tratto_web.js";

const ZOOM_MIN = 0.25;
const ZOOM_MAX = 5;
const ZOOM_STEP = 1.25;
// Pages this far out of view (in view heights) are painted ahead; further
// out their pixels are released.
const AHEAD = 1;
// The gap between pages, and around them (as in index.html).
const GAP = 16;

const $ = (id) => document.getElementById(id);
const ui = {
  file: $("file"),
  title: $("title"),
  zoomIn: $("zoom-in"),
  zoomOut: $("zoom-out"),
  zoomLabel: $("zoom-label"),
  fit: $("fit"),
  pagesLabel: $("pages-label"),
  exportPdf: $("export"),
  notice: $("notice"),
  view: $("view"),
  pages: $("pages"),
  empty: $("empty"),
};

let reader = null;
let zoom = 1;
let fitting = true;
let pdfium = "not loaded";
// Page elements, and which are painted at the current zoom.
let pageEls = [];
const painted = new Set();

function notify(text) {
  ui.notice.textContent = text || "";
  ui.notice.classList.toggle("shown", Boolean(text));
}

/** Loads PDFium's script (a classic one, defining `PDFiumModule`). */
function loadScript(src) {
  return new Promise((resolve, reject) => {
    const s = document.createElement("script");
    s.src = src;
    s.onload = resolve;
    s.onerror = () => reject(new Error(`${src} is missing`));
    document.head.append(s);
  });
}

async function start() {
  const rust = await init();
  try {
    await loadScript("pdfium/pdfium.js");
    const module = await globalThis.PDFiumModule();
    pdfium = initialize_pdfium_render(module, rust, false) ? "ready" : "failed to start";
  } catch (e) {
    pdfium = e.message;
    console.warn("PDFium is not available:", e);
  }
  // Text boxes are drawn in the app's font: wait for it, not a fallback.
  await document.fonts.load("16px Inter").catch(() => {});

  ui.file.addEventListener("change", () => ui.file.files[0] && openFile(ui.file.files[0]));
  addEventListener("dragover", (e) => {
    e.preventDefault();
    document.body.classList.add("dragging");
  });
  addEventListener("dragleave", (e) => e.relatedTarget === null && document.body.classList.remove("dragging"));
  addEventListener("drop", (e) => {
    e.preventDefault();
    document.body.classList.remove("dragging");
    const file = e.dataTransfer.files[0];
    if (file) openFile(file);
  });
  ui.zoomIn.addEventListener("click", () => setZoom(zoom * ZOOM_STEP));
  ui.zoomOut.addEventListener("click", () => setZoom(zoom / ZOOM_STEP));
  ui.fit.addEventListener("click", () => setZoom(fitZoom(), true));
  ui.exportPdf.addEventListener("click", exportPdf);
  addEventListener("keydown", (e) => {
    if (!reader || e.target instanceof HTMLInputElement) return;
    if (e.key === "+" || e.key === "=") setZoom(zoom * ZOOM_STEP);
    else if (e.key === "-") setZoom(zoom / ZOOM_STEP);
    else if (e.key === "0") setZoom(fitZoom(), true);
  });
  ui.view.addEventListener("scroll", update, { passive: true });
  addEventListener("resize", () => fitting && reader && setZoom(fitZoom(), true));

  // `?file=<url>`: opens that file (it must allow being fetched from here).
  const url = new URLSearchParams(location.search).get("file");
  if (url) {
    try {
      const response = await fetch(url);
      if (!response.ok) throw new Error(`${response.status} ${response.statusText}`);
      const name = decodeURIComponent(new URL(url, location.href).pathname.split("/").pop() || "file");
      openBytes(new Uint8Array(await response.arrayBuffer()), name);
    } catch (e) {
      notify(`Cannot fetch ${url}: ${e.message}`);
    }
  }
}

async function openFile(file) {
  openBytes(new Uint8Array(await file.arrayBuffer()), file.name);
}

function openBytes(bytes, name) {
  let opened;
  try {
    opened = new Reader(bytes, name);
  } catch (e) {
    const why = e.message || String(e);
    notify(why.includes("PDF support") ? `${why}. PDFium: ${pdfium}.` : `Cannot open ${name}: ${why}`);
    return;
  }
  if (reader) reader.free();
  reader = opened;
  notify("");
  document.title = `${reader.title} – Tratto Reader`;
  ui.title.textContent = reader.title;
  ui.empty.hidden = true;
  for (const b of [ui.zoomIn, ui.zoomOut, ui.fit]) b.disabled = false;
  ui.exportPdf.disabled = !reader.isNote;
  warned = false;
  layOut();
  ui.view.scrollTop = 0;
  setZoom(fitZoom(), true);
}

/** One element per page, painted when it comes into view (`update`). */
function layOut() {
  ui.pages.replaceChildren();
  painted.clear();
  pageEls = [];
  for (let i = 0; i < reader.pageCount; i++) {
    const el = document.createElement("div");
    el.className = "page";
    el.append(document.createElement("canvas"));
    ui.pages.append(el);
    pageEls.push(el);
  }
}

/** Paints the pages in view (and those `AHEAD` of it), releases the others. */
function update() {
  if (!reader) return;
  const step = shown(reader.pageHeight) + GAP;
  const margin = ui.view.clientHeight * AHEAD;
  const top = ui.view.scrollTop - GAP - margin;
  const bottom = ui.view.scrollTop + ui.view.clientHeight + margin;
  const first = Math.max(0, Math.floor(top / step));
  const last = Math.min(pageEls.length - 1, Math.floor(bottom / step));
  for (const i of painted) if (i < first || i > last) release(i);
  for (let i = first; i <= last; i++) paint(i);
  const middle = ui.view.scrollTop + ui.view.clientHeight / 2;
  const current = Math.min(pageEls.length - 1, Math.max(0, Math.floor((middle - GAP) / step)));
  ui.pagesLabel.textContent = `${current + 1} / ${reader.pageCount}`;
}

let warned = false;

function paint(i) {
  if (painted.has(i) || !reader) return;
  painted.add(i);
  const canvas = pageEls[i].firstChild;
  try {
    const missing = reader.render(i, canvas, zoom * devicePixelRatio);
    if (missing && !warned) {
      warned = true;
      notify(missing.includes("PDF support") ? `${missing}. PDFium: ${pdfium}.` : `Not everything can be shown: ${missing}.`);
    }
  } catch (e) {
    notify(`Page ${i + 1} cannot be shown: ${e.message || e}`);
  }
}

/** Frees the pixels of a page well out of view. */
function release(i) {
  painted.delete(i);
  const canvas = pageEls[i].firstChild;
  canvas.width = 0;
  canvas.height = 0;
}

/**
 * A page length in CSS pixels at the current zoom, as large as the canvas
 * `render` makes (whole device pixels): a canvas stretched by a fraction of
 * a pixel is blurred all over.
 */
function shown(length) {
  return Math.max(1, Math.round(length * zoom * devicePixelRatio)) / devicePixelRatio;
}

function fitZoom() {
  const room = ui.view.clientWidth - 32;
  return Math.min(1.5, Math.max(ZOOM_MIN, room / reader.pageWidth));
}

function setZoom(z, fit = false) {
  if (!reader) return;
  // Keep the same part of the document in view.
  const anchor = ui.view.scrollTop / Math.max(1, ui.pages.scrollHeight);
  zoom = Math.min(ZOOM_MAX, Math.max(ZOOM_MIN, z));
  fitting = fit;
  ui.zoomLabel.textContent = `${Math.round(zoom * 100)}%`;
  const w = `${shown(reader.pageWidth)}px`;
  const h = `${shown(reader.pageHeight)}px`;
  for (const el of pageEls) {
    el.style.width = w;
    el.style.height = h;
  }
  ui.view.scrollTop = anchor * ui.pages.scrollHeight;
  // Repainted at the new zoom: those in view now, the others when they come.
  painted.clear();
  update();
}

function exportPdf() {
  let bytes;
  try {
    bytes = reader.exportPdf();
  } catch (e) {
    notify(`Cannot export: ${e.message || e}`);
    return;
  }
  const url = URL.createObjectURL(new Blob([bytes], { type: "application/pdf" }));
  const a = document.createElement("a");
  a.href = url;
  a.download = `${reader.title || "note"}.pdf`;
  a.click();
  setTimeout(() => URL.revokeObjectURL(url), 10_000);
}

start().catch((e) => notify(`The reader cannot start: ${e.message || e}`));
