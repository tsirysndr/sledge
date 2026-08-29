//! ACOS3 file access via ISO 7816 commands.
//!
//! Untested against real personalization — access conditions are set when the
//! card is personalized, so these may be rejected with a security-status SW.
//! No PIN is ever submitted automatically.

use crate::card::{Connected, expect_ok};
use std::error::Error;

pub fn select_file(c: &Connected, file_id: &[u8]) -> Result<(), Box<dyn Error>> {
    let mut apdu = vec![0x00, 0xA4, 0x00, 0x00, file_id.len() as u8];
    apdu.extend_from_slice(file_id);
    let r = c.transmit(&apdu)?;
    expect_ok(&r).map_err(|e| {
        format!("SELECT FILE failed: {e} (check the file ID and access conditions)")
    })?;
    Ok(())
}

pub fn read_binary(c: &Connected, offset: usize, length: usize) -> Result<Vec<u8>, Box<dyn Error>> {
    let mut data = Vec::with_capacity(length);
    let mut addr = offset;
    let end = offset + length;
    while addr < end {
        let chunk = std::cmp::min(255, end - addr);
        // READ BINARY: 00 B0 <addr-hi> <addr-lo> <len>
        let apdu = [
            0x00,
            0xB0,
            ((addr >> 8) & 0xFF) as u8,
            (addr & 0xFF) as u8,
            chunk as u8,
        ];
        let response = c.transmit(&apdu)?;
        data.extend_from_slice(expect_ok(&response)?);
        addr += chunk;
    }
    Ok(data)
}

pub fn update_binary(c: &Connected, offset: usize, data: &[u8]) -> Result<(), Box<dyn Error>> {
    let mut addr = offset;
    for chunk in data.chunks(255) {
        // UPDATE BINARY: 00 D6 <addr-hi> <addr-lo> <len> <data...>
        let mut apdu = vec![
            0x00,
            0xD6,
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
