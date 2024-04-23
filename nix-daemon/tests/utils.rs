// SPDX-FileCopyrightText: 2024 embr <git@liclac.eu>
//
// SPDX-License-Identifier: EUPL-1.2

pub fn init_logging() {
    use tracing_subscriber::prelude::*;

    tracing_subscriber::Registry::default()
        .with(tracing_subscriber::filter::LevelFilter::TRACE)
        .with(tracing_subscriber::fmt::layer())
        .try_init()
        .unwrap_or_default();
}
