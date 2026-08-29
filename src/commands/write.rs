use crate::aturi::Dict;
use crate::card::{CardKind, Connected};
use crate::cli::WriteArgs;
use crate::util::parse_hex;
use crate::{acos, sle};
use std::error::Error;

pub fn run(c: &Connected, args: &WriteArgs, dict: &Dict) -> Result<(), Box<dyn Error>> {
    match c.kind {
        CardKind::Sle5528 => write_sle(c, args, dict),
        CardKind::Acos3 => write_acos(c, args, dict),
        CardKind::Unknown => Err("unknown card type; cannot write".into()),
    }
}

/// Build the write payload: the text, optionally padded up to `--length` with
/// `fill` (SLE uses 0xFF for its erased state; ACOS uses 0x00). An `at://` URI
/// or `rocksky://favorites/<did>` is compactly encoded first (auto-detected);
/// anything else is stored as text. Newlines split the text into payloads —
/// compact blob, then the rest as text — matching rocksky-desktop's layout.
fn payload(args: &WriteArgs, fill: u8, dict: &Dict) -> Result<Vec<u8>, Box<dyn Error>> {
    let parts: Vec<&str> = args.text.split('\n').collect();
    let mut p = crate::aturi::encode_payloads(&parts, dict)?;
    if crate::aturi::is_encoded(&p) || crate::aturi::is_favorites(&p) {
        println!(
            "Detected compact-encodable URI — encoded {} → {} bytes.",
            args.text.len(),
            p.len()
        );
    }
    if let Some(len) = args.length {
        if p.len() > len {
            return Err(format!("payload is {} bytes but --length is {}", p.len(), len).into());
        }
        p.resize(len, fill);
    }
    Ok(p)
}

fn write_sle(c: &Connected, args: &WriteArgs, dict: &Dict) -> Result<(), Box<dyn Error>> {
    let mut payload = payload(args, 0xFF, dict)?;
    // Without an explicit --length, a write owns the whole span: pad with the
    // erased value so nothing left by a longer previous write survives past
    // the end of the new one. Matches rocksky-desktop's write behavior.
    if args.length.is_none() {
        let span = sle::WRITE_SPAN.min(crate::card::SLE5528_SIZE.saturating_sub(args.offset));
        if payload.len() < span {
            payload.resize(span, 0xFF);
        }
    }
    let payload = &payload;
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
    if back == *payload {
        println!("Wrote and verified {} bytes.", payload.len());
        Ok(())
    } else {
        Err("write verification mismatch (read-back differs)".into())
    }
}

fn write_acos(c: &Connected, args: &WriteArgs, dict: &Dict) -> Result<(), Box<dyn Error>> {
    let payload = &payload(args, 0x00, dict)?;
    // ACOS defaults to the user data file FF04 when --file is omitted.
    let file = args.file.as_deref().unwrap_or("FF04");
    let file_id = parse_hex("--file", file)?;
    let pin = args.pin.as_ref().map(|p| p.as_bytes().to_vec());

    // Without an explicit --length, a text write owns the file: the rest of the
    // records (after the text) are cleared to 0x00 so no stale data is left.
    let clear_rest = args.length.is_none();

    println!();
    println!(
        "Plan: SELECT {} → WRITE RECORD {} byte(s) starting at record {}{}.",
        hex::encode_upper(&file_id),
        payload.len(),
        args.record,
        if clear_rest {
            ", then clear the rest of the file"
        } else {
            ""
        }
    );
    match &pin {
        Some(p) => println!(
            "      submit code slot {} ({} bytes) first.",
            args.code,
            p.len()
        ),
        None => {
            println!("      no code will be submitted (protected files will reject the write).")
        }
    }

    if !args.yes {
        println!();
        println!("Dry run. Re-run with --yes to actually write.");
        return Ok(());
    }

    let start = args.record;
    let code = args.code;
    let written = c.with_transaction(|tx| {
        acos::select_file(tx, &file_id)?;
        if let Some(p) = &pin {
            acos::submit_code(tx, code, p)?;
            println!("Code accepted.");
        }
        let reclen = acos::record_len(tx)?;

        // Pre-flight capacity check so we never do a partial write.
        let count = acos::record_count(tx, reclen)?;
        let available = count.saturating_sub(start) * reclen;
        if payload.len() > available {
            return Err(format!(
                "{} bytes won't fit: file {} has {} bytes free from record {} \
                 ({} records × {} bytes). Use a larger file or shorter data.",
                payload.len(),
                hex::encode_upper(&file_id),
                available,
                start,
                count,
                reclen
            )
            .into());
        }

        acos::write_records(tx, start, reclen, payload)?;

        if clear_rest {
            let used = payload.len().div_ceil(reclen);
            let cleared = acos::clear_records_from(tx, start + used, reclen)?;
            if cleared > 0 {
                println!("Cleared {cleared} trailing record(s) to 0x00.");
            }
        }

        // Verify by reading the same records back.
        let back = acos::read_records(tx, start, reclen, Some(payload.len()))?;
        Ok(back.starts_with(payload) || back == pad(payload, reclen))
    })?;

    if written {
        println!("Wrote and verified {} byte(s).", payload.len());
        Ok(())
    } else {
        Err("write verification mismatch (read-back differs)".into())
    }
}

/// Pad `data` up to a whole number of `reclen`-byte records with 0x00, matching
/// how `write_records` stores the final record.
fn pad(data: &[u8], reclen: usize) -> Vec<u8> {
    let mut v = data.to_vec();
    let rem = v.len() % reclen;
    if rem != 0 {
        v.resize(v.len() + (reclen - rem), 0x00);
    }
    v
}
