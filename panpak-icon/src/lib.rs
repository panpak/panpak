// SPDX-License-Identifier: MIT OR Apache-2.0
//! The check that an icon file passes before it is installed in the user's icon folder, and the cleaning that makes a
//! real-world PNG pass it.
//!
//! An icon is untrusted input that host libraries decode outside the sandbox (gdk-pixbuf, Qt). The rules
//! (docs/DESKTOP-INTEGRATION.md, section 1): PNG only; the chunks are walked with their CRC and only a short list
//! is allowed (no text, profile or private chunk); the image is square, 16 to 1024 pixels, 8 bits, not interlaced;
//! the compressed data inflates to exactly the size the header promises, and nothing follows it. The file is
//! installed as it is read: nothing is re-encoded. Pixels are never decoded here.
//!
//! [`normalize`] is the one function that turns a PNG into the form [`check`] accepts: it keeps `IHDR`, `PLTE`, `tRNS`,
//! `IDAT` and `IEND` byte for byte and drops every other chunk (text, colour profile, gamma, resolution, time, background,
//! animation and unknown chunks). The pixels and the image data are not touched, and the transparency of a palette image
//! stays (`tRNS`). `pack` and `convert` run it; `install` only checks, so it accepts nothing but a normalized file.

use flate2::{bufread::ZlibDecoder, Crc};
use std::io::Read;

/// The largest icon file.
pub const MAX_FILE: usize = 256 * 1024;
/// The largest file [`normalize`] reads (a real icon may carry a few large text or profile chunks).
pub const MAX_INPUT: usize = 4 * 1024 * 1024;
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
            b"PLTE" | b"tRNS" => {
                if idat_started {
                    return Err(format!("chunk {name} after the image data"));
                }
                if seen.contains(&kind) {
                    return Err(format!("chunk {name} twice"));
                }
                let ok = match &kind {
                    b"PLTE" => matches!(color, 2 | 3 | 6) && len.is_multiple_of(3) && (3..=768).contains(&len),
                    _ => matches!(color, 0 | 2 | 3) && len <= 768,
                };
                if !ok {
                    return Err(format!("chunk {name} is not valid here"));
                }
            }
            _ => return Err(format!("chunk {name} is not accepted (only IHDR PLTE tRNS IDAT IEND; `panpak pack` removes the others)")),
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

/// The result of [`normalize`]: the cleaned file and the chunks that were dropped (type and count, in file order).
#[derive(Debug, PartialEq, Eq)]
pub struct Normalized {
    pub data: Vec<u8>,
    pub dropped: Vec<(String, usize)>,
}

