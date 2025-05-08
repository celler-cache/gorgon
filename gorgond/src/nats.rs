// SPDX-FileCopyrightText: 2025 embr <git@liclac.eu>
//
// SPDX-License-Identifier: EUPL-1.2

use std::{collections::BTreeMap, error::Error as StdError};

use async_nats::{
    jetstream::{self, consumer, stream},
    ServerAddr,
};
use backon::{ExponentialBuilder, Retryable};
use chrono::prelude::*;
use diesel::prelude::*;
use diesel_async::{
    pooled_connection::deadpool::Pool, AsyncConnection, AsyncPgConnection, RunQueryDsl,
};
use either::Either;
use futures::stream::TryStreamExt;
use gorgond_client::events;
use humantime::Duration;
use miette::{Context, IntoDiagnostic, Result};
use scoped_futures::ScopedFutureExt;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, instrument, trace};
use uuid::Uuid;

use crate::{models, schema};

#[derive(Debug, Clone, clap::Parser)]
#[command(next_help_heading = "Events")]
struct EventsArgs {
    /// How often to request a batch of events.
    #[arg(long, default_value = "20s", env = "GORGOND_EVENT_BATCH_INTERVAL")]
    event_batch_interval: Duration,

    /// How many messages to acknowledge concurrently.
    #[arg(long, default_value = "4", env = "GORGOND_EVENT_ACK_CONCURRENCY")]
    event_ack_concurrency: usize,
}

#[derive(Debug, Clone, clap::Parser)]
#[command(next_help_heading = "NATS Consumer")]
pub struct NatsArgs {
    /// NATS server to connect to.
    #[arg(
        long,
        short = 'N',
        default_value = "nats://localhost:4222",
        env = "NATS_URL"
    )]
    nats_url: ServerAddr,

    /// Name of the events stream. Created if not present.
    #[arg(
        long,
        default_value = "gorgon-events",
        env = "GORGOND_NATS_EVENTS_STREAM"
    )]
    nats_events_stream: String,
    /// Subjects the events stream consumes, set during creation.
    ///
    /// Has no effect if changed after the stream is already created.
    #[arg(
        long,
        default_values = ["gorgon.events", "gorgon.events.>"],
        env = "GORGOND_NATS_EVENTS_SUBJECTS",
    )]
    nats_events_subjects: Vec<String>,
    /// Name of the durable events consumer.
    #[arg(long, default_value = "gorgond", env = "GORGOND_NATS_EVENTS_CONSUMER")]
    nats_events_consumer: String,
    /// Maximum number of events to be consumed in one batch.
    #[arg(long, default_value = "50", env = "GORGOND_NATS_EVENTS_MAX_MESSAGES")]
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
    let client = async_nats::connect(args.nats_url.clone())
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

    token
        .run_until_cancelled_owned(async move {
            // Tick every <interval>. If a tick is missed, tick immediately and maintain
            // time between ticks rather than doing intervals since the first tick.
            let mut ticker = tokio::time::interval(*args.events.event_batch_interval);
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

            let mut last_tick = tokio::time::Instant::now();
            loop {
                let tick = ticker.tick().await;
                let delay = Duration::from(tick.duration_since(last_tick));
                debug!(%delay, "-> Requesting batch...");
                last_tick = tick;

                // Request a batch of NATS messages.
                let got_batch = (async || -> Result<bool> {
                    let batch = consumer
                        .fetch()
                        .max_messages(args.nats_events_max_messages)
                        .messages()
                        .await
                        .into_diagnostic()
                        .context("Couldn't request batch")?;
                    let msgs: Vec<_> = batch
                        .map_err(|err| miette::Report::new(BoxedError(err)))
                        .try_collect()
                        .await
                        .context("Couldn't collect messages")?;
                    if !msgs.is_empty() {
                        consume_batch(&pool, msgs).await?;
                        Ok(true)
                    } else {
                        trace!("-> No pending messages");
                        Ok(false)
                    }
                })
                .retry(ExponentialBuilder::default())
                .notify(|err, d| {
                    let d = Duration::from(d);
                    error!("Failed to consume batch - retrying in: {d}\n\n{err:?}");
                })
                .await?;

                // If we got a batch of messages, there's probably an in-progress build
                // producing more. Don't wait before requesting another batch.
                if got_batch {
                    trace!("-> Requesting next batch immediately");
                    ticker.reset_immediately();
                }
            }
        })
        .await
        .unwrap_or(Ok(()))
}

