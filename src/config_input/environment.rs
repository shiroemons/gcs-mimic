/// Withdrawn client variables share one definition across server and helper diagnostics.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RenamedEnvironmentVariable {
    Endpoint,
    GrpcEndpoint,
    SyncToken,
    Binary,
    Port,
    Image,
    TlsDir,
}

impl RenamedEnvironmentVariable {
    pub const ALL: [Self; 7] = [
        Self::Endpoint,
        Self::GrpcEndpoint,
        Self::SyncToken,
        Self::Binary,
        Self::Port,
        Self::Image,
        Self::TlsDir,
    ];

    const fn definition(self) -> (&'static str, &'static str, &'static str) {
        match self {
            Self::Endpoint => (
                "ENDPOINT",
                "client examples",
                "use GCS_MIMIC_LISTEN for server binding",
            ),
            Self::GrpcEndpoint => (
                "GRPC_ENDPOINT",
                "client examples",
                "use GCS_MIMIC_GRPC_LISTEN for server binding",
            ),
            Self::SyncToken => (
                "SYNC_TOKEN",
                "the sync CLI",
                "the server does not read client tokens",
            ),
            Self::Binary => (
                "BINARY",
                "the Go subprocess package and signed-browser script",
                "there is no server equivalent",
            ),
            Self::Port => (
                "PORT",
                "Compose host port publication",
                "use GCS_MIMIC_LISTEN for server binding",
            ),
            Self::Image => (
                "IMAGE",
                "Compose image selection",
                "there is no server equivalent",
            ),
            Self::TlsDir => (
                "TLS_DIR",
                "Compose mount selection",
                "use GCS_MIMIC_TLS_CERT and GCS_MIMIC_TLS_KEY for server certificates",
            ),
        }
    }

    /// The withdrawn variable that a server setting name such as `port` stands for, the name
    /// being the lowercase suffix after `GCS_MIMIC_`.
    pub(crate) fn from_setting_name(name: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|variable| variable.suffix().eq_ignore_ascii_case(name))
    }

    pub const fn suffix(self) -> &'static str {
        self.definition().0
    }

    pub const fn server_alternative(self) -> &'static str {
        self.definition().2
    }

    pub fn old_name(self) -> String {
        format!("GCS_MIMIC_{}", self.suffix())
    }

    pub fn name(self) -> String {
        format!("GCSM_{}", self.suffix())
    }

    pub fn fix(self) -> String {
        let (suffix, purpose, alternative) = self.definition();
        format!("GCS_MIMIC_{suffix} was renamed to GCSM_{suffix} for {purpose}; {alternative}.")
    }
}
