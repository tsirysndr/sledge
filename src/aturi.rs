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
    if decode(&out, dict).as_deref() != Some(uri) {
        return Err("internal error: URI did not round-trip".to_string());
    }
    Ok(out)
}

/// Decode a compact AT-URI back to its string form, or `None` if `data` isn't
/// one of ours.
pub fn decode(data: &[u8], dict: &Dict) -> Option<String> {
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

    let _ = p;
    Some(uri)
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
        assert_eq!(decode(&enc, d).as_deref(), Some(uri), "roundtrip {uri}");
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
}
