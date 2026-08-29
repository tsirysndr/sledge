//! Contactless NFC tag access via the ACS/CCID pseudo-APDUs.
//!
//! A PC/SC reader presents a contactless storage card as an ordinary card with
//! a synthesised ATR (PC/SC part 3, §3.1.3.2.3.1) and exposes its memory
//! through `FF B0` (read binary) and `FF D6` (update binary). Two tag families
//! carry the same NDEF message over different addressing, and the ATR says
//! which is on the reader:
//!
//! - **NFC Forum Type 2** (NTAG213/215/216, MIFARE Ultralight): 4-byte pages,
//!   read and written directly, no authentication.
//! - **MIFARE Classic**: 16-byte blocks in 4-block sectors, each sector behind
//!   a key exchange, with NDEF mapped on per NXP AN1305. Sold as "NFC tags" as
//!   often as Type 2 ones, and the reason a naive write fails with SW 6300.
//!
//! Everything runs inside a PC/SC transaction (see [`crate::card::Txn`]) so the
//! card is not reset between the authenticate and the block it authenticated.

use crate::card::{Txn, expect_ok};
use crate::ndef;
use std::error::Error;

/// Type 2 user memory starts at page 4; pages 0-3 are UID, lock bytes and the
/// capability container.
const FIRST_DATA_PAGE: u8 = 4;
const PAGE_LEN: usize = 4;

/// Data bytes per Classic sector: three 16-byte blocks, the fourth being the
/// sector trailer.
const CLASSIC_BLOCK_LEN: usize = 16;
const CLASSIC_SECTOR_BYTES: usize = 3 * CLASSIC_BLOCK_LEN;

/// The NFC Forum's well-known key A for NDEF sectors on Classic (AN1305 §3.3).
/// An NDEF-formatted tag carries this; a factory-blank one does not.
const NDEF_KEY: [u8; 6] = [0xD3, 0xF7, 0xD3, 0xF7, 0xD3, 0xF7];
/// The factory transport key. Authenticating with it means the tag has never
/// been NDEF-formatted.
const FACTORY_KEY: [u8; 6] = [0xFF; 6];
/// Key A of the MAD sector, fixed by the MAD spec so any reader can read it.
const MAD_KEY: [u8; 6] = [0xA0, 0xA1, 0xA2, 0xA3, 0xA4, 0xA5];

/// What kind of contactless tag is on the reader.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum NfcTag {
    /// NTAG21x / MIFARE Ultralight — page-addressed, no authentication.
    Type2,
    /// MIFARE Classic: 16-byte blocks behind a per-sector key exchange. The
    /// number is how many 4-block sectors it has (16 on a 1K, 40 on a 4K).
    Classic { name: &'static str, sectors: u8 },
}

impl NfcTag {
    pub fn label(self) -> String {
        match self {
            NfcTag::Type2 => "NFC Forum Type 2 tag (NTAG21x / MIFARE Ultralight)".into(),
            NfcTag::Classic { name, sectors } => format!("{name}, {sectors} usable sectors"),
        }
    }

    /// How many bytes of NDEF the tag can hold, as far as the ATR alone says.
    /// Type 2 has to be probed on the card ([`capacity`]), so it answers `None`.
    pub fn declared_capacity(self) -> Option<usize> {
        match self {
            NfcTag::Type2 => None,
            NfcTag::Classic { sectors, .. } => Some((sectors as usize - 1) * CLASSIC_SECTOR_BYTES),
        }
    }
}

/// Identify a contactless storage card from its (reader-synthesised) ATR.
///
/// The card-name bytes are located by the RID that precedes them rather than by
/// a fixed offset — the historical-byte prefix varies between readers. A
/// storage-card ATR whose name is not recognised is treated as Type 2, which is
/// what an unbranded NTAG clone almost always is.
pub fn from_atr(atr: &[u8]) -> Option<NfcTag> {
    const RID: [u8; 5] = [0xA0, 0x00, 0x00, 0x03, 0x06];
    let at = atr.windows(RID.len()).position(|w| w == RID)?;
    // RID, then one byte of standard (SS), then the two-byte card name.
    let name = atr.get(at + RID.len() + 1..at + RID.len() + 3)?;
    Some(match (name[0], name[1]) {
        (0x00, 0x03) => NfcTag::Type2,
        (0x00, 0x01) => NfcTag::Classic {
            name: "MIFARE Classic 1K",
            sectors: 16,
        },
        // Only the first 32 sectors of a 4K are the 4-block kind this addresses;
        // the 8 above them are 16-block sectors, and 31 usable sectors is far
        // more room than an NDEF message of ours ever needs.
        (0x00, 0x02) => NfcTag::Classic {
            name: "MIFARE Classic 4K",
            sectors: 32,
        },
        (0x00, 0x26) => NfcTag::Classic {
            name: "MIFARE Mini",
            sectors: 5,
        },
        (0x00, 0x36) | (0x00, 0x37) => NfcTag::Classic {
            name: "MIFARE Plus",
            sectors: 32,
        },
        _ => NfcTag::Type2,
    })
}

