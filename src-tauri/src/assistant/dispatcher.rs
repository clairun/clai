//! The one path that turns pending session signals into a run: every send,
//! task assignment, task result and run end calls [`kick`]. Stateless; the
//! durable state is `session_signals` and
//! `idx_assistant_runs_one_active_per_session`, which arbitrates concurrent
//! kicks. In a task session every claimed run is a new task cycle.

use tauri::Manager;

use crate::assistant::engine::{self, AssistantDeps, RunTurnInput};
use crate::assistant::events::AssistantUiEvent;
use crate::assistant::repository::{self, CreateRunParams};
use crate::assistant::runtime;
use crate::assistant::signals;
use crate::assistant::tools::workspace_tasks;
use crate::assistant::types::{
    AssistantRun, MessageId, ProviderConnection, RunId, RunStatus, RunTrigger,
};
use crate::assistant::workspace_tasks_repo::{self, WorkspaceTaskRow};
use crate::db::DbPool;
use crate::AppState;

#[derive(Debug)]
pub enum Kick {
    /// Nothing pending.
    Idle,
    /// A run is active; pending signals wait for its end.
    Busy { run_id: RunId },
    Started {
        run: AssistantRun,
        message_ids: Vec<MessageId>,
        /// The task whose new cycle this run is, in a task session.
        task: Option<Box<WorkspaceTaskRow>>,
    },
    /// Signals are pending but their provider connection no longer exists.
    Deferred,
}

/// Starts a run for the session's pending signals when it has no active run.
pub async fn kick(deps: &AssistantDeps, session_id: &str) -> Result<Kick, String> {
    let session = repository::get_session(&deps.pool, session_id)
        .await?
        .ok_or_else(|| format!("Assistant session not found: {}", session_id))?;
    let app_state = deps.app.state::<AppState>();
    let outcome = claim(&deps.pool, session_id, |connection_id| {
        app_state
            .config_manager
            .lock()
            .ok()?
            .get_provider_connection(connection_id)
    })
    .await?;

    if let Kick::Started {
        run,
        message_ids,
        task,
    } = &outcome
    {
        let _ = deps.emit(
            &session,
            Some(&run.id),
            AssistantUiEvent::QueuedMessagesDelivered {
                message_ids: message_ids.clone(),
            },
        );
        let _ = deps.emit(
            &session,
            Some(&run.id),
            AssistantUiEvent::RunQueued { run: run.clone() },
        );
        let task_id = task.as_ref().map(|task| task.id.clone());
        if let Some(task) = task {
            workspace_tasks::emit_task_updated(deps, WorkspaceTaskRow::clone(task)).await;
        }
        spawn_run(deps.clone(), run, task_id);
    }
    Ok(outcome)
}

/// The database half of [`kick`]: creates the run and claims the batch in
/// one transaction. The run insert comes first so the transaction takes the
/// write lock before it reads anything it depends on.
pub(crate) async fn claim(
    pool: &DbPool,
    session_id: &str,
    connection: impl Fn(&str) -> Option<ProviderConnection>,
) -> Result<Kick, String> {
    if let Some(run_id) = repository::active_run_id(pool, session_id).await? {
        return Ok(Kick::Busy { run_id });
    }
    let open = signals::list_open_content(pool, session_id).await?;
    if open.is_empty() {
        return Ok(Kick::Idle);
    }
    // Signals without a connection (task events) run on the session's
    // latest connection.
    let connection_id = match open
        .iter()
        .find_map(|content| content.connection_id.clone())
    {
        Some(connection_id) => Some(connection_id),
        None => repository::latest_run_connection_id(pool, session_id).await?,
    };
    let Some(connection) = connection_id.as_deref().and_then(&connection) else {
        tracing::warn!(
            session_id,
            "Pending messages have no usable provider connection; leaving them pending"
        );
        return Ok(Kick::Deferred);
    };

    // A task session's runs keep the worker trigger: it selects the worker
    // prompt.
    let trigger = if workspace_tasks_repo::is_task_session(pool, session_id).await? {
        RunTrigger::WorkspaceTask
    } else {
        RunTrigger::SignalWake
    };

    let mut tx = pool
        .begin()
        .await
        .map_err(|e| format!("Failed to start dispatch transaction: {}", e))?;
    let run = repository::insert_run_if_idle(
        &mut tx,
        CreateRunParams {
            session_id: session_id.to_string(),
            status: RunStatus::Queued,
            trigger,
            connection_id: connection.id.clone(),
            protocol_id: connection.protocol_id.clone(),
            model_id: connection.model_id.clone(),
            error: None,
        },
    )
    .await?;
    let Some(run) = run else {
        drop(tx);
        // Another kick won; if its run already ended, its own kick took over.
        return Ok(match repository::active_run_id(pool, session_id).await? {
            Some(run_id) => Kick::Busy { run_id },
            None => Kick::Idle,
        });
    };
    let message_ids = signals::claim_batch(&mut tx, session_id, &run.id, &connection.id).await?;
    if message_ids.is_empty() {
        // Claimed by a run that started and ended since the read above.
        return Ok(Kick::Idle);
    }
    let task = workspace_tasks_repo::open_cycle(&mut tx, session_id, &run.id).await?;
    tx.commit()
        .await
        .map_err(|e| format!("Failed to commit dispatch transaction: {}", e))?;
    Ok(Kick::Started {
        run,
        message_ids,
        task: task.map(Box::new),
    })
}

