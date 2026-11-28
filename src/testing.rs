//! What the integration tests in `tests/` and the programs in `examples/` use from the library,
//! under the paths they name. The crate is not published and the rest of it is private, so these
//! re-exports and [`crate::app`] are its whole Rust surface: a module that moves changes its path
//! here rather than in every test, and the lint reports an item nothing uses as dead code.

pub mod auth {
    pub use crate::auth::Auth;

    pub mod signature {
        pub use crate::auth::signature::{
            SignUrlRequest, encode_query_component, parse_timestamp, sign_policy, sign_url,
            timestamp,
        };
    }

    pub mod test_signer {
        pub use crate::auth::test_signer::{ID, PRIVATE_KEY_PEM};
    }
}

pub mod bucket {
    pub use crate::bucket::CatalogError;
}

pub mod clock {
    pub use crate::clock::{Clock, SystemClock};
}

pub mod config {
    pub use crate::config::Config;
}

pub mod config_input {
    pub use crate::config_input::RenamedEnvironmentVariable;
}

pub mod diagnostics {
    pub use crate::diagnostics::Diagnostics;
}

pub mod import {
    pub use crate::import::{
        BodyStream, ChangedField, Fingerprint, ImportError, ImportFailure, MetadataMode, Options,
        SourcePlatform, Summary, execute, inspect,
    };
}

pub mod json {
    pub use crate::json::{ApiError, parse_insert};
}

pub mod mime {
    pub use crate::mime::SeedContentType;
}

pub mod object {
    pub use crate::object::{
        CustomTime, ExpectedHashes, MapPatch, ObjectRecord, Patch, UploadChunk, WriteMode,
    };
}

pub mod seed {
    pub use crate::seed::{SeedOutcome, import};
}

pub mod server {
    pub use crate::{
        server::{Stop, TcpListeners, serve},
        service::state::State,
    };
}

pub mod storage {
    pub use crate::storage::{
        Catalog, CatalogBacking, CatalogOpenError, CatalogOpenFailure, CatalogOptions,
        RecoveredCatalog,
    };

    pub mod admin {
        pub use crate::storage::admin::{
            AdminError, BucketDefinition, BucketName, GenerationCondition, ObjectName, Origin,
            Publication, PublishMode,
        };
    }
}

pub mod tls {
    pub use crate::tls::{DEVELOPMENT_CA_PEM, ServerSecurity, client_config};
}

pub mod url {
    pub use crate::url::{encode_object_name, public_object_url};
}
