use axum::http::{HeaderValue, StatusCode};
use axum::response::IntoResponse;
use tera::{Context, Tera};
use thiserror::Error;
use tracing::error;

use super::AppState;
use crate::http::{apply_gateway_security_headers, mark_uncacheable};
use crate::proxy::ProxyError;

#[derive(Debug, Error)]
pub enum TemplateLoadError {
    #[error("failed to load template {name}")]
    Load {
        name: &'static str,
        #[source]
        source: tera::Error,
    },
}

#[derive(Debug, Error)]
pub enum TemplateRenderError {
    #[error("failed to render template {name}")]
    Render {
        name: &'static str,
        #[source]
        source: tera::Error,
    },
}

#[derive(Debug, Error)]
pub enum AppError {
    #[error(transparent)]
    Proxy(#[from] ProxyError),
    #[error(transparent)]
    Template(#[from] TemplateRenderError),
}

pub fn load_templates() -> Result<Tera, TemplateLoadError> {
    let mut tera = Tera::default();
    add_template(
        &mut tera,
        "_layout.html",
        include_str!("../../templates/_layout.html"),
    )?;
    add_template(
        &mut tera,
        "login.html",
        include_str!("../../templates/login.html"),
    )?;
    add_template(
        &mut tera,
        "wake-status.html",
        include_str!("../../templates/wake-status.html"),
    )?;
    add_template(
        &mut tera,
        "error.html",
        include_str!("../../templates/error.html"),
    )?;
    Ok(tera)
}

pub(super) fn render_login(
    state: &AppState,
    next: Option<&str>,
    error: Option<&str>,
    status: StatusCode,
) -> axum::response::Response {
    let mut context = Context::new();
    context.insert("next", &next.unwrap_or("/"));
    context.insert("error", &error.unwrap_or(""));
    let mut response = render_template(state, "login.html", context, status);
    mark_uncacheable(response.headers_mut());
    response
}

pub(super) fn render_template(
    state: &AppState,
    name: &'static str,
    context: Context,
    status: StatusCode,
) -> axum::response::Response {
    match try_render_template(state, name, context, status) {
        Ok(response) => response,
        Err(error) => {
            error!(template = name, ?error, "template render failed");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Template rendering failed",
            )
                .into_response()
        }
    }
}

fn add_template(
    tera: &mut Tera,
    name: &'static str,
    content: &'static str,
) -> Result<(), TemplateLoadError> {
    tera.add_raw_template(name, content)
        .map_err(|source| TemplateLoadError::Load { name, source })
}

fn try_render_template(
    state: &AppState,
    name: &'static str,
    context: Context,
    status: StatusCode,
) -> Result<axum::response::Response, TemplateRenderError> {
    let rendered = state
        .templates
        .render(name, &context)
        .map_err(|source| TemplateRenderError::Render { name, source })?;
    let mut response = rendered.into_response();
    *response.status_mut() = status;
    response.headers_mut().insert(
        axum::http::header::CONTENT_TYPE,
        HeaderValue::from_static("text/html; charset=utf-8"),
    );
    apply_gateway_security_headers(response.headers_mut());
    Ok(response)
}

pub(super) fn app_error_response(state: &AppState, error: AppError) -> axum::response::Response {
    match error {
        AppError::Proxy(proxy_error) => {
            error!(?proxy_error, "proxy request failed");
            let mut context = Context::new();
            context.insert("error", &proxy_error.to_string());
            match try_render_template(state, "error.html", context, StatusCode::BAD_GATEWAY) {
                Ok(response) => response,
                Err(template_error) => app_error_response(state, template_error.into()),
            }
        }
        AppError::Template(template_error) => {
            error!(?template_error, "template render failed");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Template rendering failed",
            )
                .into_response()
        }
    }
}
