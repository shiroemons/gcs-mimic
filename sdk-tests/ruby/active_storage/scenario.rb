# frozen_string_literal: true

require "bundler/setup"
require "base64"
require "digest"
require "securerandom"
require "stringio"
require_relative "local_http"

if ARGV == ["--self-check"]
  %w[http://127.0.0.1:4443 http://localhost:4443 http://[::1]:4443].each { |raw| LocalHTTP.origin(raw) }
  %w[https://127.0.0.1 http://storage.googleapis.com http://169.254.169.254
     http://127.0.0.1.example.invalid http://user@127.0.0.1 http://127.0.0.1/path
     http://127.0.0.1?query=1].each do |raw|
    begin
      LocalHTTP.origin(raw)
    rescue ArgumentError
      next
    end
    raise "unsafe origin accepted: #{raw}"
  end
  ENV["GCS_MIMIC_ENDPOINT"] = "http://127.0.0.1:4443"
  %w[http://storage.googleapis.com http://127.0.0.1:4444 https://127.0.0.1:4443].each do |raw|
    begin
      LocalHTTP.validate!(URI(raw))
    rescue ArgumentError
      next
    end
    raise "outbound origin accepted: #{raw}"
  end
  puts "Active Storage origin self-check: PASS"
  exit
end

LocalHTTP.install!
ENV["RAILS_ENV"] = "test"
ENV["DATABASE_URL"] = "sqlite3::memory:"
ENV["GCS_MIMIC_BUCKET"] = "active-storage-#{SecureRandom.hex(8)}"
require_relative "config/application"
Rails.application.initialize!
ActiveRecord::Migration.verbose = false
load File.join(Gem.loaded_specs.fetch("activestorage").full_gem_path,
               "db/migrate/20170806125915_create_active_storage_tables.rb")
CreateActiveStorageTables.new.change

service = ActiveStorage::Blob.service
fixture_client = Google::Cloud::Storage.anonymous(endpoint: "#{ENV.fetch('GCS_MIMIC_ENDPOINT').delete_suffix('/')}/")
fixture_client.service.project = "local-project"
bucket = fixture_client.create_bucket(ENV.fetch("GCS_MIMIC_BUCKET"))
results = []

def assert(condition, message)
  raise message unless condition
end

def fixture_blob(bucket, name, bytes, content_type: "text/plain")
  blob = ActiveStorage::Blob.create_before_direct_upload!(filename: name, byte_size: bytes.bytesize,
    checksum: Base64.strict_encode64(Digest::MD5.digest(bytes)), content_type: content_type)
  # Independent seeding lets download/delete/signing run even when the normal upload fails.
  bucket.create_file(StringIO.new(bytes), blob.key, content_type: content_type)
  blob
end

run_case = lambda do |name, workaround: false, &block|
  begin
    block.call
    status = workaround ? "PASS_WITH_WORKAROUND" : "PASS"
    results << [name, status]
    puts "#{name}: #{status}#{' (OAuth authorization removed)' if workaround}"
  rescue StandardError => e
    results << [name, "FAIL"]
    puts "#{name}: FAIL #{e.class}: #{e.message.lines.first.to_s.strip}"
  end
end

begin
  puts "Rails #{Rails.version}; Ruby #{RUBY_VERSION}; google-cloud-storage #{Gem.loaded_specs.fetch('google-cloud-storage').version}"
  run_case.call("AS-DEFAULT-AUTH") do
    blob = ActiveStorage::Blob.create_and_upload!(io: StringIO.new("default auth\n"), filename: "auth.txt")
    assert(blob.download == "default auth\n", "default authentication upload bytes differ")
  end
  run_case.call("AS-LOCAL-CREDENTIALS") do
    credentials = File.open(File.join(__dir__, "../../../fixtures/signing/test-only-private.pem")) do |key|
      Google::Auth::ServiceAccountCredentials.make_creds(
        json_key_io: StringIO.new(JSON.generate(type: "service_account", project_id: "example-project",
          client_email: "local-admin@example.invalid", private_key: key.read)),
        scope: "https://www.googleapis.com/auth/devstorage.full_control")
    end
    credentials.access_token = "example-local-token"
    credentials.expires_at = Time.now + 86_400
    local_service = ActiveStorage::Service::GCSService.new(project: "example-project",
      bucket: bucket.name, credentials: credentials,
      endpoint: "#{ENV.fetch('GCS_MIMIC_ENDPOINT').delete_suffix('/')}/")
    key = "local-credentials/#{SecureRandom.hex(8)}"
    bytes = "local credential bytes"
    begin
      local_service.upload(key, StringIO.new(bytes), content_type: "text/plain")
      assert(local_service.download(key) == bytes, "local credential download differs")
      uri = URI(local_service.url(key, expires_in: 300, disposition: :inline,
        filename: ActiveStorage::Filename.new("example.txt"), content_type: "text/plain"))
      LocalHTTP.validate!(uri)
      assert(URI.decode_www_form(uri.query).to_h.key?("GoogleAccessId"), "local credential URL is not V2")
      response = Net::HTTP.get_response(uri)
      assert(response.is_a?(Net::HTTPSuccess) && response.body == bytes, "local credential signed GET failed")
    ensure
      local_service.delete(key)
    end
    assert(!local_service.exist?(key), "local credential delete left object")
  end
  # Conditional operation coverage must not count as default-configuration success.
  client = service.send(:client)
  client.service.service.authorization = nil
  client.service.service.client.builder.delete(Google::Apis::Core::FollowRedirectsMiddleware)
  run_case.call("AS-UPLOAD", workaround: true) do
    blob = ActiveStorage::Blob.create_and_upload!(io: StringIO.new("active storage text\n"), filename: "hello.txt")
    assert(blob.content_type == "text/plain", "default MIME identification failed")
    assert(blob.download == "active storage text\n", "default upload bytes differ")
    binary = ActiveStorage::Blob.create_and_upload!(io: StringIO.new("\x00\xff".b), filename: "data.bin",
      content_type: "application/octet-stream")
    assert(binary.download == "\x00\xff".b, "explicit binary upload bytes differ")
  end
  run_case.call("AS-DOWNLOAD", workaround: true) do
    bytes = "download and open\n"
    blob = fixture_blob(bucket, "download.txt", bytes)
    assert(blob.download == bytes, "download bytes differ")
    blob.open { |file| assert(file.read == bytes, "open bytes differ") }
  end
  run_case.call("AS-DELETE", workaround: true) do
    blob = fixture_blob(bucket, "picture.png", "image fixture", content_type: "image/png")
    key = blob.key
    variant_key = "variants/#{key}/fixture"
    bucket.create_file(StringIO.new("variant"), variant_key, content_type: "image/png")
    neighbor = "variants/#{key}-neighbor/fixture"
    bucket.create_file(StringIO.new("keep"), neighbor, content_type: "image/png")
    blob.purge
    assert(!ActiveStorage::Blob.exists?(blob.id) && !service.exist?(key), "purge left blob or object")
    assert(!service.exist?(variant_key), "purge left variant prefix")
    assert(service.exist?(neighbor), "purge removed neighboring prefix")
    service.delete_prefixed("variants/#{key}-neighbor/")
    assert(!service.exist?(neighbor), "delete_prefixed left matching object")
  end
  run_case.call("AS-SIGNED-URL", workaround: true) do
    blob = fixture_blob(bucket, "signed.txt", "signed download")
    uri = URI(blob.url)
    LocalHTTP.validate!(uri)
    response = Net::HTTP.get_response(uri)
    assert(response.is_a?(Net::HTTPSuccess) && response.body == "signed download", "signed GET failed: #{response.code}")
  end
  run_case.call("AS-DIRECT-UPLOAD", workaround: true) do
    bytes = "browser equivalent PUT"
    checksum = Base64.strict_encode64(Digest::MD5.digest(bytes))
    key = "direct/#{SecureRandom.hex(8)}"
    uri = URI(service.url_for_direct_upload(key, expires_in: 300, content_type: "text/plain",
      content_length: bytes.bytesize, checksum: checksum))
    headers = service.headers_for_direct_upload(key, content_type: "text/plain", checksum: checksum)
    LocalHTTP.validate!(uri)
    request = Net::HTTP::Put.new(uri, headers.compact)
    request["Content-Type"] = "text/plain"
    request.body = bytes
    response = Net::HTTP.start(uri.hostname, uri.port, nil) { |http| http.request(request) }
    assert(response.is_a?(Net::HTTPSuccess), "direct PUT failed: #{response.code}")
    assert(bucket.file(key).download.string == bytes, "direct upload bytes differ")
  end
  run_case.call("AS-EXIST-COMPOSE", workaround: true) do
    first = fixture_blob(bucket, "first.txt", "first")
    second = fixture_blob(bucket, "second.txt", "second")
    assert(service.exist?(first.key) && !service.exist?("missing-#{SecureRandom.hex(8)}"), "exist? result differs")
    assert(service.respond_to?(:compose), "Rails GCS compose is unavailable")
    key = "composed/#{SecureRandom.hex(8)}"
    service.compose([first.key, second.key], key)
    assert(service.download(key) == "firstsecond", "compose order or bytes differ")
    assert(service.exist?(first.key) && service.exist?(second.key), "compose deleted sources")
  end
ensure
  begin
    bucket.files.all(&:delete)
    bucket.delete
  rescue StandardError => e
    results << ["CLEANUP", "FAIL"]
    puts "CLEANUP: FAIL #{e.class}: #{e.message.lines.first.to_s.strip}"
  end
end
failures = results.count { |_, status| status == "FAIL" }
passes = results.count { |_, status| status == "PASS" }
conditional = results.count { |_, status| status == "PASS_WITH_WORKAROUND" }
puts "Summary: #{passes} PASS, #{conditional} PASS_WITH_WORKAROUND, #{failures} FAIL"
exit(failures.zero? ? 0 : 1)
