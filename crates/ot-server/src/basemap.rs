//! The maps' basemap: raster or vector tiles from the XYZ tile server the
//! settings name (`basemap_tiles_url`), fetched by this server for the browser. The
//! page's content security policy stays `'self'`, browsers need no route to
//! the tile server, and a key in its URL never leaves this server. Nothing
//! is cached here: the browser keeps each tile for a day.

use std::sync::OnceLock;
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;

use crate::control::{ApiError, AppState};
use crate::settings_api::{BasemapTilesFormat, fill_tile_url};

pub fn routes() -> Router<AppState> {
    Router::new().route("/basemap/{z}/{x}/{y}", get(tile))
}

/// The deepest zoom level served.
pub const MAX_ZOOM: u32 = 24;
/// The largest tile accepted from the tile server.
pub const MAX_TILE_BYTES: usize = 4 * 1024 * 1024;
/// What the browser may keep a tile for.
pub const CACHE_CONTROL: &str = "private, max-age=86400";
const TIMEOUT: Duration = Duration::from_secs(10);
const IMAGE_TYPES: &[&str] = &["image/png", "image/jpeg", "image/webp", "image/avif"];
const VECTOR_TYPES: &[&str] = &[
    "application/x-protobuf",
    "application/vnd.mapbox-vector-tile",
    "application/protobuf",
];

/// One client for every tile: no redirects (a tile server that moves is
/// set anew), a 10 s limit, and this build's name.
fn client() -> &'static reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .timeout(TIMEOUT)
            .redirect(reqwest::redirect::Policy::none())
            .user_agent(concat!("OpenTrack/", env!("CARGO_PKG_VERSION")))
            .build()
            .unwrap_or_default()
    })
}

fn bad_gateway(m: &str) -> Response {
    (
        StatusCode::BAD_GATEWAY,
        axum::Json(serde_json::json!({ "error": m })),
    )
        .into_response()
}

/// Tile coordinates from the path: z 0..=24, x and y 0..2^z.
fn coordinates(z: &str, x: &str, y: &str) -> Result<(u32, u64, u64), ApiError> {
    let z: u32 = z
        .parse()
        .ok()
        .filter(|z| *z <= MAX_ZOOM)
        .ok_or_else(|| ApiError::bad_request(format!("z: 0 to {MAX_ZOOM}")))?;
    let n = 1u64 << z;
    let at = |v: &str, name: &str| {
        v.parse::<u64>()
            .ok()
            .filter(|v| *v < n)
            .ok_or_else(|| ApiError::bad_request(format!("{name}: 0 to {} at zoom {z}", n - 1)))
    };
    Ok((z, at(x, "x")?, at(y, "y")?))
}

/// The tile's supported media type for the configured representation.
fn tile_type(v: Option<&HeaderValue>, format: BasemapTilesFormat) -> Option<&'static str> {
    let t = v?.to_str().ok()?;
    let t = t.split(';').next()?.trim().to_ascii_lowercase();
    let supported = match format {
        BasemapTilesFormat::Raster => IMAGE_TYPES,
        BasemapTilesFormat::Vector => VECTOR_TYPES,
    };
    supported.iter().copied().find(|i| *i == t)
}

