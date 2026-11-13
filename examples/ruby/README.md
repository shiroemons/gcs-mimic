# Ruby example

Creates a bucket, uploads, lists, downloads and deletes an object with `google-cloud-storage` 1.62.2. Needs Ruby 4.0.9 and Bundler 4.0.23.

## Start gcs-mimic

```sh
docker run -d --name gcs-mimic -p 127.0.0.1:4443:4443 \
  ghcr.io/shiroemons/gcs-mimic:0.20.1
```

## Run

```sh
./run.sh
```

Set `GCSM_ENDPOINT` to use another address (default `http://localhost:4443`).

## Client setup

```ruby
storage = Google::Cloud::Storage.anonymous(endpoint: "http://localhost:4443/")
# The anonymous client has no public API to set the project buckets need.
storage.service.project = "local-project"
```
