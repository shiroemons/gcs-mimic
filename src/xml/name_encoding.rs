//! How an XML listing writes names: as stored, or URL-encoded on `encoding-type=url`.

use super::XmlWriter;

/// How names are written: as stored, or with `encoding-type=url` in the listing's style.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum NameEncoding {
    Plain,
    /// Object listing version 1: every byte but RFC 3986 unreserved ones becomes `%XX`, `/`
    /// included. Version 1 with `encoding-type` was not recorded.
    Percent,
    /// Object listing version 2 and the multipart upload listing: form style, keeping `/` and
    /// writing a space as `+` (recorded 2026-11-16).
    Form,
}

impl NameEncoding {
    pub(super) fn element(self, w: &mut XmlWriter, name: &str, value: &str) {
        match self {
            Self::Plain => w.element(name, value),
            Self::Percent => w.element(
                name,
                &crate::url::encode_object_name(value).replace('/', "%2F"),
            ),
            Self::Form => w.element(name, &crate::url::encode_listing_name(value)),
        }
    }

    /// `EncodingType` names the encoding of a URL-encoded listing.
    pub(super) fn write_encoding_type(self, w: &mut XmlWriter) {
        match self {
            Self::Plain => {}
            Self::Percent | Self::Form => w.element("EncodingType", "url"),
        }
    }
}
