use crate::card::{CardKind, Connected, SLE5528_SIZE};
use crate::cli::ReadArgs;
use crate::util::{decode_text, hexdump, parse_hex};
use crate::{acos, sle};
use std::error::Error;

pub fn run(c: &Connected, args: &ReadArgs) -> Result<(), Box<dyn Error>> {
    let data = match c.kind {
        CardKind::Sle5528 => {
            sle::select_type(c)?;
            let length = args.length.unwrap_or(SLE5528_SIZE - args.offset);
            sle::read(c, args.offset, length)?
        }
        CardKind::Acos3 => {
            let file = args
                .file
                .as_deref()
                .ok_or("ACOS cards require --file <ID> (hex EF id, e.g. FF04)")?;
            acos::select_file(c, &parse_hex("--file", file)?)?;
            let length = args.length.ok_or("ACOS reads require --length <N>")?;
            acos::read_binary(c, args.offset, length)?
        }
        CardKind::Unknown => return Err("unknown card type; cannot read".into()),
    };

    println!();
    if args.raw {
        hexdump(&data);
    } else {
        println!("{}", decode_text(&data));
    }
    Ok(())
}