/// Turn a PNG into the form [`check`] accepts, deterministically: the signature, then `IHDR`, `PLTE`, `tRNS`, every `IDAT`
/// and `IEND` copied byte for byte in the order of the file, and nothing else. Every chunk of the input, kept or dropped,
/// must have a right checksum, and the input must end with `IEND` (anything after it is dropped). The result is run
/// through [`check`]: an image that is not acceptable for another reason (not square, interlaced, 16 bits ...) is an
/// error. Normalizing a normalized file gives the same bytes and drops nothing.
pub fn normalize(data: &[u8]) -> Result<Normalized, String> {
    if data.len() > MAX_INPUT {
        return Err(format!("larger than {} MiB", MAX_INPUT / (1024 * 1024)));
    }
    if data.len() < SIGNATURE.len() || data[..8] != SIGNATURE {
        return Err("not a PNG file (the signature is wrong; SVG and other formats are not accepted)".into());
    }
    let mut out = SIGNATURE.to_vec();
    let mut dropped: Vec<(String, usize)> = Vec::new();
    let mut pos = 8;
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
        let mut crc = Crc::new();
        crc.update(&data[pos + 4..pos + 8 + len]);
        if crc.sum() != be32(&data[pos + 8 + len..]) {
            return Err(format!("bad checksum in chunk {}", String::from_utf8_lossy(&kind)));
        }
        let whole = &data[pos..pos + 12 + len];
        pos += 12 + len;
        if matches!(&kind, b"IHDR" | b"PLTE" | b"tRNS" | b"IDAT" | b"IEND") {
            out.extend_from_slice(whole);
        } else {
            let name = String::from_utf8_lossy(&kind).into_owned();
            match dropped.iter_mut().find(|(n, _)| *n == name) {
                Some((_, c)) => *c += 1,
                None => dropped.push((name, 1)),
            }
        }
        if &kind == b"IEND" {
            if pos != data.len() {
                dropped.push(("(data after IEND)".into(), 1));
            }
            break;
        }
    }
    check(&out)?;
    Ok(Normalized { data: out, dropped })
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
        // Gamma, colour space and resolution are metadata: they are dropped by `normalize`, and `check` does not accept them.
        let with_meta = png(&[ihdr(32, 32, 8, 6, 0), chunk(b"gAMA", &[0, 1, 0, 0]), chunk(b"sRGB", &[0]), chunk(b"pHYs", &[0; 9]), idat(&raw(32, 4)), chunk(b"IEND", b"")]);
        refused(&with_meta, "chunk gAMA is not accepted");
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
        refused(&png(&[ihdr(16, 16, 8, 3, 0), chunk(b"PLTE", &[0; 3]), chunk(b"tRNS", &[0]), chunk(b"tRNS", &[0]), idat(&raw(16, 1)), end()]), "twice");
        refused(&png(&[ihdr(16, 16, 8, 3, 0), chunk(b"PLTE", &[0; 3]), idat(&raw(16, 1)), chunk(b"tRNS", &[0]), end()]), "after the image data");
        refused(&png(&[ihdr(16, 16, 8, 6, 0), chunk(b"tRNS", &[0; 2]), idat(&raw(16, 4)), end()]), "not valid here");
        refused(&png(&[ihdr(16, 16, 8, 6, 0), chunk(b"gAMA", &[0; 4]), idat(&raw(16, 4)), end()]), "not accepted");
        refused(&png(&[ihdr(16, 16, 8, 0, 0), chunk(b"PLTE", &[0; 3]), idat(&raw(16, 1)), end()]), "not valid here");
        refused(&png(&[ihdr(16, 16, 8, 3, 0), idat(&raw(16, 1)), end()]), "PLTE");
        let split = {
            let mut e = ZlibEncoder::new(Vec::new(), Compression::default());
            e.write_all(&raw(16, 4)).unwrap();
            let z = e.finish().unwrap();
            let (a, b) = z.split_at(z.len() / 2);
            png(&[ihdr(16, 16, 8, 6, 0), chunk(b"IDAT", a), chunk(b"tEXt", b"k\0v"), chunk(b"IDAT", b), end()])
        };
        refused(&split, "not accepted");
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

    /// A palette icon with a transparent first colour, plus a pile of chunks that must go.
    fn busy_palette() -> Vec<u8> {
        let trns = chunk(b"tRNS", &[0, 255]);
        png(&[
            ihdr(32, 32, 8, 3, 0),
            chunk(b"gAMA", &[0, 1, 0, 0]),
            chunk(b"cHRM", &[0; 32]),
            chunk(b"sRGB", &[0]),
            chunk(b"iCCP", b"p\0\0xyz"),
            chunk(b"PLTE", &[0, 0, 0, 255, 255, 255]),
            trns,
            chunk(b"bKGD", &[0]),
            chunk(b"pHYs", &[0; 9]),
            chunk(b"tIME", &[7, 233, 1, 1, 0, 0, 0]),
            chunk(b"tEXt", b"Software\0test"),
            chunk(b"tEXt", b"Comment\0two"),
            chunk(b"zTXt", b"k\0\0x"),
            chunk(b"iTXt", b"k\0\0\0\0\0x"),
            chunk(b"acTL", &[0; 8]),
            chunk(b"fcTL", &[0; 26]),
            chunk(b"prVt", b"private"),
            idat(&raw(32, 1)),
            chunk(b"fdAT", &[0; 8]),
            chunk(b"eXIf", b"x"),
            chunk(b"IEND", b""),
        ])
    }

    fn clean_palette() -> Vec<u8> {
        png(&[ihdr(32, 32, 8, 3, 0), chunk(b"PLTE", &[0, 0, 0, 255, 255, 255]), chunk(b"tRNS", &[0, 255]), idat(&raw(32, 1)), chunk(b"IEND", b"")])
    }

    #[test]
    fn normalize_keeps_five_kinds_of_chunk_and_the_transparency() {
        let n = normalize(&busy_palette()).unwrap();
        // Byte for byte the file that has only the kept chunks: nothing of the image data was touched.
        assert_eq!(n.data, clean_palette());
        assert_eq!(check(&n.data), Ok(Icon { side: 32 }));
        // The transparency table is still there, unchanged.
        let at = n.data.windows(4).position(|w| w == b"tRNS").unwrap();
        assert_eq!(&n.data[at + 4..at + 6], &[0, 255]);
        let names: Vec<_> = n.dropped.iter().map(|(k, c)| format!("{k}x{c}")).collect();
        assert_eq!(
            names,
            ["gAMAx1", "cHRMx1", "sRGBx1", "iCCPx1", "bKGDx1", "pHYsx1", "tIMEx1", "tEXtx2", "zTXtx1", "iTXtx1", "acTLx1", "fcTLx1", "prVtx1", "fdATx1", "eXIfx1"]
        );
        // The busy file itself is not acceptable for install.
        assert!(check(&busy_palette()).is_err());
    }

    #[test]
    fn normalizing_twice_gives_the_same_bytes_and_a_clean_file_is_left_alone() {
        let once = normalize(&busy_palette()).unwrap();
        let twice = normalize(&once.data).unwrap();
        assert_eq!(twice.data, once.data);
        assert!(twice.dropped.is_empty());
        assert_eq!(normalize(&busy_palette()).unwrap(), once, "the same input always gives the same output");
        let clean = rgba(48);
        assert_eq!(normalize(&clean).unwrap(), Normalized { data: clean, dropped: vec![] });
        // The order of the kept chunks is the order of the file, also with the image data in several chunks.
        let r = raw(16, 4);
        let mut e = ZlibEncoder::new(Vec::new(), Compression::default());
        e.write_all(&r).unwrap();
        let z = e.finish().unwrap();
        let (a, b) = z.split_at(z.len() / 2);
        let split = png(&[ihdr(16, 16, 8, 6, 0), chunk(b"IDAT", a), chunk(b"tEXt", b"k\0v"), chunk(b"IDAT", b), chunk(b"IEND", b"")]);
        let n = normalize(&split).unwrap();
        assert_eq!(n.data, png(&[ihdr(16, 16, 8, 6, 0), chunk(b"IDAT", a), chunk(b"IDAT", b), chunk(b"IEND", b"")]));
        assert_eq!(check(&n.data), Ok(Icon { side: 16 }));
    }

    #[test]
    fn normalize_refuses_what_it_cannot_make_acceptable() {
        let err = |d: &[u8], why: &str| match normalize(d) {
            Err(e) => assert!(e.contains(why), "{e:?} does not say {why:?}"),
            Ok(n) => panic!("accepted, dropped {:?}", n.dropped),
        };
        err(b"<svg/>", "signature");
        err(&[], "signature");
        // A bad checksum is refused also in a chunk that would be dropped.
        let mut bad = busy_palette();
        let at = bad.windows(4).position(|w| w == b"tEXt").unwrap();
        bad[at + 5] ^= 1;
        err(&bad, "bad checksum in chunk tEXt");
        err(&busy_palette()[..60], "chunk");
        err(&png(&[ihdr(32, 32, 8, 6, 0), idat(&raw(32, 4))]), "IEND");
        // Image rules are the ones of `check`.
        err(&png(&[ihdr(32, 32, 8, 6, 1), chunk(b"tEXt", b"k\0v"), idat(&raw(32, 4)), chunk(b"IEND", b"")]), "interlaced");
        err(&png(&[ihdr(32, 16, 8, 6, 0), idat(&raw(16, 4)), chunk(b"IEND", b"")]), "not square");
        err(&png(&[ihdr(8, 8, 8, 6, 0), idat(&raw(8, 4)), chunk(b"IEND", b"")]), "side must be");
        err(&png(&[ihdr(32, 32, 16, 6, 0), idat(&raw(32, 8)), chunk(b"IEND", b"")]), "bit depth");
        err(&png(&[ihdr(32, 32, 8, 6, 0), idat(&raw(31, 4)), chunk(b"IEND", b"")]), "inflates to");
        err(&png(&[ihdr(32, 32, 8, 3, 0), idat(&raw(32, 1)), chunk(b"IEND", b"")]), "PLTE");
        err(&vec![0; MAX_INPUT + 1], "larger");
        // A result over the limit of an icon is refused, not cut.
        let big = {
            let r = raw(256, 4);
            let mut e = ZlibEncoder::new(Vec::new(), Compression::none());
            e.write_all(&r).unwrap();
            png(&[ihdr(256, 256, 8, 6, 0), chunk(b"IDAT", &e.finish().unwrap()), chunk(b"IEND", b"")])
        };
        err(&big, "larger than 256 KiB");
    }

    #[test]
    fn trailing_data_after_iend_is_dropped_and_said() {
        let mut d = rgba(16);
        d.extend_from_slice(b"trailing");
        let n = normalize(&d).unwrap();
        assert_eq!(n.data, rgba(16));
        assert_eq!(n.dropped, [("(data after IEND)".to_string(), 1)]);
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
                assert_eq!(normalize(&data).unwrap().data, data, "{name}: a clean file is not changed");
                n += 1;
            } else if name.starts_with("bad-") {
                let data = std::fs::read(e.path()).unwrap();
                assert!(check(&data).is_err(), "{name}");
                // A text chunk or a colour profile only needs cleaning; the other files are not acceptable.
                match name.as_str() {
                    "bad-text.png" | "bad-icc.png" => {
                        let n = normalize(&data).unwrap();
                        assert_eq!(check(&n.data), Ok(Icon { side: 32 }), "{name}");
                        assert_eq!(n.dropped.len(), 1, "{name}: {:?}", n.dropped);
                    }
                    _ => assert!(normalize(&data).is_err(), "{name}"),
                }
                n += 1;
            }
        }
        assert!(n >= 8, "the fixtures are missing ({n})");
    }
}
