// SPDX-FileCopyrightText: 2025 embr <git@liclac.eu>
//
// SPDX-License-Identifier: EUPL-1.2

use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use clap::{Parser, Subcommand};
use diesel_migrations::{EmbeddedMigrations, embed_migrations};
use gorgond_diesel::AsyncPgPool;
use miette::{Context, IntoDiagnostic, Result};
use tokio::runtime::{Handle, Runtime};

pub mod sync;

const MIGRATIONS: EmbeddedMigrations = embed_migrations!("migrations");

#[derive(Debug, Clone, Parser)]
pub struct Args {
    #[command(subcommand)]
    command: Command,

    #[command(flatten)]
    db: gorgond_diesel::Args,
    #[command(flatten)]
    common: gorgon_util_cli::Args,
}

#[derive(Debug, Clone, Subcommand)]
pub enum Command {
    Sync(sync::Args),
}

fn init_db(args: &gorgond_diesel::Args) -> Result<AsyncPgPool> {
    Handle::current().block_on(args.init(MIGRATIONS, "gorgond_github"))
}
pub fn run(rt: Arc<Runtime>, args: Args) -> Result<()> {
    match args.command {
        Command::Sync(cmd) => cmd.run(rt, init_db(&args.db)?),
    }
}

fn main() {
    let args = Args::parse();
    gorgon_util_cli::logger(args.common).init();
    gorgon_util_cli::main(args.common, || {
        let rt = Arc::new(
            tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .thread_name_fn({
                    static THREAD_ID: AtomicUsize = AtomicUsize::new(1);
                    move || format!("tokio-{:02}", THREAD_ID.fetch_add(1, Ordering::Relaxed))
                })
                .build()
                .into_diagnostic()
                .context("Couldn't init Tokio runtime")?,
        );
        let _enter = rt.enter();
        run(rt, args)
    });
}
