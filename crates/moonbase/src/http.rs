//! The HTTP seam. The SDK speaks to Moonbase through an [`HttpTransport`]; the
//! bundled [`UreqTransport`] is a blocking client on ureq + rustls, and you can
//! supply your own to route through an app's existing stack.

use std::collections::BTreeMap;
use std::time::Duration;

use crate::error::Result;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Method {
    Get,
    Post,
}

impl Method {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Get => "GET",
            Self::Post => "POST",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpRequest {
    pub method: Method,
    pub url: String,
    pub headers: BTreeMap<String, String>,
    pub connect_timeout: Duration,
    /// The whole exchange, connecting included.
    pub request_timeout: Duration,
    pub body: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HttpResponse {
    pub status: u16,
    /// As received. Look names up case-insensitively.
    pub headers: Vec<(String, String)>,
    pub body: String,
}

impl HttpResponse {
    pub fn new(status: u16, body: impl Into<String>) -> Self {
        Self { status, headers: Vec::new(), body: body.into() }
    }

    pub fn with_header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.headers.push((name.into(), value.into()));
        self
    }

    /// The first header with this name, compared case-insensitively.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(key, _)| key.eq_ignore_ascii_case(name)).map(|(_, value)| value.as_str())
    }

    pub(crate) fn is_success(&self) -> bool {
        (200..300).contains(&self.status)
    }
}

/// Performs one HTTP exchange.
pub trait HttpTransport: Send + Sync {
    /// Perform one exchange and return the response, whatever its status.
    ///
    /// When no response arrives at all (DNS, a refused connection, TLS, a timeout,
    /// cancellation), return [`ApiError::unreachable`](crate::ApiError::unreachable)
    /// converted into an [`Error`](crate::Error), as the bundled transport does.
    /// That is what tells "Moonbase could not be reached" apart from a failure on
    /// this machine.
    fn send(&self, request: &HttpRequest) -> Result<HttpResponse>;
}

/// A shared transport is a transport, so one can be handed to the SDK and kept too.
impl<T: HttpTransport + ?Sized> HttpTransport for std::sync::Arc<T> {
    fn send(&self, request: &HttpRequest) -> Result<HttpResponse> {
        (**self).send(request)
    }
}

#[cfg(feature = "ureq-transport")]
pub use self::ureq_transport::UreqTransport;

#[cfg(feature = "ureq-transport")]
mod ureq_transport {
    use std::io::Read;

    use super::{HttpRequest, HttpResponse, HttpTransport, Method};
    use crate::error::{ApiError, Result};

    /// Responses larger than this are cut off. Generous for a license token or a
    /// release's notes.
    const MAX_BODY_BYTES: u64 = 4 * 1024 * 1024;

    /// The bundled blocking transport: ureq with rustls, honouring the system
    /// proxy environment variables.
    ///
    /// Blocking, so call the SDK from a worker thread (or `spawn_blocking`) rather
    /// than from an async executor's own threads.
    #[derive(Debug, Clone)]
    pub struct UreqTransport {
        agent: ureq::Agent,
    }

    impl Default for UreqTransport {
        fn default() -> Self {
            Self::new()
        }
    }

    impl UreqTransport {
        pub fn new() -> Self {
            let agent = ureq::Agent::config_builder()
                // Every status is an answer the SDK interprets itself.
                .http_status_as_error(false)
                .build()
                .new_agent();
            Self { agent }
        }
    }

    fn unreachable(error: ureq::Error) -> crate::Error {
        ApiError::unreachable(format!("Could not reach the Moonbase API: {error}")).into()
    }

    impl HttpTransport for UreqTransport {
        fn send(&self, request: &HttpRequest) -> Result<HttpResponse> {
            let response = match request.method {
                Method::Get => {
                    let mut builder = self.agent.get(&request.url);
                    for (name, value) in &request.headers {
                        builder = builder.header(name, value);
                    }
                    builder
                        .config()
                        .timeout_connect(Some(request.connect_timeout))
                        .timeout_global(Some(request.request_timeout))
                        .build()
                        .call()
                }
                Method::Post => {
                    let mut builder = self.agent.post(&request.url);
                    for (name, value) in &request.headers {
                        builder = builder.header(name, value);
                    }
                    builder
                        .config()
                        .timeout_connect(Some(request.connect_timeout))
                        .timeout_global(Some(request.request_timeout))
                        .build()
                        .send(request.body.as_bytes())
                }
            }
            .map_err(unreachable)?;

            let status = response.status().as_u16();
            let headers = response
                .headers()
                .iter()
                .map(|(name, value)| (name.as_str().to_owned(), String::from_utf8_lossy(value.as_bytes()).into_owned()))
                .collect();

            let mut bytes = Vec::new();
            response
                .into_body()
                .into_reader()
                .take(MAX_BODY_BYTES)
                .read_to_end(&mut bytes)
                .map_err(|error| ApiError::unreachable(format!("Could not read the Moonbase API response: {error}")))?;

            Ok(HttpResponse { status, headers, body: String::from_utf8_lossy(&bytes).into_owned() })
        }
    }
}
