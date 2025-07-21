// SPDX-FileCopyrightText: 2025 embr <git@liclac.eu>
//
// SPDX-License-Identifier: EUPL-1.2

use chrono::prelude::*;
use diesel::{prelude::*, upsert::excluded};
use diesel_async::{AsyncConnection, RunQueryDsl};
use gorgond_client::Uuid;
use gorgond_diesel::AsyncPgPool;
use miette::{Context, IntoDiagnostic, NamedSource, Result, SourceOffset, SourceSpan, bail};
use octocrab::models::events::{self, payload::EventPayload};
use scoped_futures::ScopedFutureExt;
use tokio::runtime::Runtime;
use tracing::{Span, debug, field::Empty, info, instrument};

use crate::{TransactionError, models, schema::gorgond_github as schema};

macro_rules! excluded {
    ($($col:expr),+$(,)?) => {
        ($($col.eq(excluded($col))),+)
    };
}

#[derive(Debug, thiserror::Error, miette::Diagnostic)]
pub enum EventError {
    #[error("Invalid JSON")]
    #[diagnostic(help("This command expects a stream of JSON events on stdin."))]
    InvalidJson(#[from] serde_json::Error),

    #[error("Invalid Event")]
    FromValue(
        #[label("{2}")] SourceOffset,
        #[source_code] NamedSource<String>,
        #[source] serde_json::Error,
    ),

    #[error("Couldn't ingest event")]
    #[diagnostic(forward(2))]
    Ingest(
        #[label("{2}")] SourceSpan,
        #[source_code] NamedSource<String>,
        #[source] Box<dyn miette::Diagnostic + Send + Sync + 'static>,
    ),
}

#[derive(Debug, Clone, clap::Parser)]
#[group(id = "ingest")]
pub struct Args {}
impl Args {
    #[instrument(name = "ingest", skip_all)]
    pub fn run(self, pool: AsyncPgPool, rt: Runtime) -> Result<()> {
        let mut it =
            serde_json::Deserializer::from_reader(std::io::BufReader::new(std::io::stdin().lock()))
                .into_iter::<serde_json::Value>()
                .map(|r| r.map_err(EventError::InvalidJson));
        let (mut events, mut failed) = (0, 0);
        while let Some(value) = it.next().transpose()? {
            events += 1;
            let event = serde_json::value::from_value(value.clone()).map_err(|err| {
                let src = serde_json::to_string_pretty(&value).unwrap_or_default();
                EventError::FromValue(
                    SourceOffset::from_location(&src, err.line(), err.column()),
                    NamedSource::new("event.json", src).with_language("JSON"),
                    err,
                )
            })?;
            if let Err(err) = rt.block_on(self.ingest_event(event, &pool)) {
                failed += 1;
                let src = serde_json::to_string_pretty(&value).unwrap_or_default();
                let err = EventError::Ingest(
                    SourceSpan::new(0.into(), src.len()),
                    NamedSource::new("event.json", src),
                    err.into(),
                );
                eprintln!("{:?}", miette::Report::new(err));
            };
        }
        if failed > 0 {
            bail!("Ingesting {failed}/{events} events failed!");
        }
        Ok(())
    }

