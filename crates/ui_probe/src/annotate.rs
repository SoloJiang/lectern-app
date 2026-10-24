//! The annotated screenshot: node borders and numbers drawn over the image,
//! and the PNG file (docs/design/ui_probe.md「标注与 PNG」). Pure functions,
//! any thread. The picture is a debugging aid, not the UI, so ADR 0015 does
//! not apply to its colours.

use std::io::{self, Cursor};
use std::path::Path;

use image::{ImageFormat, Rgba, RgbaImage};

use crate::tree::Tree;

/// Border width in physical pixels.
pub const BORDER: i64 = 2;

/// The border colours, chosen by depth and cycled.
pub const COLORS: [Rgba<u8>; 4] = [
    Rgba([255, 59, 48, 255]),
    Rgba([52, 199, 89, 255]),
    Rgba([0, 122, 255, 255]),
    Rgba([255, 149, 0, 255]),
];

const WHITE: Rgba<u8> = Rgba([255, 255, 255, 255]);
const BLACK: Rgba<u8> = Rgba([0, 0, 0, 255]);

/// Glyph size in dots.
pub const GLYPH_W: u32 = 5;
pub const GLYPH_H: u32 = 7;

/// The digits 0 to 9, one `u8` per row, the left dot in bit 4.
const DIGITS: [[u8; 7]; 10] = [
    [
        0b01110, 0b10001, 0b10011, 0b10101, 0b11001, 0b10001, 0b01110,
    ],
    [
        0b00100, 0b01100, 0b00100, 0b00100, 0b00100, 0b00100, 0b01110,
    ],
    [
        0b01110, 0b10001, 0b00001, 0b00010, 0b00100, 0b01000, 0b11111,
    ],
    [
        0b11110, 0b00001, 0b00001, 0b01110, 0b00001, 0b00001, 0b11110,
    ],
    [
        0b00010, 0b00110, 0b01010, 0b10010, 0b11111, 0b00010, 0b00010,
    ],
    [
        0b11111, 0b10000, 0b11110, 0b00001, 0b00001, 0b10001, 0b01110,
    ],
    [
        0b00110, 0b01000, 0b10000, 0b11110, 0b10001, 0b10001, 0b01110,
    ],
    [
        0b11111, 0b00001, 0b00010, 0b00100, 0b01000, 0b01000, 0b01000,
    ],
    [
        0b01110, 0b10001, 0b10001, 0b01110, 0b10001, 0b10001, 0b01110,
    ],
    [
        0b01110, 0b10001, 0b10001, 0b01111, 0b00001, 0b00010, 0b01100,
    ],
];

/// The glyph rows of a digit; `digit` is 0 to 9.
pub fn glyph(digit: u32) -> [u8; 7] {
    DIGITS[(digit % 10) as usize]
}

/// Draws a border and the number of every visible node of `tree` on `image`.
/// Bounds are logical pixels and are converted with `tree.window.scale`. The
/// border is `BORDER` physical pixels, inside the node's bounds, in a colour
/// by depth; the number is white on black in the top-left corner of the
/// bounds, each dot `ceil(scale)` pixels. What lies outside the image is cut.
pub fn annotate(image: &mut RgbaImage, tree: &Tree) {
    let scale = tree.window.scale;
    let dot = scale.ceil().max(1.0) as i64;
    let physical = |v: f64| (v * scale).round() as i64;

    // A node's parent comes before it, so one pass gives every depth.
    let mut depth = vec![0usize; tree.nodes.len()];
    for (i, node) in tree.nodes.iter().enumerate() {
        if let Some(parent) = node.parent
            && parent < i
        {
            depth[i] = depth[parent] + 1;
        }
    }

    for (i, node) in tree.nodes.iter().enumerate() {
        if !node.visible {
            continue;
        }
        let b = &node.bounds;
        let (x0, y0) = (physical(b.x), physical(b.y));
        let (x1, y1) = (physical(b.x + b.w), physical(b.y + b.h));
        let color = COLORS[depth[i] % COLORS.len()];
        fill(image, x0, y0, x1, y0 + BORDER, color);
        fill(image, x0, y1 - BORDER, x1, y1, color);
        fill(image, x0, y0, x0 + BORDER, y1, color);
        fill(image, x1 - BORDER, y0, x1, y1, color);
        number(image, x0, y0, node.n, dot);
    }
}

/// Fills `[x0, x1) x [y0, y1)`, cut to the image.
fn fill(image: &mut RgbaImage, x0: i64, y0: i64, x1: i64, y1: i64, color: Rgba<u8>) {
    let (width, height) = (i64::from(image.width()), i64::from(image.height()));
    for y in y0.max(0)..y1.min(height) {
        for x in x0.max(0)..x1.min(width) {
            image.put_pixel(x as u32, y as u32, color);
        }
    }
}

/// `n` in white dots on a black plate whose top-left corner is `(x, y)`; one
/// dot of margin all round and between the digits.
fn number(image: &mut RgbaImage, x: i64, y: i64, n: usize, dot: i64) {
    let text = n.to_string();
    let digits = text.len() as i64;
    let plate_w = (digits * (i64::from(GLYPH_W) + 1) + 1) * dot;
    let plate_h = (i64::from(GLYPH_H) + 2) * dot;
    fill(image, x, y, x + plate_w, y + plate_h, BLACK);
    for (i, ch) in text.chars().enumerate() {
        let Some(digit) = ch.to_digit(10) else {
            continue;
        };
        let left = x + (1 + i as i64 * (i64::from(GLYPH_W) + 1)) * dot;
        for (row, bits) in glyph(digit).into_iter().enumerate() {
            for col in 0..GLYPH_W {
                if bits & (1 << (GLYPH_W - 1 - col)) != 0 {
                    let px = left + i64::from(col) * dot;
                    let py = y + (1 + row as i64) * dot;
                    fill(image, px, py, px + dot, py + dot, WHITE);
                }
            }
        }
    }
}

/// Encodes `image` as PNG and writes it to `path` atomically: a temporary
/// file in the same directory, then a rename. A failed write leaves no
/// temporary file behind.
pub fn write_png(image: &RgbaImage, path: &Path) -> io::Result<()> {
    let mut bytes = Cursor::new(Vec::new());
    image
        .write_to(&mut bytes, ImageFormat::Png)
        .map_err(io::Error::other)?;
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    let file_name = path.file_name().ok_or(io::ErrorKind::InvalidInput)?;
    let mut tmp_name = std::ffi::OsString::from(".");
    tmp_name.push(file_name);
    tmp_name.push(format!(".tmp-{}", std::process::id()));
    let tmp = dir.join(tmp_name);
    let result =
        std::fs::write(&tmp, bytes.into_inner()).and_then(|()| std::fs::rename(&tmp, path));
    if result.is_err() {
        // Best effort: the error that matters is the one returned.
        let _ = std::fs::remove_file(&tmp);
    }
    result
}