// ── Tag I/O ─────────────────────────────────────────────────────────────────

/// Send an APDU and return its data, rejecting anything but `90 00`.
fn send(tx: &Txn, apdu: &[u8]) -> Result<Vec<u8>, Box<dyn Error>> {
    let r = tx.transmit(apdu)?;
    Ok(expect_ok(&r)?.to_vec())
}

/// The tag's UID, as uppercase hex.
pub fn uid(tx: &Txn) -> Result<String, Box<dyn Error>> {
    Ok(hex::encode_upper(send(
        tx,
        &[0xFF, 0xCA, 0x00, 0x00, 0x00],
    )?))
}

/// Read `count` Type 2 pages starting at `page`. Readers cap a single read at
/// 16 bytes, so this chunks by four pages.
fn read_pages(tx: &Txn, page: u8, count: u8) -> Result<Vec<u8>, Box<dyn Error>> {
    let mut out = Vec::with_capacity(count as usize * PAGE_LEN);
    let mut at = page;
    let mut left = count;
    while left > 0 {
        let chunk = left.min(4);
        out.extend_from_slice(&send(tx, &[0xFF, 0xB0, 0x00, at, chunk * PAGE_LEN as u8])?);
        left -= chunk;
        // A tag whose CC claims more memory than a page number can address.
        match at.checked_add(chunk) {
            Some(next) => at = next,
            None => break,
        }
    }
    Ok(out)
}

fn write_page(tx: &Txn, page: u8, data: &[u8]) -> Result<(), Box<dyn Error>> {
    let mut apdu = vec![0xFF, 0xD6, 0x00, page, PAGE_LEN as u8];
    apdu.extend_from_slice(data);
    send(tx, &apdu)
        .map(|_| ())
        .map_err(|e| format!("page {page}: {e}").into())
}

/// Whether the tag has been locked read-only.
///
/// Byte 3 of the capability container holds the NDEF access condition: 0x00 is
/// read/write, anything else restricts writing. A tag locked this way (or with
/// its static lock bits burned) answers every write with the same SW 6300 as a
/// Classic tag, and the lock is irreversible — so it is worth saying plainly
/// rather than letting the user retry a tag that will never take a write.
pub fn is_read_only(tx: &Txn) -> bool {
    matches!(read_pages(tx, 3, 1), Ok(cc) if cc.len() >= 4 && cc[0] == 0xE1 && cc[3] != 0x00)
}

/// Usable Type 2 user memory in bytes.
///
/// The capability container (page 3, byte 2, in units of 8) is the tag's own
/// claim, and a blank tag has no CC at all. Neither is trustworthy enough to
/// size a write against — a wrong answer here is exactly the half-written tag
/// to avoid — so the claim is confirmed by reading up to the page it implies.
/// Real memory is what actually reads back.
pub fn capacity(tx: &Txn) -> usize {
    let claimed = match read_pages(tx, 3, 1) {
        Ok(cc) if cc.len() >= 3 && cc[0] == 0xE1 && cc[2] > 0 => cc[2] as usize * 8,
        // No CC (a factory-blank or non-NDEF tag): assume the smallest Type 2
        // layout and let the probe below find the rest.
        _ => 48,
    };
    readable_from(tx, FIRST_DATA_PAGE, claimed.max(48))
}

