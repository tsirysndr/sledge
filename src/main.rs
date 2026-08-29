mod acos;
mod card;
mod cli;
mod commands;
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
    let c = card::connect(cli.reader)?;
    println!();

    match &cli.command {
        Command::Detect => commands::detect::run(&c),
        Command::Inspect => commands::inspect::run(&c)?,
        Command::Read(args) => commands::read::run(&c, args)?,
        Command::Write(args) => commands::write::run(&c, args)?,
    }

    Ok(())
}
