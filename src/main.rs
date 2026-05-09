mod cache;
mod cli;
mod error;
mod git;
mod handlers;
mod models;
mod output;
mod resolver;
mod scanner;
mod tui;

use clap::Parser;

fn main() {
    let cli = cli::Cli::parse();
    if let Err(e) = handlers::dispatch(cli) {
        eprintln!("{} {:#}", console::style("error:").red().bold(), e);
        std::process::exit(1);
    }
}
