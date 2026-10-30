// SPDX-FileCopyrightText: 2026 Christian Braghette
// SPDX-License-Identifier: AGPL-3.0-or-later

//! Pictures in notes: their files, made from what is inserted (a file, the
//! clipboard's pixels), and decoded for the screen and the PDF.
//!
//! A note keeps a picture as a PNG or a JPEG. One inserted as such, not too
//! large and upright is kept byte for byte; anything else (GIF, BMP, WebP,
//! the clipboard's pixels, a very large picture, a photo stored sideways
//! with its EXIF orientation) is decoded, turned upright, made smaller if
//! it is too large, and saved as a PNG (a JPEG photo as a JPEG).

use std::fmt;
use std::io::Cursor;
use std::sync::Arc;

use image::imageops::FilterType;
use image::metadata::Orientation;
use image::{DynamicImage, ImageDecoder, ImageFormat, ImageReader, RgbaImage};

use crate::model::{Image, ImageData};

/// Pictures larger than this (pixels, on the longer side) are made smaller
/// when inserted: a page never shows more, and notes would only get heavy.
pub const MAX_SIDE: u32 = 3000;

#[derive(Debug)]
pub enum ImageError {
    /// Not a picture Tratto can read (PNG, JPEG, GIF, BMP, WebP).
    Unsupported,
    Broken(String),
}

impl fmt::Display for ImageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unsupported => write!(f, "not a picture Tratto can read (PNG, JPEG, GIF, BMP or WebP)"),
            Self::Broken(e) => write!(f, "the picture cannot be read: {e}"),
        }
    }
}

impl std::error::Error for ImageError {}

fn broken(e: impl fmt::Display) -> ImageError {
    ImageError::Broken(e.to_string())
}

impl ImageData {
    /// A picture from a file's bytes.
    pub fn from_file(bytes: Vec<u8>) -> Result<Arc<ImageData>, ImageError> {
        let format = image::guess_format(&bytes).map_err(|_| ImageError::Unsupported)?;
        if !matches!(
            format,
            ImageFormat::Png | ImageFormat::Jpeg | ImageFormat::Gif | ImageFormat::Bmp | ImageFormat::WebP
        ) {
            return Err(ImageError::Unsupported);
        }
        let mut decoder = ImageReader::with_format(Cursor::new(&bytes), format).into_decoder().map_err(broken)?;
        let (w, h) = decoder.dimensions();
        if w == 0 || h == 0 {
            return Err(broken("it is empty"));
        }
        // A photo taken holding the camera sideways is stored sideways, with
        // a note (EXIF) of how to turn it; what shows it (the screen, a PDF)
        // reads only the pixels, so they are turned here.
        let orientation = decoder.orientation().unwrap_or(Orientation::NoTransforms);
        let upright = orientation == Orientation::NoTransforms;
        if upright && matches!(format, ImageFormat::Png | ImageFormat::Jpeg) && w.max(h) <= MAX_SIDE {
            drop(decoder);
            return Ok(Arc::new(ImageData::new(w, h, bytes)));
        }
        let mut img = DynamicImage::from_decoder(decoder).map_err(broken)?;
        img.apply_orientation(orientation);
        encode(shrink(img), format == ImageFormat::Jpeg)
    }

    /// A picture from pixels, RGBA row by row (the clipboard's).
    pub fn from_rgba(width: u32, height: u32, rgba: Vec<u8>) -> Result<Arc<ImageData>, ImageError> {
        let img = RgbaImage::from_raw(width, height, rgba).ok_or_else(|| broken("wrong number of pixels"))?;
        if width == 0 || height == 0 {
            return Err(broken("it is empty"));
        }
        encode(shrink(DynamicImage::ImageRgba8(img)), false)
    }

    /// Its pixels, RGBA, made smaller to at most `max_side` on the longer
    /// side (for the screen).
    pub fn decode(&self, max_side: u32) -> Result<RgbaImage, ImageError> {
        let img = image::load_from_memory(&self.bytes).map_err(broken)?;
        let img = if img.width().max(img.height()) > max_side {
            img.resize(max_side, max_side, FilterType::Triangle)
        } else {
            img
        };
        Ok(img.into_rgba8())
    }

