use pcsc::{Card, Context, Disposition, Error as PcscError, Protocols, Scope, ShareMode};
use std::cell::RefCell;
use std::error::Error;

pub const SLE5528_ATR: &[u8] = &[0x3B, 0x04, 0x92, 0x23, 0x10, 0x91];

pub const ACOS3_ATR: &[u8] = &[
    0x3B, 0xBE, 0x11, 0x00, 0x00, 0x41, 0x01, 0x38, 0x00, 0x00, 0x04, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x01, 0x90, 0x00,
];

pub const SLE5528_SIZE: usize = 1024;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum CardKind {
    Sle5528,
    Acos3,
    Unknown,
}

impl CardKind {
    pub fn from_atr(atr: &[u8]) -> Self {
        if atr == SLE5528_ATR {
            CardKind::Sle5528
        } else if atr == ACOS3_ATR {
            CardKind::Acos3
        } else {
            CardKind::Unknown
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            CardKind::Sle5528 => "SLE (SLE5528 memory card, 1024 bytes)",
            CardKind::Acos3 => "ACOS (ACS ACOS3 / ACOS3-32, filesystem-based)",
            CardKind::Unknown => "Unknown",
        }
    }
}

const MEMORY_CARD_DRIVER_HINT: &str = "\
card returned NotTransacted talking to a memory card.

The reader connected and returned an ATR (so the card is fine), but the
CCID driver refuses the memory-card pseudo-APDUs. On macOS this means
Apple's bundled driver is active; install ACS's acsccid driver into
/usr/local/libexec/SmartCardServices/drivers/ and replug the reader.";

pub fn expect_ok(response: &[u8]) -> Result<&[u8], Box<dyn Error>> {
    if response.len() < 2 {
        return Err("response too short".into());
    }

    let n = response.len();
    let sw1 = response[n - 2];
    let sw2 = response[n - 1];

    if (sw1, sw2) != (0x90, 0x00) {
        return Err(format!("card returned SW {:02X} {:02X}", sw1, sw2).into());
    }

    Ok(&response[..n - 2])
}

pub struct Connected {
    card: RefCell<Card>,
    protocols: Protocols,
    pub kind: CardKind,
    pub atr: Vec<u8>,
}

impl Connected {
    /// Send an APDU and return the response bytes.
    ///
    /// Recovers from the transient `SCARD_W_RESET_CARD`: on macOS the OS resets
    /// the card when our shared handle connects, and it surfaces on the first
    /// APDU. We reconnect (leaving the card powered) and retry once.
    pub fn transmit(&self, apdu: &[u8]) -> Result<Vec<u8>, Box<dyn Error>> {
        match self.transmit_once(apdu) {
            Err(PcscError::ResetCard) => {
                self.card
                    .borrow_mut()
                    .reconnect(ShareMode::Shared, self.protocols, Disposition::LeaveCard)?;
                self.transmit_once(apdu).map_err(Self::map_err)
            }
            other => other.map_err(Self::map_err),
        }
    }

    fn transmit_once(&self, apdu: &[u8]) -> Result<Vec<u8>, PcscError> {
        let mut recv = [0u8; 4096];
        let card = self.card.borrow();
        card.transmit(apdu, &mut recv).map(|r| r.to_vec())
    }

    fn map_err(e: PcscError) -> Box<dyn Error> {
        match e {
            // A synchronous memory card (SLE5528) connects fine over RAW, but a
            // driver without ACS memory-card support rejects the ACR39U pseudo-
            // APDUs with NotTransacted. On macOS this means the ACS acsccid
            // driver is not installed (Apple's bundled CCID driver lacks it).
            PcscError::NotTransacted => MEMORY_CARD_DRIVER_HINT.into(),
            e => e.into(),
        }
    }
}

/// Connect to a card, handling both microprocessor and memory cards.
///
/// Microprocessor cards (ACOS3) speak the asynchronous T=0/T=1 protocols.
/// Synchronous memory cards (SLE5528) only speak the reader's RAW protocol;
/// negotiating T=0/T=1 against one yields SCARD_W_UNRESPONSIVE_CARD. So try
/// T=0/T=1 first, then fall back to RAW.
fn connect_card(ctx: &Context, reader: &std::ffi::CStr) -> Result<(Card, Protocols), Box<dyn Error>> {
    match ctx.connect(reader, ShareMode::Shared, Protocols::ANY) {
        Ok(card) => Ok((card, Protocols::ANY)),
        Err(PcscError::UnresponsiveCard | PcscError::ProtoMismatch) => {
            Ok((ctx.connect(reader, ShareMode::Shared, Protocols::RAW)?, Protocols::RAW))
        }
        Err(e) => Err(e.into()),
    }
}

pub fn connect(reader_index: usize) -> Result<Connected, Box<dyn Error>> {
    let ctx = Context::establish(Scope::User)?;

    let mut readers_buf = [0u8; 2048];
    let readers: Vec<_> = ctx.list_readers(&mut readers_buf)?.collect();
    let reader = readers.get(reader_index).ok_or_else(|| {
        format!(
            "no PC/SC reader at index {} ({} found)",
            reader_index,
            readers.len()
        )
    })?;

    println!("Reader: {}", reader.to_string_lossy());

    let (mut card, protocols) = connect_card(&ctx, reader)?;

    // Proactively re-power the card. Connecting Shared can inherit a card that
    // a prior session reset, which makes the first transmit fail with
    // SCARD_W_RESET_CARD ("the smart card has been reset"). Reconnecting with
    // ResetCard clears that stale state so commands start from a clean power-on.
    card.reconnect(ShareMode::Shared, protocols, Disposition::ResetCard)?;

    let status = card.status2_owned()?;
    let atr = status.atr().to_vec();
    let kind = CardKind::from_atr(&atr);

    println!("ATR: {}", hex::encode_upper(&atr));

    Ok(Connected {
        card: RefCell::new(card),
        protocols,
        kind,
        atr,
    })
}
