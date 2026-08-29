//! SLE5528 memory-card access via the ACR39U pseudo-APDUs.

use crate::card::{Connected, SLE5528_SIZE, expect_ok};
use std::error::Error;

/// Tell the reader the inserted card is an SLE4418/4428/5518/5528 (type 0x05).
pub fn select_type(c: &Connected) -> Result<(), Box<dyn Error>> {
    let r = c.transmit(&[0xFF, 0xA4, 0x00, 0x00, 0x01, 0x05])?;
    expect_ok(&r)?;
    Ok(())
}

/// Read the presentation-error counter. Returns the raw counter byte.
/// Does not present a code and does not decrement the counter.
pub fn error_counter(c: &Connected) -> Result<Option<u8>, Box<dyn Error>> {
    let r = c.transmit(&[0xFF, 0xB1, 0x00, 0x00, 0x03])?;
    Ok(expect_ok(&r)?.first().copied())
}

pub fn read(c: &Connected, offset: usize, length: usize) -> Result<Vec<u8>, Box<dyn Error>> {
    if offset + length > SLE5528_SIZE {
        return Err(format!(
            "read {}..{} is out of range (card is {} bytes)",
            offset,
            offset + length,
            SLE5528_SIZE
        )
        .into());
    }

    let mut memory = Vec::with_capacity(length);
    let mut addr = offset;
    let end = offset + length;

    // READ_MEMORY_CARD: FF B0 <addr-hi> <addr-lo> <len>, 32 bytes at a time.
    while addr < end {
        let chunk = std::cmp::min(32, end - addr);
        let apdu = [
            0xFF,
            0xB0,
            ((addr >> 8) & 0xFF) as u8,
            (addr & 0xFF) as u8,
            chunk as u8,
        ];
        let response = c.transmit(&apdu)?;
        memory.extend_from_slice(expect_ok(&response)?);
        addr += chunk;
    }

    Ok(memory)
}

/// Present the security code (PSC) to unlock writes. A wrong code decrements
/// the error counter and can permanently lock the card, so this is only ever
/// called on an explicit, confirmed write.
///
/// PRESENT_CODE does NOT answer `90 00`. It answers `90 <EC>`, where `EC` is
/// the error counter *after* the attempt: a correct code restores it to `FF`,
/// a wrong code leaves it with fewer bits set (and `00` means locked).
pub fn present_psc(c: &Connected, psc: &[u8]) -> Result<(), Box<dyn Error>> {
    let mut apdu = vec![0xFF, 0x20, 0x00, 0x00, psc.len() as u8];
    apdu.extend_from_slice(psc);
    let r = c.transmit(&apdu)?;

    if r.len() < 2 {
        return Err("PSC verification: response too short".into());
    }
    let (sw1, ec) = (r[r.len() - 2], r[r.len() - 1]);

    match (sw1, ec) {
        (0x90, 0xFF) => Ok(()), // counter restored -> code accepted
        (0x90, 0x00) => Err("PSC rejected and the card is now locked (counter 00)".into()),
        (0x90, ec) => Err(format!(
            "PSC rejected (wrong code); error counter is now {:02X}, not FF",
            ec
        )
        .into()),
        (sw1, sw2) => Err(format!("PSC verification failed: card returned SW {:02X} {:02X}", sw1, sw2).into()),
    }
}

pub fn write(c: &Connected, offset: usize, data: &[u8]) -> Result<(), Box<dyn Error>> {
    if offset + data.len() > SLE5528_SIZE {
        return Err(format!(
            "write {}..{} is out of range (card is {} bytes)",
            offset,
            offset + data.len(),
            SLE5528_SIZE
        )
        .into());
    }

    let mut addr = offset;
    // WRITE_MEMORY_CARD: FF D0 <addr-hi> <addr-lo> <len> <data...>, in chunks.
    for chunk in data.chunks(16) {
        let mut apdu = vec![
            0xFF,
            0xD0,
            ((addr >> 8) & 0xFF) as u8,
            (addr & 0xFF) as u8,
            chunk.len() as u8,
        ];
        apdu.extend_from_slice(chunk);
        let response = c.transmit(&apdu)?;
        expect_ok(&response)?;
        addr += chunk.len();
    }

    Ok(())
}
