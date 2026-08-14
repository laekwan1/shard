//! Tray icons drawn procedurally.
//!
//! Shipping PNGs would mean asset files that can go missing and a second place
//! to edit when the palette changes. These are cheap enough to rasterise at
//! startup and let the icon carry state: colour tracks on/off directly.

/// Straight RGBA8, the format `tray_icon::Icon::from_rgba` wants.
pub struct Rgba {
    pub pixels: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

const SIZE: u32 = 64;
/// Samples per axis. 3x3 is enough to hide staircasing at 16px tray scale.
const SUPERSAMPLE: u32 = 3;

pub const CYAN: [u8; 3] = [34, 211, 238];
pub const VIOLET: [u8; 3] = [167, 139, 250];
pub const GREY: [u8; 3] = [113, 121, 136];
pub const AMBER: [u8; 3] = [251, 191, 36];

/// Rasterise a signed distance function: negative is inside the shape.
/// Coordinates run -1.0..1.0 with the origin at the icon's centre.
fn render<F: Fn(f32, f32) -> f32>(rgb: [u8; 3], sdf: F) -> Rgba {
    render_at(SIZE, rgb, sdf)
}

fn render_at<F: Fn(f32, f32) -> f32>(size: u32, rgb: [u8; 3], sdf: F) -> Rgba {
    let mut pixels = Vec::with_capacity((size * size * 4) as usize);
    let step = 1.0 / (SUPERSAMPLE as f32);
    for py in 0..size {
        for px in 0..size {
            let mut hits = 0u32;
            for sy in 0..SUPERSAMPLE {
                for sx in 0..SUPERSAMPLE {
                    let fx = px as f32 + (sx as f32 + 0.5) * step;
                    let fy = py as f32 + (sy as f32 + 0.5) * step;
                    // Map pixel space onto -1.0..1.0.
                    let x = fx / (size as f32) * 2.0 - 1.0;
                    let y = fy / (size as f32) * 2.0 - 1.0;
                    if sdf(x, y) <= 0.0 {
                        hits += 1;
                    }
                }
            }
            let coverage = hits as f32 / (SUPERSAMPLE * SUPERSAMPLE) as f32;
            pixels.extend_from_slice(&[rgb[0], rgb[1], rgb[2], (coverage * 255.0) as u8]);
        }
    }
    Rgba { pixels, width: size, height: size }
}

/// Signed distance to a rounded box centred at the origin.
fn round_box(x: f32, y: f32, half_w: f32, half_h: f32, radius: f32) -> f32 {
    let qx = x.abs() - half_w + radius;
    let qy = y.abs() - half_h + radius;
    let outside = (qx.max(0.0).powi(2) + qy.max(0.0).powi(2)).sqrt();
    outside + qx.max(qy).min(0.0) - radius
}

/// Signed distance to an infinite band along the `y = x` diagonal.
fn diagonal_band(x: f32, y: f32, half_width: f32) -> f32 {
    (x - y).abs() / std::f32::consts::SQRT_2 - half_width
}

/// Subtract `cut` from `shape`: inside the result means inside one, outside the other.
fn subtract(shape: f32, cut: f32) -> f32 {
    shape.max(-cut)
}

/// Shard's mark: a rounded square sliced by a diagonal gap — a packet split in two.
pub fn shard(active: bool) -> Rgba {
    shard_at(SIZE, active)
}

pub fn shard_at(size: u32, active: bool) -> Rgba {
    let rgb = if active { CYAN } else { GREY };
    render_at(size, rgb, |x, y| {
        let body = round_box(x, y, 0.72, 0.72, 0.22);
        subtract(body, diagonal_band(x, y, 0.11))
    })
}

/// Veil's mark: a disc behind horizontal slits — something seen through a screen.
pub fn veil(active: bool) -> Rgba {
    veil_at(SIZE, active)
}

pub fn veil_at(size: u32, active: bool) -> Rgba {
    let rgb = if active { VIOLET } else { GREY };
    render_at(size, rgb, |x, y| {
        let disc = (x * x + y * y).sqrt() - 0.76;
        let slit_upper = (y + 0.30).abs() - 0.085;
        let slit_lower = (y - 0.30).abs() - 0.085;
        subtract(subtract(disc, slit_upper), slit_lower)
    })
}

/// Pack marks into a Windows `.ico`.
///
/// Built from the same distance fields as the tray icon, so the icon on the
/// executable and the icon in the tray cannot drift apart — which they would
/// the moment either became a checked-in image file.
///
/// 32-bit uncompressed entries: every Windows version reads them, and the
/// alternative (PNG-compressed entries) would mean carrying a deflate
/// implementation into a build script for no gain at these sizes.
pub fn to_ico(images: &[Rgba]) -> Vec<u8> {
    let mut header = Vec::with_capacity(6 + 16 * images.len());
    header.extend_from_slice(&0u16.to_le_bytes()); // reserved
    header.extend_from_slice(&1u16.to_le_bytes()); // 1 = icon
    header.extend_from_slice(&(images.len() as u16).to_le_bytes());

    let bodies: Vec<Vec<u8>> = images.iter().map(dib).collect();
    let mut offset = 6 + 16 * images.len();

    for (image, body) in images.iter().zip(&bodies) {
        // 256 is written as 0; the format has one byte for the dimension.
        header.push(if image.width >= 256 { 0 } else { image.width as u8 });
        header.push(if image.height >= 256 { 0 } else { image.height as u8 });
        header.push(0); // no palette
        header.push(0); // reserved
        header.extend_from_slice(&1u16.to_le_bytes()); // planes
        header.extend_from_slice(&32u16.to_le_bytes()); // bits per pixel
        header.extend_from_slice(&(body.len() as u32).to_le_bytes());
        header.extend_from_slice(&(offset as u32).to_le_bytes());
        offset += body.len();
    }

    let mut out = header;
    for body in bodies {
        out.extend_from_slice(&body);
    }
    out
}

/// One icon entry: a bottom-up BGRA bitmap followed by the legacy 1-bit mask.
fn dib(image: &Rgba) -> Vec<u8> {
    let (w, h) = (image.width, image.height);
    // The mask has one bit per pixel with rows padded to four bytes. Modern
    // Windows uses the alpha channel instead, but the field is not optional.
    let mask_row = (w as usize).div_ceil(32) * 4;
    let xor_len = (w * h * 4) as usize;
    let and_len = mask_row * h as usize;

    let mut v = Vec::with_capacity(40 + xor_len + and_len);
    v.extend_from_slice(&40u32.to_le_bytes()); // header size
    v.extend_from_slice(&(w as i32).to_le_bytes());
    // Double height: the structure describes both bitmaps as one image.
    v.extend_from_slice(&((h * 2) as i32).to_le_bytes());
    v.extend_from_slice(&1u16.to_le_bytes()); // planes
    v.extend_from_slice(&32u16.to_le_bytes()); // bits per pixel
    v.extend_from_slice(&0u32.to_le_bytes()); // BI_RGB, uncompressed
    v.extend_from_slice(&((xor_len + and_len) as u32).to_le_bytes());
    v.extend_from_slice(&[0u8; 16]); // resolution and palette counts

    for y in (0..h).rev() {
        for x in 0..w {
            let i = ((y * w + x) * 4) as usize;
            let p = &image.pixels[i..i + 4];
            v.extend_from_slice(&[p[2], p[1], p[0], p[3]]); // BGRA
        }
    }

    // The mask marks which pixels are see-through. Modern Windows reads the
    // alpha channel instead, but the paths that still consult this one draw a
    // filled rectangle when it says everything is opaque — which is exactly the
    // black box behind an icon that should be floating.
    for y in (0..h).rev() {
        let mut row = vec![0u8; mask_row];
        for x in 0..w {
            let alpha = image.pixels[((y * w + x) * 4 + 3) as usize];
            if alpha == 0 {
                row[(x / 8) as usize] |= 0x80 >> (x % 8);
            }
        }
        v.extend_from_slice(&row);
    }
    v
}

/// Amber variant for a degraded state: running but not fully healthy.
pub fn warn(shape_is_shard: bool) -> Rgba {
    let sdf_shard = |x: f32, y: f32| subtract(round_box(x, y, 0.72, 0.72, 0.22), diagonal_band(x, y, 0.11));
    let sdf_veil = |x: f32, y: f32| {
        let disc = (x * x + y * y).sqrt() - 0.76;
        subtract(subtract(disc, (y + 0.30).abs() - 0.085), (y - 0.30).abs() - 0.085)
    };
    if shape_is_shard {
        render(AMBER, sdf_shard)
    } else {
        render(AMBER, sdf_veil)
    }
}

#[cfg(feature = "gui")]
impl Rgba {
    pub fn to_tray_icon(&self) -> anyhow::Result<tray_icon::Icon> {
        Ok(tray_icon::Icon::from_rgba(self.pixels.clone(), self.width, self.height)?)
    }
}
