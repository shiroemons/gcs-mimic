# Seed adapter compatibility change

Fresh seed imports skip regular `.DS_Store` and `._*` OS metadata files at every
depth, including the source root. The ready event reports their count as
`seed.skipped_os_metadata`. Matching directories remain ordinary input and their
children are traversed. Other validation errors still prevent readiness.

Existing catalogs retain already imported OS metadata objects: one-time seed
does not replay, delete or rewrite stored objects. Remove those objects through
the API if desired. No catalog migration or reset is introduced.

The default Content-Type remains `application/octet-stream`. Select
`--seed-content-type extension`, `GCS_MIMIC_SEED_CONTENT_TYPE=extension` or JSON
`"seed_content_type":"extension"` to use the fixed Go/mailcap/YAML extension
table for new seed files. Host MIME databases and body sniffing are unused.
This setting does not affect API uploads or durable-source metadata.

See the [seed contract](../slices/seed.md) and
[adapter verification](../slices/import-seed-adapter.md).
