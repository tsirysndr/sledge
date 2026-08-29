use crate::card::{CardKind, Connected, SLE5528_SIZE};
use crate::util::hexdump;
use crate::{nfc, sle};
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
        CardKind::Nfc(tag) => {
            c.with_transaction(|tx| {
                println!("UID: {}", nfc::uid(tx)?);

                match tag {
                    nfc::NfcTag::Type2 => {
                        println!("Usable memory: {} bytes", nfc::capacity(tx));
                        println!(
                            "Access: {}",
                            if nfc::is_read_only(tx) {
                                "locked read-only (irreversible)"
                            } else {
                                "read/write"
                            }
                        );
                    }
                    nfc::NfcTag::Classic { .. } => {
                        println!(
                            "Usable memory: {} bytes (NDEF sectors)",
                            tag.declared_capacity().unwrap_or(0)
                        );
                        println!(
                            "Access: {}",
                            match nfc::classic_state(tx) {
                                nfc::ClassicState::Ndef => "NDEF-formatted, read/write",
                                nfc::ClassicState::Blank =>
                                    "blank (factory keys); needs --format before a write",
                                nfc::ClassicState::Foreign => "locked with unknown keys",
                            }
                        );
                    }
                }

                println!();
                let uris = nfc::read_uris(tx, tag)?;
                if uris.is_empty() {
                    println!("NDEF: (no message)");
                } else {
                    println!("NDEF URI records:");
                    for (i, uri) in uris.iter().enumerate() {
                        println!("  [{i}] {uri}");
                    }
                }

                println!();
                println!("User memory:");
                println!();
                let memory = nfc::read_memory(tx, tag)?;
                hexdump(&memory);
                std::fs::write("nfc-tag.bin", &memory)?;
                println!();
                println!("Saved raw dump to nfc-tag.bin");
                Ok(())
            })?;
        }
        CardKind::Unknown => {
            println!("Unknown card. Raw ATR: {}", hex::encode_upper(&c.atr));
        }
    }

    Ok(())
}