/// Bytes that actually read back from `page`, probing up to `limit`. Stops at
/// the first read that fails, which is where the tag's memory ends.
fn readable_from(tx: &Txn, page: u8, limit: usize) -> usize {
    let mut ok = 0;
    let mut at = page;
    while ok < limit {
        // Four pages at a time, the most a single READ BINARY returns.
        let want = ((limit - ok) / PAGE_LEN).clamp(1, 4) as u8;
        match read_pages(tx, at, want) {
            Ok(bytes) if !bytes.is_empty() => ok += bytes.len(),
            _ => break,
        }
        match at.checked_add(want) {
            Some(next) => at = next,
            None => break,
        }
    }
    ok
}

// ── MIFARE Classic ──────────────────────────────────────────────────────────
//
// Classic is not an NFC Forum tag type, but NXP AN1305 maps NDEF onto it and
// that mapping is what phones read and write — so a Classic tag someone bought
// as an "NFC tag" holds an ordinary NDEF message, just addressed differently.
//
// Layout: 16-byte blocks grouped into 4-block sectors. The last block of each
// sector is its trailer (two keys and the access bits), and sector 0 block 0 is
// factory data, so the usable space is 3 blocks per sector from sector 1 up.
// Every sector must be authenticated before any of its blocks can be touched.

/// A sector trailer: key A, three access-condition bytes, the general-purpose
/// byte, then key B.
///
/// Key B is left at the factory value on every sector, and the access bits
/// (`7F 07 88` for NDEF, `78 77 88` for the MAD) keep the trailer writable with
/// key B. Formatting is therefore reversible — a wrong trailer can be rewritten
/// rather than locking the sector for good.
fn trailer(key_a: &[u8; 6], access: [u8; 4]) -> Vec<u8> {
    let mut t = key_a.to_vec();
    t.extend_from_slice(&access);
    t.extend_from_slice(&FACTORY_KEY);
    t
}

/// CRC-8 over the MAD's bytes: polynomial 0x1D, preset 0xC7.
fn crc8_mad(data: &[u8]) -> u8 {
    let mut crc: u8 = 0xC7;
    for &b in data {
        crc ^= b;
        for _ in 0..8 {
            crc = if crc & 0x80 != 0 {
                (crc << 1) ^ 0x1D
            } else {
                crc << 1
            };
        }
    }
    crc
}

/// MAD1 blocks 1 and 2 — the directory that tells a reader which sectors hold
/// NDEF. Every sector from 1 up is claimed by the NDEF AID (0x03E1, stored
/// little-endian). Byte 0 is the CRC over everything that follows it; byte 1 is
/// the card-publisher sector, 0 for none.
fn mad_blocks(sectors: u8) -> (Vec<u8>, Vec<u8>) {
    let mut b1 = vec![0x00, 0x00];
    let mut b2 = Vec::new();
    for s in 1..sectors.min(16) {
        let aid = [0xE1, 0x03];
        if s <= 7 {
            b1.extend_from_slice(&aid);
        } else {
            b2.extend_from_slice(&aid);
        }
    }
    let mut crc_input = b1[1..].to_vec();
    crc_input.extend_from_slice(&b2);
    b1[0] = crc8_mad(&crc_input);
    (b1, b2)
}

/// Load a key into the reader's volatile key slot 0 for the next authenticate.
fn load_key(tx: &Txn, key: &[u8; 6]) -> Result<(), Box<dyn Error>> {
    let mut apdu = vec![0xFF, 0x82, 0x00, 0x00, 0x06];
    apdu.extend_from_slice(key);
    send(tx, &apdu).map(|_| ())
}

/// Authenticate `block`'s sector with the key already in slot 0, as key A.
fn authenticate(tx: &Txn, block: u8) -> Result<(), Box<dyn Error>> {
    send(
        tx,
        &[0xFF, 0x86, 0x00, 0x00, 0x05, 0x01, 0x00, block, 0x60, 0x00],
    )
    .map(|_| ())
}

/// Authenticate a sector, returning false when the key is simply wrong — the
/// caller uses that to tell an NDEF-formatted tag from a blank one, so it must
/// not be an error.
fn try_auth(tx: &Txn, key: &[u8; 6], sector: u8) -> bool {
    load_key(tx, key).is_ok() && authenticate(tx, sector * 4).is_ok()
}

fn classic_read_block(tx: &Txn, block: u8) -> Result<Vec<u8>, Box<dyn Error>> {
    send(tx, &[0xFF, 0xB0, 0x00, block, CLASSIC_BLOCK_LEN as u8])
}

