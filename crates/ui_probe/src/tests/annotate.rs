//! The annotation and the PNG file, on a made-up image and tree.

use std::collections::BTreeMap;

use image::{Rgba, RgbaImage};

use crate::annotate::{COLORS, GLYPH_H, GLYPH_W, annotate, glyph, write_png};
use crate::tree::{BoundsOut, Node, StateOut, Tree};

const BACKGROUND: Rgba<u8> = Rgba([10, 20, 30, 255]);
const BLACK: Rgba<u8> = Rgba([0, 0, 0, 255]);
const WHITE: Rgba<u8> = Rgba([255, 255, 255, 255]);

fn node(n: usize, parent: Option<usize>, visible: bool, at: (f64, f64, f64, f64)) -> Node {
    Node {
        n,
        id: format!("t.n{n}"),
        role: "group",
        label: None,
        bounds: BoundsOut {
            x: at.0,
            y: at.1,
            w: at.2,
            h: at.3,
        },
        visible,
        state: StateOut::default(),
        attrs: BTreeMap::new(),
        parent,
        children: Vec::new(),
    }
}

fn tree(scale: f64, nodes: Vec<Node>) -> Tree {
    let mut window = super::tree::window();
    window.scale = scale;
    Tree {
        schema: "ui-tree/1",
        window,
        nodes,
        warnings: Vec::new(),
    }
}

fn image(w: u32, h: u32) -> RgbaImage {
    RgbaImage::from_pixel(w, h, BACKGROUND)
}

/// Whether the plate and glyphs of `digits` at `(x, y)` with `dot` pixels
/// per dot are drawn as the font says.
fn assert_number(image: &RgbaImage, x: u32, y: u32, digits: &str, dot: u32) {
    let plate_w = (digits.len() as u32 * (GLYPH_W + 1) + 1) * dot;
    let plate_h = (GLYPH_H + 2) * dot;
    for py in 0..plate_h {
        for px in 0..plate_w {
            let (gx, gy) = (px / dot, py / dot);
            let mut want = BLACK;
            for (i, ch) in digits.chars().enumerate() {
                let left = 1 + i as u32 * (GLYPH_W + 1);
                if (left..left + GLYPH_W).contains(&gx) && (1..1 + GLYPH_H).contains(&gy) {
                    let bits = glyph(ch.to_digit(10).unwrap())[(gy - 1) as usize];
                    if bits & (1 << (GLYPH_W - 1 - (gx - left))) != 0 {
                        want = WHITE;
                    }
                }
            }
            assert_eq!(
                *image.get_pixel(x + px, y + py),
                want,
                "digits {digits} at ({px}, {py})"
            );
        }
    }
}

#[test]
fn annotate_draws_borders_by_depth_and_skips_invisible_nodes() {
    let nodes = vec![
        node(0, None, true, (0.0, 0.0, 200.0, 100.0)),
        node(1, Some(0), true, (20.0, 20.0, 60.0, 40.0)),
        node(2, Some(1), false, (100.0, 20.0, 50.0, 50.0)),
        node(3, Some(0), true, (180.0, 80.0, 60.0, 60.0)),
    ];
    let mut img = image(200, 100);
    annotate(&mut img, &tree(1.0, nodes));

    // Depth 0 is the first colour: the whole edge of the image, 2 px wide.
    assert_eq!(*img.get_pixel(100, 0), COLORS[0]);
    assert_eq!(*img.get_pixel(100, 1), COLORS[0]);
    assert_eq!(*img.get_pixel(100, 98), COLORS[0]);
    assert_eq!(*img.get_pixel(100, 99), COLORS[0]);
    assert_eq!(*img.get_pixel(0, 50), COLORS[0]);
    assert_eq!(*img.get_pixel(199, 50), COLORS[0]);
    assert_eq!(*img.get_pixel(100, 2), BACKGROUND);

    // Depth 1: the border is inside the bounds [20, 80) x [20, 60).
    for (x, y) in [
        (40, 20),
        (40, 21),
        (40, 58),
        (40, 59),
        (20, 50),
        (21, 50),
        (78, 50),
        (79, 50),
    ] {
        assert_eq!(*img.get_pixel(x, y), COLORS[1], "({x}, {y})");
    }
    for (x, y) in [
        (40, 19),
        (40, 22),
        (40, 57),
        (19, 50),
        (22, 50),
        (77, 50),
        (80, 50),
        (40, 40),
    ] {
        assert_eq!(*img.get_pixel(x, y), BACKGROUND, "({x}, {y})");
    }

    // Node 2 is not visible: nothing at its edges (it is at depth 2, which
    // would be the third colour).
    for (x, y) in [(120, 20), (100, 40), (149, 40), (120, 69)] {
        assert_eq!(*img.get_pixel(x, y), BACKGROUND, "({x}, {y})");
    }

    // Node 3 reaches past the image: the part inside is drawn, the rest is cut.
    assert_eq!(*img.get_pixel(190, 80), COLORS[1]);
    assert_eq!(*img.get_pixel(180, 90), COLORS[1]);
    assert_eq!(*img.get_pixel(190, 90), BACKGROUND);

    // The numbers, in the top-left corner of each visible node.
    assert_number(&img, 20, 20, "1", 1);
    assert_number(&img, 180, 80, "3", 1);
}

