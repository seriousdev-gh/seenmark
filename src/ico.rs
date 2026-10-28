//! Generates a multi-size .ico (16/24/32/48, 32bpp): a green disc with a
//! white checkmark occupying half the canvas. Pure Rust writer, no assets.
//! The disc is centered; the rest of the canvas is transparent, so Explorer
//! renders the badge at half the size it used to be.

const SIZES: [u32; 4] = [16, 24, 32, 48];

struct Rgb {
    r: u8,
    g: u8,
    b: u8,
}

const GREEN: Rgb = Rgb { r: 0x16, g: 0xA3, b: 0x4A };
const DARK: Rgb = Rgb { r: 0x10, g: 0x7A, b: 0x38 };
const WHITE: Rgb = Rgb { r: 0xFF, g: 0xFF, b: 0xFF };

/// Distance from point (x,y) to segment (a)->(b), in pixel units.
fn seg_dist(px: f64, py: f64, ax: f64, ay: f64, bx: f64, by: f64) -> f64 {
    let vx = bx - ax;
    let vy = by - ay;
    let wx = px - ax;
    let wy = py - ay;
    let t = ((wx * vx + wy * vy) / (vx * vx + vy * vy)).clamp(0.0, 1.0);
    let dx = wx - t * vx;
    let dy = wy - t * vy;
    (dx * dx + dy * dy).sqrt()
}

/// coverage: 0..1 alpha for an edge at `d` (signed distance, + inside).
fn aa(d: f64) -> u8 {
    ((d + 0.5) * 255.0).clamp(0.0, 255.0) as u8
}

/// One pixel of a canvas of side `s`; geometry is relative to 32px design.
fn pixel(s: u32, x: u32, y: u32) -> (u8, u8, u8, u8) {
    let f = s as f64 / 32.0;
    let (fx, fy) = (x as f64 + 0.5, y as f64 + 0.5);
    let c = s as f64 / 2.0;

    // half-size disc: r = 13.5 scaled by 0.5 relative to the canvas
    let r = 13.5 * f * 0.5;
    let stroke = (2.0 * f).max(0.9);

    let dist = ((fx - c).powi(2) + (fy - c).powi(2)).sqrt();
    let d_ring = r + 0.5 - dist;
    if d_ring < -1.0 {
        return (0, 0, 0, 0);
    }

    // check polyline, points scaled from the 32px design about its center
    let pt = |px: f64, py: f64| (c + (px - 16.0) * f * 0.5, c + (py - 16.0) * f * 0.5);
    let (x1, y1) = pt(9.5, 16.5);
    let (x2, y2) = pt(14.0, 21.5);
    let (x3, y3) = pt(23.5, 10.5);
    let d_check = seg_dist(fx, fy, x1, y1, x2, y2).min(seg_dist(fx, fy, x2, y2, x3, y3));
    let on_check = d_check < stroke;

    let a_disc = aa(d_ring);
    if on_check {
        let a = aa(-(d_check - stroke)).min(a_disc);
        (WHITE.b, WHITE.g, WHITE.r, a)
    } else if d_ring > -1.0 {
        // slight darker border near the rim
        let rim = d_ring < 1.2 * f.max(0.5);
        let col = if rim { DARK } else { GREEN };
        (col.b, col.g, col.r, a_disc)
    } else {
        (0, 0, 0, 0)
    }
}

fn dib(s: u32) -> Vec<u8> {
    let xor_len = (s * s * 4) as usize;
    let mask_row_bytes = ((s + 31) / 32 * 4) as usize;
    let mask_len = mask_row_bytes * s as usize;

    let mut out: Vec<u8> = Vec::new();
    // BITMAPINFOHEADER (height doubled: XOR + AND mask)
    out.extend_from_slice(&40u32.to_le_bytes());
    out.extend_from_slice(&(s as i32).to_le_bytes());
    out.extend_from_slice(&(2 * s as i32).to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&32u16.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes()); // BI_RGB
    out.extend_from_slice(&(xor_len as u32).to_le_bytes());
    out.extend_from_slice(&0i32.to_le_bytes());
    out.extend_from_slice(&0i32.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());

    // XOR bitmap, bottom-up BGRA
    for y in (0..s).rev() {
        for x in 0..s {
            let (b, g, r, a) = pixel(s, x, y);
            out.extend_from_slice(&[b, g, r, a]);
        }
    }
    // AND mask, bottom-up rows, 1 = not opaque
    for y in (0..s).rev() {
        let mut row = vec![0u8; mask_row_bytes];
        for x in 0..s {
            let (_, _, _, a) = pixel(s, x, y);
            if a < 128 {
                row[(x / 8) as usize] |= 0x80 >> (x % 8);
            }
        }
        out.extend_from_slice(&row);
    }
    out
}

pub fn seen_ico_bytes() -> Vec<u8> {
    let dibs: Vec<Vec<u8>> = SIZES.iter().map(|&s| dib(s)).collect();

    let mut out: Vec<u8> = Vec::new();
    // ICONDIR
    out.extend_from_slice(&[0, 0]); // reserved
    out.extend_from_slice(&1u16.to_le_bytes()); // type: icon
    out.extend_from_slice(&(SIZES.len() as u16).to_le_bytes()); // count
    let mut offset = 6u32 + 16 * SIZES.len() as u32;
    for (&s, dib) in SIZES.iter().zip(&dibs) {
        out.push(s as u8); // width (0 would mean 256; all sizes < 256 here)
        out.push(s as u8); // height
        out.push(0); // color count
        out.push(0); // reserved
        out.extend_from_slice(&1u16.to_le_bytes()); // planes
        out.extend_from_slice(&32u16.to_le_bytes()); // bpp
        out.extend_from_slice(&(dib.len() as u32).to_le_bytes());
        out.extend_from_slice(&offset.to_le_bytes());
        offset += dib.len() as u32;
    }
    for dib in dibs {
        out.extend_from_slice(&dib);
    }
    out
}
