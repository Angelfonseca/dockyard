#![allow(clippy::collapsible_if, clippy::type_complexity, clippy::too_many_arguments, clippy::manual_checked_ops, clippy::unnecessary_sort_by, clippy::manual_is_multiple_of, clippy::collapsible_match)]

mod app;
mod catalog;
mod cli;
mod collector;
mod config;
mod engine;
mod ext;
mod forms;
mod input;
mod model;
mod shell;
mod tui;
mod ui;
mod util;
mod web;

use anyhow::Result;
use clap::Parser;

#[tokio::main]
async fn main() {
    let cli = cli::Cli::parse();
    let r: Result<()> = match cli.cmd {
        None => tui::run().await,
        Some(cmd) => cli::run(cmd).await,
    };
    if let Err(e) = r {
        eprintln!("error: {e:#}");
        std::process::exit(1);
    }
}
