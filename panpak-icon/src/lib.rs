// SPDX-License-Identifier: MIT OR Apache-2.0
//! The check that an icon file passes before it is installed in the user's icon folder.
//!
//! An icon is untrusted input that host libraries decode outside the sandbox (gdk-pixbuf, Qt). The rules
//! (docs/DESKTOP-INTEGRATION.md, section 1): PNG only; the chunks are walked with their CRC and only a short list
//! is allowed (no text, profile or private chunk); the image is square, 16 to 1024 pixels, 8 bits, not interlaced;
//! the compressed data inflates to exactly the size the header promises, and nothing follows it. The file is
//! installed as it is read: nothing is re-encoded. Pixels are never decoded here.

use flate2::{bufread::ZlibDecoder, Crc};
use std::io::Read;

/// The largest icon file.
pub const MAX_FILE: usize = 256 * 1024;
pub const MIN_SIDE: u32 = 16;
pub const MAX_SIDE: u32 = 1024;
const SIGNATURE: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];

/// A file that passed: the side of the square image.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Icon {
    pub side: u32,
}

fn be32(b: &[u8]) -> u32 {
    u32::from_be_bytes([b[0], b[1], b[2], b[3]])
}

/// Check a file. The error says which rule it broke.
pub fn check(data: &[u8]) -> Result<Icon, String> {
    if data.len() > MAX_FILE {
        return Err(format!("larger than {} KiB", MAX_FILE / 1024));
    }
    if data.len() < SIGNATURE.len() || data[..8] != SIGNATURE {
        return Err("not a PNG file (the signature is wrong; SVG and other formats are not accepted)".into());
    }
    let mut pos = 8;
    let mut seen: Vec<[u8; 4]> = Vec::new();
    let (mut side, mut color, mut idat, mut last_idat) = (0u32, 0u8, Vec::<u8>::new(), false);
    let mut idat_started = false;
    let mut first = true;
    loop {
        if data.len() - pos < 12 {
            return Err("the file ends inside a chunk header, or has no IEND".into());
        }
        let len = be32(&data[pos..]) as usize;
        let kind: [u8; 4] = data[pos + 4..pos + 8].try_into().unwrap();
        if !kind.iter().all(u8::is_ascii_alphabetic) {
            return Err("a chunk has a bad type".into());
        }
        if len > data.len() - pos - 12 {
            return Err("a chunk is longer than the file".into());
        }
        let body = &data[pos + 8..pos + 8 + len];
        let mut crc = Crc::new();
        crc.update(&data[pos + 4..pos + 8 + len]);
        if crc.sum() != be32(&data[pos + 8 + len..]) {
            return Err(format!("bad checksum in chunk {}", String::from_utf8_lossy(&kind)));
        }
        pos += 12 + len;
        let name = String::from_utf8_lossy(&kind).into_owned();
        if first {
            if &kind != b"IHDR" || len != 13 {
                return Err("the first chunk is not a 13 byte IHDR".into());
            }
            let (w, h) = (be32(body), be32(&body[4..]));
            if w != h {
                return Err(format!("not square ({w}x{h})"));
            }
            if !(MIN_SIDE..=MAX_SIDE).contains(&w) {
                return Err(format!("{w}x{h} pixels: the side must be {MIN_SIDE} to {MAX_SIDE}"));
            }
            if body[8] != 8 {
                return Err(format!("bit depth {}: only 8 is accepted", body[8]));
            }
            if !matches!(body[9], 0 | 2 | 3 | 4 | 6) {
                return Err(format!("colour type {} is not valid", body[9]));
            }
            if body[10] != 0 || body[11] != 0 {
                return Err("compression or filter method is not the standard one".into());
            }
            if body[12] != 0 {
                return Err("interlaced images are not accepted".into());
            }
            (side, color) = (w, body[9]);
            first = false;
            seen.push(kind);
            continue;
        }
        match &kind {
            b"IDAT" => {
                if last_idat {
                    return Err("IDAT chunks are not together".into());
                }
                if color == 3 && !seen.contains(b"PLTE") {
                    return Err("a palette image without PLTE".into());
                }
                idat_started = true;
                idat.extend_from_slice(body);
            }
            b"IEND" => {
                if len != 0 {
                    return Err("IEND is not empty".into());
                }
                if pos != data.len() {
                    return Err("data after IEND".into());
                }
                if !idat_started {
                    return Err("no image data".into());
                }
                break;
            }
            b"PLTE" | b"tRNS" | b"gAMA" | b"sRGB" | b"pHYs" => {
                if idat_started {
                    return Err(format!("chunk {name} after the image data"));
                }
                if seen.contains(&kind) {
                    return Err(format!("chunk {name} twice"));
                }
                let ok = match &kind {
                    b"PLTE" => matches!(color, 2 | 3 | 6) && len.is_multiple_of(3) && (3..=768).contains(&len),
                    b"tRNS" => matches!(color, 0 | 2 | 3) && len <= 768,
                    b"gAMA" => len == 4,
                    b"sRGB" => len == 1 && body[0] <= 3,
                    _ => len == 9,
                };
                if !ok {
                    return Err(format!("chunk {name} is not valid here"));
                }
            }
            _ => return Err(format!("chunk {name} is not accepted (only IHDR PLTE tRNS gAMA sRGB pHYs IDAT IEND)")),
        }
        if &kind != b"IDAT" {
            if idat_started {
                last_idat = true;
            }
            seen.push(kind);
        }
    }
    // The inflated size is fixed by the header: nothing less, nothing more.
    let channels = match color {
        0 | 3 => 1u64,
        4 => 2,
        2 => 3,
        _ => 4,
    };
    let row = 1 + u64::from(side) * channels;
    let want = row * u64::from(side);
    let mut dec = ZlibDecoder::new(&idat[..]);
    let mut raw = Vec::with_capacity(want as usize);
    if dec.by_ref().take(want + 1).read_to_end(&mut raw).is_err() {
        return Err("the image data does not inflate".into());
    }
    if raw.len() as u64 != want {
        return Err(format!("the image data inflates to {} bytes, the header needs {want}", raw.len()));
    }
    // The stream must also have ended on its own, with nothing behind it.
    let mut extra = [0u8; 1];
    if !matches!(dec.read(&mut extra), Ok(0)) || !dec.get_ref().is_empty() {
        return Err("the image data does not end where the header says".into());
    }
    if raw.chunks(row as usize).any(|r| r[0] > 4) {
        return Err("a scanline has an unknown filter".into());
    }
    Ok(Icon { side })
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::{write::ZlibEncoder, Compression};
    use std::io::Write;

    fn chunk(kind: &[u8; 4], body: &[u8]) -> Vec<u8> {
        let mut c = Crc::new();
        c.update(kind);
        c.update(body);
        let mut v = (body.len() as u32).to_be_bytes().to_vec();
        v.extend_from_slice(kind);
        v.extend_from_slice(body);
        v.extend_from_slice(&c.sum().to_be_bytes());
        v
    }

    fn ihdr(w: u32, h: u32, depth: u8, color: u8, interlace: u8) -> Vec<u8> {
        let mut b = w.to_be_bytes().to_vec();
        b.extend_from_slice(&h.to_be_bytes());
        b.extend_from_slice(&[depth, color, 0, 0, interlace]);
        chunk(b"IHDR", &b)
    }

    fn idat(raw: &[u8]) -> Vec<u8> {
        let mut e = ZlibEncoder::new(Vec::new(), Compression::default());
        e.write_all(raw).unwrap();
        chunk(b"IDAT", &e.finish().unwrap())
    }

    fn png(parts: &[Vec<u8>]) -> Vec<u8> {
        let mut v = SIGNATURE.to_vec();
        for p in parts {
            v.extend_from_slice(p);
        }
        v
    }

    fn raw(side: usize, channels: usize) -> Vec<u8> {
        let mut v = Vec::new();
        for y in 0..side {
            v.push((y % 5) as u8);
            v.extend((0..side * channels).map(|x| (x * 7 + y) as u8));
        }
        v
    }

    fn rgba(side: u32) -> Vec<u8> {
        png(&[ihdr(side, side, 8, 6, 0), idat(&raw(side as usize, 4)), chunk(b"IEND", b"")])
    }

    #[test]
    fn a_good_icon_passes() {
        assert_eq!(check(&rgba(16)), Ok(Icon { side: 16 }));
        assert_eq!(check(&rgba(256)), Ok(Icon { side: 256 }));
        for (color, ch) in [(0u8, 1usize), (2, 3), (4, 2), (6, 4)] {
            let p = png(&[ihdr(32, 32, 8, color, 0), idat(&raw(32, ch)), chunk(b"IEND", b"")]);
            assert_eq!(check(&p), Ok(Icon { side: 32 }), "colour type {color}");
        }
        let pal = png(&[ihdr(32, 32, 8, 3, 0), chunk(b"PLTE", &[0, 0, 0, 255, 255, 255]), chunk(b"tRNS", &[0, 255]), idat(&raw(32, 1)), chunk(b"IEND", b"")]);
        assert_eq!(check(&pal), Ok(Icon { side: 32 }));
        let with_meta = png(&[ihdr(32, 32, 8, 6, 0), chunk(b"gAMA", &[0, 1, 0, 0]), chunk(b"sRGB", &[0]), chunk(b"pHYs", &[0; 9]), idat(&raw(32, 4)), chunk(b"IEND", b"")]);
        assert_eq!(check(&with_meta), Ok(Icon { side: 32 }));
        // The data in several IDAT chunks.
        let r = raw(32, 4);
        let mut e = ZlibEncoder::new(Vec::new(), Compression::default());
        e.write_all(&r).unwrap();
        let z = e.finish().unwrap();
        let (a, b) = z.split_at(z.len() / 2);
        let split = png(&[ihdr(32, 32, 8, 6, 0), chunk(b"IDAT", a), chunk(b"IDAT", b), chunk(b"IEND", b"")]);
        assert_eq!(check(&split), Ok(Icon { side: 32 }));
    }

    fn refused(data: &[u8], why: &str) {
        match check(data) {
            Err(e) => assert!(e.contains(why), "{e:?} does not say {why:?}"),
            Ok(i) => panic!("accepted {i:?}, expected {why:?}"),
        }
    }

    #[test]
    fn what_is_not_a_png_is_refused() {
        refused(b"", "signature");
        refused(b"<svg xmlns=\"http://www.w3.org/2000/svg\"/>", "SVG");
        refused(b"GIF89a", "signature");
        refused(&SIGNATURE, "chunk header");
        let mut sig = rgba(16);
        sig[1] = b'X';
        refused(&sig, "signature");
        refused(&vec![0; MAX_FILE + 1], "larger");
    }

    #[test]
    fn the_header_rules() {
        let end = || chunk(b"IEND", b"");
        refused(&png(&[ihdr(16, 32, 8, 6, 0), idat(&raw(16, 4)), end()]), "not square");
        refused(&png(&[ihdr(8, 8, 8, 6, 0), idat(&raw(8, 4)), end()]), "side must be");
        refused(&png(&[ihdr(1025, 1025, 8, 6, 0), idat(&[0]), end()]), "side must be");
        refused(&png(&[ihdr(0, 0, 8, 6, 0), idat(&[0]), end()]), "side must be");
        refused(&png(&[ihdr(16, 16, 16, 6, 0), idat(&raw(16, 8)), end()]), "bit depth");
        refused(&png(&[ihdr(16, 16, 4, 3, 0), idat(&raw(16, 1)), end()]), "bit depth");
        refused(&png(&[ihdr(16, 16, 8, 5, 0), idat(&raw(16, 4)), end()]), "colour type");
        refused(&png(&[ihdr(16, 16, 8, 6, 1), idat(&raw(16, 4)), end()]), "interlaced");
        refused(&png(&[chunk(b"IDAT", &[0]), end()]), "IHDR");
        refused(&png(&[chunk(b"IHDR", &[0; 12]), end()]), "IHDR");
        refused(&png(&[ihdr(16, 16, 8, 6, 0), ihdr(16, 16, 8, 6, 0), idat(&raw(16, 4)), end()]), "not accepted");
    }

    #[test]
    fn only_listed_chunks_are_accepted() {
        let end = || chunk(b"IEND", b"");
        for kind in [b"tEXt", b"zTXt", b"iTXt", b"iCCP", b"tIME", b"eXIf", b"bKGD", b"cHRM", b"sBIT", b"hIST", b"sPLT", b"acTL", b"prVt", b"IDAx"] {
            refused(&png(&[ihdr(16, 16, 8, 6, 0), chunk(kind, b"x"), idat(&raw(16, 4)), end()]), "not accepted");
        }
        let after = png(&[ihdr(16, 16, 8, 6, 0), idat(&raw(16, 4)), chunk(b"tEXt", b"a\0b"), end()]);
        refused(&after, "not accepted");
        refused(&png(&[ihdr(16, 16, 8, 6, 0), chunk(b"gAMA", &[0; 4]), chunk(b"gAMA", &[0; 4]), idat(&raw(16, 4)), end()]), "twice");
        refused(&png(&[ihdr(16, 16, 8, 6, 0), idat(&raw(16, 4)), chunk(b"gAMA", &[0; 4]), end()]), "after the image data");
        refused(&png(&[ihdr(16, 16, 8, 6, 0), chunk(b"gAMA", &[0; 3]), idat(&raw(16, 4)), end()]), "not valid here");
        refused(&png(&[ihdr(16, 16, 8, 0, 0), chunk(b"PLTE", &[0; 3]), idat(&raw(16, 1)), end()]), "not valid here");
        refused(&png(&[ihdr(16, 16, 8, 3, 0), idat(&raw(16, 1)), end()]), "PLTE");
        let split = {
            let mut e = ZlibEncoder::new(Vec::new(), Compression::default());
            e.write_all(&raw(16, 4)).unwrap();
            let z = e.finish().unwrap();
            let (a, b) = z.split_at(z.len() / 2);
            png(&[ihdr(16, 16, 8, 6, 0), chunk(b"IDAT", a), chunk(b"gAMA", &[0; 4]), chunk(b"IDAT", b), end()])
        };
        refused(&split, "after the image data");
    }

    #[test]
    fn framing_and_checksums() {
        let good = rgba(16);
        let mut crc = good.clone();
        let n = crc.len();
        crc[n - 1] ^= 1;
        refused(&crc, "checksum");
        let mut bit = good.clone();
        bit[20] ^= 1;
        refused(&bit, "checksum");
        refused(&good[..good.len() - 1], "chunk header");
        refused(&good[..good.len() - 12], "chunk header");
        let mut trailing = good.clone();
        trailing.push(0);
        refused(&trailing, "after IEND");
        let mut huge = good.clone();
        huge[8..12].copy_from_slice(&u32::MAX.to_be_bytes());
        refused(&huge, "longer than the file");
        // A chunk type that is not letters.
        let mut kind = good;
        kind[12] = b'1';
        refused(&kind, "bad type");
        refused(&png(&[ihdr(16, 16, 8, 6, 0), idat(&raw(16, 4))]), "chunk header");
        refused(&png(&[ihdr(16, 16, 8, 6, 0), chunk(b"IEND", b"")]), "no image data");
        refused(&png(&[ihdr(16, 16, 8, 6, 0), idat(&raw(16, 4)), chunk(b"IEND", b"x")]), "IEND is not empty");
    }

    #[test]
    fn the_image_data_has_exactly_the_promised_size() {
        let end = || chunk(b"IEND", b"");
        // Too little, too much, a bomb, garbage, and bytes behind the stream.
        refused(&png(&[ihdr(16, 16, 8, 6, 0), idat(&raw(15, 4)), end()]), "inflates to");
        refused(&png(&[ihdr(16, 16, 8, 6, 0), idat(&raw(17, 4)), end()]), "inflates to");
        refused(&png(&[ihdr(16, 16, 8, 6, 0), idat(&vec![0u8; 64 * 1024 * 1024]), end()]), "inflates to");
        refused(&png(&[ihdr(16, 16, 8, 6, 0), chunk(b"IDAT", b"not zlib at all"), end()]), "inflate");
        let mut e = ZlibEncoder::new(Vec::new(), Compression::default());
        e.write_all(&raw(16, 4)).unwrap();
        let mut z = e.finish().unwrap();
        z.extend_from_slice(b"tail");
        refused(&png(&[ihdr(16, 16, 8, 6, 0), chunk(b"IDAT", &z), end()]), "does not end");
        // A truncated stream.
        let mut e = ZlibEncoder::new(Vec::new(), Compression::default());
        e.write_all(&raw(16, 4)).unwrap();
        let z = e.finish().unwrap();
        refused(&png(&[ihdr(16, 16, 8, 6, 0), chunk(b"IDAT", &z[..z.len() - 6]), end()]), "inflate");
        // A scanline with an unknown filter type.
        let mut bad = raw(16, 4);
        bad[0] = 9;
        refused(&png(&[ihdr(16, 16, 8, 6, 0), idat(&bad), end()]), "filter");
    }

    #[test]
    fn files_from_real_encoders_pass() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data");
        let mut n = 0;
        for e in std::fs::read_dir(dir).unwrap().flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if let Some(side) = name.strip_prefix("ok-").and_then(|r| r.split(|c: char| !c.is_ascii_digit()).next()).and_then(|d| d.parse().ok()) {
                let data = std::fs::read(e.path()).unwrap();
                assert_eq!(check(&data), Ok(Icon { side }), "{name}");
                n += 1;
            } else if name.starts_with("bad-") {
                assert!(check(&std::fs::read(e.path()).unwrap()).is_err(), "{name}");
                n += 1;
            }
        }
        assert!(n >= 8, "the fixtures are missing ({n})");
    }
}
