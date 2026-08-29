use crate::aturi::Dict;
use crate::card::{CardKind, Connected, SLE5528_SIZE};
use crate::cli::ReadArgs;
use crate::util::{hexdump, parse_hex};
use crate::{acos, sle};
use std::error::Error;

pub fn run(c: &Connected, args: &ReadArgs, dict: &Dict) -> Result<(), Box<dyn Error>> {
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
