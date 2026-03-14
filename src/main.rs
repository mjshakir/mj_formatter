mod app;
mod cli;
mod config;
mod engine;
mod files;
mod model;
mod parser;
mod policy;
mod graph;
mod runtime;
mod text_scan;

use anyhow::Result;
use app::runner::App;

fn main() -> Result<()> {
    App::run()
}
