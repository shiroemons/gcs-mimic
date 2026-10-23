# fake-gcs-server configuration verification

Checked on **2026-10-24** against **v1.56.1**, commit
`896ece51e49eed0946089f3089f6343855c0d106`, in a temporary source checkout.
The source archive SHA-256 is
`13fa1ed9cff0216baedaedb162b3cf37575bba955fb55fd4b76acb9b818494dc`.

The inventory was traced to every production `FAKE_GCS_` read in
`internal/config/config.go`, then compared with CLI declarations, option
conversion and the Docker entrypoint. The parser's SHA-256 is
`5517104974a0834c033cd0404562b77cc6673b4d14d295c5178757dbd8687a67`.

## Executed scope

The existing upstream configuration tests and two temporary assertion tests
passed with Go **1.27.3**:

```sh
go test -mod=readonly -count=1 -v ./internal/config
```

The selected Go toolchain was supplied through `GOTOOLCHAIN=go1.27.3`;
dependency and build caches were isolated. The final output was:

```text
PASS
ok  github.com/fsouza/fake-gcs-server/internal/config  0.573s
```

The inventory probe set all 19 environment variables and asserted the loaded
fields and converted server options. The edge probe asserted:

- Empty values use defaults.
- CLI values override valid environment values.
- The image's `-data /data` overrides the seed environment variable.
- An appended `-data` overrides the entrypoint value.
- Invalid numeric environment input fails before CLI parsing.
- The unused second HTTP port is still parsed.
- A high port fails final range validation.
- A CLI value can replace a parseable high environment port.
- Unknown environment names are ignored.
- `trace` is rejected despite appearing in help.
- Event topic without project fails.
- Disabled notification configuration does not validate event names.
- Enabled notifications validate event names.
- HTTP-only operation uses the normal port.

The temporary tests were inspection probes, not committed regression tests.
No Google service, metadata server or Pub/Sub publisher was contacted.
The full upstream suite, actual dual-listener traffic, certificate loading and
notification delivery were not exercised here. Existing filesystem and CORS
probes retain their separate [evidence](fake-gcs-server-filesystem.txt).

The [configuration reference](../fake-gcs-server-configuration.md) records all
settings, native mappings and pinned source links. Neither source inspection
nor this config-only execution establishes full runtime compatibility.
