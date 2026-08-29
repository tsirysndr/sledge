use clap::{Args, Parser, Subcommand};

/// A smart-card read/write CLI for ACS memory (SLE) and ACOS cards over PC/SC.
#[derive(Parser)]
#[command(name = "sledge", version, about)]
pub struct Cli {
    /// Reader to use, as a 0-based index into the PC/SC reader list.
    #[arg(long, default_value_t = 0, global = true)]
    pub reader: usize,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand)]
pub enum Command {
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

    /// Actually perform the write. Without this flag the command is a dry run.
    #[arg(long)]
    pub yes: bool,
}
