mod commands;
mod crypto;
mod db;
mod interactive;
mod models;
mod paths;
mod placeholder;
mod shell;
mod transfer;
mod tui;
mod vars;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "inr", version, about = "A secure, searchable vault for the commands you run every day")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Search saved commands and run the selected one
    S,
    /// Add a new command
    A {
        /// The command to save, e.g. "ssh %s:host"
        template: String,
    },
    /// Delete a command by id
    D {
        /// The command's nanoid
        id: String,
    },
    /// Edit a command's description and text, interactively
    E {
        /// The command's nanoid
        id: String,
    },
    /// Initialize inr (set the master password)
    I,
    /// Show recent execution history
    H,
    /// Manage saved env variables
    Env {
        #[command(subcommand)]
        action: EnvCommands,
    },
    /// Export all commands and envs to an encrypted transfer file
    Export {
        /// Path to write the transfer file to
        path: String,
    },
    /// Import commands and envs from an encrypted transfer file
    Import {
        /// Path to the transfer file to read
        path: String,
    },
}

#[derive(Subcommand)]
enum EnvCommands {
    /// Add an env variable (prefix with s:/t:/nf:/ni:, default is text)
    A {
        /// e.g. "s:apikey", "nf:threshold", or "username" (defaults to text)
        name: String,
    },
    /// Search env variables and copy the selected value to the clipboard
    S,
    /// Remove an env variable by id
    R {
        /// The env's nanoid
        id: String,
    },
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Commands::S => commands::search::run(),
        Commands::A { template } => commands::add::run(template),
        Commands::D { id } => commands::delete::run(id),
        Commands::E { id } => commands::edit::run(id),
        Commands::I => commands::init::run(),
        Commands::H => commands::history::run(),
        Commands::Env { action } => match action {
            EnvCommands::A { name } => commands::env_add::run(name),
            EnvCommands::S => commands::env_search::run(),
            EnvCommands::R { id } => commands::env_remove::run(id),
        },
        Commands::Export { path } => commands::export::run(path),
        Commands::Import { path } => commands::import::run(path),
    }
}