/// `GET /basemap/{z}/{x}/{y}`: the tile, from the tile server. 404 when no
/// tile URL is set or the tile server has no such tile (open ocean, say);
/// 502 when it fails or answers with a media type that does not match the
/// configured raster or vector representation.
async fn tile(
    State(s): State<AppState>,
    Path((z, x, y)): Path<(String, String, String)>,
) -> Result<Response, ApiError> {
    let (z, x, y) = coordinates(&z, &x, &y)?;
    let saved = s.with_db(|db| db.app_settings()).await?;
    let template = saved["basemap_tiles_url"].as_str().unwrap_or_default();
    if template.is_empty() {
        return Err(ApiError::not_found("no basemap tiles are set"));
    }
    let format = serde_json::from_value(
        saved
            .get("basemap_tiles_format")
            .cloned()
            .unwrap_or_else(|| serde_json::json!("raster")),
    )
    .unwrap_or_default();
    let url = fill_tile_url(template, z, x, y);
    // Logged by host only: the URL may carry a key.
    let host = reqwest::Url::parse(&url)
        .ok()
        .and_then(|u| u.host_str().map(str::to_owned))
        .unwrap_or_default();
    let fail = |reason: &str, status: Option<StatusCode>| {
        tracing::warn!(
            host = %host,
            status = status.map(|s| s.as_u16()),
            "basemap tile: {reason}"
        );
        bad_gateway(&format!("tile server: {reason}"))
    };
    let mut res = match client().get(&url).send().await {
        Ok(r) => r,
        Err(e) => {
            let reason = if e.is_timeout() {
                "timed out"
            } else if e.is_connect() {
                "unreachable"
            } else {
                "request failed"
            };
            return Ok(fail(reason, None));
        }
    };
    // The tile server's address, logged when it changes (ASD STIG V-222470).
    static PEERS: OnceLock<ot_core::netlog::PeerLog> = OnceLock::new();
    if let Some(peer) = res.remote_addr()
        && PEERS.get_or_init(Default::default).changed(&host, peer)
    {
        tracing::info!(component = "basemap", %host, peer_addr = %peer, "tile server reached");
    }
    let status = res.status();
    if status == StatusCode::NOT_FOUND {
        return Err(ApiError::not_found("no such tile"));
    }
    if !status.is_success() {
        return Ok(fail(&format!("answered {}", status.as_u16()), Some(status)));
    }
    let Some(kind) = tile_type(res.headers().get(header::CONTENT_TYPE), format) else {
        let wanted = match format {
            BasemapTilesFormat::Raster => "not a PNG, JPEG, WebP or AVIF image",
            BasemapTilesFormat::Vector => "not a Mapbox vector tile",
        };
        return Ok(fail(wanted, Some(status)));
    };
    // PMTiles and most vector tile servers store MVT payloads gzipped. reqwest
    // does not decompress them in this build, so preserve the encoding for
    // MapLibre rather than handing it compressed bytes without the header.
    let content_encoding = res
        .headers()
        .get(header::CONTENT_ENCODING)
        .filter(|v| v.as_bytes().eq_ignore_ascii_case(b"gzip"))
        .cloned();
    if res
        .content_length()
        .is_some_and(|n| n > MAX_TILE_BYTES as u64)
    {
        return Ok(fail("tile larger than 4 MiB", Some(status)));
    }
    let mut body = Vec::new();
    loop {
        match res.chunk().await {
            Ok(Some(c)) => {
                if body.len() + c.len() > MAX_TILE_BYTES {
                    return Ok(fail("tile larger than 4 MiB", Some(status)));
                }
                body.extend_from_slice(&c);
            }
            Ok(None) => break,
            Err(e) => {
                let reason = if e.is_timeout() {
                    "timed out"
                } else {
                    "tile cut short"
                };
                return Ok(fail(reason, Some(status)));
            }
        }
    }
    let mut headers = HeaderMap::new();
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_static(kind));
    headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static(CACHE_CONTROL),
    );
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    if let Some(encoding) = content_encoding {
        headers.insert(header::CONTENT_ENCODING, encoding);
    }
    Ok((StatusCode::OK, headers, Body::from(body)).into_response())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coordinates_are_bounded_by_zoom() {
        assert_eq!(coordinates("0", "0", "0").unwrap(), (0, 0, 0));
        assert_eq!(coordinates("2", "3", "3").unwrap(), (2, 3, 3));
        assert_eq!(
            coordinates("24", "16777215", "0").unwrap(),
            (24, 16_777_215, 0)
        );
        for (z, x, y) in [
            ("25", "0", "0"),
            ("-1", "0", "0"),
            ("0", "1", "0"),
            ("2", "0", "4"),
            ("2", "x", "0"),
            ("1", "0", "1.png"),
        ] {
            assert!(coordinates(z, x, y).is_err(), "{z}/{x}/{y}");
        }
    }

    #[test]
    fn only_configured_tile_types_pass() {
        let t = |s: &str, format| tile_type(Some(&HeaderValue::from_str(s).unwrap()), format);
        assert_eq!(
            t("image/png", BasemapTilesFormat::Raster),
            Some("image/png")
        );
        assert_eq!(
            t("Image/JPEG; charset=binary", BasemapTilesFormat::Raster),
            Some("image/jpeg")
        );
        assert_eq!(
            t("application/x-protobuf", BasemapTilesFormat::Vector),
            Some("application/x-protobuf")
        );
        assert_eq!(
            t(
                "application/vnd.mapbox-vector-tile",
                BasemapTilesFormat::Vector
            ),
            Some("application/vnd.mapbox-vector-tile")
        );
        assert_eq!(t("image/png", BasemapTilesFormat::Vector), None);
        assert_eq!(
            t("application/x-protobuf", BasemapTilesFormat::Raster),
            None
        );
        assert_eq!(t("text/html", BasemapTilesFormat::Raster), None);
        assert_eq!(tile_type(None, BasemapTilesFormat::Raster), None);
    }
}
