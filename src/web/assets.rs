use axum::http::HeaderValue;
use axum::http::header::CACHE_CONTROL;
use axum::response::{IntoResponse, Response};
use serde::Serialize;

use crate::http::apply_gateway_security_headers;

const PWA_ICON_192_PNG: &[u8] = include_bytes!("../../assets/icons/icon-192.png");
const PWA_ICON_512_PNG: &[u8] = include_bytes!("../../assets/icons/icon-512.png");
const PWA_ICON_MASKABLE_512_PNG: &[u8] = include_bytes!("../../assets/icons/icon-maskable-512.png");
const APPLE_TOUCH_ICON_PNG: &[u8] = include_bytes!("../../assets/icons/apple-touch-icon.png");
const PWA_SERVICE_WORKER_JS: &str = r#"const STATIC_CACHE_NAME = "wake-static-v1";
const STATIC_CACHE_ASSETS = [
  "/manifest.webmanifest",
  "/apple-touch-icon.png",
  "/icons/icon-192.png",
  "/icons/icon-512.png",
  "/icons/icon-maskable-512.png"
];

const OFFLINE_HTML = "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width,initial-scale=1\"><title>Offline</title><style>body{font-family:-apple-system,BlinkMacSystemFont,\"Segoe UI\",sans-serif;background:#eef4ed;color:#1b2a20;margin:0;display:grid;place-items:center;min-height:100vh;padding:1rem}main{max-width:32rem;background:#fffdf8;border:1px solid #d4e1d1;border-radius:14px;padding:1rem 1.2rem}h1{margin:.1rem 0 .5rem;font-size:1.25rem}p{margin:0;line-height:1.5}</style></head><body><main><h1>Offline</h1><p>This shortcut needs a network connection to reach your secured gateway. Reconnect and try again.</p></main></body></html>";

self.addEventListener("install", (event) => {
  event.waitUntil(
    caches
      .open(STATIC_CACHE_NAME)
      .then((cache) => cache.addAll(STATIC_CACHE_ASSETS))
      .then(() => self.skipWaiting())
  );
});

self.addEventListener("activate", (event) => {
  event.waitUntil(
    caches
      .keys()
      .then((keys) => Promise.all(keys.filter((key) => key !== STATIC_CACHE_NAME).map((key) => caches.delete(key))))
      .then(() => self.clients.claim())
  );
});

const cacheFirst = async (request) => {
  const cache = await caches.open(STATIC_CACHE_NAME);
  const cached = await cache.match(request);
  if (cached) {
    return cached;
  }

  const response = await fetch(request);
  if (response && response.ok) {
    cache.put(request, response.clone());
  }
  return response;
};

self.addEventListener("fetch", (event) => {
  const request = event.request;
  if (request.method !== "GET") {
    return;
  }

  const url = new URL(request.url);
  if (url.origin !== self.location.origin) {
    return;
  }

  if (STATIC_CACHE_ASSETS.includes(url.pathname)) {
    event.respondWith(cacheFirst(request));
    return;
  }

  if (request.mode === "navigate") {
    event.respondWith(
      fetch(request).catch(
        () =>
          new Response(OFFLINE_HTML, {
            status: 503,
            headers: {
              "Content-Type": "text/html; charset=utf-8",
              "Cache-Control": "no-store"
            }
          })
      )
    );
    return;
  }

  event.respondWith(fetch(request));
});
"#;

#[derive(Serialize)]
struct WebManifest {
    name: &'static str,
    short_name: &'static str,
    id: &'static str,
    start_url: &'static str,
    scope: &'static str,
    display: &'static str,
    background_color: &'static str,
    theme_color: &'static str,
    description: &'static str,
    icons: Vec<WebManifestIcon>,
}

#[derive(Serialize)]
struct WebManifestIcon {
    src: &'static str,
    sizes: &'static str,
    #[serde(rename = "type")]
    mime_type: &'static str,
    purpose: &'static str,
}

pub(crate) async fn web_manifest() -> Response {
    let mut response = axum::Json(WebManifest {
        name: "wakeproxy",
        short_name: "Wake",
        id: "/",
        start_url: "/",
        scope: "/",
        display: "standalone",
        background_color: "#eef4ed",
        theme_color: "#eef4ed",
        description: "Secure wake-and-proxy gateway for your upstream app.",
        icons: vec![
            WebManifestIcon {
                src: "/icons/icon-192.png",
                sizes: "192x192",
                mime_type: "image/png",
                purpose: "any",
            },
            WebManifestIcon {
                src: "/icons/icon-512.png",
                sizes: "512x512",
                mime_type: "image/png",
                purpose: "any",
            },
            WebManifestIcon {
                src: "/icons/icon-maskable-512.png",
                sizes: "512x512",
                mime_type: "image/png",
                purpose: "any maskable",
            },
        ],
    })
    .into_response();
    response.headers_mut().insert(
        axum::http::header::CONTENT_TYPE,
        HeaderValue::from_static("application/manifest+json"),
    );
    response.headers_mut().insert(
        CACHE_CONTROL,
        HeaderValue::from_static("public, max-age=3600"),
    );
    apply_gateway_security_headers(response.headers_mut());
    response
}

pub(crate) async fn service_worker() -> Response {
    let mut response = PWA_SERVICE_WORKER_JS.into_response();
    response.headers_mut().insert(
        axum::http::header::CONTENT_TYPE,
        HeaderValue::from_static("application/javascript; charset=utf-8"),
    );
    response
        .headers_mut()
        .insert(CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    response
        .headers_mut()
        .insert("service-worker-allowed", HeaderValue::from_static("/"));
    apply_gateway_security_headers(response.headers_mut());
    response
}

pub(crate) async fn pwa_icon_192() -> Response {
    png_asset_response(PWA_ICON_192_PNG)
}

pub(crate) async fn pwa_icon_512() -> Response {
    png_asset_response(PWA_ICON_512_PNG)
}

pub(crate) async fn pwa_icon_maskable_512() -> Response {
    png_asset_response(PWA_ICON_MASKABLE_512_PNG)
}

pub(crate) async fn apple_touch_icon() -> Response {
    png_asset_response(APPLE_TOUCH_ICON_PNG)
}

pub(crate) async fn gateway_health() -> impl IntoResponse {
    let mut response = (axum::http::StatusCode::OK, "ok").into_response();
    apply_gateway_security_headers(response.headers_mut());
    response
}

fn png_asset_response(bytes: &'static [u8]) -> Response {
    let mut response = bytes.to_vec().into_response();
    response.headers_mut().insert(
        axum::http::header::CONTENT_TYPE,
        HeaderValue::from_static("image/png"),
    );
    response.headers_mut().insert(
        CACHE_CONTROL,
        HeaderValue::from_static("public, max-age=604800, immutable"),
    );
    apply_gateway_security_headers(response.headers_mut());
    response
}