#[derive(Debug, thiserror::Error, miette::Diagnostic)]
#[error(transparent)]
struct BoxedError(Box<dyn StdError + Sync + Send>);

#[instrument(name = "batch", skip_all)]
async fn consume_batch(
    pool: &Pool<AsyncPgConnection>,
    msgs: Vec<jetstream::Message>,
) -> Result<()> {
    #[derive(Debug, Default, AsChangeset)]
    #[diesel(table_name = crate::schema::builds, check_for_backend(diesel::pg::Pg))]
    struct BatchBuildUpdate {
        pub result: Option<models::BuildResult>,
        pub heartbeat_at: Option<DateTime<Utc>>,
        pub started_at: Option<DateTime<Utc>>,
        pub ended_at: Option<DateTime<Utc>>,
    }

    // Parse messages.
    let num_msgs = msgs.len();
    debug!(num_msgs, "Consuming batch...");
    let (mut tasks, mut events, mut logs) = (Vec::new(), Vec::new(), Vec::new());
    let mut builds = BTreeMap::<Uuid, BatchBuildUpdate>::new();
    for msg in msgs.iter() {
        let event: events::BuildEvent = serde_json::from_slice(&msg.payload)
            .into_diagnostic()
            .context("Couldn't parse event")?;
        trace!(?event, "-> Event");

        let ts = event.timestamp;
        let build_update = builds.entry(event.build_id).or_default();
        if build_update.heartbeat_at.is_none_or(|v| v < ts) {
            build_update.heartbeat_at = Some(event.timestamp);
        }
        match event.event {
            events::BuildEventKind::Started => build_update.started_at = Some(ts),
            events::BuildEventKind::Stopped(result) => {
                build_update.ended_at = Some(ts);
                build_update.result = Some(result.into());
            }
            _ => {}
        }
        match models::BuildEvent::from_client(event) {
            Either::Left((event, task)) => {
                events.push(event);
                tasks.extend(task);
            }
            Either::Right(log) => logs.push(log),
        }
    }

    // Shove events into the database.
    let (num_builds, num_tasks, num_events, num_logs) =
        (builds.len(), tasks.len(), events.len(), logs.len());
    debug!(
        num_builds,
        num_tasks, num_events, num_logs, "-> Inserting events..."
    );
    pool.get()
        .await
        .into_diagnostic()
        .context("Couldn't get database connection")?
        .transaction::<_, diesel::result::Error, _>(move |conn| {
            async move {
                if !tasks.is_empty() {
                    diesel::insert_into(schema::build_tasks::table)
                        .values(&tasks)
                        .on_conflict(schema::build_tasks::id)
                        .do_nothing()
                        .execute(conn)
                        .await?;
                }
                if !events.is_empty() {
                    diesel::insert_into(schema::build_events::table)
                        .values(&events)
                        .on_conflict(schema::build_events::id)
                        .do_nothing()
                        .execute(conn)
                        .await?;
                }
                if !logs.is_empty() {
                    diesel::insert_into(schema::build_logs::table)
                        .values(&logs)
                        .on_conflict(schema::build_logs::id)
                        .do_nothing()
                        .execute(conn)
                        .await?;
                }
                for (build_id, update) in builds {
                    diesel::update(schema::builds::table)
                        .filter(schema::builds::id.eq(build_id))
                        .set(&update)
                        .execute(conn)
                        .await?;
                }
                Ok(())
            }
            .scope_boxed()
        })
        .await
        .into_diagnostic()
        .context("Couldn't insert events!")?;

    // Acknowledge messages, or they'll be redelivered.
    debug!(num_msgs, "-> Acknowledging messages...");
    futures::stream::iter(msgs.into_iter().map(Ok))
        .try_for_each_concurrent(4, async |msg| {
            msg.ack()
                .await
                .map_err(BoxedError)
                .with_context(|| format!("Couldn't acknowledge message: {msg:?}"))
        })
        .await?;

    info!(
        num_msgs,
        num_tasks, num_events, num_logs, "Processed Events"
    );
    Ok(())
}
