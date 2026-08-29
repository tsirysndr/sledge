//! ACOS3 file access.
//!
//! ACOS3 uses ACS's proprietary command set (class `0x80`) over T=0, and its
//! files are **record-based**, not transparent. The command flow is:
//!   SELECT FILE   80 A4 00 00 02 <FID>
//!   READ RECORD   80 B2 <rec#> 00 <Le>
//!   WRITE RECORD  80 D2 <rec#> 00 <Lc> <data...>
//!   SUBMIT CODE   80 20 <code-ref> 00 <len> <code...>  (ref 07 = Issuer Code)
//!
//! Reads are commonly free; writes usually require a submitted code, so a write
//! to a protected file answers `69 82` (security status not satisfied) until
//! the right code is presented. Callers run these inside a transaction
//! (`Connected::with_transaction`) so the card is not reset mid-sequence.

use crate::card::Txn;
use std::error::Error;

const CLA: u8 = 0x80;

fn sw(r: &[u8]) -> (u8, u8) {
    let n = r.len();
    if n >= 2 { (r[n - 2], r[n - 1]) } else { (0, 0) }
}

pub fn select_file(tx: &Txn, file_id: &[u8]) -> Result<(), Box<dyn Error>> {
    let mut apdu = vec![CLA, 0xA4, 0x00, 0x00, file_id.len() as u8];
    apdu.extend_from_slice(file_id);
    match sw(&tx.transmit(&apdu)?) {
        (0x90, 0x00) => Ok(()),
        (0x6A, 0x82) => Err("SELECT FILE: file not found (6A 82)".into()),
        (a, b) => Err(format!("SELECT FILE failed: SW {a:02X} {b:02X}").into()),
    }
}

/// Determine a file's record length by reading record 0 with growing `Le`:
/// ACOS answers `90 00` while `Le` <= record length and `67 00` once it is
/// larger, so the record length is the last `Le` that succeeded.
pub fn record_len(tx: &Txn) -> Result<usize, Box<dyn Error>> {
    let mut len = 0usize;
    for le in 1u8..=255 {
        let r = tx.transmit(&[CLA, 0xB2, 0x00, 0x00, le])?;
        match sw(&r) {
            (0x90, 0x00) => len = le as usize,
            (0x67, 0x00) => break, // Le past the record length
            (0x6A, 0x83) => break, // no record 0 (empty file)
            (0x69, 0x82) => return Err("record read needs authentication (69 82)".into()),
            (a, b) => return Err(format!("READ RECORD probe failed: SW {a:02X} {b:02X}").into()),
        }
    }
    if len == 0 {
        return Err("could not determine record length (file empty or protected)".into());
    }
    Ok(len)
}

/// Count how many records the selected file has, by reading from record 0 until
/// the card reports out-of-range (`6A 83`). Used to compute file capacity.
pub fn record_count(tx: &Txn, reclen: usize) -> Result<usize, Box<dyn Error>> {
    let mut n = 0usize;
    while n <= 0xFF {
        match sw(&tx.transmit(&[CLA, 0xB2, n as u8, 0x00, reclen as u8])?) {
            (0x90, 0x00) => n += 1,
            (0x6A, 0x83) => break, // past the last record
            (0x69, 0x82) => break, // protected: can't probe further
            (a, b) => return Err(format!("record-count probe: SW {a:02X} {b:02X}").into()),
        }
    }
    Ok(n)
}

/// Read records starting at `start`, each `reclen` bytes, until the card
/// reports no more records (`6A 83`) or `max_bytes` is reached.
pub fn read_records(
    tx: &Txn,
    start: usize,
    reclen: usize,
    max_bytes: Option<usize>,
) -> Result<Vec<u8>, Box<dyn Error>> {
    let mut out = Vec::new();
    let mut rec = start;
    while rec <= 0xFF {
        if let Some(max) = max_bytes {
            if out.len() >= max {
                break;
            }
        }
        let r = tx.transmit(&[CLA, 0xB2, rec as u8, 0x00, reclen as u8])?;
        match sw(&r) {
            (0x90, 0x00) => out.extend_from_slice(&r[..r.len() - 2]),
            (0x6A, 0x83) => break, // record not found -> end of file
            (a, b) => return Err(format!("READ RECORD {rec} failed: SW {a:02X} {b:02X}").into()),
        }
        rec += 1;
    }
    if let Some(max) = max_bytes {
        out.truncate(max);
    }
    Ok(out)
}

/// Write `data` as full `reclen`-byte records starting at record `start`.
/// The final record is padded with `0x00` to `reclen`.
pub fn write_records(
    tx: &Txn,
    start: usize,
    reclen: usize,
    data: &[u8],
) -> Result<(), Box<dyn Error>> {
    for (i, chunk) in data.chunks(reclen).enumerate() {
        let mut rec = chunk.to_vec();
        rec.resize(reclen, 0x00);
        let mut apdu = vec![CLA, 0xD2, (start + i) as u8, 0x00, reclen as u8];
        apdu.extend_from_slice(&rec);
        match sw(&tx.transmit(&apdu)?) {
            (0x90, 0x00) => {}
            (0x69, 0x82) => {
                return Err(format!(
                    "WRITE RECORD {}: security status not satisfied (69 82) — this file \
                     requires an authenticated code; pass --pin (and --code if needed)",
                    start + i
                )
                .into());
            }
            (0x6A, 0x83) => {
                return Err(
                    format!("WRITE RECORD {}: record out of range (6A 83)", start + i).into(),
                );
            }
            (a, b) => {
                return Err(
                    format!("WRITE RECORD {} failed: SW {a:02X} {b:02X}", start + i).into(),
                );
            }
        }
    }
    Ok(())
}

/// Overwrite records from `start` to the end of the file with `0x00`, stopping
/// when the card reports the record is out of range (`6A 83`). Returns the
/// number of records cleared. Used so a text write "owns" the whole file
/// instead of leaving stale records behind after the text.
pub fn clear_records_from(tx: &Txn, start: usize, reclen: usize) -> Result<usize, Box<dyn Error>> {
    let zeros = vec![0u8; reclen];
    let mut cleared = 0;
    let mut rec = start;
    while rec <= 0xFF {
        let mut apdu = vec![CLA, 0xD2, rec as u8, 0x00, reclen as u8];
        apdu.extend_from_slice(&zeros);
        match sw(&tx.transmit(&apdu)?) {
            (0x90, 0x00) => {
                cleared += 1;
                rec += 1;
            }
            (0x6A, 0x83) => break, // past the last record -> end of file
            (a, b) => return Err(format!("clear record {rec} failed: SW {a:02X} {b:02X}").into()),
        }
    }
    Ok(cleared)
}

/// Present a code (PIN / issuer code) to unlock protected operations.
/// `code_ref` is the code reference in `P1` (e.g. `0x07` = Issuer Code, per the
/// ACOS3 spec: `80 20 07 00 08 <IC>`); `P2` is `0x00`. `code` is the raw bytes.
pub fn submit_code(tx: &Txn, code_ref: u8, code: &[u8]) -> Result<(), Box<dyn Error>> {
    let mut apdu = vec![CLA, 0x20, code_ref, 0x00, code.len() as u8];
    apdu.extend_from_slice(code);
    match sw(&tx.transmit(&apdu)?) {
        (0x90, 0x00) => Ok(()),
        (0x63, c) => Err(format!("code rejected; {} attempt(s) remaining", c & 0x0F).into()),
        (0x69, 0x83) => Err("code is blocked (69 83)".into()),
        (a, b) => Err(format!("SUBMIT CODE failed: SW {a:02X} {b:02X}").into()),
    }
}
