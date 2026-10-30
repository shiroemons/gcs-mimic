# frozen_string_literal: true

# Verifies the documented gcs_emulator entry in config/storage.yml and the
# bucket file in buckets.json: model attachments reach the emulator through the
# ordinary Active Storage service, googleauth never contacts Google OAuth, and
# browser-facing routes and CORS behave as the Rails recipe describes.
require "bundler/setup"
require "base64"
require "digest"
require "securerandom"
require "stringio"
require_relative "local_http"

LocalHTTP.install!
production = ARGV == ["--production"]
ENV["RAILS_ENV"] = production ? "production" : "test"
ENV["DATABASE_URL"] = "sqlite3::memory:"
ENV["GCS_MIMIC_BUCKET"] = "unused-#{SecureRandom.hex(8)}"
ENV["ACTIVE_STORAGE_SERVICE"] = production ? "local_gcs" : "gcs_emulator"
ENV["GCS_EMULATOR_ENDPOINT"] = "#{ENV.fetch('GCS_MIMIC_ENDPOINT').delete_suffix('/')}/"
require_relative "config/application"
Rails.application.initialize!

if production
  # Loading the service registry evaluates storage.yml as production does.
  service_name = ActiveStorage::Blob.service.name.to_s
  key = Rails.application.config.active_storage.service_configurations
    .dig("gcs_emulator", "credentials", "private_key")
  passed = service_name == "local_gcs" && key.nil?
  puts "AS-EMULATOR-PRODUCTION-NO-KEY: #{passed ? 'PASS' : "FAIL service #{service_name}, key generated: #{!key.nil?}"}"
  exit(passed ? 0 : 1)
end

require "rack/mock"
require "active_support/testing/time_helpers"
include ActiveSupport::Testing::TimeHelpers

ActiveRecord::Migration.verbose = false
load File.join(Gem.loaded_specs.fetch("activestorage").full_gem_path,
               "db/migrate/20170806125915_create_active_storage_tables.rb")
CreateActiveStorageTables.new.change
ActiveRecord::Schema.define { create_table(:documents) { |t| t.string :title } }

class Document < ActiveRecord::Base
  has_one_attached :file
end

fixture_client = Google::Cloud::Storage.anonymous(endpoint: ENV.fetch("GCS_EMULATOR_ENDPOINT"))
fixture_client.service.project = "local-project"
bucket = fixture_client.bucket("local-bucket")
browser_origin = "http://localhost:3000"
app = Rack::MockRequest.new(Rails.application)
routes = Rails.application.routes.url_helpers
results = []

def assert(condition, message)
  raise message unless condition
end

def attach(title, bytes)
  Document.create!(title: title,
    file: {io: StringIO.new(bytes), filename: "#{title}.txt", content_type: "text/plain"}).reload
end

def direct_upload_blob(bytes)
  ActiveStorage::Blob.create_before_direct_upload!(filename: "direct.txt", byte_size: bytes.bytesize,
    checksum: Base64.strict_encode64(Digest::MD5.digest(bytes)), content_type: "text/plain")
end

