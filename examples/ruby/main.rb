# Creates a bucket, uploads, lists, downloads and deletes an object in a local gcs-mimic.
require "google/cloud/storage"
require "securerandom"
require "stringio"

endpoint = ENV.fetch("GCS_MIMIC_ENDPOINT", "http://localhost:4443")

# An anonymous client never looks up Google credentials; creating and listing buckets
# still needs a project.
storage = Google::Cloud::Storage.anonymous(endpoint: "#{endpoint.chomp('/')}/")
storage.service.project = "local-project"

bucket = storage.create_bucket("example-ruby-#{SecureRandom.hex(4)}")
puts "Created bucket #{bucket.name}"
begin
  bucket.create_file(StringIO.new("Hello from Ruby!"), "hello.txt", content_type: "text/plain")
  puts "Uploaded hello.txt"

  puts "Objects: #{bucket.files.map(&:name).join(', ')}"

  content = bucket.file("hello.txt").download.read
  puts "Downloaded hello.txt: #{content}"
ensure
  bucket.files.each(&:delete)
  bucket.delete
  puts "Deleted bucket #{bucket.name}"
end
