//! `flux.http`: asynchronous HTTP for extensions.
//!
//! * Fully async on the extension's own runtime — a slow server never blocks the UI or other
//!   extensions, and dropping the calling task (view closed, command cancelled) aborts the request.
//! * Needs the `network` permission; the manifest's optional `allowed_hosts` restricts the hosts
//!   (checked on the URL **and on every redirect hop**).
//! * Private/loopback/link-local addresses are refused unless the host is listed *exactly* in
//!   `allowed_hosts` (checked again at connect time, so DNS rebinding cannot sneak past).
//! * Bodies are size-capped; transport problems come back as `nil, {kind=…, message=…}`.

use std::{
    cell::{Cell, OnceCell},
    net::{IpAddr, SocketAddr},
    rc::Rc,
    sync::Arc,
    time::Duration,
};

use flux_api::is_private_ip;
use mlua::{Lua, Table, UserData, UserDataMethods, Value};
use reqwest::{
    Client, Method, Url,
    dns::{Addrs, Name, Resolve, Resolving},
    header::{HeaderMap, HeaderName, HeaderValue},
    redirect,
};
use tokio::sync::Notify;

use crate::ctx::CmdCtx;

pub const DEFAULT_TIMEOUT_S: f64 = 30.0;
pub const DEFAULT_MAX_BYTES: usize = 10 * 1024 * 1024;
const MAX_REDIRECTS: usize = 5;

// ------------------------------------------------------------ cancellation

/// `flux.task.token()`: lets one callback cancel an operation started by another.
#[derive(Clone, Default)]
pub struct Token(Rc<TokenInner>);

#[derive(Default)]
struct TokenInner {
    cancelled: Cell<bool>,
    notify: Notify,
}

impl Token {
    pub fn cancel(&self) {
        self.0.cancelled.set(true);
        self.0.notify.notify_waiters();
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.cancelled.get()
    }

    /// Completes when the token is cancelled.
    pub async fn cancelled(&self) {
        loop {
            let notified = self.0.notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if self.is_cancelled() {
                return;
            }
            notified.await;
        }
    }
}

impl UserData for Token {
    fn add_methods<M: UserDataMethods<Self>>(m: &mut M) {
        m.add_method("cancel", |_, t, ()| {
            t.cancel();
            Ok(())
        });
        m.add_method("is_cancelled", |_, t, ()| Ok(t.is_cancelled()));
    }
}

// ------------------------------------------------------------------ policy

/// Which hosts an extension may reach.
#[derive(Debug, Clone, Default)]
pub struct HostPolicy {
    /// Lowercase exact hosts or `*.suffix` patterns. Empty = any public host.
    pub allowed: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct HttpError {
    pub kind: &'static str,
    pub message: String,
}

impl HttpError {
    fn new(kind: &'static str, message: impl Into<String>) -> Self {
        Self { kind, message: message.into() }
    }
}

impl HostPolicy {
    fn pattern_matches(pattern: &str, host: &str) -> bool {
        match pattern.strip_prefix("*.") {
            Some(suffix) => host.len() > suffix.len() + 1 && host.ends_with(suffix) && host.as_bytes()[host.len() - suffix.len() - 1] == b'.',
            None => pattern == host,
        }
    }

    pub fn host_allowed(&self, host: &str) -> bool {
        self.allowed.is_empty() || self.allowed.iter().any(|p| Self::pattern_matches(p, host))
    }

    /// Listed verbatim (no wildcard): the only way to reach a private address.
    pub fn explicitly_listed(&self, host: &str) -> bool {
        self.allowed.iter().any(|p| p == host)
    }

