//! NDEF messages: the format NFC tags carry, and the one phones read.
//!
//! Only URI records are handled — that is what a tag written by this tool (or
//! by rocksky-desktop, or by a phone's tag writer) holds. A message is wrapped
//! in a TLV (`03 <len> <message> FE`) inside the tag's user memory, and the
//! encoders here produce bytes identical to rocksky-desktop's, so a tag written
//! by either is read back the same by both.

/// NDEF URI abbreviations (NFC Forum URI RTD, table 6), in code order. Index 0
/// is "no prefix", which is what `at://` and `rocksky://` use.
const URI_PREFIXES: [&str; 36] = [
    "",
    "http://www.",
    "https://www.",
    "http://",
    "https://",
    "tel:",
    "mailto:",
    "ftp://anonymous:anonymous@",
    "ftp://ftp.",
    "ftps://",
    "sftp://",
    "smb://",
    "nfs://",
    "ftp://",
    "dav://",
    "news:",
    "telnet://",
    "imap:",
    "rtsp://",
    "urn:",
    "pop:",
    "sip:",
    "sips:",
    "tftp:",
    "btspp://",
    "btl2cap://",
    "btgoep://",
    "tcpobex://",
    "irdaobex://",
    "file://",
    "urn:epc:id:",
    "urn:epc:tag:",
    "urn:epc:pat:",
    "urn:epc:raw:",
    "urn:epc:",
    "urn:nfc:",
];

/// The URI's abbreviation code and the remainder that follows it.
/// Longest match wins, so "https://www." beats "https://".
fn abbreviate(uri: &str) -> (u8, &str) {
    URI_PREFIXES
        .iter()
        .enumerate()
        .skip(1)
        .filter(|(_, p)| uri.starts_with(*p))
        .max_by_key(|(_, p)| p.len())
        .map(|(i, p)| (i as u8, &uri[p.len()..]))
        .unwrap_or((0, uri))
}

/// Encode `uris` as NDEF URI records wrapped in a TLV, padded to `align` bytes.
///
/// Order carries meaning: a reader tries the records front to back, so the
/// portable URI goes first and any fallback after it. A reader that only looks
/// at the first record — a phone, an older build — sees exactly the
/// single-record tag it would have seen anyway.
pub fn encode_uris(uris: &[&str], align: usize) -> Vec<u8> {
    let mut records = Vec::new();
    for (i, uri) in uris.iter().enumerate() {
        let (code, rest) = abbreviate(uri);
        let mut payload = vec![code];
        payload.extend_from_slice(rest.as_bytes());

        // SR|TNF=1 (well known), plus MB on the first record and ME on the
        // last — both on a lone record, which is the 0xD1 a single-URI tag has.
        let mut flags = 0x11u8;
        if i == 0 {
            flags |= 0x80;
        }
        if i + 1 == uris.len() {
            flags |= 0x40;
        }

        records.extend_from_slice(&[flags, 0x01, payload.len() as u8, b'U']);
        records.extend_from_slice(&payload);
    }

    let mut tlv = vec![0x03];
    if records.len() < 0xFF {
        tlv.push(records.len() as u8);
    } else {
        tlv.push(0xFF);
        tlv.extend_from_slice(&(records.len() as u16).to_be_bytes());
    }
    tlv.extend_from_slice(&records);
    tlv.push(0xFE); // terminator
    pad_to(&mut tlv, align);
    tlv
}

/// Zero-pad to a whole number of `align`-byte units. Tag memory is written in
/// fixed-size units — 4-byte pages on Type 2, 16-byte blocks on Classic — and a
/// short trailing write is rejected outright, so the message always ends on a
/// boundary.
pub fn pad_to(bytes: &mut Vec<u8>, align: usize) {
    while !bytes.len().is_multiple_of(align) {
        bytes.push(0x00);
    }
}

/// Walk the TLV chain in a tag's user memory and return the NDEF message bytes.
pub fn find_tlv(data: &[u8]) -> Option<&[u8]> {
    let mut i = 0;
    while i < data.len() {
        match data[i] {
            0x00 => i += 1,      // NULL padding
            0xFE => return None, // terminator
            tag => {
                let (len, header) = match data.get(i + 1)? {
                    0xFF => (
                        u16::from_be_bytes([*data.get(i + 2)?, *data.get(i + 3)?]) as usize,
                        4,
                    ),
                    n => (*n as usize, 2),
                };
                let start = i + header;
                let end = start.checked_add(len)?;
                if end > data.len() {
                    return None;
                }
                if tag == 0x03 {
                    return Some(&data[start..end]);
                }
                i = end;
            }
        }
    }
    None
}

