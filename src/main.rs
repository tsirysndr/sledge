mod acos;
mod aturi;
mod card;
mod cli;
mod commands;
mod config;
mod ndef;
mod nfc;
mod sle;
mod util;

use clap::Parser;
use cli::{Cli, Command};
use std::error::Error;
use std::process::ExitCode;

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            // Print with Display (not Debug) so multi-line hints render readably.
            eprintln!("Error: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), Box<dyn Error>> {
    let cli = Cli::parse();

    if matches!(cli.command, Command::Readers) {
        let names = card::list_readers()?;
        if names.is_empty() {
            println!("No PC/SC readers found.");
        }
        for (i, name) in names.iter().enumerate() {
            println!("[{i}] {name}");
        }
        return Ok(());
    }

    let dict = config::load(cli.config.as_deref())?;
    let c = card::connect(cli.reader.as_deref())?;
    println!();

    match &cli.command {
        Command::Readers => unreachable!(),
        Command::Detect => commands::detect::run(&c),
        Command::Inspect => commands::inspect::run(&c)?,
        Command::Read(args) => commands::read::run(&c, args, &dict)?,
        Command::Write(args) => commands::write::run(&c, args, &dict)?,
        Command::Clear(args) => commands::clear::run(&c, args)?,
    }

    Ok(())
}
