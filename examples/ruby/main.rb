require "google/cloud/storage"
require "securerandom"
require "stringio"

endpoint = ENV.fetch("GCS_MIMIC_ENDPOINT", "http://localhost:4443")

# Anonymous client for the emulator endpoint; buckets need a project.
storage = Google::Cloud::Storage.anonymous(endpoint: "#{endpoint}/")
storage.service.project = "local-project"

bucket = storage.create_bucket("example-ruby-#{SecureRandom.hex(4)}")
puts "Created bucket #{bucket.name}"

file = bucket.create_file(StringIO.new("Hello from Ruby!"), "hello.txt")
puts "Uploaded #{file.name}"

bucket.files.each { |f| puts "Listed #{f.name}" }

content = file.download.read
puts "Downloaded #{file.name}: #{content}"

file.delete
bucket.delete
puts "Deleted bucket #{bucket.name}"
