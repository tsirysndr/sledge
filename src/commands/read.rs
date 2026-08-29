use crate::aturi::Dict;
use crate::card::{CardKind, Connected, SLE5528_SIZE};
use crate::cli::ReadArgs;
use crate::util::{hexdump, parse_hex};
use crate::{acos, nfc, sle};
use std::error::Error;

pub fn run(c: &Connected, args: &ReadArgs, dict: &Dict) -> Result<(), Box<dyn Error>> {
    // An NFC tag carries an NDEF message, not a byte range, so it decodes to
    // URI records rather than to the payload blob the contact cards hold.
    if let CardKind::Nfc(tag) = c.kind {
        return read_nfc(c, args, tag);
    }

    let data = match c.kind {
        CardKind::Sle5528 => {
            sle::select_type(c)?;
            // Default to the write span, not the whole card: the tail holds
            // manufacturer/protection bytes that would read back as payload.
            let length = args
                .length
                .unwrap_or(sle::WRITE_SPAN.min(SLE5528_SIZE - args.offset));
            sle::read(c, args.offset, length)?
        }
        CardKind::Acos3 => {
            // ACOS defaults to the user data file FF04 when --file is omitted.
            let file = args.file.as_deref().unwrap_or("FF04");
            let fid = parse_hex("--file", file)?;
            let start = args.record;
            let max = args.length;
            let pin = args.pin.as_ref().map(|p| p.as_bytes().to_vec());
            let code = args.code;
            c.with_transaction(|tx| {
                acos::select_file(tx, &fid)?;
                if let Some(p) = &pin {
                    acos::submit_code(tx, code, p)?;
                }
                let reclen = acos::record_len(tx)?;
                acos::read_records(tx, start, reclen, max)
            })?
        }
        CardKind::Nfc(_) => unreachable!("handled above"),
        CardKind::Unknown => return Err("unknown card type; cannot read".into()),
    };

    println!();
    if args.raw {
        hexdump(&data);
    } else {
        // The payload decoder skips erased fill at both ends (the data may sit
        // past an offset another tool chose), decodes a compact at:// or
        // favorites blob, and returns any trailing newline-separated text —
        // the layout rocksky-desktop writes.
        let payloads = crate::aturi::decode_payloads(&data, dict);
        if payloads.is_empty() {
            println!("(no data)");
        } else {
            for p in payloads {
                println!("{p}");
            }
        }
    }
    Ok(())
}

/// Read an NFC tag: its NDEF URI records, or its raw user memory with --raw.
fn read_nfc(c: &Connected, args: &ReadArgs, tag: nfc::NfcTag) -> Result<(), Box<dyn Error>> {
    let raw = args.raw;
    c.with_transaction(|tx| {
        println!("UID: {}", nfc::uid(tx)?);
        println!();
        if raw {
            hexdump(&nfc::read_memory(tx, tag)?);
            return Ok(());
        }
        let uris = nfc::read_uris(tx, tag)?;
        if uris.is_empty() {
            println!("(no NDEF message)");
        } else {
            for uri in uris {
                println!("{uri}");
            }
        }
        Ok(())
    })
}