def preflight(uri, origin, headers)
  request = Net::HTTP::Options.new(uri)
  request["Origin"] = origin
  request["Access-Control-Request-Method"] = "PUT"
  request["Access-Control-Request-Headers"] = headers.keys.map(&:downcase).join(",")
  Net::HTTP.start(uri.hostname, uri.port) { |http| http.request(request) }
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
  puts "Rails #{Rails.version}; googleauth #{Gem.loaded_specs.fetch('googleauth').version}"
  run_case.call("AS-EMULATOR-BUCKETS-FILE") do
    assert(bucket, "buckets.json did not create local-bucket")
    rule = bucket.cors.first
    assert(rule&.origin == [browser_origin] && rule.methods == ["PUT"], "buckets.json CORS rule is missing")
  end
  run_case.call("AS-EMULATOR-ATTACH") do
    assert(ActiveStorage::Blob.service.name.to_s == "gcs_emulator", "gcs_emulator service is not selected")
    document = attach("attached", "attached bytes\n")
    assert(document.file.download == "attached bytes\n", "attachment bytes differ")
    assert(bucket.file(document.file.key)&.download&.string == "attached bytes\n", "object is not in the emulator")
  end
  run_case.call("AS-EMULATOR-URL") do
    uri = URI(attach("signed", "signed bytes").file.url)
    LocalHTTP.validate!(uri)
    response = Net::HTTP.get_response(uri)
    assert(response.is_a?(Net::HTTPSuccess) && response.body == "signed bytes", "signed GET failed: #{response.code}")
  end
  run_case.call("AS-EMULATOR-REDIRECT-TARGET") do
    # Default redirect mode sends the browser to the endpoint host, such as gcs:4443 in Compose.
    path = routes.rails_blob_path(attach("redirect", "redirect bytes").file, only_path: true)
    response = app.get(path, "HTTP_HOST" => "127.0.0.1")
    location = URI(response.location.to_s)
    assert(response.status == 302, "blob route returned #{response.status}")
    LocalHTTP.validate!(location)
  end
  run_case.call("AS-EMULATOR-PROXY") do
    # Proxy mode serves the bytes from the Rails app, so the browser never sees the endpoint.
    previous = ActiveStorage.resolve_model_to_route
    ActiveStorage.resolve_model_to_route = :rails_storage_proxy
    begin
      path = routes.rails_blob_path(attach("proxy", "proxied bytes").file, only_path: true)
      assert(path.include?("/proxy/"), "proxy route is not selected: #{path}")
      response = app.get(path, "HTTP_HOST" => "127.0.0.1")
      assert(response.status == 200 && response.body == "proxied bytes", "proxy GET returned #{response.status}")
      assert(response.location.nil?, "proxy response redirected")
    ensure
      ActiveStorage.resolve_model_to_route = previous
    end
  end
  run_case.call("AS-EMULATOR-DIRECT-UPLOAD") do
    bytes = "direct upload bytes"
    blob = direct_upload_blob(bytes)
    uri = URI(blob.service_url_for_direct_upload)
    LocalHTTP.validate!(uri)
    request = Net::HTTP::Put.new(uri, blob.service_headers_for_direct_upload)
    request.body = bytes
    response = Net::HTTP.start(uri.hostname, uri.port) { |http| http.request(request) }
    assert(response.is_a?(Net::HTTPSuccess), "direct PUT failed: #{response.code}")
    document = Document.create!(title: "direct", file: blob.signed_id)
    assert(document.reload.file.download == bytes, "direct upload bytes differ")
  end
  run_case.call("AS-EMULATOR-DIRECT-UPLOAD-CORS") do
    # The same preflight and PUT a browser sends from the app origin.
    bytes = "browser upload bytes"
    blob = direct_upload_blob(bytes)
    uri = URI(blob.service_url_for_direct_upload)
    headers = blob.service_headers_for_direct_upload
    allowed = preflight(uri, browser_origin, headers)
    assert(allowed.is_a?(Net::HTTPSuccess), "allowed preflight returned #{allowed.code}")
    assert(allowed["Access-Control-Allow-Origin"] == browser_origin, "preflight did not allow #{browser_origin}")
    denied = preflight(uri, "http://evil.invalid", headers)
    assert(denied["Access-Control-Allow-Origin"].nil?, "preflight allowed an unlisted origin")
    request = Net::HTTP::Put.new(uri, headers.merge("Origin" => browser_origin))
    request.body = bytes
    response = Net::HTTP.start(uri.hostname, uri.port) { |http| http.request(request) }
    assert(response.is_a?(Net::HTTPSuccess), "browser PUT failed: #{response.code}")
    assert(response["Access-Control-Allow-Origin"] == browser_origin, "PUT response lacks CORS headers")
    assert(bucket.file(blob.key)&.download&.string == bytes, "browser upload bytes differ")
  end
  run_case.call("AS-EMULATOR-V4-DIRECT-UPLOAD") do
    # cache_control switches Rails to V4 signing and adds a Cache-Control header the
    # browser must be allowed to send.
    bytes = "v4 upload bytes"
    blob = ActiveStorage::Blob.create_before_direct_upload!(filename: "v4.txt", byte_size: bytes.bytesize,
      checksum: Base64.strict_encode64(Digest::MD5.digest(bytes)), content_type: "text/plain",
      service_name: "gcs_emulator_cache_control")
    uri = URI(blob.service_url_for_direct_upload)
    LocalHTTP.validate!(uri)
    assert(URI.decode_www_form(uri.query).to_h.key?("X-Goog-Algorithm"), "direct upload URL is not V4")
    headers = blob.service_headers_for_direct_upload
    assert(headers["Cache-Control"] == "public, max-age=3600", "Cache-Control header is missing")
    allowed = preflight(uri, browser_origin, headers)
    assert(allowed["Access-Control-Allow-Origin"] == browser_origin, "V4 preflight did not allow #{browser_origin}")
    request = Net::HTTP::Put.new(uri, headers.merge("Origin" => browser_origin))
    request.body = bytes
    response = Net::HTTP.start(uri.hostname, uri.port) { |http| http.request(request) }
    assert(response.is_a?(Net::HTTPSuccess), "V4 PUT failed: #{response.code}")
    stored = bucket.file(blob.key)
    assert(stored&.download&.string == bytes, "V4 upload bytes differ")
    assert(stored.cache_control == "public, max-age=3600", "Cache-Control was not stored")
  end
  run_case.call("AS-EMULATOR-NO-OAUTH-LATER") do
    # Any OAuth token request would leave the loopback origin and fail the guard.
    travel(400.days) do
      assert(attach("later", "a year later").file.download == "a year later", "late upload bytes differ")
    end
  end
  run_case.call("AS-EMULATOR-PURGE") do
    document = attach("purge", "purge me")
    key = document.file.key
    document.file.purge
    assert(!ActiveStorage::Blob.service.exist?(key) && bucket.file(key).nil?, "purge left the object")
  end
ensure
  begin
    bucket&.files&.all(&:delete)
  rescue StandardError => e
    results << ["CLEANUP", "FAIL"]
    puts "CLEANUP: FAIL #{e.class}: #{e.message.lines.first.to_s.strip}"
  end
end
failures = results.count { |_, status| status == "FAIL" }
puts "Summary: #{results.size - failures} PASS, #{failures} FAIL"
exit(failures.zero? ? 0 : 1)
