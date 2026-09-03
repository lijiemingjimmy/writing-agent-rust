use std::{collections::VecDeque, convert::Infallible, time::Duration};

use axum::{
    Json, Router,
    extract::{Path, Query, State, rejection::JsonRejection},
    http::{HeaderMap, StatusCode},
    response::{
        Sse,
        sse::{Event, KeepAlive},
    },
    routing::{get, post},
};
use futures_util::stream::{self, Stream};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::broadcast;

use crate::{
    AppError, AppState,
    agent::{RunSubscription, SessionPreparation, UserTurn},
    api::{
        ApiError,
        dto::{ChatResponse, CreateRunResponse, RunRequest, RunResponse},
        parse_json, parse_optional_json,
    },
    domain::{RunEvent, RunId, RunStatus, SessionId},
    store::sessions::MessageRepository,
};

const MAX_STEPS: u32 = 32;
const MAX_CURSOR: u64 = i64::MAX as u64;

#[derive(Default, Deserialize)]
struct EventsQuery {
    after_seq: Option<String>,
}

#[derive(Default, Deserialize)]
struct CancelRequest {
    reason: Option<String>,
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/", post(create_run))
        .route("/{id}", get(get_run))
        .route("/{id}/events", get(run_events))
        .route("/{id}/cancel", post(cancel_run))
}

async fn create_run(
    State(state): State<AppState>,
    payload: Result<Json<RunRequest>, JsonRejection>,
) -> Result<(StatusCode, Json<CreateRunResponse>), ApiError> {
    let request = parse_json(payload)?;
    let handle = start_run(&state, request).await?;
    Ok((
        StatusCode::ACCEPTED,
        Json(CreateRunResponse {
            run_id: handle.run_id.to_legacy_hex(),
            session_id: handle.session_id.to_legacy_hex(),
        }),
    ))
}

async fn get_run(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<RunResponse>, ApiError> {
    let run_id = parse_run_id(&id)?;
    Ok(Json(state.run_engine.get(run_id).await?.into()))
}

async fn cancel_run(
    State(state): State<AppState>,
    Path(id): Path<String>,
    payload: Result<Option<Json<CancelRequest>>, JsonRejection>,
) -> Result<Json<RunResponse>, ApiError> {
    let run_id = parse_run_id(&id)?;
    let reason = parse_optional_json(payload)?
        .and_then(|request| request.reason)
        .unwrap_or_else(|| "user_requested".to_owned());
    state.run_engine.cancel(run_id, &reason).await?;
    Ok(Json(state.run_engine.get(run_id).await?.into()))
}

async fn run_events(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(query): Query<EventsQuery>,
    headers: HeaderMap,
) -> Result<Sse<impl Stream<Item = Result<Event, Infallible>>>, ApiError> {
    let run_id = parse_run_id(&id)?;
    let after_query = query.after_seq.as_deref().and_then(parse_cursor);
    let after_header = headers
        .get("last-event-id")
        .and_then(|value| value.to_str().ok())
        .and_then(parse_cursor);
    let after_seq = after_query
        .into_iter()
        .chain(after_header)
        .max()
        .unwrap_or(0);
    let subscription = state.run_engine.subscribe(run_id, after_seq).await?;
    let stream = event_stream(state, run_id, after_seq, subscription);
    Ok(Sse::new(stream).keep_alive(
        KeepAlive::new()
            .interval(Duration::from_secs(15))
            .text("heartbeat"),
    ))
}

pub(crate) async fn chat(
    State(state): State<AppState>,
    payload: Result<Json<RunRequest>, JsonRejection>,
) -> Result<Json<ChatResponse>, ApiError> {
    let request = parse_json(payload)?;
    let handle = start_run(&state, request).await?;
    wait_terminal(&state, handle.run_id).await?;
    let run = state.run_engine.get(handle.run_id).await?;
    match run.status {
        RunStatus::Completed => {}
        RunStatus::Cancelled => return Err(ApiError::conflict("agent run was cancelled")),
        RunStatus::BudgetExceeded => {
            return Err(ApiError::conflict("agent run exceeded its budget"));
        }
        RunStatus::Failed => return Err(ApiError::bad_gateway("agent execution failed")),
        RunStatus::Queued | RunStatus::Running => {
            return Err(ApiError::bad_gateway("agent run did not terminate"));
        }
    }

    let run_id_hex = handle.run_id.to_legacy_hex();
    let message = MessageRepository::new(state.pool)
        .list_by_session(handle.session_id)
        .await?
        .into_iter()
        .rev()
        .find(|message| {
            message.role == "assistant"
                && message.metadata_json.get("run_id").and_then(Value::as_str)
                    == Some(run_id_hex.as_str())
        })
        .ok_or_else(|| AppError::NotFound("assistant message".to_owned()))?;
    let mut metadata = message
        .metadata_json
        .as_object()
        .cloned()
        .unwrap_or_default();
    metadata.insert("run_id".to_owned(), Value::String(run_id_hex));
    let current_skill = metadata
        .get("selected_skill")
        .and_then(Value::as_str)
        .or_else(|| metadata.get("skill_id").and_then(Value::as_str))
        .map(str::to_owned);
    let awaiting_slots = metadata
        .get("awaiting_slots")
        .or_else(|| {
            metadata
                .get("student_progress")
                .and_then(|progress| progress.get("pending_questions"))
        })
        .or_else(|| {
            metadata
                .get("student_progress")
                .and_then(|progress| progress.get("awaiting_slots"))
        })
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::to_owned)
        .collect();
    Ok(Json(ChatResponse {
        session_id: handle.session_id.to_legacy_hex(),
        reply: message.content,
        current_skill,
        awaiting_slots,
        metadata: Value::Object(metadata),
    }))
}

