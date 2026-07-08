use std::net::SocketAddr;

use common::{SharedState, web::api::api_root};
use log::error;
use warp::{Filter, Rejection, Reply, hyper::StatusCode, reply::Response};

/// Filter that optionally enforces HTTP Basic Auth based on config.
fn basic_auth(state: SharedState) -> impl Filter<Extract = (), Error = Rejection> + Clone {
    warp::header::optional::<String>("authorization")
        .and(warp::any().map(move || state.clone()))
        .and_then(|auth_header: Option<String>, state: SharedState| async move {
            let require_auth =
                state.config.api_server.as_ref().and_then(|ws| ws.require_auth).unwrap_or(false);

            if !require_auth {
                return Ok(());
            }

            let auth_header = match auth_header {
                Some(h) => h,
                None => {
                    return Err(warp::reject::custom(AuthRequired));
                },
            };

            if !auth_header.starts_with("Basic ") {
                return Err(warp::reject::custom(AuthRequired));
            }

            let encoded = &auth_header[6..];
            let decoded = match base64_decode(encoded) {
                Some(d) => d,
                None => return Err(warp::reject::custom(AuthRequired)),
            };

            let parts: Vec<&str> = decoded.splitn(2, ':').collect();
            if parts.len() != 2 {
                return Err(warp::reject::custom(AuthRequired));
            }

            let username = parts[0];
            let password = parts[1];

            match state.db.verify_web_user(username, password).await {
                Ok(true) => Ok(()),
                Ok(false) | Err(_) => Err(warp::reject::custom(AuthRequired)),
            }
        })
        .untuple_one()
}

fn base64_decode(input: &str) -> Option<String> {
    use base64::Engine;
    let engine = base64::engine::general_purpose::STANDARD;
    let bytes = engine.decode(input).ok()?;
    String::from_utf8(bytes).ok()
}

#[derive(Debug)]
struct AuthRequired;

impl warp::reject::Reject for AuthRequired {}

async fn handle_auth_rejection(err: Rejection) -> Result<impl Reply, Rejection> {
    if err.find::<AuthRequired>().is_some() {
        let mut resp = Response::default();
        *resp.status_mut() = StatusCode::UNAUTHORIZED;
        resp.headers_mut().insert(
            "WWW-Authenticate",
            warp::http::HeaderValue::from_static("Basic realm=\"lumen\""),
        );
        return Ok(resp);
    }
    Err(err)
}

pub async fn start_webserver<A: Into<SocketAddr> + 'static>(
    bind_addr: A, shared_state: SharedState,
) {
    let root =
        warp::get().and(warp::path::end()).map(|| warp::reply::html(include_str!("home.html")));

    let shared_state1 = shared_state.clone();
    let auth = basic_auth(shared_state1.clone());
    let api = warp::path("api").and(auth).and(api_root(shared_state1));

    let metrics = warp::get().and(warp::path("metrics")).and(warp::path::end()).map(move || {
        let mut res = String::new();
        if let Err(err) =
            prometheus_client::encoding::text::encode(&mut res, &shared_state.metrics.registry)
        {
            error!("failed to encode metrics: {err}");
            let mut r = Response::default();
            *r.status_mut() = StatusCode::INTERNAL_SERVER_ERROR;
            r
        } else {
            warp::reply::Response::new(res.into())
        }
    });

    let routes = root.or(api.recover(handle_auth_rejection)).or(metrics);

    warp::serve(routes).run(bind_addr).await;
}
