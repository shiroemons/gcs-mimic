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
  unless uri.is_a?(URI::HTTP) && uri.userinfo.nil? && [nil, "", "/"].include?(uri.path) &&
         uri.query.nil? && uri.fragment.nil? &&
         (uri.hostname == "localhost" || IPAddr.new(uri.hostname).loopback?)
    raise ArgumentError, "GCS_MIMIC_ENDPOINT must be a loopback HTTP or HTTPS origin"
  end
  uri
rescue URI::InvalidURIError, IPAddr::InvalidAddressError
  raise ArgumentError, "invalid local HTTP or HTTPS emulator origin"
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
  raise "HTTP loopback rejected" unless emulator_origin("http://127.0.0.1:4443").scheme == "http"
  raise "IPv6 loopback rejected" unless emulator_origin("http://[::1]:4443").hostname == "::1"
  %w[https://storage.googleapis.com http://storage.googleapis.com https://169.254.169.254
     ftp://127.0.0.1 https://127.0.0.1.example.invalid
     https://user@127.0.0.1 https://127.0.0.1/storage/v1 https://127.0.0.1?cloud=1].each do |raw|
    begin
      emulator_origin(raw)
    rescue ArgumentError
      next
    end
    raise "unsafe origin accepted: #{raw}"
  end
  %w[http https].each do |scheme|
    origin = emulator_origin("#{scheme}://127.0.0.1:4443")
    observed = { chunks: 0, locations: [] }
    app = ->(_) { Faraday::Response.new(Faraday::Env.from(status: 200, response_headers: {})) }
    guard = EmulatorOriginGuard.new(app, origin, observed)
    guard.call(Faraday::Env.new(method: :get, url: origin))
    other_scheme = scheme == "http" ? "https" : "http"
    ["https://storage.googleapis.com", "http://169.254.169.254", "https://127.0.0.1:4444",
     "#{other_scheme}://127.0.0.1:4443"].each do |raw|
      begin
        guard.call(Faraday::Env.new(method: :get, url: URI(raw)))
      rescue ArgumentError
        next
      end
      raise "outbound origin accepted: #{raw}"
    end
  end
  puts "Ruby HTTP/HTTPS harness origin self-check passed"
  exit
end

origin = emulator_origin(ENV.fetch("GCS_MIMIC_ENDPOINT"))
if origin.scheme == "https"
  ca = ENV.fetch("GCS_MIMIC_CA_CERT")
  raise "CA certificate is unreadable" unless File.file?(ca) && File.readable?(ca)
end
%w[http_proxy https_proxy HTTP_PROXY HTTPS_PROXY all_proxy ALL_PROXY].each { |key| ENV.delete(key) }

# Anonymous construction and an optional static fixture token prevent credential discovery.
storage = Google::Cloud::Storage.anonymous(endpoint: "#{origin.to_s.delete_suffix('/')}/",
                                          retries: 0, timeout: 10, upload_chunk_size: 256 * 1024)
storage.service.project = "local-project"
api = storage.service.service
api.authorization = ENV["GCS_MIMIC_TOKEN"] if ENV.key?("GCS_MIMIC_TOKEN")
connection = api.client
connection.ssl.ca_file = ca if ca
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
objects = []
Tempfile.create("gcs-mimic-ruby-upload") do |source|
  source.binmode
  9.times { source.write((0..255).to_a.pack("C*") * 256) }
  source.flush
  expected = Digest::SHA256.file(source.path).hexdigest
  begin
    # High-level create always sends unsupported Autoclass settings in the pinned SDK.
    created = api.insert_bucket(storage.service.project,
      Google::Apis::StorageV1::Bucket.new(name: "sdk-ruby-basic-#{SecureRandom.hex(8)}"))
    bucket = storage.bucket(created.name)
    raise "bucket get mismatch" unless storage.bucket(bucket.name).name == bucket.name
    raise "bucket absent from list" unless storage.buckets.any? { |item| item.name == bucket.name }
    bucket.update { |item| item.labels = { "transport" => origin.scheme } }
    bucket.reload!
    raise "bucket PATCH mismatch" unless bucket.labels == { "transport" => origin.scheme }
    api.update_bucket(bucket.name,
      Google::Apis::StorageV1::Bucket.new(labels: { "replacement" => "yes" }, storage_class: "NEARLINE"),
      if_metageneration_match: bucket.metageneration)
    bucket.reload!
    raise "bucket PUT mismatch" unless bucket.labels == { "replacement" => "yes" } &&
                                      bucket.storage_class == "NEARLINE"

    source.rewind
    object = bucket.create_file(source, "dir/日本語 +%?.bin", content_type: "application/octet-stream",
                                if_generation_match: 0)
    objects << object
    raise "object size mismatch" unless object.size == source.size
    raise "object metadata mismatch" unless bucket.file(object.name).generation == object.generation
    raise "object absent from list" unless bucket.files.any? { |item| item.name == object.name }
    object.update { |item| item.metadata = { "operation" => "patch" } }
    object.reload!
    raise "object PATCH mismatch" unless object.metadata == { "operation" => "patch" }
    api.update_object(bucket.name, object.name,
      Google::Apis::StorageV1::Object.new(metadata: { "operation" => "put" },
                                        content_type: "application/octet-stream"),
      if_metageneration_match: object.metageneration)
    object.reload!
    raise "object PUT mismatch" unless object.metadata == { "operation" => "put" }

    # File#copy delegates to rewrite; the generated call exercises objects.copy itself.
    copied = api.copy_object(bucket.name, object.name, bucket.name, "copy.bin", if_generation_match: 0)
    objects << bucket.file(copied.name)
    rewritten = object.rewrite("rewrite.bin", if_generation_match: 0)
    objects << rewritten
    composed = bucket.compose([object, rewritten], "compose.bin", if_generation_match: 0,
                              delete_source_objects: false) do |destination|
      destination.content_type = "application/octet-stream"
    end
    objects << composed
    raise "derived object size mismatch" unless objects[1..2].all? { |item| item.size == source.size } &&
                                               composed.size == 2 * source.size
    Tempfile.create("gcs-mimic-ruby-download") do |download|
      objects.each do |item|
        item.download(download.path)
        if item == composed
          digest = Digest::SHA256.new
          2.times do
            source.rewind
            while (chunk = source.read(64 * 1024))
              digest.update(chunk)
            end
          end
          expected_digest = digest.hexdigest
        else
          expected_digest = expected
        end
        raise "download bytes mismatch: #{item.name}" unless
          Digest::SHA256.file(download.path).hexdigest == expected_digest
      end
    end
    raise "resumable chunks were not sent" unless observed[:chunks] == 3
    raise "resumable Location missing" unless observed[:locations].length == 1
    location = URI(observed[:locations].first)
    raise "resumable Location was not the emulator origin" unless
      [location.scheme, location.host, location.port] == [origin.scheme, origin.host, origin.port]
    objects.each(&:delete)
    objects.clear
    bucket.delete
    bucket = nil
    puts "Ruby #{RUBY_VERSION}; google-cloud-storage #{Google::Cloud::Storage::VERSION}; #{origin.scheme.upcase} 6 bucket methods, 9 object methods, derived bytes and 3 resumable chunks passed"
  ensure
    bucket&.files&.each(&:delete)
    bucket&.delete
  end
end
