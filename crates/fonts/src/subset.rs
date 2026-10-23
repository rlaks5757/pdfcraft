//! A subset Type0/CIDFontType2 (Identity-H) font for the text of one generated appearance, cut
//! from a bundled craft-fonts face (the Korean one), with a ToUnicode map for copy and search.
//!
//! Used when the text has characters the standard fonts can't encode (`?` in WinAnsi). The font
//! dictionary is built with its stream objects *nested* (FontFile2, ToUnicode): the appearance
//! builder has no document, so the caller moves them into indirect objects when it stores the
//! appearance (`pdfcraft_annot`'s `apply_appearance`).

use std::collections::BTreeMap;
use std::fmt::Write as _;

use pdfcraft_cos::{Dict, Object, PdfString, Stream};
use skrifa::{
    FontRef, MetadataProvider,
    instance::{LocationRef, Size},
    raw::TableProvider,
};

use crate::CraftFont;

/// Whether [`build`] can cut a subset of `face`: a TrueType (`glyf`) program with outline bounds
/// and glyph ids that fit a CID. CFF faces aren't supported (their program needs FontFile3).
fn usable(face: &CraftFont) -> bool {
    FontRef::new(face.bytes).is_ok_and(|f| {
        f.glyf().is_ok() && f.metrics(Size::unscaled(), LocationRef::default()).bounds.is_some() && f.maxp().is_ok_and(|m| m.num_glyphs() < u16::MAX)
    })
}

/// The face used for text outside WinAnsi: the first usable Korean craft-fonts face (it also has
/// the Latin letters), if this build embeds one.
pub fn face() -> Option<&'static CraftFont> {
    crate::ui_korean_fonts().into_iter().find(|f| usable(f))
}

/// The face, when it has a glyph for every character of `text` (line breaks aside).
pub fn face_for(text: &str) -> Option<&'static CraftFont> {
    face().filter(|f| crate::embedding::covers(f.bytes, text))
}

/// Advance of `s` at `size` points in `face` (characters it lacks count as zero).
pub fn width(face: &CraftFont, s: &str, size: f64) -> f64 {
    let Ok(font) = FontRef::new(face.bytes) else { return 0.0 };
    let upem = f64::from(font.head().map_or(1000, |h| h.units_per_em()).max(1));
    let metrics = font.glyph_metrics(Size::unscaled(), LocationRef::default());
    let charmap = font.charmap();
    let sum: f64 = s.chars().map(|c| charmap.map(c).and_then(|g| metrics.advance_width(g)).map_or(0.0, f64::from)).sum();
    sum / upem * size
}

/// A subset font for one run of text.
#[derive(Clone)]
pub struct SubsetFont {
    /// The Type0 font dictionary (streams nested, see the module doc).
    pub dict: Dict,
    cids: BTreeMap<char, u16>,
}

impl SubsetFont {
    /// The text as 2-byte big-endian CIDs, a PDF hex string (`<…>`) for `Tj`. Characters the
    /// subset doesn't hold (none, when built from the same text) draw `.notdef`.
    pub fn show(&self, s: &str) -> Vec<u8> {
        let mut out = String::from("<");
        for c in s.chars() {
            let _ = write!(out, "{:04X}", self.cids.get(&c).copied().unwrap_or(0));
        }
        out.push('>');
        out.into_bytes()
    }
}

/// The subsets cut most recently, by glyph set, so the same characters are cut once.
const CACHE_LIMIT: usize = 32;

thread_local! {
    static CACHE: std::cell::RefCell<std::collections::HashMap<u64, SubsetFont>> = std::cell::RefCell::new(std::collections::HashMap::new());
}

/// [`build`], reusing a subset of the same characters cut before.
pub fn build(face: &CraftFont, text: &str) -> Option<SubsetFont> {
    let key = glyph_set_hash(face, text);
    if let Some(hit) = CACHE.with(|c| c.borrow().get(&key).cloned()) {
        return Some(hit);
    }
    let font = cut(face, text)?;
    CACHE.with(|c| {
        let mut c = c.borrow_mut();
        if c.len() >= CACHE_LIMIT {
            c.clear();
        }
        c.insert(key, font.clone());
    });
    Some(font)
}

/// A 64-bit hash of the face and the distinct characters of `text` (line breaks aside).
fn glyph_set_hash(face: &CraftFont, text: &str) -> u64 {
    let set: std::collections::BTreeSet<char> = text.chars().filter(|c| !matches!(c, '\n' | '\r')).collect();
    // FNV-1a over the face's size and name, then the characters in order.
    let mut h = 0xcbf2_9ce4_8422_2325u64;
    let mut mix = |v: u64| {
        for b in v.to_le_bytes() {
            h = (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3);
        }
    };
    mix(face.bytes.len() as u64);
    face.family.bytes().for_each(|b| mix(u64::from(b)));
    set.into_iter().for_each(|c| mix(u64::from(c)));
    h
}

