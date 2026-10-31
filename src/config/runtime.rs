use super::{Config, Scheme};
use crate::url::{PublicOrigin, UrlError, normalize_base_url};
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

#[derive(Clone, Debug)]
pub(crate) struct EndpointSnapshot {
    pub(crate) api_base_url: String,
    pub(crate) public_base_url: Option<PublicOrigin>,
    pub(crate) xml_virtual_host_domains: Vec<String>,
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
        let mut current = self.current.write().map_err(|_| EndpointError::Poisoned)?;
        Ok(current
            .get_or_insert_with(|| {
                Arc::new(EndpointSnapshot {
                    api_base_url: api_base_url.to_owned(),
                    public_base_url: config.public_base_url.clone(),
                    xml_virtual_host_domains: config.xml_virtual_host_domains.clone(),
                })
            })
            .clone())
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
            candidate.api_base_url = normalize_base_url(value)?;
            let uri: hyper::Uri = candidate
                .api_base_url
                .parse()
                .map_err(|_| UrlError::InvalidBaseUrl)?;
            let scheme = uri.scheme_str().ok_or(UrlError::MissingScheme)?;
            let authority = uri.authority().ok_or(UrlError::MissingAuthority)?;
            PublicOrigin::parse(&format!("{scheme}://{authority}"))?;
        }
        if let Some(value) = public_host.filter(|value| !value.is_empty()) {
            let authority: hyper::http::uri::Authority =
                value.parse().map_err(|_| EndpointError::PublicHost)?;
            if authority.as_str().contains('@') || value.contains(['/', '?', '#']) {
                return Err(EndpointError::PublicHost);
            }
            let scheme = if config.scheme == Scheme::Http {
                "http"
            } else {
                "https"
            };
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
        let uri: hyper::Uri = candidate
            .api_base_url
            .parse()
            .map_err(|_| UrlError::InvalidBaseUrl)?;
        if uri.authority().is_some_and(|authority| {
            crate::xml::bucket_for_authority(
                authority.as_str(),
                &candidate.xml_virtual_host_domains,
            )
            .is_some()
        }) {
            return Err(EndpointError::ApiVirtualHost);
        }
        *current = Some(Arc::new(candidate));
        Ok(())
    }
}

#[cfg(test)]
mod tests;