#[test]
fn annotate_cycles_the_four_colours() {
    // A chain of five nested nodes: depth 4 wraps to the first colour.
    let nodes: Vec<Node> = (0..5)
        .map(|n| {
            let inset = 10.0 * n as f64;
            node(
                n,
                n.checked_sub(1),
                true,
                (inset, inset, 200.0 - 2.0 * inset, 100.0 - 2.0 * inset),
            )
        })
        .collect();
    let mut img = image(200, 100);
    annotate(&mut img, &tree(1.0, nodes));
    for depth in 0..5usize {
        let inset = 10 * depth as u32;
        // The middle of the top edge, right of the number plate.
        assert_eq!(
            *img.get_pixel(100, inset),
            COLORS[depth % 4],
            "depth {depth}"
        );
    }
}

#[test]
fn annotate_scales_bounds_and_dots_by_the_scale_factor() {
    let nodes = vec![
        node(0, None, true, (0.0, 0.0, 100.0, 50.0)),
        node(12, Some(0), true, (10.0, 10.0, 40.0, 30.0)),
    ];
    let mut img = image(200, 100);
    annotate(&mut img, &tree(2.0, nodes));
    // Physical bounds [20, 100) x [20, 80), border 2 px, dots of 2 px.
    assert_eq!(*img.get_pixel(60, 20), COLORS[1]);
    assert_eq!(*img.get_pixel(60, 21), COLORS[1]);
    assert_eq!(*img.get_pixel(60, 22), BACKGROUND);
    assert_eq!(*img.get_pixel(99, 50), COLORS[1]);
    assert_eq!(*img.get_pixel(97, 50), BACKGROUND);
    assert_number(&img, 20, 20, "12", 2);
}

#[test]
fn annotate_rounds_a_fractional_scale_up_for_the_dots() {
    let nodes = vec![node(7, None, true, (0.0, 0.0, 100.0, 60.0))];
    let mut img = image(150, 90);
    annotate(&mut img, &tree(1.5, nodes));
    assert_number(&img, 0, 0, "7", 2);
}

#[test]
fn annotate_leaves_the_image_alone_when_nothing_is_visible() {
    let mut img = image(50, 50);
    annotate(
        &mut img,
        &tree(1.0, vec![node(0, None, false, (0.0, 0.0, 50.0, 50.0))]),
    );
    assert!(img.pixels().all(|p| *p == BACKGROUND));
}

#[test]
fn write_png_round_trips_and_leaves_no_temporary_file() {
    let tmp = tempfile::tempdir().unwrap();
    let mut img = image(7, 5);
    img.put_pixel(3, 2, Rgba([1, 2, 3, 4]));
    let path = tmp.path().join("shot.png");
    write_png(&img, &path).unwrap();

    let back = image::open(&path).unwrap().to_rgba8();
    assert_eq!(back.dimensions(), (7, 5));
    assert_eq!(back, img);
    let names: Vec<_> = std::fs::read_dir(tmp.path())
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    assert_eq!(names, ["shot.png"]);

    // A second write replaces the file.
    write_png(&image(2, 2), &path).unwrap();
    assert_eq!(image::open(&path).unwrap().to_rgba8().dimensions(), (2, 2));
}

#[test]
fn write_png_fails_without_a_directory_and_writes_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("missing").join("shot.png");
    assert!(write_png(&image(2, 2), &path).is_err());
    assert_eq!(std::fs::read_dir(tmp.path()).unwrap().count(), 0);
}
