require "google/cloud/storage"
require "securerandom"
require "stringio"

endpoint = ENV.fetch("GCS_MIMIC_ENDPOINT", "http://localhost:4443")

# Anonymous client for the emulator endpoint. Creating a bucket needs a
# project, and the anonymous client has no public API to set one, so this sets
# it on the underlying service.
storage = Google::Cloud::Storage.anonymous(endpoint: "#{endpoint}/")
storage.service.project = "local-project"

bucket = storage.create_bucket("example-ruby-#{SecureRandom.hex(4)}")
puts "created bucket #{bucket.name}"

file = bucket.create_file(StringIO.new("Hello from gcs-mimic!"), "hello.txt")
puts "uploaded #{file.name}"

bucket.files.each { |f| puts "listed #{f.name} (#{f.size} bytes)" }

puts "downloaded: #{file.download.read}"

file.delete
puts "deleted #{file.name}"
bucket.delete
puts "deleted bucket #{bucket.name}"