async fn start_run(
    state: &AppState,
    request: RunRequest,
) -> Result<crate::agent::RunHandle, ApiError> {
    let (session_id, create_session) = match request.session_id.as_deref() {
        Some(value) => {
            let id = SessionId::parse_legacy(value).map_err(|_| ApiError::invalid_identifier())?;
            (id, false)
        }
        None => (SessionId::new(), true),
    };
    let user_id = clean(request.student_id.as_deref())
        .or_else(|| clean(request.user_id.as_deref()))
        .map(str::to_owned);
    let student_name = clean(request.student_name.as_deref()).map(str::to_owned);
    let mut turn = UserTurn::new(session_id, request.message)
        .with_limits(MAX_STEPS, None, None)
        .with_web_search(request.enable_web_search);
    if let Some(action) = request.action {
        turn = turn.with_action(action.as_str());
    }
    Ok(state
        .run_engine
        .start_prepared(
            turn,
            SessionPreparation {
                create_session,
                user_id,
                student_name,
            },
        )
        .await?)
}

fn clean(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|value| !value.is_empty())
}

async fn wait_terminal(state: &AppState, run_id: RunId) -> Result<(), AppError> {
    let mut subscription = state.run_engine.subscribe(run_id, 0).await?;
    let mut last_seq = 0;
    for event in &subscription.replay {
        last_seq = last_seq.max(event.seq);
        if event.is_terminal() {
            return Ok(());
        }
    }
    loop {
        match subscription.recv().await {
            Ok(event) => {
                if event.seq <= last_seq {
                    continue;
                }
                last_seq = event.seq;
                if event.is_terminal() {
                    return Ok(());
                }
            }
            Err(broadcast::error::RecvError::Lagged(_)) => {
                for event in state.run_engine.events(run_id, last_seq).await? {
                    last_seq = event.seq;
                    if event.is_terminal() {
                        return Ok(());
                    }
                }
            }
            Err(broadcast::error::RecvError::Closed) => {
                for event in state.run_engine.events(run_id, last_seq).await? {
                    last_seq = event.seq;
                    if event.is_terminal() {
                        return Ok(());
                    }
                }
                if state.run_engine.get(run_id).await?.status.is_terminal() {
                    return Ok(());
                }
                subscription = state.run_engine.subscribe(run_id, last_seq).await?;
            }
        }
    }
}

struct EventStreamState {
    app: AppState,
    run_id: RunId,
    replay: VecDeque<RunEvent>,
    subscription: RunSubscription,
    last_seq: u64,
    finished: bool,
}

fn event_stream(
    app: AppState,
    run_id: RunId,
    after_seq: u64,
    subscription: RunSubscription,
) -> impl Stream<Item = Result<Event, Infallible>> {
    let replay = subscription.replay.iter().cloned().collect();
    stream::unfold(
        EventStreamState {
            app,
            run_id,
            replay,
            subscription,
            last_seq: after_seq,
            finished: false,
        },
        |mut state| async move {
            loop {
                if state.finished {
                    return None;
                }
                if let Some(event) = state.replay.pop_front() {
                    if event.seq <= state.last_seq {
                        continue;
                    }
                    state.last_seq = event.seq;
                    state.finished = event.is_terminal();
                    return Some((Ok(sse_event(&event)), state));
                }
                match state.subscription.recv().await {
                    Ok(event) => {
                        if event.seq <= state.last_seq {
                            continue;
                        }
                        state.last_seq = event.seq;
                        state.finished = event.is_terminal();
                        return Some((Ok(sse_event(&event)), state));
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => {
                        let Ok(missing) = state
                            .app
                            .run_engine
                            .events(state.run_id, state.last_seq)
                            .await
                        else {
                            return None;
                        };
                        state.replay = missing.into();
                    }
                    Err(broadcast::error::RecvError::Closed) => {
                        let Ok(missing) = state
                            .app
                            .run_engine
                            .events(state.run_id, state.last_seq)
                            .await
                        else {
                            return None;
                        };
                        if !missing.is_empty() {
                            state.replay = missing.into();
                            continue;
                        }
                        match state.app.run_engine.get(state.run_id).await {
                            Ok(run) if run.status.is_terminal() => return None,
                            Ok(_) => match state
                                .app
                                .run_engine
                                .subscribe(state.run_id, state.last_seq)
                                .await
                            {
                                Ok(subscription) => {
                                    state.replay = subscription.replay.iter().cloned().collect();
                                    state.subscription = subscription;
                                }
                                Err(_) => return None,
                            },
                            Err(_) => return None,
                        }
                    }
                }
            }
        },
    )
}

fn sse_event(event: &RunEvent) -> Event {
    let data = json!({
        "run_id": event.run_id.to_legacy_hex(),
        "seq": event.seq,
        "kind": event.kind,
        "payload": event.payload,
        "created_at": event.created_at,
    });
    Event::default()
        .id(event.seq.to_string())
        .event(event.kind.clone())
        .data(serde_json::to_string(&data).unwrap_or_else(|_| "{}".to_owned()))
}

fn parse_run_id(value: &str) -> Result<RunId, ApiError> {
    RunId::parse_legacy(value).map_err(|_| ApiError::invalid_identifier())
}

fn parse_cursor(value: &str) -> Option<u64> {
    (!value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit()))
        .then(|| value.parse::<u64>().ok())
        .flatten()
        .filter(|value| *value <= MAX_CURSOR)
}
