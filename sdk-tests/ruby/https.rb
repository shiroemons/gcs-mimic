# frozen_string_literal: true

require "bundler/setup"
require "digest"
require "google/cloud/storage"
require "ipaddr"
require "securerandom"
require "tempfile"
require "uri"

def emulator_origin(raw)
  uri = URI.parse(raw)
  unless uri.is_a?(URI::HTTPS) && uri.userinfo.nil? && [nil, "", "/"].include?(uri.path) &&
         uri.query.nil? && uri.fragment.nil? &&
         (uri.hostname == "localhost" || IPAddr.new(uri.hostname).loopback?)
    raise ArgumentError, "GCS_MIMIC_ENDPOINT must be a loopback HTTPS origin"
  end
  uri
rescue URI::InvalidURIError, IPAddr::InvalidAddressError
  raise ArgumentError, "invalid local HTTPS emulator origin"
end

class EmulatorOriginGuard < Faraday::Middleware
  def initialize(app, origin, observed)
    super(app)
    @origin = [origin.scheme, origin.host, origin.port]
    @observed = observed
  end

  def call(env)
    unless [env.url.scheme, env.url.host, env.url.port] == @origin
      raise ArgumentError, "refusing request outside the emulator origin"
    end
    @observed[:chunks] += 1 if env.method == :put && env.request_headers["Content-Range"]
    @app.call(env).on_complete do |response|
      @observed[:locations] << response.response_headers["location"] if response.response_headers["location"]
    end
  end
end

if ARGV == ["--self-check"]
  raise "loopback rejected" unless emulator_origin("https://127.0.0.1:4443").port == 4443
  raise "localhost rejected" unless emulator_origin("https://localhost:4443").host == "localhost"
  %w[https://storage.googleapis.com http://127.0.0.1 https://169.254.169.254
     https://user@127.0.0.1 https://127.0.0.1/storage/v1 https://127.0.0.1?cloud=1].each do |raw|
    begin
      emulator_origin(raw)
    rescue ArgumentError
      next
    end
    raise "unsafe origin accepted: #{raw}"
  end
  origin = emulator_origin("https://127.0.0.1:4443")
  guard = EmulatorOriginGuard.new(->(_) { raise "outbound request attempted" }, origin, {})
  begin
    guard.call(Faraday::Env.new(method: :get, url: URI("https://storage.googleapis.com")))
    raise "outbound origin accepted"
  rescue ArgumentError
    puts "Ruby HTTPS harness origin self-check passed"
  end
  exit
end

origin = emulator_origin(ENV.fetch("GCS_MIMIC_ENDPOINT"))
ca = ENV.fetch("GCS_MIMIC_CA_CERT")
raise "CA certificate is unreadable" unless File.file?(ca) && File.readable?(ca)
%w[http_proxy https_proxy HTTP_PROXY HTTPS_PROXY all_proxy ALL_PROXY].each { |key| ENV.delete(key) }

# Anonymous mode prevents credential and metadata-server discovery in this local fixture.
storage = Google::Cloud::Storage.anonymous(endpoint: "#{origin.to_s.delete_suffix('/')}/",
                                          retries: 0, timeout: 10, upload_chunk_size: 256 * 1024)
storage.service.project = "sdk-ruby-local"
connection = storage.service.service.client
connection.ssl.ca_file = ca
connection.ssl.verify = true
connection.ssl.verify_hostname = true
connection.builder.delete(Google::Apis::Core::FollowRedirectsMiddleware)
connection.builder.adapter(:net_http) do |http|
  # Pin localhost dialing so hosts-file changes cannot redirect the test off-machine.
  http.ipaddr = "127.0.0.1" if origin.hostname == "localhost"
end
observed = { chunks: 0, locations: [] }
connection.builder.use(EmulatorOriginGuard, origin, observed)

bucket = nil
object = nil
Tempfile.create("gcs-mimic-ruby-upload") do |source|
  source.binmode
  9.times { source.write((0..255).to_a.pack("C*") * 256) }
  source.flush
  expected = Digest::SHA256.file(source.path).hexdigest
  begin
    # High-level create always sends unsupported Autoclass settings in the pinned SDK.
    created = storage.service.service.insert_bucket(storage.service.project,
      Google::Apis::StorageV1::Bucket.new(name: "sdk-ruby-tls-#{SecureRandom.hex(8)}"))
    bucket = storage.bucket(created.name)
    raise "bucket get mismatch" unless storage.bucket(bucket.name).name == bucket.name
    raise "bucket absent from list" unless storage.buckets.any? { |item| item.name == bucket.name }
    bucket.update { |item| item.labels = { "transport" => "https" } }
    bucket.reload!
    raise "bucket update mismatch" unless bucket.labels == { "transport" => "https" }

    source.rewind
    object = bucket.create_file(source, "dir/日本語 +%?.bin", content_type: "application/octet-stream",
                                if_generation_match: 0)
    raise "object size mismatch" unless object.size == source.size
    raise "object metadata mismatch" unless bucket.file(object.name).generation == object.generation
    raise "object absent from list" unless bucket.files.any? { |item| item.name == object.name }
    Tempfile.create("gcs-mimic-ruby-download") do |download|
      object.download(download.path)
      raise "download bytes mismatch" unless Digest::SHA256.file(download.path).hexdigest == expected
    end
    raise "resumable chunks were not sent" unless observed[:chunks] == 3
    raise "resumable Location missing" unless observed[:locations].length == 1
    location = URI(observed[:locations].first)
    raise "resumable Location was not local HTTPS" unless
      [location.scheme, location.host, location.port] == [origin.scheme, origin.host, origin.port]
    object.delete
    object = nil
    bucket.delete
    bucket = nil
    puts "Ruby #{RUBY_VERSION}; google-cloud-storage #{Google::Cloud::Storage::VERSION}; HTTPS bucket CRUD, streamed upload/download and 3 resumable chunks passed"
  ensure
    object&.delete
    bucket&.delete
  end
end
