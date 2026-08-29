mod acos;
mod aturi;
mod card;
mod cli;
mod commands;
mod config;
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
    let dict = config::load(cli.config.as_deref())?;
    let c = card::connect(cli.reader)?;
    println!();

    match &cli.command {
        Command::Detect => commands::detect::run(&c),
        Command::Inspect => commands::inspect::run(&c)?,
        Command::Read(args) => commands::read::run(&c, args, &dict)?,
        Command::Write(args) => commands::write::run(&c, args, &dict)?,
    }

    Ok(())
}
