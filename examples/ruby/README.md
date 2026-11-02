# Ruby example

`main.rb` uses the official `google-cloud-storage` gem (1.62.1) to create a
bucket, upload a text object, list the bucket, download the object and delete
both. It talks only to a local gcs-mimic and never contacts Google.

## Prerequisites

- Ruby 4.0.9 (see `.ruby-version`) and Bundler 4.0.23
- Docker, to start gcs-mimic

## Start gcs-mimic

```sh
docker run -d --name gcs-mimic -p 127.0.0.1:4443:4443 \
  -e GCS_MIMIC_SCHEME=http ghcr.io/shiroemons/gcs-mimic:0.9.0
```

## Run

```sh
./run.sh
```

`run.sh` installs the gems pinned in `Gemfile.lock` without changing it and runs
`main.rb` against `GCS_MIMIC_ENDPOINT` (default `http://localhost:4443`). It
accepts only a loopback `http://` origin. The output looks like this:

```text
Created bucket example-ruby-1a2b3c4d
Uploaded hello.txt
Objects: hello.txt
Downloaded hello.txt: Hello from Ruby!
Deleted bucket example-ruby-1a2b3c4d
```

Stop the emulator with `docker rm -f gcs-mimic`.

## Client setup

```ruby
require "google/cloud/storage"
storage = Google::Cloud::Storage.anonymous(endpoint: "http://localhost:4443/")
storage.service.project = "local-project"
```

An anonymous client never looks up Google credentials. Creating and listing
buckets still needs a project.
