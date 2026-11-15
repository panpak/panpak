// SPDX-License-Identifier: MIT OR Apache-2.0
//! A mark on an icon: a small round badge in the lower right corner, so that the entry of a Panpak app can be told from
//! the entry of the same app installed another way (docs/DESKTOP-INTEGRATION.md, section 9).
//!
//! This is the one place where pixels are decoded. The input is a file that [`crate::check`] accepts (8 bits, square, 16 to
//! 1024 pixels, the inflated size is the one the header says), so everything is bounded: at most 1024 x 1024 x 4 bytes of
//! pixels. The result is a new PNG (RGBA, 8 bits, no filter, one `IDAT`) that [`crate::check`] accepts too, or an error when
//! that is not possible (for example, the file would be over [`crate::MAX_FILE`]). The drawing is whole-number arithmetic only, so the same
//! input gives the same bytes on every machine.
use crate::{be32, check, MAX_FILE};
use flate2::{bufread::ZlibDecoder, write::ZlibEncoder, Compression, Crc};
use std::io::{Read, Write};

/// The colour of the badge disc and the colour of its ring and glyph (RGB).
const DISC: [u32; 3] = [28, 113, 216];
const WHITE: [u32; 3] = [255, 255, 255];

struct Parts {
    side: usize,
    color: u8,
    plte: Vec<u8>,
    trns: Vec<u8>,
    idat: Vec<u8>,
}

/// Read the chunks of a file that `check` accepted.
fn parts(data: &[u8], side: usize) -> Result<Parts, String> {
    let mut p = Parts { side, color: 0, plte: Vec::new(), trns: Vec::new(), idat: Vec::new() };
    let mut pos = 8;
    while pos + 12 <= data.len() {
        let len = be32(&data[pos..]) as usize;
        let kind = &data[pos + 4..pos + 8];
        let body = data.get(pos + 8..pos + 8 + len).ok_or("a chunk is longer than the file")?;
        match kind {
            b"IHDR" => p.color = *body.get(9).ok_or("short IHDR")?,
            b"PLTE" => p.plte = body.to_vec(),
            b"tRNS" => p.trns = body.to_vec(),
            b"IDAT" => p.idat.extend_from_slice(body),
            _ => {}
        }
        pos += 12 + len;
    }
    Ok(p)
}

fn channels(color: u8) -> usize {
    match color {
        0 | 3 => 1,
        4 => 2,
        2 => 3,
        _ => 4,
    }
}

/// Undo the scanline filters; `raw` is the inflated data (a filter byte, then the row).
fn unfilter(raw: &[u8], side: usize, bpp: usize) -> Result<Vec<u8>, String> {
    let row = side * bpp;
    if raw.len() != (row + 1) * side {
        return Err("the image data has the wrong size".into());
    }
    let mut out = vec![0u8; row * side];
    for y in 0..side {
        let f = raw[y * (row + 1)];
        let src = &raw[y * (row + 1) + 1..(y + 1) * (row + 1)];
        for x in 0..row {
            let a = if x >= bpp { out[y * row + x - bpp] } else { 0 };
            let b = if y > 0 { out[(y - 1) * row + x] } else { 0 };
            let c = if y > 0 && x >= bpp { out[(y - 1) * row + x - bpp] } else { 0 };
            let add = match f {
                0 => 0,
                1 => a,
                2 => b,
                3 => ((u16::from(a) + u16::from(b)) / 2) as u8,
                4 => {
                    let (pa, pb, pc) = (i32::from(a), i32::from(b), i32::from(c));
                    let p = pa + pb - pc;
                    let (da, db, dc) = ((p - pa).abs(), (p - pb).abs(), (p - pc).abs());
                    (if da <= db && da <= dc { a } else if db <= dc { b } else { c }) as u8
                }
                _ => return Err("a scanline has an unknown filter".into()),
            };
            out[y * row + x] = src[x].wrapping_add(add);
        }
    }
    Ok(out)
}

