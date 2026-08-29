//! Compact, lossless encoding for AT-URIs (`at://…`) so they fit in small card
//! files. Reversible tricks:
//!   * `did:plc:` identifiers (24 base32 chars) pack back into 15 raw bytes.
//!     Other authorities (e.g. `did:web:…`) are stored literally.
//!   * Known collection NSIDs become a 1-byte dictionary index. An unknown
//!     collection is rejected (so we never silently store an oversized URI).
//!   * Record keys that are TIDs (13 base32-sortable chars) pack into 8 bytes;
//!     other record keys are stored literally.
//!
//! On-card layout — the first byte is a marker plain UTF-8 text can't start with:
//!   0xA5 <flags> <authority> [collection] [rkey]
//!
//! Decoding stops as soon as it has consumed those fields and reports how far it
//! got, so a caller may store more of its own data straight after the blob.
//! rocksky-desktop uses that to append a newline and a library-id fallback; the
//! payload helpers at the bottom of this module speak that same layout, so a
//! card written by either tool reads in the other.

const MARK: u8 = 0xA5;
const PLC_LEN: usize = 15;

const F_AUTH_PLC: u8 = 0b0000_0001;
const F_HAS_COLL: u8 = 0b0000_0010;
const F_COLL_DICT: u8 = 0b0000_0100;
const F_HAS_RKEY: u8 = 0b0000_1000;
const F_RKEY_TID: u8 = 0b0001_0000;
const F_AUTH_DICT: u8 = 0b0010_0000;

const B32: &[u8; 32] = b"abcdefghijklmnopqrstuvwxyz234567";
const TID_B32: &[u8; 32] = b"234567abcdefghijklmnopqrstuvwxyz";

/// Configurable dictionaries. Both lists are **index-addressed and append-only**:
/// reordering or removing an entry changes existing entries' indices and would
/// make previously written cards decode wrong. Built-in defaults come first; a
/// user's config extends the lists (see `config.rs`).
#[derive(Debug, Clone, Default)]
pub struct Dict {
    /// Collection NSIDs, encoded to a 1-byte index.
    pub collections: Vec<String>,
    /// Authorities (DIDs/handles) encoded to a 1-byte index — most compact of
    /// all, so repeated authorities cost 1 byte instead of 15+.
    pub authorities: Vec<String>,
}

pub fn looks_like_aturi(s: &str) -> bool {
    s.starts_with("at://")
}

pub fn is_encoded(data: &[u8]) -> bool {
    data.first() == Some(&MARK)
}

/// Encode an `at://` URI to its compact byte form.
///
/// Errors (rather than falling back) when the URI can't be represented
/// compactly — notably an **unknown collection**, so we never silently store a
/// URI that won't round-trip through the dictionary.
pub fn encode(uri: &str, dict: &Dict) -> Result<Vec<u8>, String> {
    let rest = uri
        .strip_prefix("at://")
        .ok_or_else(|| "not an at:// URI".to_string())?;
    let segs: Vec<&str> = rest.split('/').collect();
    if segs[0].is_empty() || segs.len() > 3 {
        return Err(format!(
            "unsupported at:// URI: expected authority[/collection[/rkey]], got {uri:?}"
        ));
    }

    let mut flags = 0u8;
    let mut body = Vec::new();

    // authority: prefer the dictionary (1 byte), then did:plc packing (15
    // bytes), then a literal string (did:web, handles, …).
    if let Some(i) = dict_index(&dict.authorities, segs[0]) {
        flags |= F_AUTH_DICT;
        body.push(i);
    } else if let Some(raw) = plc_bytes(segs[0]) {
        flags |= F_AUTH_PLC;
        body.extend_from_slice(&raw);
    } else {
        push_lit(&mut body, segs[0].as_bytes())
            .ok_or_else(|| "authority is too long".to_string())?;
    }

    // collection: must be a known NSID (from the dictionary).
    if let Some(&coll) = segs.get(1) {
        let idx = dict_index(&dict.collections, coll).ok_or_else(|| {
            format!(
                "unknown collection {coll:?}; known collections: {}",
                if dict.collections.is_empty() {
                    "(none configured)".to_string()
                } else {
                    dict.collections.join(", ")
                }
            )
        })?;
        flags |= F_HAS_COLL | F_COLL_DICT;
        body.push(idx);
    }

    // record key: TID packs to 8 bytes; otherwise stored literally.
    if let Some(&rk) = segs.get(2) {
        flags |= F_HAS_RKEY;
        match pack_tid(rk) {
            Some(t) => {
                flags |= F_RKEY_TID;
                body.extend_from_slice(&t);
            }
            None => push_lit(&mut body, rk.as_bytes())
                .ok_or_else(|| "record key is too long".to_string())?,
        }
    }

    let mut out = vec![MARK, flags];
    out.extend_from_slice(&body);

    // Safety net: the value we store must reconstruct the exact input.
    if decode(&out, dict).map(|(u, _)| u).as_deref() != Some(uri) {
        return Err("internal error: URI did not round-trip".to_string());
    }
    Ok(out)
}

