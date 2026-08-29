use crate::card::{CardKind, Connected};
use crate::cli::WriteArgs;
use crate::util::parse_hex;
use crate::{acos, sle};
use std::error::Error;

pub fn run(c: &Connected, args: &WriteArgs) -> Result<(), Box<dyn Error>> {
    // Build the payload: the text, optionally padded with 0xFF to --length.
    let mut payload = args.text.as_bytes().to_vec();
    if let Some(len) = args.length {
        if payload.len() > len {
            return Err(format!("text is {} bytes but --length is {}", payload.len(), len).into());
        }
        payload.resize(len, 0xFF);
    }

    match c.kind {
        CardKind::Sle5528 => write_sle(c, args, &payload),
        CardKind::Acos3 => write_acos(c, args, &payload),
        CardKind::Unknown => Err("unknown card type; cannot write".into()),
    }
}

fn write_sle(c: &Connected, args: &WriteArgs, payload: &[u8]) -> Result<(), Box<dyn Error>> {
    let psc_hex = args.psc.as_deref().ok_or(
        "writing an SLE card requires --psc <hex> (the security code).\n\
         A WRONG code decrements the error counter and can permanently\n\
         lock the card. Only pass a PSC you know is correct.",
    )?;
    let psc = parse_hex("--psc", psc_hex)?;

    println!();
    println!(
        "Plan: write {} bytes at offset {} of the SLE card.",
        payload.len(),
        args.offset
    );
    println!(
        "      unlock with PSC {} ({} bytes).",
        hex::encode_upper(&psc),
        psc.len()
    );

    if !args.yes {
        println!();
        println!("Dry run. Re-run with --yes to actually present the PSC and write.");
        return Ok(());
    }

    sle::select_type(c)?;
    if let Some(counter) = sle::error_counter(c)? {
        println!("Error counter before write: {:02X}", counter);
        if counter == 0x00 {
            return Err("password is locked (counter 00); refusing to write".into());
        }
    }

    println!("Presenting PSC...");
    sle::present_psc(c, &psc)?;
    println!("PSC accepted. Writing...");
    sle::write(c, args.offset, payload)?;

    // Verify by reading the region back.
    let back = sle::read(c, args.offset, payload.len())?;
    if back == payload {
        println!("Wrote and verified {} bytes.", payload.len());
        Ok(())
    } else {
        Err("write verification mismatch (read-back differs)".into())
    }
}

fn write_acos(c: &Connected, args: &WriteArgs, payload: &[u8]) -> Result<(), Box<dyn Error>> {
    let file = args
        .file
        .as_deref()
        .ok_or("ACOS cards require --file <ID> (hex EF id, e.g. FF04)")?;
    let file_id = parse_hex("--file", file)?;

    println!();
    println!(
        "Plan: SELECT {} then UPDATE BINARY {} bytes at offset {}.",
        hex::encode_upper(&file_id),
        payload.len(),
        args.offset
    );

    if !args.yes {
        println!();
        println!("Dry run. Re-run with --yes to actually write.");
        return Ok(());
    }

    acos::select_file(c, &file_id)?;
    acos::update_binary(c, args.offset, payload)?;
    println!(
        "Wrote {} bytes (card accepted the UPDATE BINARY).",
        payload.len()
    );
    Ok(())
}
