//! ragekit: GTA V and GTA IV modding tool for Linux.
//! Run with no arguments for the browser, or with a command for the CLI.

mod cli;
mod gui;
mod gui_oiv;
mod gui_tex;
mod ops;
mod settings;

use clap::Parser;

fn main() {
    let cli = cli::Cli::parse();
    let keys_dir = ops::keys_dir(cli.keys.as_deref());
    let res = match &cli.cmd {
        None => gui::run(keys_dir, None),
        Some(cli::Cmd::Gui { path }) => gui::run(keys_dir, path.clone()),
        Some(cmd) => cli::run(&cli, cmd),
    };
    if let Err(e) = res {
        eprintln!("error: {e:#}");
        std::process::exit(1);
    }
}
