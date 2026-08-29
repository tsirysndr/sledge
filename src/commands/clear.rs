//! Erase a card back to its blank state.
//!
//! "Blank" is per card family, and each one's erased value is what a factory
//! card reads back as: `0xFF` across an SLE's memory, `0x00` records on ACOS,
//! and an empty NDEF message on an NFC tag. Like `write`, this is a dry run
//! until `--yes`, and it verifies by reading the region back.

use crate::card::{CardKind, Connected, SLE5528_SIZE};
use crate::cli::ClearArgs;
use crate::util::parse_hex;
use crate::{acos, nfc, sle};
use std::error::Error;

pub fn run(c: &Connected, args: &ClearArgs) -> Result<(), Box<dyn Error>> {
    match c.kind {
        CardKind::Sle5528 => clear_sle(c, args),
        CardKind::Acos3 => clear_acos(c, args),
        CardKind::Nfc(tag) => clear_nfc(c, args, tag),
        CardKind::Unknown => Err("unknown card type; cannot clear".into()),
    }
}

fn clear_sle(c: &Connected, args: &ClearArgs) -> Result<(), Box<dyn Error>> {
    if args.offset >= SLE5528_SIZE {
        return Err(format!(
            "offset {} is past the end of the card ({} bytes)",
            args.offset, SLE5528_SIZE
        )
        .into());
    }
    // Same default span as `write`: the region a write owns, not the whole card
    // — the tail holds manufacturer and protection bytes that are not payload.
    let span = args
        .length
        .unwrap_or_else(|| sle::WRITE_SPAN.min(SLE5528_SIZE - args.offset));
    let blank = vec![0xFFu8; span];

    let psc_hex = args.psc.as_deref().ok_or(
        "clearing an SLE card requires --psc <hex> (the security code).\n\
         A WRONG code decrements the error counter and can permanently\n\
         lock the card. Only pass a PSC you know is correct.",
    )?;
    let psc = parse_hex("--psc", psc_hex)?;

    println!();
    println!(
        "Plan: erase {} bytes at offset {} of the SLE card to 0xFF.",
        span, args.offset
    );
    println!(
        "      unlock with PSC {} ({} bytes).",
        hex::encode_upper(&psc),
        psc.len()
    );

    if !args.yes {
        println!();
        println!("Dry run. Re-run with --yes to actually present the PSC and erase.");
        return Ok(());
    }

    sle::select_type(c)?;
    if let Some(counter) = sle::error_counter(c)? {
        println!("Error counter before erase: {:02X}", counter);
        if counter == 0x00 {
            return Err("password is locked (counter 00); refusing to erase".into());
        }
    }

    println!("Presenting PSC...");
    sle::present_psc(c, &psc)?;
    println!("PSC accepted. Erasing...");
    sle::write(c, args.offset, &blank)?;

    if sle::read(c, args.offset, span)? == blank {
        println!("Erased and verified {span} bytes.");
        Ok(())
    } else {
        Err("erase verification mismatch (read-back differs)".into())
    }
}

fn clear_acos(c: &Connected, args: &ClearArgs) -> Result<(), Box<dyn Error>> {
    // ACOS defaults to the user data file FF04 when --file is omitted.
    let file = args.file.as_deref().unwrap_or("FF04");
    let file_id = parse_hex("--file", file)?;
    let pin = args.pin.as_ref().map(|p| p.as_bytes().to_vec());

    println!();
    println!(
        "Plan: SELECT {} → zero {} starting at record {}.",
        hex::encode_upper(&file_id),
        match args.length {
            Some(n) => format!("{n} byte(s)"),
            None => "every record".into(),
        },
        args.record
    );
    match &pin {
        Some(p) => println!(
            "      submit code slot {} ({} bytes) first.",
            args.code,
            p.len()
        ),
        None => {
            println!("      no code will be submitted (protected files will reject the erase).")
        }
    }

    if !args.yes {
        println!();
        println!("Dry run. Re-run with --yes to actually erase.");
        return Ok(());
    }

    let start = args.record;
    let code = args.code;
    let length = args.length;
    let (cleared, verified) = c.with_transaction(|tx| {
        acos::select_file(tx, &file_id)?;
        if let Some(p) = &pin {
            acos::submit_code(tx, code, p)?;
            println!("Code accepted.");
        }
        let reclen = acos::record_len(tx)?;

        let cleared = match length {
            // A whole-file erase runs to the end of the file, which is what the
            // card itself reports — no capacity guess needed.
            None => acos::clear_records_from(tx, start, reclen)? * reclen,
            Some(n) => {
                let count = acos::record_count(tx, reclen)?;
                let available = count.saturating_sub(start) * reclen;
                if n > available {
                    return Err(format!(
                        "{n} bytes won't fit: file {} has {available} bytes from record {start} \
                         ({count} records × {reclen} bytes).",
                        hex::encode_upper(&file_id)
                    )
                    .into());
                }
                let records = n.div_ceil(reclen);
                acos::write_records(tx, start, reclen, &vec![0u8; records * reclen])?;
                records * reclen
            }
        };

        let back = acos::read_records(tx, start, reclen, Some(cleared))?;
        Ok((cleared, back.iter().all(|&b| b == 0x00)))
    })?;

    if cleared == 0 {
        println!("Nothing to erase (no records from record {start}).");
        return Ok(());
    }
    if verified {
        println!("Erased and verified {cleared} byte(s).");
        Ok(())
    } else {
        Err("erase verification mismatch (read-back differs)".into())
    }
}

fn clear_nfc(c: &Connected, args: &ClearArgs, tag: nfc::NfcTag) -> Result<(), Box<dyn Error>> {
    println!();
    println!("Plan: erase the tag to an empty NDEF message.");

    let confirmed = args.yes;
    c.with_transaction(|tx| {
        println!("      UID {}", nfc::uid(tx)?);

        match tag {
            nfc::NfcTag::Type2 => {
                println!(
                    "      {} bytes of user memory will be zeroed.",
                    nfc::capacity(tx)
                );
                if nfc::is_read_only(tx) {
                    return Err("this tag is locked read-only and can't be erased".into());
                }
            }
            nfc::NfcTag::Classic { .. } => match nfc::classic_state(tx) {
                nfc::ClassicState::Ndef => println!(
                    "      {} bytes of NDEF sectors will be zeroed.",
                    tag.declared_capacity().unwrap_or(0)
                ),
                nfc::ClassicState::Blank => {
                    println!("      tag is factory-blank; there is nothing to erase.")
                }
                nfc::ClassicState::Foreign => {
                    return Err(
                        "this MIFARE Classic tag is locked with keys we don't have, \
                                so it can't be erased"
                            .into(),
                    );
                }
            },
        }

        if !confirmed {
            println!();
            println!("Dry run. Re-run with --yes to actually erase.");
            return Ok(());
        }

        let cleared = nfc::clear(tx, tag)?;
        if cleared == 0 {
            println!("Nothing to erase; the tag was already blank.");
            return Ok(());
        }

        let back = nfc::read_uris(tx, tag)?;
        if back.is_empty() {
            println!("Erased and verified {cleared} byte(s).");
            Ok(())
        } else {
            Err("erase verification mismatch (records still readable)".into())
        }
    })
}
