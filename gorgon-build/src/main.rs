// SPDX-FileCopyrightText: 2025 embr <git@liclac.eu>
//
// SPDX-License-Identifier: EUPL-1.2

// Clippy really doesn't like our Error enum, and puts a warning on *everything*
// returning one if it's not silenced like this.
#![allow(clippy::result_large_err)]

use std::{cell::RefCell, process::Stdio};

use async_nats::{jetstream, ServerAddr, Subject};
use backon::{BlockingRetryable, ExponentialBuilder, Retryable};
use bytes::Bytes;
use clap::Parser;
use gorgon_build_helper::{
    event_handler, helper::HelperCaller, set_event_handler, Event, EventHandler,
};
use gorgond_client::{
    api,
    events::{BuildEvent, BuildEventKind},
    models::{Build, BuildResult},
    Client, Uuid, Yoke,
};
use log::{error, info};
use miette::{miette, Context, IntoDiagnostic, Result};
use thingbuf::{mpsc, recycling::WithCapacity};
use tokio_util::sync::CancellationToken;

#[derive(Debug, Clone, clap::Parser)]
#[command(next_help_heading = "NATS Producer")]
pub struct NatsArgs {
    /// NATS server to connect to.
    #[arg(
        long,
        short = 'N',
        env = "GORGON_NATS_ADDR",
        default_value = "localhost:4222"
    )]
    nats_addr: ServerAddr,

    /// NATS subject to publish events to. '*' is replaced with -i/--id.
    #[arg(
        long,
        env = "GORGON_NATS_SUBJECT",
        default_value = "gorgon.events.build.*"
    )]
    nats_events_subject: String,
}

#[derive(Debug, Clone, Parser)]
pub struct Args {
    /// Base URL for the Gorgon API to talk to.
    #[arg(
        long,
        short = 'B',
        env = "GORGON_BASE_URL",
        default_value = "http://localhost:9999/"
    )]
    base_url: reqwest::Url,

    /// Arbitrary ID of this builder.
    #[arg(short, long, env = "GORGON_BUILD_ID")]
    id: String,

    /// Number of buffers to allocate for in-flight events.
    #[arg(long, env = "GORGON_BUILD_EVENT_BUFFERS", default_value = "50")]
    event_buffers: usize,

    #[command(flatten)]
    nats: NatsArgs,

    #[command(flatten)]
    helpers: gorgon_util_cli::HelperArgs,
    #[command(flatten)]
    common: gorgon_util_cli::Args,
}

async fn run_nats_publisher(
    token: CancellationToken,
    jetstream: jetstream::Context,
    subject: Subject,
    event_rx: mpsc::Receiver<Vec<u8>, WithCapacity>,
) -> Result<()> {
    while let Some(Some(buf)) = token.run_until_cancelled(event_rx.recv_ref()).await {
        let bytes = Bytes::copy_from_slice(&buf);
        (async || -> Result<()> {
            jetstream
                .publish(subject.clone(), bytes.clone())
                .await
                .into_diagnostic()
                .context("Couldn't publish message")?
                .await
                .into_diagnostic()
                .context("Couldn't confirm message acknowledgement")?;
            Ok(())
        })
        .retry(ExponentialBuilder::new())
        .notify(|err, d| {
            let d = humantime::Duration::from(d);
            error!("Couldn't publish event, retrying in {d}...\n\n{err:?}");
        })
        .await
        .unwrap();
    }
    Ok(())
}

struct NatsEventHandler {
    next: &'static dyn EventHandler,
    build_id: Uuid,
    sender: RefCell<Option<mpsc::Sender<Vec<u8>, WithCapacity>>>,
}
impl NatsEventHandler {
    fn stop(&self) {
        self.sender.replace(None);
    }
}
impl EventHandler for NatsEventHandler {
    fn handle(&self, target: &str, mut event: Event) {
        if let Event::Build(event) = &mut event {
            event.build_id = self.build_id;

            let sender = self.sender.borrow();
            let sender = sender
                .as_ref()
                .expect("Event received after sender was dropped");
            (|| -> Result<()> {
                let mut buf = sender.try_send_ref().into_diagnostic()?;
                serde_json::to_writer_pretty(&mut *buf, &event).into_diagnostic()?;
                Ok(())
            })
            .retry(ExponentialBuilder::new())
            .notify(|err, d| {
                let d = humantime::Duration::from(d);
                error!("Couldn't enqueue event, retrying in {d}...\n\n{err:?}")
            })
            .call()
            .unwrap();
        }
        self.next.handle(target, event);
    }
}