fn classic_write_block(tx: &Txn, block: u8, data: &[u8]) -> Result<(), Box<dyn Error>> {
    let mut apdu = vec![0xFF, 0xD6, 0x00, block, CLASSIC_BLOCK_LEN as u8];
    apdu.extend_from_slice(data);
    send(tx, &apdu)
        .map(|_| ())
        .map_err(|e| format!("block {block}: {e}").into())
}

/// The data blocks of sectors 1..`sectors`, in order — where the NDEF message
/// lives. Sector 0 is skipped: it holds the MIFARE Application Directory, which
/// says which sectors are NDEF, and rewriting it is a formatting operation.
fn classic_data_blocks(sectors: u8) -> Vec<u8> {
    (1..sectors)
        .flat_map(|s| (0..3).map(move |b| s * 4 + b))
        .collect()
}

/// Whether a Classic tag is NDEF-formatted, factory-blank, or neither.
pub enum ClassicState {
    /// Carries the NFC Forum key: an NDEF message can be read and written.
    Ndef,
    /// Still on the factory transport key: never formatted, but formattable.
    Blank,
    /// Locked with keys we do not have.
    Foreign,
}

pub fn classic_state(tx: &Txn) -> ClassicState {
    if try_auth(tx, &NDEF_KEY, 1) {
        ClassicState::Ndef
    } else if try_auth(tx, &FACTORY_KEY, 1) {
        ClassicState::Blank
    } else {
        ClassicState::Foreign
    }
}

/// Turn a factory-blank Classic tag into an NDEF one.
///
/// Only ever called on a tag that still answers to the factory key, so there is
/// nothing of anyone's to destroy. The NDEF sectors are done first and the MAD
/// last: a format interrupted halfway leaves a tag whose directory still says
/// "not NDEF", which is what it was to begin with.
fn classic_format(tx: &Txn, sectors: u8) -> Result<(), Box<dyn Error>> {
    for sector in 1..sectors {
        if !try_auth(tx, &FACTORY_KEY, sector) {
            return Err(format!("sector {sector} wouldn't authenticate to format").into());
        }
        for block in 0..3u8 {
            classic_write_block(tx, sector * 4 + block, &[0u8; CLASSIC_BLOCK_LEN])?;
        }
        classic_write_block(
            tx,
            sector * 4 + 3,
            &trailer(&NDEF_KEY, [0x7F, 0x07, 0x88, 0x40]),
        )?;
    }

    if !try_auth(tx, &FACTORY_KEY, 0) {
        return Err("the MAD sector wouldn't authenticate to format".into());
    }
    let (b1, b2) = mad_blocks(sectors);
    classic_write_block(tx, 1, &b1)?;
    classic_write_block(tx, 2, &b2)?;
    // GPB 0xC1: MAD present, version 1, multi-application.
    classic_write_block(tx, 3, &trailer(&MAD_KEY, [0x78, 0x77, 0x88, 0xC1]))?;
    Ok(())
}

/// The raw NDEF-area bytes of a Classic tag, sector by sector.
///
/// Stops at the first sector that won't authenticate, which is the end of the
/// NDEF area — sectors beyond it belong to other applications.
fn classic_read_data(tx: &Txn, sectors: u8) -> Vec<u8> {
    let mut data = Vec::new();
    for sector in 1..sectors {
        if !try_auth(tx, &NDEF_KEY, sector) {
            break;
        }
        for block in 0..3u8 {
            match classic_read_block(tx, sector * 4 + block) {
                Ok(bytes) => data.extend_from_slice(&bytes),
                Err(_) => break,
            }
        }
    }
    data
}

