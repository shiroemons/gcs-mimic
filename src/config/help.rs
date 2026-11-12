//! The server's `--help` text. It lives beside the settings it lists so that tests can compare
//! the two, and the documents that repeat the list, without a running binary.

/// What `gcs-mimic --help` prints.
pub const HELP: &str = "gcs-mimic — local Google Cloud Storage emulator
Usage: gcs-mimic [serve] [--config PATH] [OPTIONS] (serve is implied when the first argument is an option)
       gcs-mimic sync status|findings|deletions|conflicts|rescan|approve|resolve|wait --endpoint URL [OPTIONS]
       gcs-mimic healthcheck [--url URL] [--ca-cert PATH]
       gcs-mimic import fake-gcs-server --source DIR --data-dir NEW [OPTIONS] (see import fake-gcs-server --help)
       gcs-mimic tls ca
       gcs-mimic url public --base-url URL --bucket NAME --object NAME
       gcs-mimic url sign --base-url URL --bucket NAME --object NAME --method GET --expires 15m [--signer ID --private-key PATH]
       gcs-mimic test-signer credentials (prints the published built-in test signer's service account JSON)
       gcs-mimic --help | -h | -help | --version

Common options (one or two hyphens; --name VALUE or --name=VALUE; CLI > GCS_MIMIC_<UPPER_SNAKE_CASE> environment > JSON config > default):
  --scheme MODE                auto (auto|http|https|both; auto serves HTTP and HTTPS on the primary port; both uses HTTPS there and HTTP on --port-http)
  --port N                     4443 (keeps the listen IP; CLI only, GCS_MIMIC_LISTEN in the environment)
  --port-http N                8000 (native HTTP port; requires scheme=both; CLI only)
  --host IP                    127.0.0.1, 0.0.0.0 in the image (keeps the listen port; CLI only)
  --backend MODE               disk (disk|memory; memory never opens data-dir, loses data at exit)
  --seed-dir PATH              none (read-only fixtures; replayed on every startup; image: /data)
  --log-level LEVEL            info (debug|info|warn|error; warning aliases warn)

Other serve options:
  --default-location US|EU|ASIA  Native bucket creation default (US); explicit locations win
  --default-soft-delete-retention-seconds N  0 (new buckets without a softDeletePolicy; 0 keeps no deleted data, 1..3155760000 retains it; 604800 is the Cloud Storage default)
  --lifecycle-execution MODE   enabled|disabled (default: disabled with sync-dir, enabled otherwise; enabled also deletes synchronized host files)
  --listen IP:PORT             127.0.0.1:4443
  --listen-http IP:PORT        primary listen IP:8000 (HTTP listener; scheme=both only)
  --grpc-listen IP:PORT        none (optional separate plaintext h2c gRPC listener; e.g. 127.0.0.1:4444)
  --trusted-listen IP:PORT     none (strict only: extra plain HTTP listener for the application's own servers; requests without credentials act as --trusted-principal, browser requests get 403, Host names other than IP literals, localhost names and single labels get 421; never publish it)
  --trusted-principal ID       built-in trusted-app@gcs-mimic.invalid with roles/storage.admin in the buckets document's projects and seed_project (registered identity, or the enabled test signer, that requests without credentials on --trusted-listen act as; requires it)
  --trusted-allowed-hosts LIST none (comma-separated dotted DNS names --trusted-listen also accepts as Host, such as a service alias; at most 32; requires --trusted-listen)
  --unix-socket PATH           none (plain HTTP Unix socket; private 0700 parent, scheme=auto or http, no TCP/gRPC/TLS)
  --tls-cert PATH              embedded development certificate (custom PEM chain; at most 1 MiB)
  --tls-key PATH               embedded public test key (custom PEM private key; at most 1 MiB)
  --tls-handshake-timeout-ms N  5000 (0 disables; HTTPS listener)
  --data-dir PATH              data (exclusive persistent catalog)
  --memory-max-bytes N          half of available memory, 64 MiB to 1 GiB (RAM blob bytes plus chunk overhead; 1..1099511627776)
  --memory-catalog-max-bytes N  16777216 (RAM SQLite pages; 1048576..1099511627776; separate from RSS)
  --default-seed-dir PATH      none (seed root when seed_dir is unset and PATH exists; image: /data)
  --seed-project ID            local-project (owning project for seeded buckets)
  --max-seed-entries N         0 (files and directories in seed_dir; memory grows with them; 0 is unlimited)
  --max-seed-depth N           0 (0..1024; directory levels below a seed bucket; 0 is unlimited)
  --seed-content-type MODE     extension (extension|octet-stream; pinned MIME table)
  --sync-dir PATH              none (existing working tree; initial export after ready)
  --sync-delete-guard BOOL     true (deletion ingestion pending)
  --sync-reinitialize ID       startup only (exact catalog ID; missing marker only)
  --max-sync-entries N         0 (files and directories in sync_dir, also the watches Linux may use; scans take longer, not more memory; 0 is unlimited)
  --max-sync-depth N           0 (0..1024; directory levels below sync_dir, counting the bucket; 0 is unlimited)
  --buckets-file PATH          none (create missing buckets before seed; JSON v1, at most 1 GiB, or 1 KiB per allowed bucket and at least 32 MiB under a bucket limit)
  --buckets JSON               none (the --buckets-file document itself, with the same format and limits; not with --buckets-file)
  --buckets-file-mode MODE     reconcile for --buckets, create for --buckets-file (create|reconcile; reconcile also applies each entry's written fields to its existing bucket; requires --buckets-file or --buckets)
  --max-buckets-per-batch N    0 (buckets one buckets document or one seed may declare, not all buckets; memory grows with them; 0 is unlimited)
  --api-base-url URL           Each request's Host, else the bound listener (set to fix the advertised endpoint)
  --public-base-url URL        none (public origin; XML host routing and explicit-config URL helpers)
  --admin-enabled BOOL         false (local management API; isolate the listener when enabled)
  --admin-principal ID         none (strict instance administrator for local management except sync; required with --admin-enabled in strict mode)
  --admin-allowed-hosts LIST   none (comma-separated DNS names allowed to reach /_internal management; at most 32)
  --local-creator-email EMAIL local-user@gcs-mimic.invalid (permissive native: creator recorded as owner and ACL OWNER of new objects; empty disables; grants nothing)
  --xml-virtual-host-domains LIST none (comma-separated XML base DNS names; at most 32)
  --max-connections N          1024 (1..1048576; shared across listeners and TLS handshakes; raises the soft open-file limit to match)
  --max-grpc-streams N         100 (1..65536; concurrent streams per gRPC connection; each can buffer a request message of up to 4 MiB and its encoded response, so lower it to limit memory; requires --grpc-listen)
  --max-object-bytes N         5497558138880 (1..5497558138880; streamed upload cap)
  --max-resumable-sessions N   0 (0..4294967295; active resumable uploads; 0 is unlimited)
  --max-rewrite-sessions N     0 (0..4294967295; unfinished rewrites; 0 is unlimited)
  --worker-threads N           available CPUs from 2 to 8 (1..1024)
  --max-blocking-threads N     8 (1..1024)
  --header-read-timeout-ms N   30000 (0 disables; request head arrival, gRPC handshake and HTTP/2 ping acknowledgement)
  --body-inactivity-timeout-ms N 0 (0 disables; silence while a request body, response body or gRPC stream is in flight)
  --request-timeout-ms N       180000 (0 disables; handler time before a response starts, excluding request body waits)
  --connection-timeout-ms N    none (alias; sets the three settings above, and each of them named explicitly wins)
  --keep-alive-timeout-ms N    120000 (0 disables; idle time between HTTP/1.1 requests and on gRPC connections)
  --object-request-timeout-ms N 0 (0 disables; absolute upload/media deadline)
  --catalog-wait-timeout-ms N  120000 (0 disables; upload admission, advisory and commit waits, a busy upload session, and reset, reseed and seed waits)
  --shutdown-timeout-ms N      5000 (0 disables; waits for unfinished requests without a limit)
  --auth-mode MODE             permissive (permissive|strict; strict needs --identity-file or --identities unless --trusted-listen has no --trusted-principal or --test-signer is enabled)
  --compat-profile PROFILE     native (native|legacy-emulator; independent of authentication)
  --cors-fallback MODE         allow-all when permissive, none when strict (none|allow-all; XML/public CORS for buckets without stored rules)
  --signed-host-port MODE      lenient where the test signer is enabled, else exact (exact|lenient; lenient also accepts a V4 signature whose host omits the request's port, as Go storage signs)
  --identity-file PATH         none (JSON identity registry, at most 16 MiB; strict mode, or only id and public_keys with --signed-urls verify)
  --identities JSON            none (the --identity-file registry itself, with the same format and limits; not with --identity-file)
  --signed-urls MODE           ignore when permissive, verify when strict (ignore|verify; covers signed URLs, V2/V4/AWS4 Authorization headers and POST policies; permissive verify rejects bad or unknown-key signatures, such as S3 clients' made-up HMAC keys, then grants permissive access)
  --test-signer MODE           enabled with permissive --signed-urls verify or when strict identities declare its id, else disabled (enabled|disabled; built-in test-signer@gcs-mimic.invalid, whose private key is published; undeclared in strict mode it holds roles/storage.objectUser in the buckets document's projects and seed_project; strict enabling warns; enabled needs signatures to verify; local tests only)
  --pubsub-emulator-host HOST:PORT none (explicit local HTTP Pub/Sub emulator; PUBSUB_EMULATOR_HOST also accepted)
  --event-pubsub-project-id ID  none (global notification topic project; needs local emulator)
  --event-pubsub-topic ID       none (global notification topic name)
  --event-bucket NAME           all buckets (global notification filter)
  --event-object-prefix PREFIX all object names (global notification filter)
  --event-list LIST            finalize (comma-separated finalize,delete,metadataUpdate,archive; none disables the global rule)
  GCS_MIMIC_CONFIG selects a JSON config file; --config overrides it.

fake-gcs-server flags preserve working requests or fail before startup:
  Accepted: --backend filesystem -> --backend disk; --public-host -> --public-base-url;
  --data -> --seed-dir; --cert-location -> --tls-cert; --private-key-location -> --tls-key.
  --event.pubsub-project-id, --event.pubsub-topic, --event.bucket, --event.object-prefix
  and --event.list use native --event-* names and require a configured local Pub/Sub emulator.
  Refused: --filesystem-root, --external-url, --cors-headers, --location. Each error names
  the native alternative and the semantic difference; all refused flags are reported together.
  Accepted translations emit native_args as a JSON argument array at info level. A shell line
  is shown only without quotes, backslashes or controls in values; unsafe URLs are redacted.
  --host, --port, --port-http, --scheme and --log-level are native options.

HTTP/1 probes and scoped JSON/XML APIs are available over HTTP or TLS 1.2/1.3.
With scheme=auto each connection's first byte selects TLS or plain HTTP on the same port.
HTTPS defaults to public local-development credentials for localhost, gcs and loopback IPs.
Supply both --tls-cert and --tls-key to replace them. Export the root with gcs-mimic tls ca.
Clients must trust the issuing CA and match a certificate SAN; public test keys provide no
identity protection outside isolated local development. Healthcheck HTTPS trusts the embedded
development CA by default; --ca-cert PATH replaces that trust with a supplied CA bundle.
HTTP healthcheck rejects --ca-cert. Probes accept /_internal/health, /_internal/ready
or the compatible readiness path /_internal/healthcheck.
Without --url, healthcheck reads GCS_MIMIC_* and the selected JSON config, uses ready,
maps wildcard listeners to loopback and probes over HTTP unless scheme=https. Server CLI
overrides require an explicit --url or matching environment/configuration settings.
Authentication is permissive by default; strict mode checks local bearer tokens and grants.";

#[cfg(test)]
mod tests {
    use super::HELP;
    use crate::{config::Setting, config_input::NativeFlag};
    use std::collections::BTreeSet;

    /// The options a server accepts, spelled as flags. `config` selects the file that supplies
    /// the others, so the help names it in its usage line rather than among the options.
    fn options() -> BTreeSet<String> {
        Setting::ALL
            .iter()
            .filter(|setting| **setting != Setting::Config)
            .map(|setting| setting.as_str().replace('_', "-"))
            .chain(NativeFlag::ALL.iter().map(|flag| flag.as_str().to_owned()))
            .collect()
    }

    /// The options the help lists, one per line, between its option headings and the
    /// fake-gcs-server section, whose lines name translated flags instead.
    fn listed(help: &str) -> BTreeSet<&str> {
        help.lines()
            .skip_while(|line| !line.starts_with("Common options"))
            .take_while(|line| !line.starts_with("fake-gcs-server flags"))
            .filter_map(|line| line.strip_prefix("  --"))
            .filter_map(|line| line.split_whitespace().next())
            .collect()
    }

    #[test]
    fn help_names_every_option_and_nothing_else() {
        let listed: BTreeSet<String> = listed(HELP).into_iter().map(str::to_owned).collect();
        assert_eq!(listed, options());
        assert!(HELP.contains("[--config PATH]"));
    }

    /// The option table of the native binary guide: each row's flag and, where the row names
    /// one, its environment variable.
    fn native_binary_rows() -> Vec<(&'static str, Option<&'static str>)> {
        include_str!("../../docs/en/native-binary.md")
            .lines()
            .filter_map(|line| line.strip_prefix("| `--"))
            .filter_map(|row| {
                let (flag, rest) = row.split_once("` | ")?;
                let variable = rest
                    .strip_prefix('`')
                    .and_then(|rest| rest.split_once("` |"))
                    .map(|(variable, _)| variable);
                Some((flag, variable))
            })
            .collect()
    }

    #[test]
    fn the_native_binary_table_names_every_option_once_with_its_variable() {
        let rows = native_binary_rows();
        let flags: BTreeSet<String> = rows.iter().map(|(flag, _)| (*flag).to_owned()).collect();
        assert_eq!(flags.len(), rows.len(), "a flag has two rows");
        let mut expected = options();
        expected.insert(Setting::Config.as_str().to_owned());
        assert_eq!(flags, expected);
        for (flag, variable) in rows {
            if let Some(variable) = variable {
                let setting = format!("GCS_MIMIC_{}", flag.replace('-', "_").to_uppercase());
                assert_eq!(variable, setting, "--{flag}");
            }
        }
    }

    /// The settings `llms.txt` lists under "Every server option", one bulleted group per line.
    fn llms_settings() -> Vec<String> {
        include_str!("../../llms.txt")
            .lines()
            .skip_while(|line| !line.starts_with("**Every server option.**"))
            .skip_while(|line| !line.starts_with("- "))
            .take_while(|line| line.starts_with("- "))
            .filter_map(|line| line.split_once(": "))
            .flat_map(|(_, names)| names.split(", "))
            .map(|name| {
                name.trim_matches('`')
                    .trim_start_matches("--")
                    .replace('-', "_")
            })
            .collect()
    }

    #[test]
    fn llms_txt_lists_every_setting_once() {
        let listed = llms_settings();
        let unique: BTreeSet<&str> = listed.iter().map(String::as_str).collect();
        assert_eq!(unique.len(), listed.len(), "a setting is listed twice");
        let settings: BTreeSet<&str> = Setting::ALL
            .iter()
            .map(|setting| setting.as_str())
            .collect();
        assert_eq!(unique, settings);
    }

    #[test]
    fn help_output_keeps_its_exact_bytes() {
        let digest = ring::digest::digest(&ring::digest::SHA256, format!("{HELP}\n").as_bytes());
        let digest = digest
            .as_ref()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        assert_eq!(
            digest,
            "8a156d8d0e37ab5a4a352fd73c5124197d76a280f35702404b67df2ae18b74d7"
        );
    }
}
