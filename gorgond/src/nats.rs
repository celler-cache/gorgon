// SPDX-FileCopyrightText: 2025 embr <git@liclac.eu>
//
// SPDX-License-Identifier: EUPL-1.2

use std::error::Error as StdError;

use async_nats::{
    jetstream::{
        self,
        consumer::{self, pull},
        stream,
    },
    ServerAddr,
};
use backon::{ExponentialBuilder, RetryableWithContext};
use diesel_async::{pooled_connection::deadpool::Pool, AsyncPgConnection};
use futures::stream::TryStreamExt;
use gorgon_build_helper::events::Event;
use humantime::Duration;
use miette::{Context, IntoDiagnostic, Result};
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, instrument};

#[derive(Debug, Clone, clap::Parser)]
#[command(next_help_heading = "Events")]
struct EventsArgs {
    /// How often to request a batch of events.
    #[arg(long, default_value = "10s")]
    event_batch_interval: Duration,
}

#[derive(Debug, Clone, clap::Parser)]
#[command(next_help_heading = "NATS Consumer")]
pub struct NatsArgs {
    /// NATS server to connect to.
    #[arg(long, short = 'N', default_value = "localhost:4222")]
    nats_addr: ServerAddr,
    /// Name of the events stream. Created if not present.
    #[arg(long, default_value = "gorgon-events")]
    nats_events_stream: String,
    /// Subjects the events stream consumes, set during creation.
    ///
    /// Has no effect if changed after the stream is already created.
    #[arg(long, default_values = ["gorgon.events", "gorgon.events.>"])]
    nats_events_subjects: Vec<String>,
    /// Name of the durable events consumer.
    #[arg(long, default_value = "gorgond")]
    nats_events_consumer: String,
    /// Maximum number of events to be consumed in one batch.
    #[arg(long, default_value = "250")]
    nats_events_max_messages: usize,

    #[command(flatten)]
    events: EventsArgs,
}

#[instrument(name = "nats", level = "ERROR", skip_all)]
pub async fn run(
    token: CancellationToken,
    args: NatsArgs,
    pool: Pool<AsyncPgConnection>,
) -> Result<()> {
    info!("Connecting to NATS...");
    let client = async_nats::connect(args.nats_addr.clone())
        .await
        .into_diagnostic()
        .context("Couldn't connect to NATS")?;
    let jetstream = jetstream::new(client);

    debug!("-> Checking that stream exists...");
    let stream = jetstream
        .get_or_create_stream(stream::Config {
            subjects: args.nats_events_subjects.clone(),
            name: args.nats_events_stream.clone(),
            // Messages can only be consumed once, then deleted from the stream.
            // Only a single stream can be created covering the listed subjects.
            retention: stream::RetentionPolicy::WorkQueue,
            ..Default::default()
        })
        .await
        .into_diagnostic()
        .context("Couldn't get/create NATS stream")?;

    debug!("-> Creating consumer...");
    let consumer = stream
        .create_consumer(consumer::pull::Config {
            durable_name: Some(args.nats_events_consumer.clone()),
            name: Some(args.nats_events_consumer.clone()),
            ack_policy: consumer::AckPolicy::Explicit,
            ack_wait: std::time::Duration::from_secs(60),
            ..Default::default()
        })
        .await
        .into_diagnostic()
        .context("Couldn't create consumer")?;
    {
        let info = consumer.cached_info();
        info!(
            stream = info.stream_name,
            consumer = info.name,
            pending = info.num_pending,
            "Consuming events..."
        );
    }

    debug!("-> Creating sequence...");
    let batches = consumer
        .sequence(args.nats_events_max_messages)
        .into_diagnostic()
        .context("Couldn't create sequence!?")?;
    token
        .run_until_cancelled_owned(async move {
            let (_, r) = (async |mut batches: pull::Sequence| {
                let r = (&mut batches)
                    .map_err(miette::Report::from_err)
                    .try_for_each(|batch| consume_batch(&pool, batch))
                    .await;
                (batches, r)
            })
            .retry(ExponentialBuilder::default())
            .notify(|err, d| {
                let d = Duration::from(d);
                error!("Failed to consume batch - retrying in: {d}\n\n{err:?}");
            })
            .context(batches)
            .await;
            r
        })
        .await
        .unwrap_or(Ok(()))
}

#[derive(Debug, thiserror::Error, miette::Diagnostic)]
#[error(transparent)]
struct BatchError(Box<dyn StdError + Sync + Send>);

#[instrument(name = "consume_batch", skip_all)]
async fn consume_batch(_pool: &Pool<AsyncPgConnection>, batch: pull::Batch) -> Result<()> {
    batch
        .map_err(BatchError)
        .map_err(miette::Report::new)
        .try_for_each(async |msg| {
            let event: Event = serde_json::from_slice(&msg.payload)
                .into_diagnostic()
                .context("Couldn't parse event")?;
            debug!(?event, "-> Event");
            msg.ack()
                .await
                .map_err(BatchError)
                .context("Couldn't acknowledge message")
        })
        .await
}
