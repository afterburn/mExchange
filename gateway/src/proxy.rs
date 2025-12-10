use axum::{
    body::Body,
    extract::{Request, State},
    http::{header, Method, StatusCode},
    response::Response,
};
use reqwest::Client;

#[derive(Clone)]
pub struct ProxyState {
    pub client: Client,
    pub accounts_url: String,
    pub market_data_url: String,
}

impl ProxyState {
    pub fn new(accounts_url: String, market_data_url: String) -> Self {
        Self {
            client: Client::new(),
            accounts_url,
            market_data_url,
        }
    }
}

pub async fn proxy_accounts(
    State(proxy): State<ProxyState>,
    req: Request,
) -> Result<Response, StatusCode> {
    proxy_to_url(&proxy.client, &proxy.accounts_url, req).await
}

pub async fn proxy_market_data(
    State(proxy): State<ProxyState>,
    req: Request,
) -> Result<Response, StatusCode> {
    proxy_to_url(&proxy.client, &proxy.market_data_url, req).await
}

async fn proxy_to_url(
    client: &Client,
    base_url: &str,
    req: Request,
) -> Result<Response, StatusCode> {
    let path = req.uri().path();
    let query = req.uri().query().map(|q| format!("?{}", q)).unwrap_or_default();
    let target_url = format!("{}{}{}", base_url, path, query);

    tracing::debug!("Proxying request to: {}", target_url);

    let method = req.method().clone();
    let headers = req.headers().clone();

    // Build the proxied request
    let mut proxy_req = client.request(method.clone(), &target_url);

    // Forward relevant headers
    for (name, value) in headers.iter() {
        // Skip hop-by-hop headers
        if name == header::HOST || name == header::CONNECTION {
            continue;
        }
        if let Ok(v) = value.to_str() {
            proxy_req = proxy_req.header(name.as_str(), v);
        }
    }

    // Forward cookies
    if let Some(cookie) = headers.get(header::COOKIE) {
        if let Ok(v) = cookie.to_str() {
            proxy_req = proxy_req.header("cookie", v);
        }
    }

    // Forward body for methods that have one
    if method == Method::POST || method == Method::PUT || method == Method::PATCH {
        let body_bytes = axum::body::to_bytes(req.into_body(), 1024 * 1024)
            .await
            .map_err(|_| StatusCode::BAD_REQUEST)?;
        proxy_req = proxy_req.body(body_bytes);
    }

    // Make the request
    let proxy_res = proxy_req.send().await.map_err(|e| {
        tracing::error!("Proxy request failed: {}", e);
        StatusCode::BAD_GATEWAY
    })?;

    // Build response
    let status = StatusCode::from_u16(proxy_res.status().as_u16())
        .unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);

    let mut response_builder = Response::builder().status(status);

    // Forward response headers
    for (name, value) in proxy_res.headers().iter() {
        // Skip hop-by-hop headers
        if name == header::TRANSFER_ENCODING || name == header::CONNECTION {
            continue;
        }
        if let Ok(v) = value.to_str() {
            response_builder = response_builder.header(name.as_str(), v);
        }
    }

    // Forward set-cookie headers
    for cookie in proxy_res.cookies() {
        let cookie_str = format!(
            "{}={}; Path={}{}{}",
            cookie.name(),
            cookie.value(),
            cookie.path().unwrap_or("/"),
            if cookie.http_only() { "; HttpOnly" } else { "" },
            if cookie.secure() { "; Secure" } else { "" },
        );
        response_builder = response_builder.header(header::SET_COOKIE, cookie_str);
    }

    let body_bytes = proxy_res.bytes().await.map_err(|e| {
        tracing::error!("Failed to read proxy response body: {}", e);
        StatusCode::BAD_GATEWAY
    })?;

    response_builder
        .body(Body::from(body_bytes))
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
}
