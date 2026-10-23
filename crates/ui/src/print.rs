// SPDX-FileCopyrightText: 2026 Christian Braghette
// SPDX-License-Identifier: AGPL-3.0-or-later

//! Printing a PDF made by the Print dialog (`app::export_ui`), through the
//! system. Blocking: call it on a thread of its own.
//!
//! * **Windows**: the system's Print dialog (printer, copies, its
//!   preferences), then each page rasterised by PDFium at the printer's
//!   resolution (300 dpi at most) and drawn through GDI, fitted to the
//!   printable area and centred; a page the other way round than the paper
//!   is turned a quarter.
//! * **Linux**: the desktop portal's Print dialog (`org.freedesktop.portal
//!   .Print`, GNOME, KDE and Flatpak), which prints the PDF itself; without
//!   a portal, `lp` sends it to the default printer.
//! * **macOS**: the PDF opens in Preview, to print from there.

use std::sync::atomic::{AtomicIsize, Ordering};

/// What became of a print.
pub enum Printed {
    /// Sent to the printer: so many pages (if known).
    Sent(Option<usize>),
    /// The print dialog was cancelled.
    Cancelled,
    /// Opened in the system's PDF viewer, to print from there (macOS).
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    Opened,
}

/// The main window (its `HWND` on Windows), which the print dialog belongs
/// to; set by `remember_window`.
static WINDOW: AtomicIsize = AtomicIsize::new(0);

/// Keeps the window's handle for the print dialog. Call once, with the
/// first frame.
pub fn remember_window(frame: &eframe::Frame) {
    #[cfg(windows)]
    {
        use raw_window_handle::{HasWindowHandle, RawWindowHandle};

        if let Ok(handle) = frame.window_handle()
            && let RawWindowHandle::Win32(h) = handle.as_raw()
        {
            WINDOW.store(h.hwnd.get(), Ordering::Relaxed);
        }
    }
    #[cfg(not(windows))]
    let _ = (frame, &WINDOW, Ordering::Relaxed);
}

/// Prints the PDF `pdf` (a whole file), named `title` in the print queue.
pub fn print(pdf: Vec<u8>, title: &str) -> Result<Printed, String> {
    #[cfg(windows)]
    return windows::print(pdf, title, WINDOW.load(Ordering::Relaxed));
    #[cfg(not(windows))]
    {
        let path = temp_pdf(&pdf, title)?;
        #[cfg(target_os = "macos")]
        return std::process::Command::new("open")
            .args(["-a", "Preview"])
            .arg(&path)
            .status()
            .map_err(|e| e.to_string())
            .and_then(|s| if s.success() { Ok(Printed::Opened) } else { Err("Preview cannot open it".into()) });
        #[cfg(not(target_os = "macos"))]
        match portal::print(&path, title) {
            Ok(printed) => printed,
            // No portal (or no print dialog): the default printer, as `lp` knows it.
            Err(_) => {
                let status = std::process::Command::new("lp").arg("-t").arg(title).arg(&path).status();
                match status {
                    Ok(s) if s.success() => Ok(Printed::Sent(None)),
                    Ok(_) => Err("lp could not print it".into()),
                    Err(e) => Err(format!("no print dialog, and no lp: {e}")),
                }
            }
        }
    }
}

/// Writes `pdf` to a file of the temporary folder, named after `title`
/// (the viewer and the print queue show the name).
#[cfg(not(windows))]
fn temp_pdf(pdf: &[u8], title: &str) -> Result<std::path::PathBuf, String> {
    let name: String = title.chars().map(|c| if c.is_alphanumeric() || " -_".contains(c) { c } else { '_' }).collect();
    let name = if name.trim().is_empty() { "Tratto".to_owned() } else { name.trim().to_owned() };
    let dir = std::env::temp_dir().join("tratto-print");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let path = dir.join(format!("{name}.pdf"));
    std::fs::write(&path, pdf).map_err(|e| e.to_string())?;
    Ok(path)
}

#[cfg(windows)]
mod windows {
    use std::ptr::{null, null_mut};
    use std::sync::Arc;
    use std::sync::mpsc;