/// The pixels as RGBA, 8 bits.
fn to_rgba(p: &Parts, px: &[u8]) -> Vec<u8> {
    let n = p.side * p.side;
    let mut out = Vec::with_capacity(n * 4);
    // A colour key of `tRNS` (grey and true colour): the 8 bit sample is the low byte of each 16 bit value.
    let key_gray = (p.color == 0 && p.trns.len() >= 2).then(|| p.trns[1]);
    let key_rgb = (p.color == 2 && p.trns.len() >= 6).then(|| [p.trns[1], p.trns[3], p.trns[5]]);
    for i in 0..n {
        let (r, g, b, a) = match p.color {
            0 => {
                let v = px[i];
                (v, v, v, if key_gray == Some(v) { 0 } else { 255 })
            }
            4 => (px[2 * i], px[2 * i], px[2 * i], px[2 * i + 1]),
            2 => {
                let c = [px[3 * i], px[3 * i + 1], px[3 * i + 2]];
                (c[0], c[1], c[2], if key_rgb == Some(c) { 0 } else { 255 })
            }
            3 => {
                let k = usize::from(px[i]);
                let c = p.plte.get(3 * k..3 * k + 3).map_or([0, 0, 0], |s| [s[0], s[1], s[2]]);
                (c[0], c[1], c[2], p.trns.get(k).copied().unwrap_or(255))
            }
            _ => (px[4 * i], px[4 * i + 1], px[4 * i + 2], px[4 * i + 3]),
        };
        out.extend_from_slice(&[r, g, b, a]);
    }
    out
}

/// What the badge is at a point, in eighths of a pixel from the top left of the icon: `None` outside the disc.
fn badge_at(side: i64, x8: i64, y8: i64) -> Option<[u32; 3]> {
    // The disc: radius 0.22 of the side, kept a little away from the edge.
    let r8 = side * 8 * 22 / 100;
    let margin8 = (side * 8 / 32).max(8);
    let (cx, cy) = (side * 8 - r8 - margin8, side * 8 - r8 - margin8);
    let (dx, dy) = (x8 - cx, y8 - cy);
    let d2 = dx * dx + dy * dy;
    if d2 > r8 * r8 {
        return None;
    }
    let ring = (side * 8 / 24).max(8);
    let inner = r8 - ring;
    if d2 > inner * inner {
        return Some(WHITE);
    }
    // A box with a lid line (only where there is room for it).
    if side >= 28 {
        let half = inner * 5 / 11;
        if dx.abs() <= half && dy.abs() <= half {
            let line = (side * 8 / 64).max(4);
            return Some(if (dy + half / 3).abs() <= line / 2 { DISC } else { WHITE });
        }
    }
    Some(DISC)
}

/// Put the badge on a file that `check` accepts. See the module comment.
pub fn mark(data: &[u8]) -> Result<Vec<u8>, String> {
    let side = check(data)?.side as usize;
    let p = parts(data, side)?;
    let bpp = channels(p.color);
    let want = (side * bpp + 1) * side;
    let mut raw = Vec::with_capacity(want);
    let mut dec = ZlibDecoder::new(&p.idat[..]);
    dec.by_ref().take(want as u64 + 1).read_to_end(&mut raw).map_err(|_| "the image data does not inflate")?;
    let mut rgba = to_rgba(&p, &unfilter(&raw, side, bpp)?);
    // Four samples across and down in each pixel; the badge covers `cov` of the sixteen.
    let s = side as i64;
    for y in 0..side {
        for x in 0..side {
            let mut cov = 0u32;
            let mut sum = [0u32; 3];
            for sy in 0..4i64 {
                for sx in 0..4i64 {
                    if let Some(c) = badge_at(s, (x as i64) * 8 + sx * 2 + 1, (y as i64) * 8 + sy * 2 + 1) {
                        cov += 1;
                        for k in 0..3 {
                            sum[k] += c[k];
                        }
                    }
                }
            }
            if cov == 0 {
                continue;
            }
            let i = (y * side + x) * 4;
            let (ba, aa) = (cov * 255 / 16, u32::from(rgba[i + 3]));
            // Straight alpha, "over": the badge on what was there.
            let out_a = ba * 255 + aa * (255 - ba);
            for k in 0..3 {
                let bc = sum[k] / cov;
                let v = (bc * ba * 255 + u32::from(rgba[i + k]) * aa * (255 - ba)) / out_a;
                rgba[i + k] = v.min(255) as u8;
            }
            rgba[i + 3] = ((out_a + 127) / 255).min(255) as u8;
        }
    }
    encode(side, &rgba)
}

fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], body: &[u8]) {
    let mut c = Crc::new();
    c.update(kind);
    c.update(body);
    out.extend_from_slice(&(body.len() as u32).to_be_bytes());
    out.extend_from_slice(kind);
    out.extend_from_slice(body);
    out.extend_from_slice(&c.sum().to_be_bytes());
}

fn encode(side: usize, rgba: &[u8]) -> Result<Vec<u8>, String> {
    let mut raw = Vec::with_capacity((side * 4 + 1) * side);
    for row in rgba.chunks(side * 4) {
        raw.push(0);
        raw.extend_from_slice(row);
    }
    let mut z = ZlibEncoder::new(Vec::new(), Compression::new(9));
    z.write_all(&raw).map_err(|e| e.to_string())?;
    let z = z.finish().map_err(|e| e.to_string())?;
    let mut out = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
    let mut ihdr = (side as u32).to_be_bytes().to_vec();
    ihdr.extend_from_slice(&(side as u32).to_be_bytes());
    ihdr.extend_from_slice(&[8, 6, 0, 0, 0]);
    chunk(&mut out, b"IHDR", &ihdr);
    chunk(&mut out, b"IDAT", &z);
    chunk(&mut out, b"IEND", b"");
    if out.len() > MAX_FILE {
        return Err(format!("the marked icon would be larger than {} KiB", MAX_FILE / 1024));
    }
    check(&out)?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png(side: u32, color: u8, plte: &[u8], trns: &[u8], raw: &[u8]) -> Vec<u8> {
        let mut out = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
        let mut h = side.to_be_bytes().to_vec();
        h.extend_from_slice(&side.to_be_bytes());
        h.extend_from_slice(&[8, color, 0, 0, 0]);
        chunk(&mut out, b"IHDR", &h);
        if !plte.is_empty() {
            chunk(&mut out, b"PLTE", plte);
        }
        if !trns.is_empty() {
            chunk(&mut out, b"tRNS", trns);
        }
        let mut z = ZlibEncoder::new(Vec::new(), Compression::default());
        z.write_all(raw).unwrap();
        chunk(&mut out, b"IDAT", &z.finish().unwrap());
        chunk(&mut out, b"IEND", b"");
        out
    }

    /// Filter rows of `px` (rows of `bpp * side` bytes) with the filter type `f(y)`.
    fn filtered(px: &[u8], side: usize, bpp: usize, f: impl Fn(usize) -> u8) -> Vec<u8> {
        let row = side * bpp;
        let mut raw = Vec::new();
        for y in 0..side {
            let t = f(y);
            raw.push(t);
            for x in 0..row {
                let a = if x >= bpp { px[y * row + x - bpp] } else { 0 };
                let b = if y > 0 { px[(y - 1) * row + x] } else { 0 };
                let c = if y > 0 && x >= bpp { px[(y - 1) * row + x - bpp] } else { 0 };
                let pred = match t {
                    0 => 0,
                    1 => a,
                    2 => b,
                    3 => ((u16::from(a) + u16::from(b)) / 2) as u8,
                    _ => {
                        let (pa, pb, pc) = (i32::from(a), i32::from(b), i32::from(c));
                        let p = pa + pb - pc;
                        let (da, db, dc) = ((p - pa).abs(), (p - pb).abs(), (p - pc).abs());
                        if da <= db && da <= dc { a } else if db <= dc { b } else { c }
                    }
                };
                raw.push(px[y * row + x].wrapping_sub(pred));
            }
        }
        raw
    }

    fn pixels(side: usize, bpp: usize) -> Vec<u8> {
        (0..side * side * bpp).map(|i| ((i * 31 + i / 7) % 251) as u8).collect()
    }

    /// The RGBA of a file made by `mark`.
    fn decode(data: &[u8]) -> (usize, Vec<u8>) {
        let side = check(data).unwrap().side as usize;
        let p = parts(data, side).unwrap();
        let mut raw = Vec::new();
        ZlibDecoder::new(&p.idat[..]).read_to_end(&mut raw).unwrap();
        (side, to_rgba(&p, &unfilter(&raw, side, channels(p.color)).unwrap()))
    }

    #[test]
    fn every_colour_type_and_every_filter_is_read_and_the_result_is_an_rgba_png() {
        for side in [16usize, 32, 48, 64] {
            for (color, bpp) in [(0u8, 1usize), (2, 3), (4, 2), (6, 4)] {
                let px = pixels(side, bpp);
                for filter in 0..5u8 {
                    let src = png(side as u32, color, &[], &[], &filtered(&px, side, bpp, |_| filter));
                    let marked = mark(&src).unwrap_or_else(|e| panic!("{side} {color} {filter}: {e}"));
                    assert_eq!(check(&marked).unwrap().side as usize, side);
                    // The same picture, filtered another way, gives the same marked file.
                    let other = png(side as u32, color, &[], &[], &filtered(&px, side, bpp, |y| (y % 5) as u8));
                    assert_eq!(marked, mark(&other).unwrap(), "{side} {color} {filter}");
                }
            }
        }
    }

    #[test]
    fn only_the_corner_changes_and_there_is_a_badge_there() {
        let side = 64usize;
        let px = pixels(side, 4);
        let src = png(side as u32, 6, &[], &[], &filtered(&px, side, 4, |_| 0));
        let (_, before) = decode(&src);
        let (s, after) = decode(&mark(&src).unwrap());
        assert_eq!(s, side);
        // The upper left half of the icon is as it was; the lower right corner has changed.
        for y in 0..side / 2 {
            for x in 0..side / 2 {
                let i = (y * side + x) * 4;
                assert_eq!(before[i..i + 4], after[i..i + 4], "pixel {x},{y}");
            }
        }
        let changed = (0..side * side).filter(|i| before[i * 4..i * 4 + 4] != after[i * 4..i * 4 + 4]).count();
        assert!(changed > 100 && changed < side * side / 4, "{changed} pixels changed");
        let (cx, cy) = (side - 14 - 2, side - 14 - 2);
        let i = (cy * side + cx) * 4;
        assert_ne!(before[i..i + 4], after[i..i + 4]);
        assert_eq!(after[i + 3], 255, "the badge is opaque in the middle");
    }

    #[test]
    fn a_transparent_icon_gets_an_opaque_badge_and_stays_transparent_elsewhere() {
        let side = 32usize;
        let src = png(side as u32, 6, &[], &[], &filtered(&vec![0u8; side * side * 4], side, 4, |_| 0));
        let (_, after) = decode(&mark(&src).unwrap());
        assert_eq!(after[3], 0, "the top left corner is still transparent");
        assert!(after.chunks(4).any(|p| p[3] == 255));
    }

    #[test]
    fn a_palette_and_a_colour_key_keep_their_transparency() {
        let side = 32usize;
        let idx: Vec<u8> = (0..side * side).map(|i| (i % 2) as u8).collect();
        let pal = png(side as u32, 3, &[10, 20, 30, 200, 100, 50], &[0, 255], &filtered(&idx, side, 1, |_| 0));
        let (_, a) = decode(&mark(&pal).unwrap());
        assert_eq!(a[3], 0, "index 0 is transparent");
        assert_eq!(a[4..8], [200, 100, 50, 255]);
        let gray: Vec<u8> = (0..side * side).map(|i| if i % 2 == 0 { 7 } else { 9 }).collect();
        let keyed = png(side as u32, 0, &[], &[0, 7], &filtered(&gray, side, 1, |_| 0));
        let (_, a) = decode(&mark(&keyed).unwrap());
        assert_eq!((a[3], a[7]), (0, 255));
    }

    #[test]
    fn the_result_is_the_same_every_time_and_what_is_not_acceptable_is_refused() {
        let side = 48usize;
        let src = png(side as u32, 6, &[], &[], &filtered(&pixels(side, 4), side, 4, |_| 4));
        assert_eq!(mark(&src).unwrap(), mark(&src).unwrap());
        assert!(mark(b"<svg/>").is_err());
        assert!(mark(&src[..src.len() - 3]).is_err());
        // Marking does not break the check of an icon that was marked already.
        let once = mark(&src).unwrap();
        assert!(mark(&once).is_ok());
    }

    #[test]
    fn an_icon_that_would_be_too_large_once_marked_is_refused_not_cut() {
        // Noise does not compress: 1024 x 1024 RGBA is far over the limit whatever is done to it.
        let side = 1024usize;
        let mut x = 0x1234_5678u32;
        let px: Vec<u8> = (0..side * side * 4)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 17;
                x ^= x << 5;
                x as u8
            })
            .collect();
        let big = png(side as u32, 6, &[], &[], &filtered(&px, side, 4, |_| 0));
        assert!(mark(&big).is_err());
    }
}
