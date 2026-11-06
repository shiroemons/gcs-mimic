use super::Config;
use crate::url::{PublicOrigin, UrlError, normalize_base_url};
use hyper::http::uri::Authority;
use std::{
    fmt,
    sync::{Arc, RwLock},
};

/// Shared endpoint state; each request retains one immutable snapshot across awaits.
#[derive(Clone, Debug, Default)]
pub struct RuntimeEndpoints {
    current: Arc<RwLock<Option<Arc<EndpointSnapshot>>>>,
    declared_domains: Option<Arc<[String]>>,
}

/// Whether an operator chose the advertised API base.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ApiBaseSource {
    /// `api_base_url` or a runtime `externalUrl` fixed it, so no request can change it.
    Explicit,
    /// Nothing fixed it. One address cannot reach the server from both the host and a peer
    /// container, so each request advertises the authority it reached the server through.
    Derived,
}

/// The transport of the connection a request arrived on. `scheme = both` serves HTTP and HTTPS
/// listeners side by side, so the configured scheme cannot name it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ConnectionScheme {
    Http,
    Https,
}

impl ConnectionScheme {
    fn as_str(self) -> &'static str {
        match self {
            Self::Http => "http",
            Self::Https => "https",
        }
    }
}

/// An absolute API base and the authority it names.
#[derive(Clone, Debug)]
struct ApiBase {
    url: String,
    authority: Authority,
}

impl TryFrom<&str> for ApiBase {
    type Error = UrlError;

    fn try_from(url: &str) -> Result<Self, UrlError> {
        let uri: hyper::Uri = url.parse().map_err(|_| UrlError::InvalidBaseUrl)?;
        let authority = uri.authority().ok_or(UrlError::MissingAuthority)?.clone();
        Ok(Self {
            url: url.to_owned(),
            authority,
        })
    }
}

impl ApiBase {
    /// A base from a runtime `externalUrl`, whose origin must also be a valid public origin.
    fn from_external_url(value: &str) -> Result<Self, UrlError> {
        let url = normalize_base_url(value)?;
        let uri: hyper::Uri = url.parse().map_err(|_| UrlError::InvalidBaseUrl)?;
        let scheme = uri.scheme_str().ok_or(UrlError::MissingScheme)?;
        let authority = uri.authority().ok_or(UrlError::MissingAuthority)?;
        PublicOrigin::parse(&format!("{scheme}://{authority}"))?;
        Ok(Self {
            authority: authority.clone(),
            url,
        })
    }
}

/// The endpoint settings every request shares. Nothing here comes from a request, so a trust
/// decision that reads only this type cannot be steered by a client's Host.
#[derive(Clone, Debug)]
pub(crate) struct EndpointSnapshot {
    /// The explicit base, or the one the bound listener implies.
    fixed_api_base: ApiBase,
    api_base_source: ApiBaseSource,
    pub(crate) public_base_url: Option<PublicOrigin>,
    pub(crate) xml_virtual_host_domains: Vec<String>,
}

impl EndpointSnapshot {
    /// Endpoints with an explicit API base, for tests that build a request context by hand.
    #[cfg(test)]
    pub(crate) fn explicit(
        api_base_url: &str,
        public_base_url: Option<PublicOrigin>,
        xml_virtual_host_domains: Vec<String>,
    ) -> Self {
        Self {
            fixed_api_base: ApiBase::try_from(api_base_url).unwrap(),
            api_base_source: ApiBaseSource::Explicit,
            public_base_url,
            xml_virtual_host_domains,
        }
    }

    /// One request's view with no request authority, for tests that have no request head.
    #[cfg(test)]
    pub(crate) fn without_request(self: Arc<Self>) -> RequestEndpoints {
        RequestEndpoints {
            snapshot: self,
            requested_api_base: None,
        }
    }

    pub(crate) fn fixed_api_authority(&self) -> &Authority {
        &self.fixed_api_base.authority
    }

    /// The view one request holds across its awaits.
    pub(crate) fn for_request(
        self: Arc<Self>,
        scheme: ConnectionScheme,
        uri: &hyper::Uri,
        headers: &hyper::HeaderMap,
    ) -> RequestEndpoints {
        let requested_api_base = match self.api_base_source {
            ApiBaseSource::Explicit => None,
            ApiBaseSource::Derived => self.requested_api_base(scheme, uri, headers),
        };
        RequestEndpoints {
            snapshot: self,
            requested_api_base,
        }
    }

    /// The authority the request addressed on its connection's own scheme, when usable. Forwarded
    /// headers are ignored because no proxy is trusted.
    fn requested_api_base(
        &self,
        scheme: ConnectionScheme,
        uri: &hyper::Uri,
        headers: &hyper::HeaderMap,
    ) -> Option<ApiBase> {
        // RFC 9112 section 3.2.2: an authority in the request target overrides Host.
        let requested = uri
            .authority()
            .cloned()
            .or_else(|| crate::xml::request_authority(headers))?;
        // A host that selects an XML virtual bucket names that bucket and not the API, for the
        // same reason a configured base may not select one. The domain under it is the name that
        // reached this server, where the bound listener's address may not reach it from a peer.
        let authority =
            crate::xml::virtual_host_api_authority(&requested, &self.xml_virtual_host_domains)
                .unwrap_or(requested);
        crate::url::is_advertisable_authority(&authority).then(|| ApiBase {
            url: format!("{}://{authority}", scheme.as_str()),
            authority,
        })
    }
}

/// One request's view of the endpoints. Only this type can hold a base derived from a request, so
/// a function that takes [`EndpointSnapshot`] instead cannot be handed one.
#[derive(Clone, Debug)]
pub(crate) struct RequestEndpoints {
    snapshot: Arc<EndpointSnapshot>,
    /// Set only when no base is explicit and the request addressed a usable authority.
    requested_api_base: Option<ApiBase>,
}

