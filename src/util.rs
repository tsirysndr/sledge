use std::error::Error;

pub fn hexdump(data: &[u8]) {
    for (row, chunk) in data.chunks(16).enumerate() {
        print!("{:04X}: ", row * 16);

        for i in 0..16 {
            if let Some(b) = chunk.get(i) {
                print!("{:02X} ", b);
            } else {
                print!("   ");
            }
        }

        print!(" ");

        for &b in chunk {
            let c = if b.is_ascii_graphic() || b == b' ' {
                b as char
            } else {
                '.'
            };
            print!("{c}");
        }

        println!();
    }
}

pub fn parse_hex(label: &str, s: &str) -> Result<Vec<u8>, Box<dyn Error>> {
    hex::decode(s.trim().replace(' ', ""))
        .map_err(|e| format!("invalid hex for {label} ({s:?}): {e}").into())
}
