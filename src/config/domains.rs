use crate::{
    http_header::{TextOrder, sole},
    net::{TrailingDot, normalized_host},
};
use hyper::HeaderMap;

/// XML virtual-host base domains: distinct lowercase DNS names, at most 32, longest first so that
/// the most specific configured suffix owns a host that several of them match.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct VirtualHostDomains(Vec<String>);

/// Adding the domain would exceed the limit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct DomainLimit;

/// Whether [`VirtualHostDomains::insert`] added the domain.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Inserted {
    Added,
    AlreadyListed,
}

impl VirtualHostDomains {
    pub(crate) const MAX: usize = 32;

    /// Adds `domain`, a validated lowercase DNS name, unless it is listed. A listed domain is
    /// checked before the limit, so repeating one never counts against it.
    pub(crate) fn insert(&mut self, domain: String) -> Result<Inserted, DomainLimit> {
        if self.0.contains(&domain) {
            return Ok(Inserted::AlreadyListed);
        }
        if self.0.len() == Self::MAX {
            return Err(DomainLimit);
        }
        self.0.push(domain);
        // Stable, so domains of equal length keep the order they were added in.
        self.0.sort_by_key(|domain| std::cmp::Reverse(domain.len()));
        Ok(Inserted::Added)
    }
}

impl std::ops::Deref for VirtualHostDomains {
    type Target = [String];

    fn deref(&self) -> &[String] {
        &self.0
    }
}

pub(crate) fn bucket_for_authority(host: &str, domains: &[String]) -> Option<String> {
    let authority: hyper::http::uri::Authority = host.parse().ok()?;
    split_virtual_host(&authority, domains).map(|(bucket, _)| bucket)
}

/// The bucket a host selects, and the configured domain it selects that bucket under.
fn split_virtual_host<'d>(
    authority: &hyper::http::uri::Authority,
    domains: &'d [String],
) -> Option<(String, &'d str)> {
    if authority.as_str().contains('@')
        || (authority.as_str().contains(':') && authority.port_u16().is_none())
    {
        return None;
    }
    let host = normalized_host(authority.host(), TrailingDot::Preserve);
    for domain in domains {
        if host == *domain {
            return None;
        }
        if let Some(bucket) = host.strip_suffix(domain).and_then(|s| s.strip_suffix('.')) {
            return Some((bucket.to_owned(), domain));
        }
    }
    None
}

/// The API's authority behind a host that selects a virtual bucket: the configured domain, which
/// the configuration already lowercased, and the port the client reached it through.
pub(crate) fn virtual_host_api_authority(
    authority: &hyper::http::uri::Authority,
    domains: &[String],
) -> Option<hyper::http::uri::Authority> {
    let (_, domain) = split_virtual_host(authority, domains)?;
    let port = authority.as_str().strip_prefix(authority.host())?;
    format!("{domain}{port}").parse().ok()
}

pub(crate) fn request_authority(headers: &HeaderMap) -> Option<hyper::http::uri::Authority> {
    let host = sole(headers, "host").text(TextOrder::ValueFirst).ok()??;
    let authority: hyper::http::uri::Authority = host.parse().ok()?;
    if authority.as_str().contains('@')
        || (authority.as_str() != authority.host() && authority.port_u16().is_none())
    {
        return None;
    }
    Some(authority)
}

#[cfg(test)]
impl VirtualHostDomains {
    /// The list loading would build from these domains, for a configuration built in a test.
    pub(crate) fn of(domains: &[&str]) -> Self {
        let mut list = Self::default();
        for domain in domains {
            list.insert((*domain).to_owned()).unwrap();
        }
        list
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn insert_keeps_the_longest_first_and_counts_only_new_domains() {
        let mut domains = VirtualHostDomains::default();
        for domain in ["a.test", "storage.test", "b.test", "c.test"] {
            assert_eq!(domains.insert(domain.to_owned()), Ok(Inserted::Added));
        }
        // Domains of equal length keep the order they were added in.
        assert_eq!(*domains, ["storage.test", "a.test", "b.test", "c.test"]);

        let mut full = VirtualHostDomains::default();
        for index in 0..VirtualHostDomains::MAX {
            assert_eq!(full.insert(format!("d{index}.test")), Ok(Inserted::Added));
        }
        // A listed domain is recognized before the limit applies.
        assert_eq!(
            full.insert("d0.test".to_owned()),
            Ok(Inserted::AlreadyListed)
        );
        assert_eq!(full.insert("extra.test".to_owned()), Err(DomainLimit));
        assert_eq!(full.len(), VirtualHostDomains::MAX);
        assert!(!full.iter().any(|domain| domain == "extra.test"));
    }
}