/// Cut a subset of `face` holding the glyphs of `text` (line breaks aside).
fn cut(face: &CraftFont, text: &str) -> Option<SubsetFont> {
    if !usable(face) {
        return None;
    }
    let font = FontRef::new(face.bytes).ok()?;
    let charmap = font.charmap();
    let mut remap = subsetter::GlyphRemapper::new();
    let mut chars: BTreeMap<char, u16> = BTreeMap::new();
    for c in text.chars().filter(|c| !matches!(c, '\n' | '\r')) {
        if chars.contains_key(&c) {
            continue;
        }
        let old = u16::try_from(charmap.map(c)?.to_u32()).ok().filter(|g| *g != 0)?;
        chars.insert(c, remap.remap(old));
    }
    let program = subsetter::subset(face.bytes, 0, &remap).ok()?;

    let m = font.metrics(Size::unscaled(), LocationRef::default());
    let scale = 1000.0 / f64::from(m.units_per_em.max(1));
    let advances = font.glyph_metrics(Size::unscaled(), LocationRef::default());
    // One /W entry per new glyph id (the CID), in order; ToUnicode maps each CID back to its char.
    let mut widths = vec![0.0; usize::from(remap.num_gids())];
    let mut cmap = String::from(
        "/CIDInit /ProcSet findresource begin\n12 dict begin\nbegincmap\n/CMapName /PCUnicode def\n/CMapType 2 def\n1 begincodespacerange\n<0000> <FFFF>\nendcodespacerange\n",
    );
    for new in 0..remap.num_gids() {
        let old = remap.remapped_gids().nth(usize::from(new))?;
        widths[usize::from(new)] = advances.advance_width(skrifa::GlyphId::new(u32::from(old))).map_or(0.0, f64::from) * scale;
    }
    let entries: Vec<(char, u16)> = chars.iter().map(|(c, g)| (*c, *g)).collect();
    // A CMap block holds at most 100 entries.
    for block in entries.chunks(100) {
        let _ = writeln!(cmap, "{} beginbfchar", block.len());
        for (c, cid) in block {
            let hex: String = c.to_string().encode_utf16().map(|u| format!("{u:04X}")).collect();
            let _ = writeln!(cmap, "<{cid:04X}> <{hex}>");
        }
        cmap.push_str("endbfchar\n");
    }
    cmap.push_str("endcmap\nCMapName currentdict /CMap defineresource pop\nend\nend\n");

    // ISO 32000-2 9.6.4: six capital letters, here from the face and the whole glyph set.
    let tag = |key: &str| {
        let mut h = glyph_set_hash(face, text);
        let letters: String = (0..6)
            .map(|_| {
                let l = char::from(b'A' + (h % 26) as u8);
                h /= 26;
                l
            })
            .collect();
        format!("{letters}+{key}")
    };
    let base = tag(&face.family.chars().filter(char::is_ascii_alphanumeric).collect::<String>());
    let mut file = Dict::new();
    file.set(b"Length1".to_vec(), Object::Int(program.len() as i64));
    let mut descriptor = Dict::new();
    descriptor.set(b"Type".to_vec(), Object::name("FontDescriptor"));
    descriptor.set(b"FontName".to_vec(), Object::name(&base));
    descriptor.set(b"Flags".to_vec(), Object::Int(4));
    descriptor.set(b"ItalicAngle".to_vec(), Object::Real(f64::from(m.italic_angle)));
    descriptor.set(b"Ascent".to_vec(), Object::Real(f64::from(m.ascent) * scale));
    descriptor.set(b"Descent".to_vec(), Object::Real(f64::from(m.descent) * scale));
    descriptor.set(b"CapHeight".to_vec(), Object::Real(f64::from(m.cap_height.unwrap_or(m.ascent)) * scale));
    descriptor.set(b"StemV".to_vec(), Object::Int(80));
    let b = m.bounds?;
    descriptor.set(b"FontBBox".to_vec(), Object::Array([b.x_min, b.y_min, b.x_max, b.y_max].map(|v| Object::Real(f64::from(v) * scale)).to_vec()));
    descriptor.set(b"FontFile2".to_vec(), Object::Stream(Stream::flate(file, &program)));

    let mut system = Dict::new();
    system.set(b"Registry".to_vec(), Object::String(PdfString::literal(b"Adobe".to_vec())));
    system.set(b"Ordering".to_vec(), Object::String(PdfString::literal(b"Identity".to_vec())));
    system.set(b"Supplement".to_vec(), Object::Int(0));
    let mut cid = Dict::new();
    cid.set(b"Type".to_vec(), Object::name("Font"));
    cid.set(b"Subtype".to_vec(), Object::name("CIDFontType2"));
    cid.set(b"BaseFont".to_vec(), Object::name(&base));
    cid.set(b"CIDSystemInfo".to_vec(), Object::Dict(system));
    cid.set(b"FontDescriptor".to_vec(), Object::Dict(descriptor));
    cid.set(b"CIDToGIDMap".to_vec(), Object::name("Identity"));
    cid.set(b"W".to_vec(), Object::Array(vec![Object::Int(0), Object::Array(widths.into_iter().map(Object::Real).collect())]));
    let mut top = Dict::new();
    top.set(b"Type".to_vec(), Object::name("Font"));
    top.set(b"Subtype".to_vec(), Object::name("Type0"));
    top.set(b"BaseFont".to_vec(), Object::name(&base));
    top.set(b"Encoding".to_vec(), Object::name("Identity-H"));
    top.set(b"DescendantFonts".to_vec(), Object::Array(vec![Object::Dict(cid)]));
    top.set(b"ToUnicode".to_vec(), Object::Stream(Stream::flate(Dict::new(), cmap.as_bytes())));
    Some(SubsetFont { dict: top, cids: chars })
}