/// Write an NDEF TLV across a Classic tag's NDEF sectors, formatting a blank
/// tag first when `format_blank` is set.
///
/// Only data blocks are touched on an already-formatted tag. Sector trailers
/// hold the keys and access bits, and a wrong value there locks the sector
/// permanently.
fn classic_write_ndef(
    tx: &Txn,
    bytes: &[u8],
    sectors: u8,
    format_blank: bool,
) -> Result<(), Box<dyn Error>> {
    match classic_state(tx) {
        ClassicState::Ndef => {}
        ClassicState::Blank if format_blank => {
            println!("Blank MIFARE Classic tag — formatting it for NDEF first...");
            classic_format(tx, sectors)?;
        }
        ClassicState::Blank => {
            return Err("this MIFARE Classic tag has never been NDEF-formatted; \
                        re-run with --format to format it (it is blank, so nothing is lost)"
                .into());
        }
        ClassicState::Foreign => {
            return Err(
                "this MIFARE Classic tag is locked with keys we don't have, so it can't be written"
                    .into(),
            );
        }
    }

    // A block write is all 16 bytes or nothing, so the TLV is padded out to a
    // whole block.
    let mut padded = bytes.to_vec();
    ndef::pad_to(&mut padded, CLASSIC_BLOCK_LEN);

    let blocks = classic_data_blocks(sectors);
    let chunks: Vec<&[u8]> = padded.chunks(CLASSIC_BLOCK_LEN).collect();
    if chunks.len() > blocks.len() {
        return Err(format!(
            "this tag holds {} bytes of NDEF; the message needs {}.",
            blocks.len() * CLASSIC_BLOCK_LEN,
            padded.len()
        )
        .into());
    }

    // Same ordering rule as Type 2: the block carrying the TLV header goes last,
    // so an interrupted write leaves a tag that reads as blank, not as half a
    // record. Authentication is per sector and is re-asserted on each crossing.
    let mut plan: Vec<(u8, &[u8])> = blocks.into_iter().zip(chunks).collect();
    if plan.is_empty() {
        return Err("nothing to write".into());
    }
    let head = plan.remove(0);
    plan.push(head);

    let mut authed = None;
    for (block, chunk) in plan {
        let sector = block / 4;
        if authed != Some(sector) {
            if !try_auth(tx, &NDEF_KEY, sector) {
                return Err(format!("sector {sector} wouldn't authenticate").into());
            }
            authed = Some(sector);
        }
        classic_write_block(tx, block, chunk)?;
    }
    Ok(())
}

// ── NDEF over either family ─────────────────────────────────────────────────

/// The tag's whole user memory, as it would be dumped.
pub fn read_memory(tx: &Txn, tag: NfcTag) -> Result<Vec<u8>, Box<dyn Error>> {
    if let NfcTag::Classic { sectors, .. } = tag {
        return Ok(classic_read_data(tx, sectors));
    }

    // Read the whole user memory in one sweep: TLVs before the NDEF one (lock
    // and memory control) push the message to an offset we can't predict.
    let pages = (capacity(tx) / PAGE_LEN).min(256) as u16;
    let mut data = Vec::new();
    let mut page = FIRST_DATA_PAGE;
    let mut left = pages;
    while left > 0 {
        let chunk = left.min(4) as u8;
        match read_pages(tx, page, chunk) {
            Ok(bytes) => data.extend_from_slice(&bytes),
            // Reading past the end of a smaller tag than the CC advertised.
            Err(_) => break,
        }
        page = match page.checked_add(chunk) {
            Some(p) => p,
            None => break,
        };
        left -= chunk as u16;
    }
    Ok(data)
}

/// Every URI record on the tag, in tag order.
pub fn read_uris(tx: &Txn, tag: NfcTag) -> Result<Vec<String>, Box<dyn Error>> {
    let data = read_memory(tx, tag)?;
    Ok(ndef::find_tlv(&data)
        .map(ndef::uri_records)
        .unwrap_or_default())
}

/// Write a TLV across a Type 2 tag's pages.
fn type2_write_ndef(tx: &Txn, bytes: &[u8]) -> Result<(), Box<dyn Error>> {
    if is_read_only(tx) {
        return Err("this tag is locked read-only and can't be rewritten".into());
    }

    let room = capacity(tx);
    if bytes.len() > room {
        return Err(format!(
            "this tag holds {room} bytes; the message needs {}. \
             Use an NTAG215 or larger.",
            bytes.len()
        )
        .into());
    }

    let pages: Vec<(u8, &[u8])> = bytes
        .chunks(PAGE_LEN)
        .enumerate()
        .map(|(i, chunk)| {
            let page = u8::try_from(i)
                .ok()
                .and_then(|i| FIRST_DATA_PAGE.checked_add(i))
                .ok_or("the message is longer than this tag can address")?;
            Ok((page, chunk))
        })
        .collect::<Result<_, Box<dyn Error>>>()?;

    // The first page carries the TLV header, and without it the message is not
    // an NDEF message at all. Writing it last means a write that dies partway —
    // tag pulled off the reader, memory smaller than it claimed — leaves a tag
    // that reads as blank rather than as a corrupt half-record.
    let (first, rest) = pages.split_at(1);
    for (page, chunk) in rest {
        write_page(tx, *page, chunk)?;
    }
    for (page, chunk) in first {
        write_page(tx, *page, chunk)?;
    }
    Ok(())
}

