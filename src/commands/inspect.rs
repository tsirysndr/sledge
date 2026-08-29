use crate::card::{CardKind, Connected, SLE5528_SIZE};
use crate::sle;
use crate::util::hexdump;
use std::error::Error;

pub fn run(c: &Connected) -> Result<(), Box<dyn Error>> {
    println!("Card: {}", c.kind.label());
    println!();

    match c.kind {
        CardKind::Sle5528 => {
            sle::select_type(c)?;
            println!("Card type selected.");

            if let Some(counter) = sle::error_counter(c)? {
                println!("Error counter raw value: {:02X}", counter);
                match counter {
                    0xFF => println!("Password state: clear / last verification OK"),
                    0x00 => println!("WARNING: password is locked"),
                    x => println!("Password state: partially consumed ({:02X})", x),
                }
            }

            println!();
            println!("Reading {}-byte memory...", SLE5528_SIZE);
            println!();
            let memory = sle::read(c, 0, SLE5528_SIZE)?;
            hexdump(&memory);
            std::fs::write("sle5528.bin", &memory)?;
            println!();
            println!("Saved raw dump to sle5528.bin");
        }
        CardKind::Acos3 => {
            println!("ACOS3 is filesystem-based. ATR inspection only; no PIN,");
            println!("authentication, write, or personalization commands were sent.");
            println!("Use `read`/`write --file <ID>` to access a specific EF.");
        }
        CardKind::Unknown => {
            println!("Unknown card. Raw ATR: {}", hex::encode_upper(&c.atr));
        }
    }

    Ok(())
}
