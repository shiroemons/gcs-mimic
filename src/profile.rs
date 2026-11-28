//! Whether gcs-mimic answers as Cloud Storage does or keeps fake-gcs-server's behavior where the
//! two differ. Bucket names, bucket resources and the adapters read the profile, so it lives
//! below the configuration that sets it.

use crate::choice::string_enum;

// Only parsed: no output names the profile.
string_enum! {
    table
    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
    pub(crate) enum CompatProfile {
        #[default]
        Native => "native",
        /// fake-gcs-server's behavior where it differs from Cloud Storage, for migrations.
        FakeGcsServer => "fake-gcs-server",
    };
    from_str(crate::choice::InvalidChoice,
        "legacy-emulator" => Err(crate::choice::InvalidChoice(
            "compat_profile=legacy-emulator was renamed; set compat_profile=fake-gcs-server",
        )),
        _ => Err(crate::choice::InvalidChoice("compat_profile must be native or fake-gcs-server")))
}