    use tratto_core::pdf_pages::{self, PdfFile};
    use windows_sys::Win32::Foundation::{GlobalFree, HWND};
    use windows_sys::Win32::Graphics::Gdi::{
        BI_RGB, BITMAPINFO, BITMAPINFOHEADER, DIB_RGB_COLORS, DeleteDC, GetDeviceCaps, HALFTONE, HDC, HORZRES,
        LOGPIXELSX, SRCCOPY, SetBrushOrgEx, SetStretchBltMode, StretchDIBits, VERTRES,
    };
    use windows_sys::Win32::Storage::Xps::{AbortDoc, DOCINFOW, EndDoc, EndPage, StartDocW, StartPage};
    use windows_sys::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx, CoUninitialize};
    use windows_sys::Win32::UI::Controls::Dialogs::{
        PD_NOCURRENTPAGE, PD_NOPAGENUMS, PD_NOSELECTION, PD_RESULT_PRINT, PD_RETURNDC, PD_USEDEVMODECOPIESANDCOLLATE,
        PRINTDLGEXW, PrintDlgExW, START_PAGE_GENERAL,
    };

    use super::Printed;

    /// Pages are rasterised at this resolution at most (the printer's if lower).
    const MAX_DPI: f32 = 300.0;

    pub fn print(pdf: Vec<u8>, title: &str, window: isize) -> Result<Printed, String> {
        if window == 0 {
            return Err("the window is not ready".into());
        }
        // The dialog is COM based; this thread is the dialog's only.
        // SAFETY: plain call; balanced by `CoUninitialize` below.
        let com = unsafe { CoInitializeEx(null(), COINIT_APARTMENTTHREADED as u32) } >= 0;
        let result = dialog(window as HWND).and_then(|dc| match dc {
            Some(dc) => {
                let printed = print_pages(dc, PdfFile::new(pdf), title, None);
                // SAFETY: the printer DC the dialog returned, used no more.
                unsafe { DeleteDC(dc) };
                printed.map(|n| Printed::Sent(Some(n)))
            }
            None => Ok(Printed::Cancelled),
        });
        if com {
            // SAFETY: balances the successful `CoInitializeEx` above.
            unsafe { CoUninitialize() };
        }
        result
    }

    /// The system's Print dialog, modal to `owner`; the chosen printer's DC,
    /// or `None` if cancelled. Pages are chosen in Tratto's own dialog, so
    /// it offers none; copies are the printer driver's.
    fn dialog(owner: HWND) -> Result<Option<HDC>, String> {
        // SAFETY: all-zero is a valid PRINTDLGEXW (null handles and pointers).
        let mut pd: PRINTDLGEXW = unsafe { std::mem::zeroed() };
        pd.lStructSize = size_of::<PRINTDLGEXW>() as u32;
        pd.hwndOwner = owner;
        pd.Flags = PD_RETURNDC | PD_NOPAGENUMS | PD_NOSELECTION | PD_NOCURRENTPAGE | PD_USEDEVMODECOPIESANDCOLLATE;
        pd.nCopies = 1;
        pd.nStartPage = START_PAGE_GENERAL;
        // SAFETY: `pd` is initialised as the call requires and outlives it.
        let hr = unsafe { PrintDlgExW(&mut pd) };
        // SAFETY: the dialog allocated these (or left them null); we keep
        // none of them.
        unsafe {
            if !pd.hDevMode.is_null() {
                GlobalFree(pd.hDevMode);
            }
            if !pd.hDevNames.is_null() {
                GlobalFree(pd.hDevNames);
            }
        }
        if hr < 0 {
            return Err(format!("the print dialog failed (error {hr:#010x})"));
        }
        if pd.dwResultAction != PD_RESULT_PRINT {
            if !pd.hDC.is_null() {
                // SAFETY: a DC the dialog returned, not used.
                unsafe { DeleteDC(pd.hDC) };
            }
            return Ok(None);
        }
        if pd.hDC.is_null() {
            return Err("the printer cannot be used".into());
        }
        Ok(Some(pd.hDC))
    }

    /// Prints every page of `file` on the printer `dc`; the number printed.
    /// `output`: a file the printer writes to instead (a printer to a file,
    /// e.g. Microsoft Print to PDF).
    fn print_pages(dc: HDC, file: Arc<PdfFile>, title: &str, output: Option<&str>) -> Result<usize, String> {
        let sizes = pdf_pages::page_sizes(&file).map_err(|e| e.to_string())?;
        let wide = |s: &str| s.encode_utf16().chain([0]).collect::<Vec<u16>>();
        let (name, output) = (wide(title), output.map(wide));
        let doc = DOCINFOW {
            cbSize: size_of::<DOCINFOW>() as i32,
            lpszDocName: name.as_ptr(),
            lpszOutput: output.as_ref().map_or(null(), |o| o.as_ptr()),
            ..Default::default()
        };
        // SAFETY: a printer DC and a DOCINFOW whose strings outlive the call.
        if unsafe { StartDocW(dc, &doc) } <= 0 {
            return Err("the printer refused the document".into());
        }
        for (page, size) in sizes.iter().enumerate() {
            if let Err(e) = print_page(dc, &file, page as u32, *size) {
                // SAFETY: a document started on `dc` above.
                unsafe { AbortDoc(dc) };
                return Err(e);
            }
        }
        // SAFETY: as above.
        if unsafe { EndDoc(dc) } <= 0 {
            return Err("the printer did not take the document".into());
        }
        Ok(sizes.len())
    }

    /// One page of `size` (points): fitted to the printable area, centred,
    /// turned a quarter if it lies the other way round than the paper.
    fn print_page(dc: HDC, file: &Arc<PdfFile>, page: u32, size: [f32; 2]) -> Result<(), String> {
        // SAFETY: queries on a valid printer DC.
        let (area_w, area_h, dpi) = unsafe {
            (
                GetDeviceCaps(dc, HORZRES as i32) as f32,
                GetDeviceCaps(dc, VERTRES as i32) as f32,
                GetDeviceCaps(dc, LOGPIXELSX as i32).max(72) as f32,
            )
        };
        let turn = (size[0] > size[1]) != (area_w > area_h);
        let [w, h] = if turn { [size[1], size[0]] } else { size };
        let scale = (area_w / w).min(area_h / h);
        let (dest_w, dest_h) = (w * scale, h * scale);
        // The page's width in pixels before turning it.
        let source_w = if turn { dest_h } else { dest_w };
        let width = source_w.min(size[0] / 72.0 * MAX_DPI.min(dpi)).max(1.0) as u32;

        let (reply, answer) = mpsc::channel();
        pdf_pages::render(file, page, width, None, reply, Arc::new(|| {}));
        let rendered = answer.recv().map_err(|_| "the PDF worker stopped".to_owned())?;
        let (iw, ih, rgba) = rendered.image.map_err(|e| e.to_string())?;
        let (iw, ih, mut pixels) = if turn { turn_quarter(iw, ih, &rgba) } else { (iw, ih, rgba) };
        // GDI wants BGRA.
        for px in pixels.chunks_exact_mut(4) {
            px.swap(0, 2);
        }
        let info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: iw as i32,
                // Top-down rows.
                biHeight: -(ih as i32),
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB,
                ..Default::default()
            },
            ..Default::default()
        };
        let (x, y) = (((area_w - dest_w) / 2.0) as i32, ((area_h - dest_h) / 2.0) as i32);
        // SAFETY: a page of a started document on a valid DC; `pixels` holds
        // `iw * ih` BGRA pixels as `info` describes, and outlives the call.
        unsafe {
            if StartPage(dc) <= 0 {
                return Err("the printer refused a page".into());
            }
            SetStretchBltMode(dc, HALFTONE);
            SetBrushOrgEx(dc, 0, 0, null_mut());
            let lines = StretchDIBits(
                dc,
                x,
                y,
                dest_w as i32,
                dest_h as i32,
                0,
                0,
                iw as i32,
                ih as i32,
                pixels.as_ptr().cast(),
                &info,
                DIB_RGB_COLORS,
                SRCCOPY,
            );
            if lines <= 0 {
                return Err("the page could not be drawn for the printer".into());
            }
            if EndPage(dc) <= 0 {
                return Err("the printer refused a page".into());
            }
        }
        Ok(())
    }

    /// `rgba` (`w` x `h`) turned a quarter clockwise.
    fn turn_quarter(w: u32, h: u32, rgba: &[u8]) -> (u32, u32, Vec<u8>) {
        let (w, h) = (w as usize, h as usize);
        let mut out = vec![0; rgba.len()];
        for y in 0..h {
            for x in 0..w {
                // (x, y) goes to (h - 1 - y, x) of an h-wide image.
                let from = (y * w + x) * 4;
                let to = (x * h + (h - 1 - y)) * 4;
                out[to..to + 4].copy_from_slice(&rgba[from..from + 4]);
            }
        }
        (h as u32, w as u32, out)
    }

    #[cfg(test)]
    mod tests {
        use std::ptr::null;

        use tratto_core::model::Document;
        use tratto_core::pdf_export::{self, PdfNote, PdfOptions};
        use tratto_core::pdf_pages::PdfFile;
        use windows_sys::Win32::Graphics::Gdi::{CreateDCW, DeleteDC};

        use super::{print_pages, turn_quarter};

        /// Prints a two-page note through GDI with Windows' own PDF printer,
        /// to a file (no dialog). Needs PDFium: run with `TRATTO_PDFIUM`
        /// naming it, e.g. `target/debug/pdfium.dll`.
        #[test]
        #[ignore = "needs PDFium and the Microsoft Print to PDF printer"]
        fn prints_through_gdi() {
            let mut doc = Document::default();
            doc.page_count = 2;
            let note = PdfNote { doc: &doc, title: "Test".into() };
            let pdf = pdf_export::export_bytes(&[note], &PdfOptions::default()).unwrap();
            let out = std::env::temp_dir().join("tratto-print-test.pdf");
            let _ = std::fs::remove_file(&out);
            let printer: Vec<u16> = "Microsoft Print to PDF".encode_utf16().chain([0]).collect();
            // SAFETY: a null-terminated printer name; the DC is deleted below.
            let dc = unsafe { CreateDCW(null(), printer.as_ptr(), null(), null()) };
            assert!(!dc.is_null(), "no Microsoft Print to PDF printer");
            let printed = print_pages(dc, PdfFile::new(pdf), "Test", out.to_str());
            // SAFETY: the DC created above.
            unsafe { DeleteDC(dc) };
            assert_eq!(printed, Ok(2));
            // The printer writes the file when the job is spooled.
            for _ in 0..50 {
                if std::fs::metadata(&out).is_ok_and(|m| m.len() > 0) {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
            let bytes = std::fs::read(&out).expect("the printed file");
            assert!(bytes.starts_with(b"%PDF"), "a PDF was printed");
            let pages = tratto_core::pdf_pages::page_sizes(&PdfFile::new(bytes)).unwrap();
            assert_eq!(pages.len(), 2, "both pages were printed");
        }

        #[test]
        fn a_quarter_turn_is_clockwise() {
            // 2 x 1: A B  ->  1 x 2: A over B.
            let (a, b) = ([1, 1, 1, 255], [2, 2, 2, 255]);
            let (w, h, out) = turn_quarter(2, 1, &[a, b].concat());
            assert_eq!((w, h), (1, 2));
            assert_eq!(out, [a, b].concat());
            // 1 x 2: A over B  ->  2 x 1: B A.
            let (w, h, out) = turn_quarter(1, 2, &[a, b].concat());
            assert_eq!((w, h), (2, 1));
            assert_eq!(out, [b, a].concat());
        }
    }
}

#[cfg(all(unix, not(target_os = "macos")))]
mod portal {
    use std::collections::HashMap;
    use std::fs::File;
    use std::os::fd::AsFd;
    use std::path::Path;

    use zbus::blocking::{Connection, Proxy};
    use zbus::zvariant::{Fd, OwnedValue, Value};

    use super::Printed;

    const DESTINATION: &str = "org.freedesktop.portal.Desktop";
    const PATH: &str = "/org/freedesktop/portal/desktop";
    /// A request's response code when the user cancelled (0: done, 2: failed).
    const CANCELLED: u32 = 1;

    /// The portal's Print dialog, then the portal prints `pdf`. `Err`: no
    /// portal to print with (or its dialog failed); `Ok(Err)`: printing
    /// failed once a printer was chosen (not tried elsewhere: `lp` would
    /// print on another one).
    pub fn print(pdf: &Path, title: &str) -> zbus::Result<Result<Printed, String>> {
        let conn = Connection::session()?;
        let portal = Proxy::new(&conn, DESTINATION, PATH, "org.freedesktop.portal.Print")?;
        let empty = HashMap::<&str, Value>::new;

        let (code, results) = request(&conn, "tratto_prepare", |options| {
            portal.call_method("PreparePrint", &("", title, empty(), empty(), options)).map(drop)
        })?;
        match code {
            0 => {}
            CANCELLED => return Ok(Ok(Printed::Cancelled)),
            _ => return Err(zbus::Error::Failure("the print dialog failed".into())),
        }
        let token = results.get("token").and_then(|v| u32::try_from(v).ok()).unwrap_or(0);

        let file = File::open(pdf).map_err(|e| zbus::Error::Failure(e.to_string()))?;
        let (code, _) = request(&conn, "tratto_print", |mut options| {
            options.insert("token", Value::from(token));
            portal.call_method("Print", &("", title, Fd::from(file.as_fd()), options)).map(drop)
        })?;
        Ok(match code {
            0 => Ok(Printed::Sent(None)),
            CANCELLED => Ok(Printed::Cancelled),
            _ => Err("the printing service could not print it".into()),
        })
    }

    /// Calls a portal method through `call` (given its options, with the
    /// request's `handle_token`) and waits for the request's Response: its
    /// code (0 done, 1 cancelled, 2 failed) and results.
    fn request(
        conn: &Connection,
        token: &str,
        call: impl FnOnce(HashMap<&str, Value>) -> zbus::Result<()>,
    ) -> zbus::Result<(u32, HashMap<String, OwnedValue>)> {
        // The request's path follows from our bus name and the token, so the
        // answer is listened for before asking (it could come first).
        let sender = conn.unique_name().map(|n| n.trim_start_matches(':').replace('.', "_")).unwrap_or_default();
        let path = format!("{PATH}/request/{sender}/{token}");
        let request = Proxy::new(conn, DESTINATION, path, "org.freedesktop.portal.Request")?;
        let mut responses = request.receive_signal("Response")?;
        call(HashMap::from([("handle_token", Value::from(token))]))?;
        let message = responses.next().ok_or_else(|| zbus::Error::Failure("the portal did not answer".into()))?;
        message.body().deserialize()
    }
}