    pub fn check(&self, url: &Url) -> Result<(), HttpError> {
        if !matches!(url.scheme(), "http" | "https") {
            return Err(HttpError::new("invalid", format!("only http:// and https:// URLs are allowed, got '{}://'", url.scheme())));
        }
        let host = url.host_str().ok_or_else(|| HttpError::new("invalid", "the URL has no host"))?.trim_matches(['[', ']']).to_lowercase();
        if !self.host_allowed(&host) {
            return Err(HttpError::new("denied", format!("host '{host}' is not in this extension's allowed_hosts")));
        }
        if let Ok(ip) = host.parse::<IpAddr>() {
            if is_private_ip(ip) && !self.explicitly_listed(&host) {
                return Err(HttpError::new("denied", format!("'{host}' is a private address; list it in allowed_hosts to reach it")));
            }
        }
        Ok(())
    }
}

struct SafeResolver(Arc<HostPolicy>);

impl Resolve for SafeResolver {
    fn resolve(&self, name: Name) -> Resolving {
        let policy = self.0.clone();
        Box::pin(async move {
            let host = name.as_str().to_lowercase();
            let all: Vec<SocketAddr> = tokio::net::lookup_host((host.as_str(), 0)).await?.collect();
            let allowed: Vec<SocketAddr> = all.into_iter().filter(|a| policy.explicitly_listed(&host) || !is_private_ip(a.ip())).collect();
            if allowed.is_empty() {
                let e: Box<dyn std::error::Error + Send + Sync> = format!("'{host}' resolves to a private address; list it in allowed_hosts to reach it").into();
                return Err(e);
            }
            Ok(Box::new(allowed.into_iter()) as Addrs)
        })
    }
}

/// Per-worker HTTP state: clients are created lazily, on the worker's own runtime.
pub struct HttpState {
    pub policy: Arc<HostPolicy>,
    version: String,
    redirecting: OnceCell<Client>,
    direct: OnceCell<Client>,
}

impl HttpState {
    pub fn new(allowed_hosts: Vec<String>, version: &str) -> Self {
        Self {
            policy: Arc::new(HostPolicy { allowed: allowed_hosts }),
            version: version.to_string(),
            redirecting: OnceCell::new(),
            direct: OnceCell::new(),
        }
    }