/// Decode every well-known URI record in an NDEF message, in order.
///
/// A malformed record ends the walk and keeps whatever came before it: the
/// first record is the one that matters, so a damaged trailing record must not
/// cost the caller the good record ahead of it.
pub fn uri_records(message: &[u8]) -> Vec<String> {
    let mut uris = Vec::new();
    let mut i = 0;
    while i + 3 <= message.len() {
        let flags = message[i];
        let short = flags & 0x10 != 0;
        let il = flags & 0x08 != 0;
        let type_len = message[i + 1] as usize;

        let (payload_len, mut cursor) = if short {
            (message[i + 2] as usize, i + 3)
        } else {
            let Some(bytes) = message.get(i + 2..i + 6) else {
                break;
            };
            (
                u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) as usize,
                i + 6,
            )
        };
        if il {
            let Some(id_len) = message.get(cursor) else {
                break;
            };
            cursor += 1 + *id_len as usize;
        }

        let Some(rec_type) = message.get(cursor..cursor + type_len) else {
            break;
        };
        cursor += type_len;
        let Some(payload) = message.get(cursor..cursor + payload_len) else {
            break;
        };

        // TNF 1 (well known) + type "U".
        if flags & 0x07 == 0x01 && rec_type == b"U" && !payload.is_empty() {
            let prefix = URI_PREFIXES.get(payload[0] as usize).copied().unwrap_or("");
            uris.push(format!(
                "{prefix}{}",
                String::from_utf8_lossy(&payload[1..])
            ));
        }

        if flags & 0x40 != 0 {
            break; // ME: last record
        }
        i = cursor + payload_len;
    }
    uris
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_uri_round_trips() {
        let uri = "at://did:plc:abcd/app.rocksky.album/3kabc";
        let bytes = encode_uris(&[uri], 4);
        assert!(bytes.len().is_multiple_of(4));
        let message = find_tlv(&bytes).expect("TLV");
        assert_eq!(uri_records(message), vec![uri.to_string()]);
    }

    #[test]
    fn a_lone_record_has_both_message_flags() {
        let bytes = encode_uris(&["at://x"], 4);
        // 03 <len> then the record header: MB|ME|SR|TNF=1 == 0xD1.
        assert_eq!(bytes[2], 0xD1);
    }

    #[test]
    fn several_uris_keep_their_order() {
        let uris = ["at://did:plc:abcd/x/1", "rocksky://library/album/42"];
        let bytes = encode_uris(&uris, 16);
        assert!(bytes.len().is_multiple_of(16));
        let got = uri_records(find_tlv(&bytes).expect("TLV"));
        assert_eq!(got, uris.map(String::from).to_vec());
    }

    #[test]
    fn known_prefixes_are_abbreviated() {
        let bytes = encode_uris(&["https://www.example.com/a"], 4);
        let message = find_tlv(&bytes).expect("TLV");
        // Payload starts right after flags/type-len/payload-len/'U'.
        assert_eq!(message[4], 0x02); // "https://www."
        assert_eq!(uri_records(message), vec!["https://www.example.com/a"]);
    }

    #[test]
    fn at_uris_are_stored_whole() {
        let bytes = encode_uris(&["at://x"], 4);
        let message = find_tlv(&bytes).expect("TLV");
        assert_eq!(message[4], 0x00); // no prefix code
    }

    #[test]
    fn a_tlv_after_padding_and_other_tlvs_is_found() {
        let mut memory = vec![0x00, 0x00]; // NULL TLVs
        memory.extend_from_slice(&[0x01, 0x03, 0xAA, 0xBB, 0xCC]); // lock control TLV
        memory.extend_from_slice(&encode_uris(&["at://y"], 4));
        let got = uri_records(find_tlv(&memory).expect("TLV"));
        assert_eq!(got, vec!["at://y"]);
    }

    #[test]
    fn blank_memory_has_no_message() {
        assert!(find_tlv(&[0x00; 32]).is_none());
        assert!(find_tlv(&[0xFE, 0x00, 0x00]).is_none());
    }

    #[test]
    fn a_truncated_record_keeps_the_ones_before_it() {
        let bytes = encode_uris(&["at://a", "at://b"], 4);
        let message = find_tlv(&bytes).expect("TLV");
        let cut = &message[..message.len() - 3];
        assert_eq!(uri_records(cut), vec!["at://a"]);
    }
}