/// Write `uris` as an NDEF message, replacing whatever the tag held.
pub fn write_uris(
    tx: &Txn,
    tag: NfcTag,
    uris: &[&str],
    format_blank: bool,
) -> Result<(), Box<dyn Error>> {
    match tag {
        NfcTag::Classic { sectors, .. } => {
            let bytes = ndef::encode_uris(uris, CLASSIC_BLOCK_LEN);
            classic_write_ndef(tx, &bytes, sectors, format_blank)
        }
        NfcTag::Type2 => type2_write_ndef(tx, &ndef::encode_uris(uris, PAGE_LEN)),
    }
}

/// Erase the tag: an empty NDEF message, and the user memory behind it zeroed.
///
/// The empty message matters — a tag wiped to all-zeroes has no NDEF mapping at
/// all and a phone reports it as unformatted, where an empty message reads as
/// the blank-but-writable tag the user asked for. The zero fill behind it is
/// what makes this an erase rather than a short write: nothing of the old
/// message survives past the new terminator to be recovered.
///
/// Returns the number of bytes cleared.
pub fn clear(tx: &Txn, tag: NfcTag) -> Result<usize, Box<dyn Error>> {
    match tag {
        NfcTag::Classic { sectors, .. } => {
            // A factory-blank tag has no NDEF mapping to erase, and formatting
            // one to then blank it would be a stranger thing to do than nothing.
            if matches!(classic_state(tx), ClassicState::Blank) {
                return Ok(0);
            }
            let room = (sectors as usize - 1) * CLASSIC_SECTOR_BYTES;
            let mut bytes = ndef::encode_uris(&[], CLASSIC_BLOCK_LEN);
            bytes.resize(room, 0x00);
            classic_write_ndef(tx, &bytes, sectors, false)?;
            Ok(room)
        }
        NfcTag::Type2 => {
            let room = capacity(tx);
            let mut bytes = ndef::encode_uris(&[], PAGE_LEN);
            bytes.resize(room.max(bytes.len()), 0x00);
            let cleared = bytes.len();
            type2_write_ndef(tx, &bytes)?;
            Ok(cleared)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ntag_atr_is_type_2() {
        let atr = hex::decode("3B8F8001804F0CA000000306030000000068").unwrap();
        assert!(matches!(from_atr(&atr), Some(NfcTag::Type2)));
    }

    #[test]
    fn classic_1k_atr_is_recognised() {
        let atr = hex::decode("3B8F8001804F0CA0000003060300010000006A").unwrap();
        match from_atr(&atr) {
            Some(NfcTag::Classic { name, sectors }) => {
                assert_eq!(name, "MIFARE Classic 1K");
                assert_eq!(sectors, 16);
            }
            _ => panic!("expected Classic 1K"),
        }
    }

    #[test]
    fn a_contact_card_atr_is_not_a_tag() {
        assert!(from_atr(&[0x3B, 0x04, 0x92, 0x23, 0x10, 0x91]).is_none());
    }

    #[test]
    fn classic_capacity_leaves_out_the_mad_sector() {
        let tag = NfcTag::Classic {
            name: "MIFARE Classic 1K",
            sectors: 16,
        };
        assert_eq!(tag.declared_capacity(), Some(15 * 48));
    }

    #[test]
    fn the_mad_claims_every_ndef_sector() {
        let (b1, b2) = mad_blocks(16);
        assert_eq!(b1.len(), 16);
        assert_eq!(b2.len(), 16);
        // Sector 1's AID sits right after the CRC and info bytes.
        assert_eq!(&b1[2..4], &[0xE1, 0x03]);
        let mut crc_input = b1[1..].to_vec();
        crc_input.extend_from_slice(&b2);
        assert_eq!(b1[0], crc8_mad(&crc_input));
    }

    #[test]
    fn data_blocks_skip_the_trailers() {
        let blocks = classic_data_blocks(3);
        assert_eq!(blocks, vec![4, 5, 6, 8, 9, 10]);
    }
}