/// Decode a compact AT-URI, returning it and how many bytes it occupied, or
/// `None` if `data` isn't one of ours.
///
/// The length matters: anything after the blob is the caller's, not ours.
pub fn decode(data: &[u8], dict: &Dict) -> Option<(String, usize)> {
    if data.first() != Some(&MARK) {
        return None;
    }
    let flags = *data.get(1)?;
    let mut p = 2usize;

    let authority = if flags & F_AUTH_DICT != 0 {
        let idx = *data.get(p)?;
        p += 1;
        dict.authorities.get(idx as usize)?.clone()
    } else if flags & F_AUTH_PLC != 0 {
        let raw = data.get(p..p + PLC_LEN)?;
        p += PLC_LEN;
        format!("did:plc:{}", b32_encode(raw))
    } else {
        let (b, np) = read_lit(data, p)?;
        p = np;
        String::from_utf8_lossy(b).into_owned()
    };
    let mut uri = format!("at://{authority}");

    if flags & F_HAS_COLL != 0 {
        let coll = if flags & F_COLL_DICT != 0 {
            let idx = *data.get(p)?;
            p += 1;
            dict.collections.get(idx as usize)?.to_string()
        } else {
            let (b, np) = read_lit(data, p)?;
            p = np;
            String::from_utf8_lossy(b).into_owned()
        };
        uri.push('/');
        uri.push_str(&coll);
    }

    if flags & F_HAS_RKEY != 0 {
        let rkey = if flags & F_RKEY_TID != 0 {
            let b = data.get(p..p + 8)?;
            p += 8;
            unpack_tid(b)?
        } else {
            let (b, np) = read_lit(data, p)?;
            p = np;
            String::from_utf8_lossy(b).into_owned()
        };
        uri.push('/');
        uri.push_str(&rkey);
    }

    Some((uri, p))
}

/// Marker for a compact favorites reference, written by rocksky-desktop.
/// Distinct from [`MARK`], and like it a byte plain UTF-8 text cannot start
/// with. Favorites are a query owned by a person, not a record, so there is no
/// AT-URI to pack — the card names the person instead.
const FAV_MARK: u8 = 0xA6;
const FAV_PLC: u8 = 0b0000_0001;

pub const FAVORITES_PREFIX: &str = "rocksky://favorites/";

pub fn is_favorites(data: &[u8]) -> bool {
    data.first() == Some(&FAV_MARK)
}

/// Encode `rocksky://favorites/<did>` compactly. `did` is the raw identifier.
pub fn encode_favorites(did: &str) -> Result<Vec<u8>, String> {
    let mut flags = 0u8;
    let mut body = Vec::new();
    if let Some(raw) = plc_bytes(did) {
        flags |= FAV_PLC;
        body.extend_from_slice(&raw);
    } else {
        push_lit(&mut body, did.as_bytes()).ok_or_else(|| "the DID is too long".to_string())?;
    }

    let mut out = vec![FAV_MARK, flags];
    out.extend_from_slice(&body);
    match decode_favorites(&out) {
        Some((back, _)) if back == format!("{FAVORITES_PREFIX}{did}") => Ok(out),
        _ => Err("internal error: favorites did not round-trip".to_string()),
    }
}

/// Decode a compact favorites reference and how many bytes it occupied.
pub fn decode_favorites(data: &[u8]) -> Option<(String, usize)> {
    if data.first() != Some(&FAV_MARK) {
        return None;
    }
    let flags = *data.get(1)?;
    let mut p = 2usize;
    let did = if flags & FAV_PLC != 0 {
        let raw = data.get(p..p + PLC_LEN)?;
        p += PLC_LEN;
        format!("did:plc:{}", b32_encode(raw))
    } else {
        let (b, np) = read_lit(data, p)?;
        p = np;
        String::from_utf8_lossy(b).into_owned()
    };
    Some((format!("{FAVORITES_PREFIX}{did}"), p))
}