fn init_client(args: &Args) -> Result<Client> {
    Ok(Client::new(args.base_url.clone())?)
}

async fn request_build(args: &Args, client: &Client) -> Result<Yoke<Build<'static>>> {
    info!("Requesting something to build...");
    Ok(client
        .workers()
        .slug(&args.id)
        .internal()
        .allocate_build(api::WorkerInternalAllocateBuildRequest {})
        .await?
        .map_project(|rsp, _| rsp.build))
}

pub fn run(args: Args) -> miette::Result<()> {
    let (event_tx, event_rx) =
        mpsc::with_recycle::<Vec<u8>, WithCapacity>(args.event_buffers, WithCapacity::default());

    // We need to make some async API calls, but most of what this program does is very
    // sync-shaped. So instead of this being an async program that spends most of its
    // runtime doing `block_in_place` or `spawn_blocking`, just make a runtime on the
    // current thread and `block_on` the occasional async block.
    let main_rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .into_diagnostic()
        .context("Couldn't create async runtime")?;

    // Connect to NATS first.
    info!("Connecting to NATS...");
    let nats = main_rt
        .block_on(async_nats::connect(&args.nats.nats_addr))
        .into_diagnostic()
        .context("Couldn't connect to NATS")?;
    let jetstream = jetstream::new(nats);

    let subject = Subject::from(args.nats.nats_events_subject.clone().replace("*", &args.id));
    let stream = main_rt
        .block_on(jetstream.stream_by_subject(&*subject))
        .into_diagnostic()
        .with_context(|| format!("Couldn't look up NATS Stream for subject: {subject}"))?;
    info!(subject=subject.as_str(), stream; "-> Publishing to NATS Stream");

    let token = CancellationToken::new();
    let join_nats = main_rt.spawn(run_nats_publisher(
        token.child_token(),
        jetstream,
        subject,
        event_rx,
    ));

    // Ask gorgond for something to do.
    let client = init_client(&args).context("Couldn't build gorgond client")?;
    let build_ = main_rt
        .block_on(request_build(&args, &client))
        .context("Couldn't get something to build; lost and without purpose")?;
    let build = build_.get();
    info!(id:% = build.id, __log__=build_.backing_cart(); " -> Got a task!");

    // Find builder.
    let caller = args.helpers.caller()?;
    let builder = caller.helper(format!("gorgon-build-{}", build.spec.kind))?;
    let mut build_task = builder
        .with(&args.helpers)
        .with_arg("-")
        .with_stdin(Stdio::piped())
        .into_task(None);

    // Before we start the build, we need to install an event handler.
    // SAFETY: This is only safe to do before any events start being emitted.
    let events_handler = Box::leak(Box::new(NatsEventHandler {
        next: event_handler(),
        build_id: build.id,
        sender: RefCell::new(Some(event_tx)),
    }));
    unsafe { set_event_handler(events_handler) };

    // Start the builder process...
    info!("Starting build...");
    events_handler.handle(
        "gorgon-build",
        Event::Build(BuildEvent::from(BuildEventKind::Started)),
    );
    let mut build_child = build_task.start().context("Couldn't start build")?;

    // ...feed it the build context over stdin...
    serde_json::to_writer_pretty(
        build_child
            .child
            .stdin
            .as_mut()
            .ok_or(miette!("Builder process has no stdin!?"))?,
        &build,
    )
    .into_diagnostic()
    .context("Couldn't write Build to builder's stdin")?;

    // ...and wait for it to finish - note that the build failing does not mean we fail.
    let result = match build_child.wait_checked().context("Build Failed") {
        Err(err) => {
            error!(__log__:debug=err; "Build Failed");
            BuildResult::Failure
        }
        Ok(_) => {
            info!("Build Succeeded!");
            BuildResult::Success
        }
    };
    events_handler.handle(
        "gorgon-build",
        Event::Build(BuildEvent::from(BuildEventKind::Stopped(result))),
    );

    // Shut down the events handler, which will cause the publisher to shut down once all
    // outstanding events have been processed. The handler will panic if any more events
    // are received after this call!
    events_handler.stop();

    info!("Waiting for outstanding events to be delivered...");
    main_rt
        .block_on(join_nats)
        .into_diagnostic()
        .context("NATS Publisher Panicked!")?
        .context("NATS Publisher Error")?;

    info!("All done - goodbye!");
    Ok(())
}

fn main() {
    let args = Args::parse();
    gorgon_util_cli::logger(args.common.log_format, args.common.level()).init();
    gorgon_util_cli::main(args.common.output_format, || run(args));
}