    /// Its pixels, RGBA, at each of `max_sides` (as [`Self::decode`]), the
    /// file decoded once.
    pub fn decode_sizes(&self, max_sides: &[u32]) -> Result<Vec<RgbaImage>, ImageError> {
        let mut out = Vec::with_capacity(max_sides.len());
        self.decode_each(max_sides, |rgba| out.push(rgba))?;
        Ok(out)
    }

    /// As [`Self::decode_sizes`], each size handed to `each` as soon as it
    /// is made (in the order of `max_sides`).
    pub fn decode_each(&self, max_sides: &[u32], mut each: impl FnMut(RgbaImage)) -> Result<(), ImageError> {
        let img = image::load_from_memory(&self.bytes).map_err(broken)?;
        for &side in max_sides {
            each(if img.width().max(img.height()) > side {
                img.resize(side, side, FilterType::Triangle).into_rgba8()
            } else {
                img.to_rgba8()
            });
        }
        Ok(())
    }
}

/// `img`, no larger than [`MAX_SIDE`].
fn shrink(img: DynamicImage) -> DynamicImage {
    if img.width().max(img.height()) > MAX_SIDE { img.resize(MAX_SIDE, MAX_SIDE, FilterType::Triangle) } else { img }
}

/// `img` saved as a JPEG (`jpeg`, a photo; no transparency) or a PNG.
fn encode(img: DynamicImage, jpeg: bool) -> Result<Arc<ImageData>, ImageError> {
    let mut out = Vec::new();
    let (w, h) = (img.width(), img.height());
    if jpeg {
        let rgb = DynamicImage::ImageRgb8(img.into_rgb8());
        rgb.write_to(&mut Cursor::new(&mut out), ImageFormat::Jpeg).map_err(broken)?;
    } else {
        img.write_to(&mut Cursor::new(&mut out), ImageFormat::Png).map_err(broken)?;
    }
    Ok(Arc::new(ImageData::new(w, h, out)))
}

/// `rgba` (unmultiplied, row by row) as a PNG file.
pub fn encode_png(width: u32, height: u32, rgba: Vec<u8>) -> Result<Vec<u8>, ImageError> {
    let img = RgbaImage::from_raw(width, height, rgba).ok_or_else(|| broken("wrong number of pixels"))?;
    let mut out = Vec::new();
    DynamicImage::ImageRgba8(img).write_to(&mut Cursor::new(&mut out), ImageFormat::Png).map_err(broken)?;
    Ok(out)
}

/// Where a picture of `data` goes when inserted with its centre at
/// `center` on a page whose paper is `paper` (page space; an infinite
/// page's goes left of and above its corner too): at its own size, one
/// pixel a document unit, made smaller to fit in most of the page, and kept
/// on it.
pub fn place(id: u64, page: u32, center: emath::Pos2, data: Arc<ImageData>, paper: emath::Rect) -> Image {
    let (w, h) = (data.width as f32, data.height as f32);
    let fit = (paper.width() * 0.8 / w).min(paper.height() * 0.8 / h).min(1.0);
    let (width, height) = (w * fit, h * fit);
    let x = (center.x - width * 0.5).clamp(paper.left(), (paper.right() - width).max(paper.left()));
    let y = (center.y - height * 0.5).clamp(paper.top(), (paper.bottom() - height).max(paper.top()));
    Image { id, page, x, y, width, height, data }
}

