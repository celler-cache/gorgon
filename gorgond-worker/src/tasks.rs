// SPDX-FileCopyrightText: 2025 embr <git@liclac.eu>
//
// SPDX-License-Identifier: EUPL-1.2

use backon::{ExponentialBuilder, Retryable};
use crossbeam::channel::Sender;
use diesel_async::AsyncConnection;
use gorgond_diesel::{AsyncPgPool, TransactionError, transaction};
use miette::{Context, IntoDiagnostic, Result};
use scoped_futures::ScopedFutureExt;
use tokio::runtime::Runtime;
use tokio_util::sync::CancellationToken;
use tracing::{Instrument, Span, debug, info_span, instrument, warn};
use tracing_indicatif::span_ext::IndicatifSpanExt;

pub mod repos;

pub enum Task {
    Repo(repos::RepoTask),
}
impl Task {
    #[instrument(skip_all)]
    pub async fn watch(args: &crate::Args, send: Sender<Task>, pool: AsyncPgPool) -> Result<()> {
        Span::current().pb_set_style(&crate::PB_STYLE_BLANK);

        let dt = (args.poll_interval.to_std())
            .into_diagnostic()
            .context("Invalid poll interval!?")?;
        let mut ticker = tokio::time::interval(dt);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

        let sleep_span = info_span!("sleep");
        sleep_span.pb_set_message(&format!(
            "Sleeping for {}...",
            humantime::Duration::from(dt)
        ));
        loop {
            ticker.tick().instrument(sleep_span.clone()).await;
            if !send.is_empty() {
                debug!("Waiting for tasks to finish...");
                continue;
            }

            (async || Self::poll(&send, &pool).await)
                .retry(ExponentialBuilder::default())
                .notify(notify)
                .await?;
        }
    }
    #[instrument(skip_all)]
    async fn poll(send: &Sender<Task>, pool: &AsyncPgPool) -> Result<()> {
        debug!("Checking for new tasks...");

        let mut conn = (pool.get().await)
            .into_diagnostic()
            .context("Couldn't get a connection")?;

        conn.transaction::<(), TransactionError, _>(|conn| {
            async move { Ok(repos::RepoTask::poll(send, conn).await?) }.scope_boxed()
        })
        .await?;

        Ok(())
    }

    #[instrument(skip_all)]
    pub fn run(
        &self,
        args: &crate::Args,
        rt: &Runtime,
        token: &CancellationToken,
        pool: &AsyncPgPool,
    ) -> Result<()> {
        let mut conn = (rt.block_on(pool.get()))
            .into_diagnostic()
            .context("Couldn't get a connection")?;
        transaction(rt, &mut conn, |conn| match self {
            Self::Repo(t) => Ok(t.run(args, rt, token, conn)?),
        })
    }
}

fn notify(err: &miette::Report, retry: std::time::Duration) {
    let retry = humantime::Duration::from(retry);
    warn!(%retry, "{err:?}")
}
