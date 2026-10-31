#[cfg(test)]
pub(super) use crate::storage::catalog::fault::Step;

#[cfg(test)]
pub(super) fn hit(data: &std::path::Path, step: Step) -> Result<(), super::Error> {
    crate::storage::catalog::fault::hit(data, step)
        .map_err(crate::storage::catalog::objects::object_unavailable)
        .map_err(super::Error::from)
}