    #[instrument(name = "event", skip_all, fields(
        event_id = Empty, kind = Empty, action = Empty,
        repo_id = Empty, actor_id = Empty, org_id = Empty, push_id = Empty, pr_id = Empty,
    ))]
    async fn ingest_event(&self, e: events::Event, pool: &AsyncPgPool) -> Result<()> {
        if let Err(kind) = models::EventKind::try_from(e.r#type.clone()) {
            debug!(?kind, "Skipping event");
            return Ok(());
        }

        let span = Span::current();
        let (now, current_at) = (Utc::now(), e.created_at);

        let event = models::Event::try_from_event(now, Uuid::now_v7(), &e).context("Event")?;
        let repo = models::Repo::try_from_event(now, current_at, &e.repo).context("Repo")?;
        let actor = models::Actor::try_from_event(now, current_at, &e.actor).context("Actor")?;
        span.record("event_id", event.0.id)
            .record("repo_id", repo.id)
            .record("actor_id", actor.id)
            .record("kind", event.0.kind.as_ref());

        let org = (e.org.as_ref())
            .map(|o| models::Org::try_from_event(now, current_at, o))
            .transpose()
            .context("Org")?
            .inspect(|org| {
                span.record("org_id", org.id);
            });

        let pr = match e.payload.as_ref().and_then(|p| p.specific.as_ref()) {
            Some(EventPayload::PullRequestEvent(p)) => models::PullRequest::try_from_event(
                now,
                current_at,
                event.0.repo_id,
                Uuid::now_v7(),
                &p.pull_request,
            )
            .inspect(|(pr, _, _)| {
                span.record("pr_id", pr.id)
                    .record("action", format!("{:?}", p.action));
            })
            .map(Some)
            .context("Pull Request")?,
            _ => None,
        };

        pool.get()
            .await
            .into_diagnostic()?
            .transaction(|conn| {
                async move {
                    diesel::insert_into(schema::repos::table)
                        .values(&repo)
                        .on_conflict(schema::repos::id)
                        .do_update()
                        .set(excluded!(
                            schema::repos::name,
                            schema::repos::current_at,
                            schema::repos::updated_at,
                        ))
                        .execute(conn)
                        .await
                        .into_diagnostic()
                        .context("Couldn't insert Repo")?;
                    diesel::insert_into(schema::actors::table)
                        .values(&actor)
                        .on_conflict(schema::actors::id)
                        .do_update()
                        .set(excluded!(
                            schema::actors::login,
                            schema::actors::display_login,
                            schema::actors::avatar_url,
                            schema::actors::current_at,
                            schema::actors::updated_at,
                        ))
                        .execute(conn)
                        .await
                        .into_diagnostic()
                        .context("Couldn't insert Actor")?;
                    if let Some(org) = org {
                        diesel::insert_into(schema::orgs::table)
                            .values(&org)
                            .on_conflict(schema::orgs::id)
                            .do_update()
                            .set(excluded!(
                                schema::orgs::login,
                                schema::orgs::avatar_url,
                                schema::orgs::current_at,
                                schema::orgs::updated_at,
                            ))
                            .execute(conn)
                            .await
                            .into_diagnostic()
                            .context("Couldn't insert Org")?;
                    }
                    if let Some((mut pr, pr_repo, pr_ref)) = pr {
                        diesel::insert_into(schema::repos::table)
                            .values(&pr_repo)
                            .on_conflict(schema::repos::id)
                            .do_update()
                            .set(excluded!(
                                schema::repos::name,
                                schema::repos::current_at,
                                schema::repos::updated_at,
                            ))
                            .execute(conn)
                            .await
                            .into_diagnostic()
                            .context("Couldn't insert Repo")?;

                        // TODO: ON CONFLICT DO NOTHING ... RETURNING returns nothing if
                        // the row already exists, without some silly hacks. We can force
                        // something to be returned by making a pointless no-op write on
                        // every insert, but this is not ideal.
                        pr.ref_id = diesel::insert_into(schema::refs::table)
                            .values(&pr_ref)
                            .on_conflict((schema::refs::repo_id, schema::refs::ref_name))
                            .do_update()
                            .set(excluded!(schema::refs::repo_id, schema::refs::ref_name))
                            .returning(schema::refs::id)
                            .get_result::<Uuid>(conn)
                            .await
                            .into_diagnostic()
                            .context("Couldn't insert Pull Request Ref")?;

                        diesel::insert_into(schema::pull_requests::table)
                            .values(&pr)
                            .on_conflict(schema::pull_requests::id)
                            .do_update()
                            .set(excluded!(
                                schema::pull_requests::repo_id,
                                schema::pull_requests::number,
                                schema::pull_requests::ref_id,
                                schema::pull_requests::merge_request_id,
                                schema::pull_requests::title,
                                schema::pull_requests::body,
                                schema::pull_requests::current_at,
                                schema::pull_requests::updated_at,
                            ))
                            .execute(conn)
                            .await
                            .into_diagnostic()
                            .context("Couldn't insert Pull Request")?;
                    }
                    // TODO: Same problem with pointless writes here.
                    let (mut event, event_ref) = event;
                    if let Some(event_ref) = event_ref {
                        event.ref_id = diesel::insert_into(schema::refs::table)
                            .values(&event_ref)
                            .on_conflict((schema::refs::repo_id, schema::refs::ref_name))
                            .do_update()
                            .set(excluded!(schema::refs::repo_id, schema::refs::ref_name))
                            .returning(schema::refs::id)
                            .get_result::<Uuid>(conn)
                            .await
                            .map(Some)
                            .into_diagnostic()
                            .context("Couldn't insert Event Ref")?;
                    }
                    diesel::insert_into(schema::events::table)
                        .values(&event)
                        .on_conflict(schema::events::id)
                        .do_update()
                        .set(excluded!(
                            schema::events::repo_id,
                            schema::events::actor_id,
                            schema::events::org_id,
                            schema::events::kind,
                            schema::events::public,
                            schema::events::payload,
                            schema::events::emitted_at,
                            schema::events::updated_at,
                            schema::events::pull_request_id,
                            schema::events::pull_request_action,
                            schema::events::ref_id,
                            schema::events::head,
                        ))
                        .execute(conn)
                        .await
                        .into_diagnostic()
                        .context("Couldn't insert Event")?;
                    Ok::<_, TransactionError>(())
                }
                .scope_boxed()
            })
            .await
            .into_diagnostic()?;

        info!("-> Ingested");
        Ok(())
    }
}