fn spawn_run(deps: AssistantDeps, run: &AssistantRun, task_id: Option<String>) {
    let run_registration = runtime::register_run(&run.id);
    let input = RunTurnInput {
        session_id: run.session_id.clone(),
        run_id: Some(run.id.clone()),
        trigger: run.trigger.clone(),
        connection_id: run.connection_id.clone(),
        cancel_token: run_registration.token(),
        // The run's input is its claimed signals (`list_content_for_run`).
        trigger_message_id: None,
    };
    tauri::async_runtime::spawn(async move {
        let _run_registration = run_registration;
        let session_id = input.session_id.clone();
        let run_id = input.run_id.clone().unwrap_or_default();
        if let Some(task_id) = &task_id {
            workspace_tasks::start_task_cycle(&deps, task_id, &run_id).await;
        }
        if let Err(e) = engine::run_session_turn(&deps, input).await {
            tracing::error!("Assistant engine error for run {}: {}", run_id, e);
        }
        if let Some(task_id) = &task_id {
            workspace_tasks::finalize_task_cycle(&deps, task_id, &run_id).await;
        }
        kick_logged(&deps, &session_id).await;
    });
}

/// [`kick`] that logs instead of returning its error. Every run end calls it
/// so input that arrived meanwhile runs next.
pub(crate) async fn kick_logged(deps: &AssistantDeps, session_id: &str) {
    if let Err(error) = kick(deps, session_id).await {
        tracing::error!(
            session_id,
            error,
            "Failed to dispatch pending session signals"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::assistant::types::{ContentPart, SessionContext};
    use crate::db::test_support::workspace_pool;

    fn connection(id: &str) -> ProviderConnection {
        ProviderConnection {
            id: id.to_string(),
            name: id.to_string(),
            protocol_id: "openai".into(),
            provider_id: "openai".into(),
            auth_mode: crate::assistant::types::AuthMode::DeveloperApiKey,
            base_url: None,
            secret_ref: String::new(),
            model_id: "model".into(),
            account_label: None,
            enabled: true,
            created_at: 0,
            updated_at: 0,
        }
    }

    async fn send(pool: &DbPool, session_id: &str, text: &str) -> String {
        repository::create_user_message_with_content(
            pool,
            session_id.to_string(),
            vec![ContentPart::Text { text: text.into() }],
            Some("conn"),
        )
        .await
        .unwrap()
        .id
    }

    async fn active_run(pool: &DbPool, session_id: &str) -> AssistantRun {
        repository::create_run(
            pool,
            CreateRunParams {
                session_id: session_id.to_string(),
                status: RunStatus::Running,
                trigger: RunTrigger::Scheduled,
                connection_id: "conn".into(),
                protocol_id: "openai".into(),
                model_id: "model".into(),
                error: None,
            },
        )
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn send_on_idle_session_starts_run_via_kick() {
        let (_tmp, pool) = workspace_pool().await;
        let session = repository::create_session(&pool, SessionContext::default())
            .await
            .unwrap();
        let message_id = send(&pool, &session.id, "hello").await;

        let Kick::Started {
            run, message_ids, ..
        } = claim(&pool, &session.id, |id| Some(connection(id)))
            .await
            .unwrap()
        else {
            panic!("an idle session with a pending message must start a run");
        };
        assert_eq!(run.trigger, RunTrigger::SignalWake);
        assert_eq!(run.status, RunStatus::Queued);
        assert_eq!(message_ids, vec![message_id.clone()]);
        let input = signals::list_content_for_run(&pool, &session.id, &run.id)
            .await
            .unwrap();
        assert_eq!(input.len(), 1);
        assert_eq!(input[0].message.id, message_id);
        assert!(signals::list_open_message_ids(&pool, &session.id)
            .await
            .unwrap()
            .is_empty());
        assert!(matches!(
            claim(&pool, &session.id, |id| Some(connection(id))).await.unwrap(),
            Kick::Busy { run_id } if run_id == run.id
        ));
    }

    #[tokio::test]
    async fn kick_returns_busy_when_run_active() {
        let (_tmp, pool) = workspace_pool().await;
        let session = repository::create_session(&pool, SessionContext::default())
            .await
            .unwrap();
        let running = active_run(&pool, &session.id).await;
        let message_id = send(&pool, &session.id, "while you work").await;

        let outcome = claim(&pool, &session.id, |id| Some(connection(id)))
            .await
            .unwrap();

        assert!(matches!(outcome, Kick::Busy { run_id } if run_id == running.id));
        assert_eq!(
            signals::list_open_message_ids(&pool, &session.id)
                .await
                .unwrap(),
            vec![message_id]
        );
    }

    #[tokio::test]
    async fn run_end_kicks_pending_user_messages() {
        let (_tmp, pool) = workspace_pool().await;
        let session = repository::create_session(&pool, SessionContext::default())
            .await
            .unwrap();
        let running = active_run(&pool, &session.id).await;
        let first = send(&pool, &session.id, "one").await;
        let second = send(&pool, &session.id, "two").await;
        assert!(matches!(
            claim(&pool, &session.id, |id| Some(connection(id)))
                .await
                .unwrap(),
            Kick::Busy { .. }
        ));

        repository::update_run_status(&pool, &running.id, RunStatus::Completed, None)
            .await
            .unwrap();
        let Kick::Started {
            run,
            mut message_ids,
            ..
        } = claim(&pool, &session.id, |id| Some(connection(id)))
            .await
            .unwrap()
        else {
            panic!("the kick after a run end must start the pending batch");
        };

        message_ids.sort();
        let mut expected = vec![first, second];
        expected.sort();
        assert_eq!(message_ids, expected);
        assert_ne!(run.id, running.id);
    }

    #[tokio::test]
    async fn kick_defers_when_the_connection_is_gone() {
        let (_tmp, pool) = workspace_pool().await;
        let session = repository::create_session(&pool, SessionContext::default())
            .await
            .unwrap();
        send(&pool, &session.id, "hello").await;

        let outcome = claim(&pool, &session.id, |_| None).await.unwrap();

        assert!(matches!(outcome, Kick::Deferred));
        assert!(repository::active_run_id(&pool, &session.id)
            .await
            .unwrap()
            .is_none());
    }

    #[tokio::test]
    async fn kick_claims_once_under_concurrent_sends() {
        let (_tmp, pool) = workspace_pool().await;
        let session = repository::create_session(&pool, SessionContext::default())
            .await
            .unwrap();

        let mut sends = tokio::task::JoinSet::new();
        for index in 0..8 {
            let pool = pool.clone();
            let session_id = session.id.clone();
            sends.spawn(async move {
                send(&pool, &session_id, &format!("message {index}")).await;
                claim(&pool, &session_id, |id| Some(connection(id)))
                    .await
                    .unwrap()
            });
        }
        let mut started = Vec::new();
        while let Some(outcome) = sends.join_next().await {
            if let Kick::Started { run, .. } = outcome.unwrap() {
                started.push(run.id);
            }
        }

        assert_eq!(started.len(), 1, "exactly one kick may start a run");
        let runs: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM assistant_runs")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(runs, 1);
        let unaccounted: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM session_signals
             WHERE NOT (status = 'pending' OR (status = 'claimed' AND claimed_run_id = ?))",
        )
        .bind(&started[0])
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(unaccounted, 0);
    }

    #[tokio::test]
    async fn run_rows_carry_run_id_and_composer_row_stamped_at_claim() {
        let (_tmp, pool) = workspace_pool().await;
        let session = repository::create_session(&pool, SessionContext::default())
            .await
            .unwrap();
        let composer = send(&pool, &session.id, "hello").await;
        let run_id_of = |id: String| {
            let pool = pool.clone();
            async move {
                sqlx::query_scalar::<_, Option<String>>(
                    "SELECT run_id FROM assistant_messages WHERE id = ?",
                )
                .bind(id)
                .fetch_one(&pool)
                .await
                .unwrap()
            }
        };
        assert_eq!(run_id_of(composer.clone()).await, None);

        let Kick::Started { run, .. } = claim(&pool, &session.id, |id| Some(connection(id)))
            .await
            .unwrap()
        else {
            panic!("expected a run");
        };
        assert_eq!(run_id_of(composer).await.as_deref(), Some(run.id.as_str()));

        // A row the run writes carries its id; so does input it takes mid-run.
        let reply = repository::create_message(
            &pool,
            repository::CreateMessageParams {
                session_id: session.id.clone(),
                role: crate::assistant::types::MessageRole::Assistant,
                content: vec![ContentPart::Text { text: "hi".into() }],
                provider_metadata: None,
                run_id: Some(run.id.clone()),
            },
        )
        .await
        .unwrap();
        let midrun = send(&pool, &session.id, "one more thing").await;
        signals::consume(&pool, &session.id, &run.id, std::slice::from_ref(&midrun))
            .await
            .unwrap();
        assert_eq!(run_id_of(reply.id).await.as_deref(), Some(run.id.as_str()));
        assert_eq!(run_id_of(midrun).await.as_deref(), Some(run.id.as_str()));
    }
}
