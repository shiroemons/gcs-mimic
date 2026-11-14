/// XML virtual-host base domains: distinct lowercase DNS names, at most 32, longest first so that
/// the most specific configured suffix owns a host that several of them match.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct VirtualHostDomains(Vec<String>);

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
