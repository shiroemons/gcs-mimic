# frozen_string_literal: true

require "bundler/setup"
require "base64"
require "digest"
require "securerandom"
require "stringio"
require_relative "local_http"

LocalHTTP.install!
$LOAD_PATH.unshift(File.expand_path("lib", __dir__))
# The storage.yml service under test; its "_browser" sibling is the explicit V4 adapter and its
# "_public" sibling serves a public bucket.
service_name = ARGV.fetch(0, "gcs_emulator_strict")
# A runner that names the buckets has created them with the server's buckets file, as the strict
# local recipe does for an identity without bucket administration. Otherwise the identity under
# test creates them, sets their policy and deletes them afterward.
provisioned = ENV.key?("GCS_EMULATOR_BUCKET")
ENV["RAILS_ENV"] = "test"
ENV["DATABASE_URL"] = "sqlite3::memory:"
ENV["GCS_EMULATOR_BUCKET"] ||= "active-storage-strict-#{SecureRandom.hex(8)}"
ENV["GCS_EMULATOR_PUBLIC_BUCKET"] = "#{ENV.fetch('GCS_EMULATOR_BUCKET')}-public" unless provisioned
ENV["GCSM_BUCKET"] = ENV.fetch("GCS_EMULATOR_BUCKET")
ENV["ACTIVE_STORAGE_SERVICE"] = service_name
require_relative "config/application"
Rails.application.initialize!
ActiveRecord::Migration.verbose = false
load File.join(Gem.loaded_specs.fetch("activestorage").full_gem_path,
               "db/migrate/20170806125915_create_active_storage_tables.rb")
CreateActiveStorageTables.new.change
ActiveRecord::Schema.define { create_table(:documents) { |t| t.string :title } }

class Document < ActiveRecord::Base
  has_one_attached :file
end

require "rack/mock"
rails_app = Rack::MockRequest.new(Rails.application)
routes = Rails.application.routes.url_helpers

# Rails loads the service configurations with Blob's lazy initialization.
ActiveStorage::Blob.service
configuration = Rails.application.config.active_storage.service_configurations.fetch(service_name)
storage = Google::Cloud::Storage.new(project: "local-project", credentials: configuration.fetch("credentials"),
  endpoint: configuration.fetch("endpoint"), universe_domain: "gcs-emulator.invalid")
browser_origin = "http://localhost:3000"
if provisioned
  # Looking a bucket up needs storage.buckets.get, which object roles lack; Rails skips it too.
  bucket = storage.bucket(ENV.fetch("GCS_EMULATOR_BUCKET"), skip_lookup: true)
  public_bucket = storage.bucket(ENV.fetch("GCS_EMULATOR_PUBLIC_BUCKET"), skip_lookup: true)
else
  bucket = storage.create_bucket(ENV.fetch("GCS_EMULATOR_BUCKET")) do |item|
    item.cors.add_rule(browser_origin, ["PUT"],
      headers: ["Content-Type", "Content-MD5", "Content-Disposition", "x-goog-content-length-range", "Cache-Control"], max_age: 300)
  end
  public_bucket = storage.create_bucket(ENV.fetch("GCS_EMULATOR_PUBLIC_BUCKET"))
  public_bucket.policy { |policy| policy.add("roles/storage.objectViewer", "allUsers") }
end
results = []

def assert(condition, message)
  raise message unless condition
end

def fetch(uri, request)
  LocalHTTP.validate!(uri)
  Net::HTTP.start(uri.hostname, uri.port, nil) { |http| http.request(request) }
end

run_case = lambda do |name, &block|
  block.call
  results << [name, "PASS"]
  puts "#{name}: PASS"
rescue StandardError => e
  results << [name, "FAIL"]
  puts "#{name}: FAIL #{e.class}: #{e.message.lines.first.to_s.strip}"
end