/// Build what goes on the card: the first payload compactly encoded when it is
/// an `at://` URI or a favorites reference, then the remaining payloads as
/// newline-separated text. This is the layout rocksky-desktop writes.
pub fn encode_payloads(payloads: &[&str], dict: &Dict) -> Result<Vec<u8>, String> {
    let Some((first, rest)) = payloads.split_first() else {
        return Err("nothing to write".into());
    };

    let mut out = if looks_like_aturi(first) {
        encode(first, dict)?
    } else if let Some(did) = first.strip_prefix(FAVORITES_PREFIX) {
        encode_favorites(did)?
    } else {
        first.as_bytes().to_vec()
    };
    for extra in rest {
        out.push(b'\n');
        out.extend_from_slice(extra.as_bytes());
    }
    Ok(out)
}

/// Recover the payload list written by [`encode_payloads`] (by this tool or by
/// rocksky-desktop).
///
/// Erased memory reads back as 0xFF (SLE) or 0x00 (ACOS), and a card is rarely
/// written from byte zero — the SLE default offset skips a protected header —
/// so the fill has to be skipped at *both* ends, not just the tail.
///
/// The compact blob is located before any tail trimming, because it knows its
/// own length and its last byte may legitimately be 0x00: a packed TID ending
/// in a zero would otherwise be trimmed away and the whole URI lost.
pub fn decode_payloads(data: &[u8], dict: &Dict) -> Vec<String> {
    let fill = |b: u8| b == 0xFF || b == 0x00;
    let start = data.iter().position(|&b| !fill(b)).unwrap_or(data.len());
    let data = &data[start..];
    if data.is_empty() {
        return Vec::new();
    }

    let mut out = Vec::new();
    let rest = if is_favorites(data) {
        match decode_favorites(data) {
            Some((uri, used)) => {
                out.push(uri);
                &data[used..]
            }
            None => return Vec::new(),
        }
    } else {
        match decode(data, dict) {
            Some((uri, used)) => {
                out.push(uri);
                &data[used..]
            }
            // Ours, but not reconstructable — better nothing than guesswork.
            None if is_encoded(data) => return Vec::new(),
            None => data,
        }
    };

    // Only now is trimming the tail safe: whatever the blob claimed is already
    // consumed, so nothing here belongs to it.
    let end = rest.iter().rposition(|&b| !fill(b)).map_or(0, |i| i + 1);
    let rest = &rest[..end];

    for line in String::from_utf8_lossy(rest).split('\n') {
        let line = line.trim_matches(|c: char| c == '\0' || c.is_whitespace());
        if !line.is_empty() {
            out.push(line.to_string());
        }
    }
    out
}

// --- helpers ---------------------------------------------------------------

/// Index of `value` in a dictionary, if present and addressable by one byte.
fn dict_index(list: &[String], value: &str) -> Option<u8> {
    list.iter()
        .position(|v| v == value)
        .filter(|&i| i <= u8::MAX as usize)
        .map(|i| i as u8)
}

/// The 15 raw bytes of a `did:plc:` authority, if it is a canonical one.
fn plc_bytes(authority: &str) -> Option<[u8; PLC_LEN]> {
    let id = authority.strip_prefix("did:plc:")?;
    let raw = b32_decode(id)?;
    if raw.len() == PLC_LEN && b32_encode(&raw) == id {
        raw.try_into().ok()
    } else {
        None
    }
}

fn push_lit(body: &mut Vec<u8>, bytes: &[u8]) -> Option<()> {
    if bytes.len() > 255 {
        return None;
    }
    body.push(bytes.len() as u8);
    body.extend_from_slice(bytes);
    Some(())
}

fn read_lit(data: &[u8], p: usize) -> Option<(&[u8], usize)> {
    let len = *data.get(p)? as usize;
    let b = data.get(p + 1..p + 1 + len)?;
    Some((b, p + 1 + len))
}

fn b32_decode(s: &str) -> Option<Vec<u8>> {
    let mut acc: u64 = 0;
    let mut nbits: u32 = 0;
    let mut out = Vec::new();
    for c in s.bytes() {
        acc = (acc << 5) | B32.iter().position(|&x| x == c)? as u64;
        nbits += 5;
        if nbits >= 8 {
            nbits -= 8;
            out.push((acc >> nbits) as u8);
            acc &= (1u64 << nbits) - 1;
        }
    }
    Some(out)
}

fn b32_encode(raw: &[u8]) -> String {
    let mut acc: u64 = 0;
    let mut nbits: u32 = 0;
    let mut s = String::new();
    for &b in raw {
        acc = (acc << 8) | b as u64;
        nbits += 8;
        while nbits >= 5 {
            nbits -= 5;
            s.push(B32[((acc >> nbits) & 0x1F) as usize] as char);
            acc &= (1u64 << nbits) - 1;
        }
    }
    if nbits > 0 {
        s.push(B32[((acc << (5 - nbits)) & 0x1F) as usize] as char);
    }
    s
}

