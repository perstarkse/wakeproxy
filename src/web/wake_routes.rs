use std::time::Duration;

use axum::extract::{Form, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use tera::Context;
use tracing::warn;

use super::AppState;
use super::security::{csrf_forbidden_response, sanitize_next};
use super::templates::render_template;
use crate::http::{mark_uncacheable, redirect_response};
use crate::wake::{WakeStartResult, WakeState};

const DEFAULT_KEEP_AWAKE_FOR_SECS: u64 = 900;
const KEEP_AWAKE_OPTION_SECS: [u64; 4] = [900, 1800, 3600, 14_400];

#[derive(serde::Deserialize)]
pub(crate) struct WakeQuery {
    pub(crate) next: Option<String>,
}

#[derive(serde::Deserialize)]
pub(crate) struct WakeStartForm {
    pub(crate) next: Option<String>,
    pub(crate) keep_awake_for_secs: Option<u64>,
}

#[derive(serde::Deserialize)]
pub(crate) struct WakePreviewQuery {
    pub(crate) next: Option<String>,
    pub(crate) state: Option<String>,
    pub(crate) reason: Option<String>,
    pub(crate) cycle_id: Option<u64>,
    pub(crate) auto: Option<String>,
}

#[derive(serde::Serialize)]
pub(crate) struct WakePollPayload {
    state: &'static str,
    cycle_id: Option<u64>,
    ready: bool,
    redirect_to: Option<String>,
    message: &'static str,
    retryable: bool,
    reason: Option<String>,
}

pub(crate) async fn wake_page(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<WakeQuery>,
) -> axum::response::Response {
    let next = sanitize_next(query.next.as_deref()).unwrap_or_else(|| "/".to_string());
    let snapshot = state.wake.status_snapshot().await;

    if snapshot.ready {
        return redirect_response(&next, Some(&headers));
    }

    let context = wake_context(
        &next,
        state.config.wake_poll_interval_ms,
        state.config.keep_awake_command.is_some(),
        state.config.keep_awake_max_secs,
        snapshot.state,
        snapshot.cycle_id,
        snapshot.reason.as_deref().unwrap_or(""),
        false,
        false,
    );

    let mut response = render_template(&state, "wake-status.html", context, StatusCode::OK);
    mark_uncacheable(response.headers_mut());
    response
}

pub(crate) async fn wake_preview_page(
    State(state): State<AppState>,
    Query(query): Query<WakePreviewQuery>,
) -> axum::response::Response {
    #[cfg(not(debug_assertions))]
    {
        let WakePreviewQuery {
            next,
            state: preview_state,
            reason,
            cycle_id,
            auto,
        } = query;
        let _ = (state, next, preview_state, reason, cycle_id, auto);
        return (StatusCode::NOT_FOUND, "Not found").into_response();
    }

    #[cfg(debug_assertions)]
    {
        let next = sanitize_next(query.next.as_deref()).unwrap_or_else(|| "/demo".to_string());
        let preview_state = query
            .state
            .as_deref()
            .and_then(wake_state_from_label)
            .unwrap_or(WakeState::Sleeping);
        let preview_auto = query
            .auto
            .as_deref()
            .map(preview_auto_from_label)
            .unwrap_or(false);
        let reason = query.reason.unwrap_or_else(|| {
            if preview_state == WakeState::Failed {
                "Preview: upstream did not accept connections yet.".to_string()
            } else {
                "".to_string()
            }
        });

        let context = wake_context(
            &next,
            1200,
            true,
            14_400,
            preview_state,
            Some(query.cycle_id.unwrap_or(4242)),
            &reason,
            true,
            preview_auto,
        );

        let mut response = render_template(&state, "wake-status.html", context, StatusCode::OK);
        mark_uncacheable(response.headers_mut());
        response
    }
}

pub(crate) async fn wake_start_submit(
    State(state): State<AppState>,
    headers: HeaderMap,
    Form(form): Form<WakeStartForm>,
) -> axum::response::Response {
    if !super::security::is_same_origin_request(&state.config, &headers, true) {
        warn!("rejected cross-origin wake-start submission");
        return csrf_forbidden_response();
    }

    let next = sanitize_next(form.next.as_deref()).unwrap_or_else(|| "/".to_string());
    let keep_awake_for = form
        .keep_awake_for_secs
        .or(Some(DEFAULT_KEEP_AWAKE_FOR_SECS))
        .filter(|seconds| *seconds > 0)
        .map(Duration::from_secs);

    if let WakeStartResult::AlreadyReady { .. } = state.wake.start_wake(keep_awake_for).await {
        return redirect_response(&next, Some(&headers));
    }

    let encoded_next = urlencoding::encode(&next);
    let target = format!("/_wake?next={encoded_next}");
    redirect_response(&target, Some(&headers))
}

pub(crate) async fn wake_poll(
    State(state): State<AppState>,
    Query(query): Query<WakeQuery>,
) -> axum::response::Response {
    let next = sanitize_next(query.next.as_deref()).unwrap_or_else(|| "/".to_string());
    let snapshot = state.wake.status_snapshot().await;

    let payload = WakePollPayload {
        state: wake_state_label(snapshot.state),
        cycle_id: snapshot.cycle_id,
        ready: snapshot.ready,
        redirect_to: snapshot.ready.then_some(next),
        message: wake_status_message(snapshot.state),
        retryable: wake_retryable(snapshot.state),
        reason: snapshot.reason,
    };

    let mut response = axum::Json(payload).into_response();
    mark_uncacheable(response.headers_mut());
    response
}

fn wake_context(
    next: &str,
    poll_interval_ms: u64,
    keep_awake_enabled: bool,
    keep_awake_max_secs: u64,
    state: WakeState,
    cycle_id: Option<u64>,
    reason: &str,
    preview_mode: bool,
    preview_auto: bool,
) -> Context {
    let mut context = Context::new();
    let keep_awake_default_secs = DEFAULT_KEEP_AWAKE_FOR_SECS.min(keep_awake_max_secs);
    let mut keep_awake_option_secs = KEEP_AWAKE_OPTION_SECS
        .into_iter()
        .filter(|seconds| *seconds <= keep_awake_max_secs)
        .collect::<Vec<_>>();
    if !keep_awake_option_secs.contains(&keep_awake_default_secs) {
        keep_awake_option_secs.push(keep_awake_default_secs);
    }
    if !keep_awake_option_secs.contains(&keep_awake_max_secs) {
        keep_awake_option_secs.push(keep_awake_max_secs);
    }
    keep_awake_option_secs.sort_unstable();

    let keep_awake_options = keep_awake_option_secs
        .into_iter()
        .map(|seconds| KeepAwakeOption {
            seconds,
            label: format_duration_label(seconds),
            selected: seconds == keep_awake_default_secs,
        })
        .collect::<Vec<_>>();
    let keep_awake_default_label = format_duration_label(keep_awake_default_secs);
    let keep_awake_max_label = format_duration_label(keep_awake_max_secs);

    context.insert("next", next);
    context.insert("poll_interval_ms", &poll_interval_ms);
    context.insert("keep_awake_enabled", &keep_awake_enabled);
    context.insert("keep_awake_options", &keep_awake_options);
    context.insert("keep_awake_default_label", &keep_awake_default_label);
    context.insert("keep_awake_max_label", &keep_awake_max_label);
    context.insert("state", wake_state_label(state));
    context.insert("status_message", wake_status_message(state));
    context.insert("retryable", &wake_retryable(state));
    context.insert("cycle_id", &cycle_id.unwrap_or_default());
    context.insert("cycle_known", &cycle_id.is_some());
    context.insert("reason", &reason);
    context.insert("preview_mode", &preview_mode);
    context.insert("preview_auto", &preview_auto);
    context
}

#[derive(serde::Serialize)]
struct KeepAwakeOption {
    seconds: u64,
    label: String,
    selected: bool,
}

fn format_duration_label(seconds: u64) -> String {
    if seconds % 3600 == 0 {
        let hours = seconds / 3600;
        if hours == 1 {
            "1 hour".to_string()
        } else {
            format!("{hours} hours")
        }
    } else if seconds % 60 == 0 {
        let minutes = seconds / 60;
        if minutes == 1 {
            "1 minute".to_string()
        } else {
            format!("{minutes} minutes")
        }
    } else if seconds == 1 {
        "1 second".to_string()
    } else {
        format!("{seconds} seconds")
    }
}

pub(crate) fn wake_state_label(state: WakeState) -> &'static str {
    match state {
        WakeState::Sleeping => "sleeping",
        WakeState::Waking => "waking",
        WakeState::PingReady => "ping_ready",
        WakeState::HttpReady => "http_ready",
        WakeState::Ready => "ready",
        WakeState::TimedOut => "timed_out",
        WakeState::Failed => "failed",
    }
}

