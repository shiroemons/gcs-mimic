# frozen_string_literal: true

require "bundler/setup"
require "base64"
require "digest"
require "securerandom"
require "stringio"
require_relative "local_http"

LocalHTTP.install!
$LOAD_PATH.unshift(File.expand_path("lib", __dir__))
ENV["RAILS_ENV"] = "test"
ENV["DATABASE_URL"] = "sqlite3::memory:"
ENV["GCS_EMULATOR_BUCKET"] = "active-storage-strict-#{SecureRandom.hex(8)}"
ENV["GCS_MIMIC_BUCKET"] = ENV.fetch("GCS_EMULATOR_BUCKET")
ENV["ACTIVE_STORAGE_SERVICE"] = "gcs_emulator_strict"
require_relative "config/application"
Rails.application.initialize!
ActiveRecord::Migration.verbose = false
load File.join(Gem.loaded_specs.fetch("activestorage").full_gem_path,
               "db/migrate/20170806125915_create_active_storage_tables.rb")
CreateActiveStorageTables.new.change

# Rails loads the service configurations with Blob's lazy initialization.
ActiveStorage::Blob.service
configuration = Rails.application.config.active_storage.service_configurations.fetch("gcs_emulator_strict")
storage = Google::Cloud::Storage.new(project: "local-project", credentials: configuration.fetch("credentials"),
  endpoint: configuration.fetch("endpoint"), universe_domain: "gcs-emulator.invalid")
browser_origin = "http://localhost:3000"
bucket = storage.create_bucket(ENV.fetch("GCS_EMULATOR_BUCKET")) do |item|
  item.cors.add_rule(browser_origin, ["PUT"],
    headers: ["Content-Type", "Content-MD5", "Content-Disposition", "Cache-Control"], max_age: 300)
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
  puts "Rails #{Rails.version}; googleauth #{Gem.loaded_specs.fetch('googleauth').version}; strict local JWT"
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
    bucket.update { |item| item.public_access_prevention = "enforced" }
    assert(fetch(uri, Net::HTTP::Get.new(uri)).body == "strict attachment bytes", "PAP blocked an authorized signer")
    blob.purge
    assert(!ActiveStorage::Blob.service.exist?(blob.key), "purge left strict object")
  end
  ["gcs_emulator_strict", "gcs_emulator_strict_browser"].each do |service_name|
    version = service_name.end_with?("browser") ? "V4" : "V2"
    run_case.call("AS-STRICT-#{version}-DIRECT-UPLOAD-CORS") do
      bytes = "strict #{version} direct bytes"
      blob = ActiveStorage::Blob.create_before_direct_upload!(filename: "direct.txt", byte_size: bytes.bytesize,
        checksum: Base64.strict_encode64(Digest::MD5.digest(bytes)), content_type: "text/plain", service_name: service_name)
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
puts "Summary: #{results.size - failures} PASS, #{failures} FAIL"
exit(failures.zero? ? 0 : 1)
