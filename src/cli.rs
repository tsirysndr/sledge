use clap::{Args, Parser, Subcommand};

/// A smart-card read/write CLI for ACS memory (SLE) and ACOS cards over PC/SC.
#[derive(Parser)]
#[command(name = "sledge", version, about)]
pub struct Cli {
    /// Reader to use: a 0-based index into the PC/SC reader list, or a
    /// (case-insensitive) substring of the reader name. With several readers
    /// plugged in and no --reader, an interactive picker is shown.
    #[arg(long, global = true)]
    pub reader: Option<String>,

    /// Path to a TOML config file with extra AT-URI collections/authorities.
    /// Defaults to ~/.config/sledge/config.toml if present.
    #[arg(long, global = true)]
    pub config: Option<String>,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand)]
pub enum Command {
    /// List connected PC/SC readers with their indices.
    Readers,

    /// Print only the detected card type (SLE or ACOS) and its ATR.
    Detect,

    /// Detect the card and print full info, dumping SLE memory to a file.
    Inspect,

    /// Read text from the card.
    Read(ReadArgs),

    /// Write text to the card.
    Write(WriteArgs),
}

#[derive(Args)]
pub struct ReadArgs {
    /// Start address / offset in bytes (SLE cards).
    #[arg(long, default_value_t = 0)]
    pub offset: usize,

    /// Number of bytes to read (default: to end of memory / file).
    #[arg(long)]
    pub length: Option<usize>,

    /// Print a raw hex dump instead of decoding as text.
    #[arg(long)]
    pub raw: bool,

    /// (ACOS only) EF file ID to select first, in hex, e.g. FF04.
    #[arg(long)]
    pub file: Option<String>,

    /// (ACOS only) Record number to start reading from.
    #[arg(long, default_value_t = 0)]
    pub record: usize,

    /// (ACOS only) Code/PIN to submit before reading a protected file (ASCII).
    #[arg(long)]
    pub pin: Option<String>,

    /// (ACOS only) Code reference for `--pin` (SUBMIT CODE P1; 7 = Issuer Code).
    #[arg(long, default_value_t = 7)]
    pub code: u8,
}

#[derive(Args)]
pub struct WriteArgs {
    /// The text to write.
    pub text: String,

    /// Start address / offset in bytes. Defaults past the SLE protected header.
    #[arg(long, default_value_t = 32)]
    pub offset: usize,

    /// Pad the written region up to this many bytes with 0xFF.
    #[arg(long)]
    pub length: Option<usize>,

    /// (SLE only) Security code (PSC) in hex, e.g. FFFF. Required to unlock writes.
    #[arg(long)]
    pub psc: Option<String>,

    /// (ACOS only) EF file ID to select first, in hex, e.g. FF04.
    #[arg(long)]
    pub file: Option<String>,

    /// (ACOS only) Record number to start writing at.
    #[arg(long, default_value_t = 0)]
    pub record: usize,

    /// (ACOS only) Code/PIN to submit before writing, as ASCII (writes to a
    /// protected file need this).
    #[arg(long)]
    pub pin: Option<String>,

    /// (ACOS only) Code reference for the PIN (the SUBMIT CODE P1; 7 = Issuer
    /// Code, 0-6 = PIN / application codes).
    #[arg(long, default_value_t = 7)]
    pub code: u8,

    /// Actually perform the write. Without this flag the command is a dry run.
    #[arg(long)]
    pub yes: bool,
}