pub(crate) fn wake_status_message(state: WakeState) -> &'static str {
    match state {
        WakeState::Sleeping => "Remote system is asleep.",
        WakeState::Waking => "Starting your remote system.",
        WakeState::PingReady => "Host is reachable. Waiting for services.",
        WakeState::HttpReady => "Service checks passed. Finalizing connection.",
        WakeState::Ready => "Ready. Redirecting to your destination.",
        WakeState::TimedOut => "Wake timed out. You can retry.",
        WakeState::Failed => "Wake failed. You can retry.",
    }
}

pub(crate) fn wake_retryable(state: WakeState) -> bool {
    matches!(
        state,
        WakeState::Sleeping | WakeState::TimedOut | WakeState::Failed
    )
}

#[cfg(any(test, debug_assertions))]
pub(crate) fn wake_state_from_label(state: &str) -> Option<WakeState> {
    match state {
        "sleeping" => Some(WakeState::Sleeping),
        "waking" => Some(WakeState::Waking),
        "ping_ready" => Some(WakeState::PingReady),
        "http_ready" => Some(WakeState::HttpReady),
        "ready" => Some(WakeState::Ready),
        "timed_out" => Some(WakeState::TimedOut),
        "failed" => Some(WakeState::Failed),
        _ => None,
    }
}

#[cfg(any(test, debug_assertions))]
pub(crate) fn preview_auto_from_label(value: &str) -> bool {
    matches!(value, "1" | "true" | "yes" | "on")
}