    fn client(&self, follow: bool) -> Result<Client, HttpError> {
        let cell = if follow { &self.redirecting } else { &self.direct };
        if let Some(c) = cell.get() {
            return Ok(c.clone());
        }
        let policy = self.policy.clone();
        let redirects = if follow {
            redirect::Policy::custom(move |attempt| {
                if attempt.previous().len() >= MAX_REDIRECTS {
                    return attempt.error(format!("too many redirects (max {MAX_REDIRECTS})"));
                }
                match policy.check(attempt.url()) {
                    Ok(()) => attempt.follow(),
                    Err(e) => attempt.error(e.message),
                }
            })
        } else {
            redirect::Policy::none()
        };
        let client = Client::builder()
            .user_agent(format!("Flux/{} (extension)", self.version))
            .redirect(redirects)
            .dns_resolver(Arc::new(SafeResolver(self.policy.clone())))
            .connect_timeout(Duration::from_secs(10))
            .build()
            .map_err(|e| HttpError::new("network", format!("could not create the HTTP client: {e}")))?;
        let _ = cell.set(client.clone());
        Ok(client)
    }
}

// ------------------------------------------------------------------ request

struct Opts {
    headers: HeaderMap,
    query: Vec<(String, String)>,
    body: Option<(Vec<u8>, Option<&'static str>)>,
    timeout: Duration,
    max_bytes: usize,
    follow: bool,
    signal: Option<Token>,
}

fn scalar(v: Value, what: &str) -> mlua::Result<String> {
    Ok(match v {
        Value::String(s) => s.to_string_lossy().to_string(),
        Value::Integer(i) => i.to_string(),
        Value::Number(n) => n.to_string(),
        Value::Boolean(b) => b.to_string(),
        other => return Err(mlua::Error::runtime(format!("{what}: expected a string, number or boolean, got {}", other.type_name()))),
    })
}

fn parse_opts(lua: &Lua, opts: Option<Table>) -> mlua::Result<Opts> {
    let mut o = Opts {
        headers: HeaderMap::new(),
        query: vec![],
        body: None,
        timeout: Duration::from_secs_f64(DEFAULT_TIMEOUT_S),
        max_bytes: DEFAULT_MAX_BYTES,
        follow: true,
        signal: None,
    };
    let Some(t) = opts else { return Ok(o) };
    const ALLOWED: &[&str] = &["method", "url", "headers", "query", "json", "body", "form", "timeout", "signal", "max_bytes", "follow_redirects"];
    for pair in t.pairs::<Value, Value>() {
        if let (Value::String(k), _) = pair? {
            let k = k.to_string_lossy();
            if !ALLOWED.contains(&k.as_ref()) {
                return Err(mlua::Error::runtime(format!("flux.http: unknown option '{k}' (allowed: {})", ALLOWED.join(", "))));
            }
        }
    }
    if let Some(h) = t.get::<Option<Table>>("headers")? {
        for pair in h.pairs::<String, Value>() {
            let (k, v) = pair?;
            let name = HeaderName::from_bytes(k.as_bytes()).map_err(|_| mlua::Error::runtime(format!("flux.http: invalid header name '{k}'")))?;
            let value = HeaderValue::from_str(&scalar(v, &format!("header '{k}'"))?).map_err(|_| mlua::Error::runtime(format!("flux.http: invalid value for header '{k}'")))?;
            o.headers.insert(name, value);
        }
    }
    if let Some(q) = t.get::<Option<Table>>("query")? {
        let mut pairs: Vec<(String, String)> = Vec::new();
        for pair in q.pairs::<String, Value>() {
            let (k, v) = pair?;
            match v {
                Value::Table(list) => {
                    for item in list.sequence_values::<Value>() {
                        pairs.push((k.clone(), scalar(item?, &format!("query '{k}'"))?));
                    }
                }
                Value::Nil => {}
                other => pairs.push((k.clone(), scalar(other, &format!("query '{k}'"))?)),
            }
        }
        pairs.sort(); // deterministic order (Lua tables are unordered)
        o.query = pairs;
    }
    let (json, body, form) = (t.get::<Value>("json")?, t.get::<Option<mlua::LuaString>>("body")?, t.get::<Option<Table>>("form")?);
    if (!matches!(json, Value::Nil) as u8) + body.is_some() as u8 + form.is_some() as u8 > 1 {
        return Err(mlua::Error::runtime("flux.http: give only one of json, body or form"));
    }
    if !matches!(json, Value::Nil) {
        let v: serde_json::Value = mlua::LuaSerdeExt::from_value(lua, json)?;
        o.body = Some((serde_json::to_vec(&v).map_err(mlua::Error::external)?, Some("application/json")));
    } else if let Some(b) = body {
        o.body = Some((b.as_bytes().to_vec(), None));
    } else if let Some(f) = form {
        let mut ser = url::form_urlencoded::Serializer::new(String::new());
        let mut pairs: Vec<(String, String)> = Vec::new();
        for pair in f.pairs::<String, Value>() {
            let (k, v) = pair?;
            pairs.push((k.clone(), scalar(v, &format!("form '{k}'"))?));
        }
        pairs.sort();
        for (k, v) in pairs {
            ser.append_pair(&k, &v);
        }
        o.body = Some((ser.finish().into_bytes(), Some("application/x-www-form-urlencoded")));
    }
    if let Some((b, _)) = &o.body {
        if b.len() > DEFAULT_MAX_BYTES {
            return Err(mlua::Error::runtime("flux.http: the request body is larger than 10 MB"));
        }
    }
    if let Some(s) = t.get::<Option<f64>>("timeout")? {
        if !(s > 0.0 && s <= 600.0) {
            return Err(mlua::Error::runtime("flux.http: timeout must be between 0 and 600 seconds"));
        }
        o.timeout = Duration::from_secs_f64(s);
    }
    if let Some(m) = t.get::<Option<f64>>("max_bytes")? {
        if m < 1.0 {
            return Err(mlua::Error::runtime("flux.http: max_bytes must be positive"));
        }
        o.max_bytes = m as usize;
    }
    if let Some(f) = t.get::<Option<bool>>("follow_redirects")? {
        o.follow = f;
    }
    if let Some(s) = t.get::<Option<mlua::UserDataRef<Token>>>("signal")? {
        o.signal = Some(s.clone());
    }
    Ok(o)
}

fn error_table(lua: &Lua, e: &HttpError) -> mlua::Result<Table> {
    let t = lua.create_table()?;
    t.set("kind", e.kind)?;
    t.set("message", e.message.as_str())?;
    let meta = lua.create_table()?;
    meta.set("__tostring", lua.create_function(|_, t: Table| t.get::<String>("message"))?)?;
    t.set_metatable(Some(meta))?;
    Ok(t)
}

struct Reply {
    status: u16,
    url: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
    redirected: bool,
}

async fn perform(client: Client, method: Method, url: Url, o: Opts) -> Result<Reply, HttpError> {
    let mut req = client.request(method, url.clone()).headers(o.headers.clone());
    if !o.query.is_empty() {
        req = req.query(&o.query);
    }
    if let Some((bytes, ct)) = o.body {
        if let Some(ct) = ct {
            if !o.headers.contains_key(reqwest::header::CONTENT_TYPE) {
                req = req.header(reqwest::header::CONTENT_TYPE, ct);
            }
        }
        req = req.body(bytes);
    }
    let mut resp = req.send().await.map_err(map_err)?;
    let status = resp.status().as_u16();
    let final_url = resp.url().to_string();
    let mut headers: Vec<(String, String)> = Vec::new();
    for (k, v) in resp.headers() {
        let v = String::from_utf8_lossy(v.as_bytes()).to_string();
        match headers.iter_mut().find(|(n, _)| n == k.as_str()) {
            Some((_, existing)) => {
                existing.push_str(", ");
                existing.push_str(&v);
            }
            None => headers.push((k.as_str().to_string(), v)),
        }
    }
    let mut body = Vec::new();
    while let Some(chunk) = resp.chunk().await.map_err(map_err)? {
        if body.len() + chunk.len() > o.max_bytes {
            return Err(HttpError::new("too_large", format!("the response is larger than max_bytes ({} bytes)", o.max_bytes)));
        }
        body.extend_from_slice(&chunk);
    }
    Ok(Reply { status, redirected: final_url != url.as_str(), url: final_url, headers, body })
}

fn map_err(e: reqwest::Error) -> HttpError {
    let mut msg = e.to_string();
    let mut src = std::error::Error::source(&e);
    while let Some(s) = src {
        msg.push_str(": ");
        msg.push_str(&s.to_string());
        src = s.source();
    }
    // never echo credentials that might be part of a URL
    let kind = if e.is_timeout() {
        "timeout"
    } else if e.is_redirect() && msg.contains("allowed_hosts") || msg.contains("private address") {
        "denied"
    } else {
        "network"
    };
    HttpError::new(kind, msg)
}

async fn request(lua: &Lua, ctx: &CmdCtx, method: String, url: String, opts: Option<Table>) -> mlua::Result<(Value, Value)> {
    ctx.require(flux_api::Capability::Network)?;
    let mut o = parse_opts(lua, opts)?;
    o.max_bytes = o.max_bytes.min(ctx.res.http_max_bytes);
    let method = Method::from_bytes(method.to_uppercase().as_bytes()).map_err(|_| mlua::Error::runtime(format!("flux.http: invalid method '{method}'")))?;
    let fail = |e: HttpError| -> mlua::Result<(Value, Value)> { Ok((Value::Nil, Value::Table(error_table(lua, &e)?))) };
    let parsed = match Url::parse(&url) {
        Ok(u) => u,
        Err(e) => return fail(HttpError::new("invalid", format!("invalid URL '{url}': {e}"))),
    };
    if let Err(e) = ctx.http.policy.check(&parsed) {
        return fail(e);
    }
    let _permit = match ctx.res.http_request() {
        Ok(p) => p,
        Err(m) => return fail(HttpError::new("too_many_requests", m)),
    };
    let client = match ctx.http.client(o.follow) {
        Ok(c) => c,
        Err(e) => return fail(e),
    };
    log::debug!("[{}] http {method} {}{}", ctx.tag(), parsed.host_str().unwrap_or(""), parsed.path());
    let (timeout, signal) = (o.timeout, o.signal.clone());
    let run = tokio::time::timeout(timeout, perform(client, method, parsed, o));
    let outcome = match signal {
        Some(token) => tokio::select! {
            r = run => r,
            _ = token.cancelled() => return fail(HttpError::new("cancelled", "the request was cancelled")),
        },
        None => run.await,
    };
    let reply = match outcome {
        Ok(Ok(r)) => r,
        Ok(Err(e)) => return fail(e),
        Err(_) => return fail(HttpError::new("timeout", format!("the request timed out after {}s", timeout.as_secs_f64()))),
    };
    let t = lua.create_table()?;
    t.set("status", reply.status)?;
    t.set("ok", (200..300).contains(&reply.status))?;
    t.set("url", reply.url)?;
    t.set("redirected", reply.redirected)?;
    let headers = lua.create_table()?;
    for (k, v) in reply.headers {
        headers.set(k, v)?;
    }
    t.set("headers", headers)?;
    t.set("body", lua.create_string(&reply.body)?)?;
    Ok((Value::Table(t), Value::Nil))
}

/// Builds `flux.http._request` (the Lua prelude adds `get/post/…` and the response methods).
pub fn install(lua: &Lua, ctx: &Rc<CmdCtx>) -> mlua::Result<Table> {
    let t = lua.create_table()?;
    let c = ctx.clone();
    t.set(
        "_request",
        lua.create_async_function(move |lua, (method, url, opts): (String, String, Option<Table>)| {
            let c = c.clone();
            async move { request(&lua, &c, method, url, opts).await }
        })?,
    )?;
    Ok(t)
}