begin
  puts "Rails #{Rails.version}; googleauth #{Gem.loaded_specs.fetch('googleauth').version}; strict local JWT; " \
       "#{service_name} as #{configuration.fetch('credentials').fetch('client_email')}; " \
       "buckets #{provisioned ? 'from the server buckets file' : 'created by that identity'}"
  run_case.call("AS-STRICT-JWT-UPLOAD-DOWNLOAD") do
    blob = ActiveStorage::Blob.create_and_upload!(io: StringIO.new("strict attachment bytes"), filename: "strict.txt")
    assert(blob.download == "strict attachment bytes", "strict download bytes differ")
    blob.open { |file| assert(file.read == "strict attachment bytes", "checksum-verified open differs") }
    uri = URI(blob.url)
    assert(URI.decode_www_form(uri.query).to_h.key?("GoogleAccessId"), "stock Rails signed URL is not V2")
    response = fetch(uri, Net::HTTP::Get.new(uri))
    assert(response.is_a?(Net::HTTPSuccess) && response.body == "strict attachment bytes", "strict signed GET failed")
    anonymous = uri.dup
    anonymous.query = nil
    assert(fetch(anonymous, Net::HTTP::Get.new(anonymous)).code.to_i == 401, "private object allowed anonymous GET")
    # The setter outside an update block is a PATCH. A block sends a PUT, which replaces the whole
    # bucket on Cloud Storage and would clear the CORS rules the direct-upload cases below need.
    # A provisioned private bucket enforces PAP from startup: changing it needs storage.buckets.update.
    bucket.public_access_prevention = "enforced" unless provisioned
    assert(fetch(uri, Net::HTTP::Get.new(uri)).body == "strict attachment bytes", "PAP blocked an authorized signer")
    blob.purge
    assert(!ActiveStorage::Blob.service.exist?(blob.key), "purge left strict object")
  end
  [service_name, "#{service_name}_browser"].each do |direct_service|
    version = direct_service.end_with?("browser") ? "V4" : "V2"
    run_case.call("AS-STRICT-#{version}-DIRECT-UPLOAD-CORS") do
      bytes = "strict #{version} direct bytes"
      blob = ActiveStorage::Blob.create_before_direct_upload!(filename: "direct.txt", byte_size: bytes.bytesize,
        checksum: Base64.strict_encode64(Digest::MD5.digest(bytes)), content_type: "text/plain", service_name: direct_service)
      uri = URI(blob.service_url_for_direct_upload)
      headers = blob.service_headers_for_direct_upload.compact
      query = URI.decode_www_form(uri.query).to_h
      assert(query.key?(version == "V4" ? "X-Goog-Algorithm" : "GoogleAccessId"), "wrong signature version")
      preflight = Net::HTTP::Options.new(uri)
      preflight["Origin"] = browser_origin
      preflight["Access-Control-Request-Method"] = "PUT"
      preflight["Access-Control-Request-Headers"] = headers.keys.map(&:downcase).join(",")
      assert(fetch(uri, preflight)["Access-Control-Allow-Origin"] == browser_origin, "strict preflight denied")
      denied = preflight.dup
      denied["Origin"] = "http://evil.invalid"
      assert(fetch(uri, denied)["Access-Control-Allow-Origin"].nil?, "unlisted origin allowed")
      put = Net::HTTP::Put.new(uri, headers.merge("Origin" => browser_origin))
      put.body = bytes
      response = fetch(uri, put)
      assert(response.is_a?(Net::HTTPSuccess), "strict direct PUT failed: #{response.code}")
      assert(response["Access-Control-Allow-Origin"] == browser_origin, "strict PUT lacks CORS")
      assert(blob.download == bytes, "strict direct bytes differ")
      if version == "V4"
        signed = URI(blob.url)
        assert(URI.decode_www_form(signed.query).to_h.key?("X-Goog-Algorithm"), "adapter download URL is not V4")
        assert(fetch(signed, Net::HTTP::Get.new(signed)).body == bytes, "adapter signed download differs")
        assert(bucket.file(blob.key).cache_control == "public, max-age=3600", "cache control was not stored")
        tampered = uri.dup
        tampered.query = "#{uri.query}&unexpected=tamper"
        bad = Net::HTTP::Put.new(tampered, headers)
        bad.body = "changed"
        assert(fetch(tampered, bad).code.to_i == 403, "tampered V4 query was accepted")
        assert(blob.download == bytes, "tampered PUT changed stored bytes")
      end
      blob.purge
    end
  end
  if provisioned
    run_case.call("AS-STRICT-NO-BUCKET-ADMIN") do
      # The identity of a provisioned run may hold only object roles: the test signer's key is public.
      principal = configuration.fetch("credentials").fetch("client_email")
      [["storage.buckets.create", -> { storage.create_bucket("#{bucket.name}-denied") }],
       ["storage.buckets.getIamPolicy", -> { public_bucket.policy }]].each do |permission, call|
        call.call
        raise "#{principal} was allowed #{permission}"
      rescue Google::Cloud::PermissionDeniedError => e
        assert(e.message.include?(principal) && e.message.include?(permission),
          "denial does not name #{principal} and #{permission}: #{e.message}")
      end
    end
  end
  run_case.call("AS-STRICT-ATTACH-PURGE") do
    # Purging an image also deletes its variants/KEY/ prefix, which lists the bucket.
    service = ActiveStorage::Blob.service
    bytes = "\x89PNG\r\n\x1a\nstrict attachment".b
    document = Document.create!(title: "attached",
      file: {io: StringIO.new(bytes), filename: "attached.png", content_type: "image/png"}).reload
    key = document.file.key
    assert(document.file.download == bytes, "attachment bytes differ")
    assert(service.exist?(key) && !service.exist?("missing-#{SecureRandom.hex(8)}"), "exist? result differs")
    variant = "variants/#{key}/fixture"
    neighbor = "variants/#{key}-neighbor/fixture"
    [variant, neighbor].each { |name| service.upload(name, StringIO.new("variant"), content_type: "image/png") }
    document.file.purge
    assert(!service.exist?(key) && !service.exist?(variant), "purge left the object or its variants prefix")
    assert(service.exist?(neighbor), "purge removed a neighboring prefix")
    service.delete_prefixed("variants/#{key}-neighbor/")
    assert(!service.exist?(neighbor), "delete_prefixed left a matching object")
  end
  run_case.call("AS-STRICT-METADATA-UPDATE") do
    # Changing a blob's content type patches the object, here with an attachment disposition.
    blob = ActiveStorage::Blob.create_and_upload!(io: StringIO.new("a,b\n"), filename: "table.csv",
      content_type: "text/plain", identify: false)
    blob.update!(content_type: "text/csv")
    stored = bucket.file(blob.key)
    assert(stored.content_type == "text/csv", "metadata update did not change the content type")
    assert(stored.content_disposition.to_s.start_with?("attachment"), "metadata update did not set the disposition")
    blob.purge
  end
  run_case.call("AS-STRICT-COMPOSE") do
    parts = %w[first second].map do |part|
      ActiveStorage::Blob.create_and_upload!(io: StringIO.new(part), filename: "#{part}.txt", content_type: "text/plain")
    end
    composed = ActiveStorage::Blob.compose(parts, filename: "composed.txt")
    assert(composed.download == "firstsecond", "compose order or bytes differ")
    assert(parts.all? { |part| ActiveStorage::Blob.service.exist?(part.key) }, "compose deleted a source")
    [*parts, composed].each(&:purge)
  end
  run_case.call("AS-STRICT-PUBLIC-BUCKET") do
    bytes = "strict public bytes"
    blob = ActiveStorage::Blob.create_and_upload!(io: StringIO.new(bytes), filename: "public.txt",
      service_name: "#{service_name}_public")
    service = blob.service
    assert(service.public? && service.exist?(blob.key), "public object is missing")
    assert(!service.exist?("missing-#{SecureRandom.hex(8)}"), "public exist? found a missing key")
    # A public service's url is unsigned. The stock SDK always names storage.googleapis.com as its
    # host, which is why the adapter case below exists; this request sends the same path to the emulator.
    url = URI(blob.url)
    assert(url.query.nil? && url.path == "/#{public_bucket.name}/#{blob.key}", "public url is not an unsigned object path")
    assert(url.host == "storage.googleapis.com", "the stock public url no longer names Google; revisit the adapter")
    local = URI("#{ENV.fetch('GCSM_ENDPOINT').delete_suffix('/')}#{url.path}")
    response = fetch(local, Net::HTTP::Get.new(local))
    assert(response.is_a?(Net::HTTPSuccess) && response.body == bytes, "anonymous public GET failed: #{response.code}")
    blob.purge
    assert(!service.exist?(blob.key), "purge left the public object")
  end
  run_case.call("AS-STRICT-PUBLIC-URL-ADAPTER") do
    # The adapter's public url names public_endpoint, so it and the redirect that image tags follow
    # reach the emulator unchanged. The custom key checks that each path segment is escaped.
    bytes = "strict adapter public bytes"
    folder = "avatars/#{SecureRandom.hex(4)}"
    key = "#{folder}/public file.txt"
    blob = ActiveStorage::Blob.create_and_upload!(io: StringIO.new(bytes), filename: "public file.txt", key: key,
      service_name: "#{service_name}_public_browser")
    url = URI(blob.url)
    expected = "#{ENV.fetch('GCSM_ENDPOINT').delete_suffix('/')}/#{public_bucket.name}/#{folder}/public%20file.txt"
    assert(url.to_s == expected, "adapter public url differs: #{url}")
    response = fetch(url, Net::HTTP::Get.new(url))
    assert(response.is_a?(Net::HTTPSuccess) && response.body == bytes, "anonymous adapter public GET failed: #{response.code}")
    redirect = rails_app.get(routes.rails_blob_path(blob, only_path: true), "HTTP_HOST" => "127.0.0.1")
    assert(redirect.status == 302 && redirect.location == url.to_s, "blob route did not redirect to the public url")
    blob.purge
    assert(!blob.service.exist?(key), "purge left the adapter public object")
  end
  run_case.call("AS-STRICT-PUBLIC-URL-INITIALIZER") do
    # The same stock service as AS-STRICT-PUBLIC-BUCKET, which named storage.googleapis.com before
    # this load, now names its endpoint. The initializer changes every GCS service in the process,
    # so this case runs last.
    load File.join(__dir__, "initializers/active_storage_gcs_emulator.rb")
    origin = "#{ENV.fetch('GCSM_ENDPOINT').delete_suffix('/')}/#{public_bucket.name}"
    bytes = "strict initializer public bytes"
    blob = ActiveStorage::Blob.create_and_upload!(io: StringIO.new(bytes), filename: "patched.txt",
      service_name: "#{service_name}_public")
    service = blob.service
    url = URI(blob.url)
    assert(url.to_s == "#{origin}/#{blob.key}", "patched public url differs: #{url}")
    response = fetch(url, Net::HTTP::Get.new(url))
    assert(response.is_a?(Net::HTTPSuccess) && response.body == bytes, "anonymous patched public GET failed: #{response.code}")
    # A public variant's key has slashes, which stay path separators.
    variant = "variants/#{blob.key}/#{SecureRandom.hex(8)}"
    service.upload(variant, StringIO.new("variant bytes"), content_type: "image/png")
    variant_url = URI(service.url(variant, expires_in: 300, filename: ActiveStorage::Filename.new("variant.png"),
      content_type: "image/png", disposition: :inline))
    assert(variant_url.to_s == "#{origin}/#{variant}", "patched variant url differs: #{variant_url}")
    assert(fetch(variant_url, Net::HTTP::Get.new(variant_url)).body == "variant bytes", "anonymous variant GET failed")
    # An entry without an endpoint, as in production, keeps the stock URL; building it sends no request.
    # Production credentials name no universe, and the SDK refuses a mismatched one.
    stock = ActiveStorage::Service::GCSService.new(public: true, project: "local-project",
      bucket: public_bucket.name, credentials: configuration.fetch("credentials").except("universe_domain"))
    stock_url = URI(stock.url("unchanged", expires_in: 300, filename: ActiveStorage::Filename.new("unchanged.txt"),
      content_type: "text/plain", disposition: :inline))
    assert(stock_url.to_s == "https://storage.googleapis.com/#{public_bucket.name}/unchanged",
      "the initializer changed an entry without an endpoint: #{stock_url}")
    service.delete(variant)
    blob.purge
    assert(!service.exist?(blob.key) && !service.exist?(variant), "purge left the patched public objects")
  end
ensure
  begin
    [bucket, public_bucket].each { |item| item.files.all(&:delete) }
    [bucket, public_bucket].each(&:delete) unless provisioned
  rescue StandardError => e
    results << ["CLEANUP", "FAIL"]
    puts "CLEANUP: FAIL #{e.class}: #{e.message.lines.first.to_s.strip}"
  end
end
failures = results.count { |_, status| status == "FAIL" }
puts "Summary: #{results.size - failures} PASS, #{failures} FAIL"
exit(failures.zero? ? 0 : 1)