impl RequestEndpoints {
    /// An explicit API base, for tests that build a request context by hand.
    #[cfg(test)]
    pub(crate) fn explicit(api_base_url: &str) -> Self {
        Arc::new(EndpointSnapshot::explicit(api_base_url, None, Vec::new())).without_request()
    }

    fn api_base(&self) -> &ApiBase {
        self.requested_api_base
            .as_ref()
            .unwrap_or(&self.snapshot.fixed_api_base)
    }

    /// The base this request's links use. The client may have chosen it, so it renders links and
    /// never decides trust.
    pub(crate) fn api_base_url(&self) -> &str {
        &self.api_base().url
    }

    /// The authority of [`Self::api_base_url`], with the same limits on its use.
    pub(crate) fn api_authority(&self) -> &Authority {
        &self.api_base().authority
    }

    pub(crate) fn api_base_source(&self) -> ApiBaseSource {
        self.snapshot.api_base_source
    }

    /// The settings no request can influence.
    pub(crate) fn snapshot(&self) -> &EndpointSnapshot {
        &self.snapshot
    }
}

#[derive(Debug)]
pub(crate) enum EndpointError {
    Url(UrlError),
    PublicHost,
    DomainLimit,
    ApiVirtualHost,
    Poisoned,
}

impl fmt::Display for EndpointError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Url(error) => error.fmt(f),
            Self::PublicHost => f.write_str("publicHost must be a host authority, for example localhost:4443; omit the scheme, path and credentials"),
            Self::DomainLimit => f.write_str("publicHost and xml_virtual_host_domains must contain at most 32 distinct DNS names"),
            Self::ApiVirtualHost => f.write_str("externalUrl host selects an XML virtual bucket; use a host outside the configured XML domains or the base domain itself"),
            Self::Poisoned => f.write_str("runtime endpoint state is unavailable; restart the local emulator"),
        }
    }
}

impl std::error::Error for EndpointError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Url(error) => Some(error),
            _ => None,
        }
    }
}

impl From<UrlError> for EndpointError {
    fn from(error: UrlError) -> Self {
        Self::Url(error)
    }
}

impl RuntimeEndpoints {
    pub(super) fn new(domains: Vec<String>) -> Self {
        Self {
            declared_domains: Some(domains.into()),
            ..Self::default()
        }
    }

    pub(crate) fn snapshot(
        &self,
        config: &Config,
        api_base_url: &str,
    ) -> Result<Arc<EndpointSnapshot>, EndpointError> {
        if let Some(current) = self
            .current
            .read()
            .map_err(|_| EndpointError::Poisoned)?
            .as_ref()
        {
            return Ok(current.clone());
        }
        let initial = Arc::new(EndpointSnapshot {
            fixed_api_base: ApiBase::try_from(api_base_url)?,
            api_base_source: match config.api_base_url {
                Some(_) => ApiBaseSource::Explicit,
                None => ApiBaseSource::Derived,
            },
            public_base_url: config.public_base_url.clone(),
            xml_virtual_host_domains: config.xml_virtual_host_domains.clone(),
        });
        let mut current = self.current.write().map_err(|_| EndpointError::Poisoned)?;
        Ok(current.get_or_insert(initial).clone())
    }

    pub(crate) fn update(
        &self,
        config: &Config,
        initial_api_base_url: &str,
        external_url: Option<&str>,
        public_host: Option<&str>,
    ) -> Result<(), EndpointError> {
        let initial = self.snapshot(config, initial_api_base_url)?;
        let mut current = self.current.write().map_err(|_| EndpointError::Poisoned)?;
        // Read the latest committed pair under the write lock to avoid losing a concurrent update.
        let mut candidate = current.as_ref().unwrap_or(&initial).as_ref().clone();
        if let Some(value) = external_url.filter(|value| !value.is_empty()) {
            candidate.fixed_api_base = ApiBase::from_external_url(value)?;
            candidate.api_base_source = ApiBaseSource::Explicit;
        }
        if let Some(value) = public_host.filter(|value| !value.is_empty()) {
            let authority: Authority = value.parse().map_err(|_| EndpointError::PublicHost)?;
            if authority.as_str().contains('@') || value.contains(['/', '?', '#']) {
                return Err(EndpointError::PublicHost);
            }
            let scheme = config.scheme.primary();
            candidate.public_base_url = Some(PublicOrigin::parse(&format!("{scheme}://{value}"))?);
            candidate.xml_virtual_host_domains = self
                .declared_domains
                .as_deref()
                .unwrap_or(&config.xml_virtual_host_domains)
                .to_vec();
            if let Some(domain) = candidate
                .public_base_url
                .as_ref()
                .and_then(PublicOrigin::dns_name)
                && !candidate
                    .xml_virtual_host_domains
                    .iter()
                    .any(|entry| entry == domain)
            {
                if candidate.xml_virtual_host_domains.len() == 32 {
                    return Err(EndpointError::DomainLimit);
                }
                candidate.xml_virtual_host_domains.push(domain.to_owned());
                candidate
                    .xml_virtual_host_domains
                    .sort_by_key(|domain| std::cmp::Reverse(domain.len()));
            }
        }
        if crate::xml::bucket_for_authority(
            candidate.fixed_api_base.authority.as_str(),
            &candidate.xml_virtual_host_domains,
        )
        .is_some()
        {
            return Err(EndpointError::ApiVirtualHost);
        }
        *current = Some(Arc::new(candidate));
        Ok(())
    }
}

#[cfg(test)]
mod tests;
