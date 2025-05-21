// SPDX-FileCopyrightText: 2025 embr <git@liclac.eu>
//
// SPDX-License-Identifier: EUPL-1.2

use std::sync::Arc;

use clap::Parser;
use gorgond_diesel::AsyncPgPool;
use miette::Result;
use tokio::runtime::Runtime;

#[derive(Debug, Clone, Parser)]
#[group(id = "sync")]
pub struct Args {}
impl Args {
    pub fn run(self, rt: Arc<Runtime>, pool: AsyncPgPool) -> Result<()> {
        Ok(())
    }
}