/// Pack a 13-char TID (base32-sortable) into 8 bytes, or `None` if it isn't one.
fn pack_tid(s: &str) -> Option<[u8; 8]> {
    let b = s.as_bytes();
    if b.len() != 13 {
        return None;
    }
    let mut v: u128 = 0;
    for &c in b {
        v = v * 32 + TID_B32.iter().position(|&x| x == c)? as u128;
    }
    if v >> 64 != 0 {
        return None; // outside the 64-bit TID range
    }
    Some((v as u64).to_be_bytes())
}

fn unpack_tid(b: &[u8]) -> Option<String> {
    let arr: [u8; 8] = b.try_into().ok()?;
    let mut v = u64::from_be_bytes(arr) as u128;
    let mut out = [0u8; 13];
    for i in (0..13).rev() {
        out[i] = TID_B32[(v % 32) as usize];
        v /= 32;
    }
    String::from_utf8(out.to_vec()).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dict() -> Dict {
        Dict {
            collections: [
                "app.rocksky.playlist",
                "app.rocksky.album",
                "app.rocksky.artist",
            ]
            .iter()
            .map(|s| s.to_string())
            .collect(),
            authorities: Vec::new(),
        }
    }

    fn roundtrip_with(uri: &str, d: &Dict) -> usize {
        let enc = encode(uri, d).expect("encode");
        assert!(is_encoded(&enc));
        assert_eq!(
            decode(&enc, d).map(|(u, _)| u).as_deref(),
            Some(uri),
            "roundtrip {uri}"
        );
        enc.len()
    }

    fn roundtrip(uri: &str) -> usize {
        roundtrip_with(uri, &dict())
    }

    #[test]
    fn full_rocksky_uri_fits_ff04() {
        let uri = "at://did:plc:7vdlgi2bflelz7mmuxoqjfcr/app.rocksky.playlist/3mttndjwxh223";
        assert!(roundtrip(uri) <= 28);
    }

    #[test]
    fn all_known_collections() {
        for c in [
            "app.rocksky.playlist",
            "app.rocksky.album",
            "app.rocksky.artist",
        ] {
            roundtrip(&format!(
                "at://did:plc:7vdlgi2bflelz7mmuxoqjfcr/{c}/3mttndjwxh223"
            ));
        }
    }

    #[test]
    fn did_web_authority() {
        roundtrip("at://did:web:example.com/app.rocksky.album/3mttndjwxh223");
        roundtrip("at://did:web:rocksky.app");
    }

    #[test]
    fn custom_config_extends_dicts() {
        // A user-supplied collection and a dictionary-encoded authority.
        let mut d = dict();
        d.collections.push("com.example.thing".to_string());
        d.authorities.push("did:web:rocksky.app".to_string());
        // authority via dict (1 byte) + custom collection + TID
        let uri = "at://did:web:rocksky.app/com.example.thing/3mttndjwxh223";
        assert!(roundtrip_with(uri, &d) <= 12);
    }

    #[test]
    fn variants() {
        roundtrip("at://did:plc:7vdlgi2bflelz7mmuxoqjfcr");
        roundtrip("at://did:plc:7vdlgi2bflelz7mmuxoqjfcr/app.rocksky.playlist");
        // known collection with a non-TID record key -> literal rkey, still ok
        roundtrip("at://did:plc:7vdlgi2bflelz7mmuxoqjfcr/app.rocksky.album/custom-key");
    }

    #[test]
    fn unknown_collection_errors() {
        let e = encode(
            "at://did:plc:7vdlgi2bflelz7mmuxoqjfcr/app.bsky.feed.post/3kabc",
            &dict(),
        )
        .unwrap_err();
        assert!(e.contains("unknown collection"), "{e}");
    }

    #[test]
    fn not_an_aturi() {
        assert!(!looks_like_aturi("hello world"));
        assert!(encode("hello world", &dict()).is_err());
    }

    const ALBUM: &str = "at://did:plc:7vdlgi2bflelz7mmuxoqjfcr/app.rocksky.album/3lhlttyitus2k";
    const ID: &str = "rocksky://library/album/rec_cuhigpho74fi003acf9g";

    /// Pinned against the bytes rocksky-desktop's encoder produces. This is the
    /// contract that lets a card written by either tool read in the other; the
    /// collection index is one byte in the middle of an otherwise opaque blob,
    /// so a drift would not fail loudly.
    #[test]
    fn matches_rocksky_desktop_byte_for_byte() {
        let hex = |b: &[u8]| b.iter().map(|x| format!("{x:02X}")).collect::<String>();
        assert_eq!(
            hex(&encode(ALBUM, &dict()).unwrap()),
            "A51FFD46B323412AC8BCFD8CA5DD0494510118B639CF9D9D6010"
        );
        assert_eq!(
            hex(&encode(
                "at://did:plc:7vdlgi2bflelz7mmuxoqjfcr/app.rocksky.playlist/3mttndjwxh223",
                &dict()
            )
            .unwrap()),
            "A51FFD46B323412AC8BCFD8CA5DD049451001967334BF9D68001"
        );
        assert_eq!(
            hex(&encode_favorites("did:plc:7vdlgi2bflelz7mmuxoqjfcr").unwrap()),
            "A601FD46B323412AC8BCFD8CA5DD049451"
        );
    }

    #[test]
    fn roundtrips_a_uri_with_its_fallback() {
        let bytes = encode_payloads(&[ALBUM, ID], &dict()).unwrap();
        assert_eq!(
            decode_payloads(&bytes, &dict()),
            vec![ALBUM.to_string(), ID.to_string()]
        );
    }

    /// A card written by rocksky-desktop reads back exactly this: erased fill
    /// before the data (it writes at its own offset), the compact blob, a
    /// newline, the library-id fallback as text, then erased fill to the end.
    #[test]
    fn reads_a_rocksky_desktop_card() {
        for fill in [0xFFu8, 0x00] {
            let mut bytes = vec![fill; 32];
            bytes.extend_from_slice(&encode_payloads(&[ALBUM, ID], &dict()).unwrap());
            bytes.resize(512, fill);
            assert_eq!(
                decode_payloads(&bytes, &dict()),
                vec![ALBUM.to_string(), ID.to_string()],
                "fill {fill:02X}"
            );
        }
        assert!(decode_payloads(&[0xFF; 64], &dict()).is_empty(), "blank");
        assert!(decode_payloads(&[0x00; 64], &dict()).is_empty(), "blank");
    }

    /// The blob's own bytes may end in 0x00 — a packed TID can. Trimming the
    /// tail before decoding would eat it and lose the whole URI, so the blob is
    /// located first and only what follows it is trimmed.
    #[test]
    fn does_not_trim_a_blob_ending_in_zero() {
        // This rkey packs to …9D6000 — a TID whose last byte really is zero.
        let uri = "at://did:plc:7vdlgi2bflelz7mmuxoqjfcr/app.rocksky.album/3lhlttyitus22";
        let blob = encode_payloads(&[uri], &dict()).unwrap();
        assert_eq!(blob.last(), Some(&0x00), "this vector must end in 0x00");

        let mut padded = blob.clone();
        padded.resize(64, 0x00);
        assert_eq!(decode_payloads(&padded, &dict()), vec![uri.to_string()]);
    }

    /// A favorites card round-trips through the payload helpers.
    #[test]
    fn roundtrips_a_favorites_card() {
        let plain = "rocksky://favorites/did:plc:7vdlgi2bflelz7mmuxoqjfcr";
        let bytes = encode_payloads(&[plain], &dict()).unwrap();
        assert_eq!(bytes.len(), 17, "marker + flags + 15 packed plc bytes");
        assert_eq!(decode_payloads(&bytes, &dict()), vec![plain.to_string()]);
    }

    /// The two markers must not be confused for one another.
    #[test]
    fn favorites_and_record_uris_stay_distinct() {
        let fav = encode_favorites("did:plc:7vdlgi2bflelz7mmuxoqjfcr").unwrap();
        let uri = encode(ALBUM, &dict()).unwrap();
        assert!(is_favorites(&fav) && !is_encoded(&fav));
        assert!(is_encoded(&uri) && !is_favorites(&uri));
        assert!(
            decode(&fav, &dict()).is_none(),
            "a favorites blob is not an AT-URI"
        );
        assert!(decode_favorites(&uri).is_none(), "and vice versa");
    }

    /// A card holding plain text still reads, it just isn't compact.
    #[test]
    fn reads_a_plain_text_card() {
        let mut bytes = format!("{ALBUM}\n{ID}").into_bytes();
        bytes.resize(256, 0xFF);
        assert_eq!(
            decode_payloads(&bytes, &dict()),
            vec![ALBUM.to_string(), ID.to_string()]
        );
    }
}
