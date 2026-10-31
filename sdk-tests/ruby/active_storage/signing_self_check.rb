# frozen_string_literal: true

require "bundler/setup"
require "active_support/all"
require "active_storage"
require "active_storage/service"
require "openssl"
require "base64"
require_relative "local_http"
require_relative "lib/active_storage/service/emulator_gcs_service"

ENV["GCS_MIMIC_ENDPOINT"] = "http://127.0.0.1:4443"
LocalHTTP.install!
private_key = File.read(File.join(__dir__, "../../../fixtures/signing/test-only-private.pem"))
public_key = OpenSSL::PKey::RSA.new(private_key).public_key
service = ActiveStorage::Service::EmulatorGcsService.new(project: "local-project", bucket: "strict-bucket",
  endpoint: "http://container.invalid:4443/", public_endpoint: "http://127.0.0.1:54443/",
  universe_domain: "gcs-emulator.invalid", cache_control: "public, max-age=3600",
  credentials: {type: "service_account", project_id: "local-project", client_email: "local-admin@example.invalid",
                universe_domain: "gcs-emulator.invalid", private_key: private_key})
checksum = Base64.strict_encode64(Digest::MD5.digest("expected bytes"))
url = URI(service.url_for_direct_upload("folder/name +.txt", expires_in: 300, checksum: checksum))
raise "final browser origin was changed" unless url.host == "127.0.0.1" && url.port == 54_443
raise "final bucket/object URI differs" unless url.path == "/strict-bucket/folder/name%20%2B.txt"
query = URI.decode_www_form(url.query).to_h
headers = {"host" => "127.0.0.1:54443", "cache-control" => "public, max-age=3600", "content-md5" => checksum}
raise "required upload headers were not signed" unless query.fetch("X-Goog-SignedHeaders") == headers.keys.sort.join(";")

# Reconstruct the signed request independently from the service and SDK signer.
verify = lambda do |request_headers|
  canonical_headers = request_headers.sort.map { |name, value| "#{name}:#{value}\n" }.join
  canonical_query = url.query.split("&").reject { |pair| pair.start_with?("X-Goog-Signature=") }.sort.join("&")
  request = ["PUT", url.path, canonical_query, canonical_headers, query.fetch("X-Goog-SignedHeaders"), "UNSIGNED-PAYLOAD"].join("\n")
  scope = query.fetch("X-Goog-Credential").split("/").drop(1).join("/")
  message = ["GOOG4-RSA-SHA256", query.fetch("X-Goog-Date"), scope, Digest::SHA256.hexdigest(request)].join("\n")
  public_key.verify(OpenSSL::Digest::SHA256.new, [query.fetch("X-Goog-Signature")].pack("H*"), message)
end
raise "signature does not bind the final request" unless verify.call(headers)
raise "changed port retained a valid signature" if verify.call(headers.merge("host" => "127.0.0.1:4443"))
raise "changed checksum retained a valid signature" if verify.call(headers.merge("content-md5" => "changed"))
%w[http://user@localhost:4443 http://localhost:4443/prefix http://localhost:4443/?query=1].each do |origin|
  begin
    ActiveStorage::Service::EmulatorGcsService.new(public_endpoint: origin)
  rescue ArgumentError
    next
  end
  raise "invalid signing origin accepted: #{origin}"
end
puts "Rails local signing self-check: PASS (final host, port, path, checksum and headers)"