/// How many colour channels a JPEG has (1 grey, 3 colour, 4 CMYK), read
/// from its frame header; `None` if it has none.
pub(crate) fn jpeg_channels(bytes: &[u8]) -> Option<u8> {
    let mut at = 2;
    while at + 4 <= bytes.len() {
        if bytes[at] != 0xFF {
            return None;
        }
        let marker = bytes[at + 1];
        let len = u16::from_be_bytes([bytes[at + 2], bytes[at + 3]]) as usize;
        // Start of frame (baseline, progressive...; not DHT, JPG, DAC).
        if (0xC0..=0xCF).contains(&marker) && !matches!(marker, 0xC4 | 0xC8 | 0xCC) {
            return bytes.get(at + 9).copied();
        }
        at += 2 + len;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png(w: u32, h: u32) -> Vec<u8> {
        let img = RgbaImage::from_pixel(w, h, image::Rgba([10, 20, 30, 128]));
        let mut out = Vec::new();
        DynamicImage::ImageRgba8(img).write_to(&mut Cursor::new(&mut out), ImageFormat::Png).unwrap();
        out
    }

    #[test]
    fn a_png_is_kept_as_it_is() {
        let bytes = png(20, 10);
        let data = ImageData::from_file(bytes.clone()).unwrap();
        assert_eq!((data.width, data.height), (20, 10));
        assert_eq!(data.bytes, bytes);
        assert_eq!(data.decode(100).unwrap().dimensions(), (20, 10));
        assert_eq!(data.decode(10).unwrap().dimensions(), (10, 5), "made smaller for the screen");
    }

    #[test]
    fn a_huge_picture_is_made_smaller() {
        let data = ImageData::from_file(png(MAX_SIDE * 2, 10)).unwrap();
        assert_eq!(data.width, MAX_SIDE);
    }

    #[test]
    fn clipboard_pixels_become_a_png() {
        let data = ImageData::from_rgba(2, 1, vec![255, 0, 0, 255, 0, 0, 255, 128]).unwrap();
        assert!(!data.is_jpeg() && data.bytes.starts_with(b"\x89PNG"));
        assert!(ImageData::from_rgba(2, 2, vec![0; 4]).is_err(), "too few pixels");
    }

    #[test]
    fn what_is_not_a_picture_is_refused() {
        assert!(matches!(ImageData::from_file(b"hello".to_vec()), Err(ImageError::Unsupported)));
    }

    #[test]
    fn jpeg_channels_are_read() {
        let img = DynamicImage::ImageRgb8(image::RgbImage::from_pixel(4, 4, image::Rgb([1, 2, 3])));
        let mut out = Vec::new();
        img.write_to(&mut Cursor::new(&mut out), ImageFormat::Jpeg).unwrap();
        assert_eq!(jpeg_channels(&out), Some(3));
        let data = ImageData::from_file(out).unwrap();
        assert!(data.is_jpeg());
    }

    #[test]
    fn a_photo_stored_sideways_is_turned_upright() {
        let img = DynamicImage::ImageRgb8(image::RgbImage::from_pixel(40, 20, image::Rgb([200, 30, 30])));
        let mut jpeg = Vec::new();
        img.write_to(&mut Cursor::new(&mut jpeg), ImageFormat::Jpeg).unwrap();
        // An EXIF block after the start of the file: orientation 6, "turn a
        // quarter clockwise" (big-endian TIFF, one entry).
        let tiff: [u8; 26] =
            [b'M', b'M', 0, 42, 0, 0, 0, 8, 0, 1, 0x01, 0x12, 0, 3, 0, 0, 0, 1, 0, 6, 0, 0, 0, 0, 0, 0];
        let mut app1 = vec![0xFF, 0xE1, 0, (2 + 6 + tiff.len()) as u8];
        app1.extend_from_slice(b"Exif\0\0");
        app1.extend_from_slice(&tiff);
        jpeg.splice(2..2, app1);
        let data = ImageData::from_file(jpeg).unwrap();
        assert_eq!((data.width, data.height), (20, 40), "upright");
        assert_eq!(data.decode(100).unwrap().dimensions(), (20, 40));
        assert!(data.is_jpeg());
    }

    #[test]
    fn placing_fits_the_page() {
        let data = ImageData::from_file(png(2000, 1000)).unwrap();
        let page = emath::Rect::from_min_max(emath::pos2(0.0, 0.0), emath::pos2(800.0, 1000.0));
        let img = place(1, 0, emath::pos2(400.0, 500.0), data.clone(), page);
        assert!((img.width - 640.0).abs() < 0.01 && (img.height - 320.0).abs() < 0.01);
        assert!(img.x >= 0.0 && img.x + img.width <= 800.0);
        // An infinite page: where it is asked for, also left of and above
        // the page's corner.
        let infinite = emath::Rect::from_min_max(emath::pos2(-1e5, -1e5), emath::pos2(1e5, 1e5));
        let img = place(1, 0, emath::pos2(-3000.0, -2000.0), data, infinite);
        assert_eq!((img.x + img.width * 0.5, img.y + img.height * 0.5), (-3000.0, -2000.0));
    }
}
