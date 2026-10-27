# frozen_string_literal: true

require "bundler/setup"
require "date"
require "digest"
require "google/cloud/storage"
require "ipaddr"
require "json"
require "net/http"
require "securerandom"
require "stringio"
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

if ARGV == ["--migration-scenario"]
  raise "migration scenario requires an anonymous client" if ENV.key?("GCS_MIMIC_TOKEN")

  require_relative "migration_scenario"
  migration_scenario(storage)
  exit
end

bucket = nil
objects = []
Tempfile.create("gcs-mimic-ruby-upload") do |source|
  source.binmode
  9.times { source.write((0..255).to_a.pack("C*") * 256) }
  source.flush
  expected = Digest::SHA256.file(source.path).hexdigest
  begin
    bucket = storage.create_bucket("sdk-ruby-basic-#{SecureRandom.hex(8)}")
    puts "ruby-default-create-bucket passed"
    regional_bucket = nil
    begin
      regional_bucket = storage.create_bucket("sdk-ruby-region-#{SecureRandom.hex(8)}", location: "ASIA-NORTHEAST1")
      raise "regional create location mismatch" unless regional_bucket.location == "ASIA-NORTHEAST1" && regional_bucket.location_type == "region"
      regional_bucket.reload!
      raise "regional get location mismatch" unless regional_bucket.location == "ASIA-NORTHEAST1" && regional_bucket.location_type == "region"
      puts "ruby-regional-location passed"
    ensure
      regional_bucket&.delete
    end
    begin
      # Use the SDK's default retry count for unsupported calls, independent of the transfer suite.
      api.request_options.retries = 3
      {
        "autoclass" => -> { storage.create_bucket("sdk-ruby-unsupported-#{SecureRandom.hex(8)}", autoclass_enabled: true) },
        "retentionPolicy" => -> { bucket.retention_period = 3600 },
        "softDeleted" => -> { bucket.files(soft_deleted: true) },
        "includeFoldersAsPrefixes" => -> { bucket.files(delimiter: "/", include_folders_as_prefixes: true) },
        "bucket ACL" => -> { bucket.acl.reload! },
        "default object ACL" => -> { bucket.default_acl.reload! },
        "object ACL" => -> { bucket.file("missing", skip_lookup: true).acl.reload! },
        "notifications" => -> { bucket.notifications },
        "HMAC keys" => -> { storage.hmac_keys.to_a },
        "serviceAccount" => -> { storage.service_account_email },
        "move" => -> { bucket.move_file("missing", "destination") },
        "restore" => -> { bucket.restore_file("missing", 1) },
        "lockRetentionPolicy" => -> { bucket.lock_retention_policy! }
      }.each do |name, call|
        started = Process.clock_gettime(Process::CLOCK_MONOTONIC)
        begin
          call.call
        rescue Google::Cloud::InvalidArgumentError, Google::Apis::ClientError => error
          elapsed = Process.clock_gettime(Process::CLOCK_MONOTONIC) - started
          raise "#{name} waited for retries: #{elapsed}" unless elapsed < 3
          api_error = error.is_a?(Google::Apis::ClientError) ? error : error.cause
          unless api_error.status_code == 400 && JSON.parse(api_error.body).dig("error", "errors", 0, "reason") == "badRequest"
            raise "#{name} differs from 400 badRequest: #{error.message}"
          end
          raise "#{name} lacks actionable diagnostics: #{error.message}" unless error.message.include?("gcs-mimic")
          next
        end
        raise "#{name} unexpectedly succeeded"
      end
      puts "ruby-unsupported-fails-fast passed"
    ensure
      api.request_options.retries = 0
    end
    uniform_bucket = nil
    begin
      uniform_bucket = storage.create_bucket("sdk-ruby-ubla-#{SecureRandom.hex(8)}") do |item|
        item.uniform_bucket_level_access = true
      end
      uniform_bucket.reload!
      raise "uniform access missing" unless uniform_bucket.uniform_bucket_level_access?
      raise "uniform access deadline missing" unless uniform_bucket.uniform_bucket_level_access_locked_at
      uniform_bucket.uniform_bucket_level_access = false
      uniform_bucket.reload!
      raise "uniform access still enabled" if uniform_bucket.uniform_bucket_level_access?
      puts "ruby-bucket-uniform-access passed"
    ensure
      uniform_bucket&.delete
    end
    lifecycle_bucket = nil
    begin
      lifecycle_bucket = storage.create_bucket("sdk-ruby-lifecycle-#{SecureRandom.hex(8)}") do |item|
        item.lifecycle { |rules| rules.add_delete_rule age: 30 }
      end
      lifecycle_bucket.reload!
      raise "created lifecycle differs" unless lifecycle_bucket.lifecycle.first.action == "Delete" && lifecycle_bucket.lifecycle.first.age == 30
      lifecycle_bucket.lifecycle do |rules|
        rules.clear
        rules.add_delete_rule age: 60
        rules.add_set_storage_class_rule "COLDLINE", age: 60
      end
      lifecycle_bucket.reload!
      raise "updated lifecycle differs" unless lifecycle_bucket.lifecycle.count == 2 && lifecycle_bucket.lifecycle.last.storage_class == "COLDLINE"
      lifecycle_bucket.lifecycle(&:clear)
      lifecycle_bucket.reload!
      raise "lifecycle not cleared" unless lifecycle_bucket.lifecycle.empty?
      puts "ruby-bucket-lifecycle-rules passed"
    ensure
      lifecycle_bucket&.delete
    end
    permissions = ["storage.objects.get", "storage.objects.create", "storage.buckets.getIamPolicy"]
    raise "bucket test permissions mismatch" unless bucket.test_permissions(*permissions).sort == permissions.sort
    if ENV.key?("GCS_MIMIC_TOKEN")
      begin
        api.authorization = "local-viewer-token"
        raise "project permissions mismatch" unless bucket.test_permissions(*permissions) == ["storage.objects.get"]
        api.authorization = "local-bucket-reader-token"
        raise "ungranted permissions mismatch" unless bucket.test_permissions(*permissions).empty?
      ensure
        api.authorization = ENV.fetch("GCS_MIMIC_TOKEN")
      end
    end
    puts "ruby-bucket-test-permissions passed"
    held_file = nil
    begin
      custom_time = DateTime.new(2026, 10, 20, 1, 2, 3)
      held_file = bucket.create_file(StringIO.new("held bytes"), "holds-custom-time", custom_time: custom_time)
      raise "upload custom time differs" unless held_file.custom_time == custom_time
      held_file.set_temporary_hold!
      held_file.reload!
      raise "temporary hold missing" unless held_file.temporary_hold?
      begin
        held_file.delete
        raise "held object was deleted"
      rescue Google::Cloud::PermissionDeniedError
        # A hold protects bytes while metadata remains editable.
      end
      held_file.release_temporary_hold!
      held_file.custom_time = custom_time + 1
      held_file.reload!
      raise "hold release/custom time differs" if held_file.temporary_hold? || held_file.custom_time != custom_time + 1
      puts "ruby-object-holds-custom-time passed"
    ensure
      held_file&.release_temporary_hold!
      held_file&.delete
    end
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

    default_file = nil
    begin
      default_file = bucket.create_file(StringIO.new("Ruby default bytes"), "extensionless")
      raise "default content type differs" unless default_file.content_type == "application/octet-stream"
      downloaded = default_file.download
      begin
        raise "default upload bytes differ" unless downloaded.read == "Ruby default bytes"
      ensure
        downloaded.close
      end
      puts "ruby-default-create-file-content-type passed"
    ensure
      default_file&.delete
    end
    observed[:chunks] = 0
    observed[:locations].clear
    source.rewind
    object = bucket.create_file(source, "dir/日本語 +%?.bin",
                                if_generation_match: 0)
    objects << object

    if ENV["GCS_MIMIC_SIGNED_TESTS"] == "1"
      key = File.read(File.expand_path("../../fixtures/signing/test-only-private.pem", __dir__))
      issuer = "local-admin@example.invalid"
      authority = origin.authority
      # The SDK's bucket-bound mode preserves host:port but omits its bucket path prefix.
      signing_file = bucket.file("#{bucket.name}/signed.txt", skip_lookup: true)
      signed_url = lambda do |method, query = {}|
        signing_file.signed_url(version: :v4, method: method, issuer: issuer, signing_key: key,
          expires: 300, scheme: origin.scheme, bucket_bound_hostname: authority,
          headers: { "Content-Type" => "text/plain" }, query: query)
      end
      call = lambda do |method, url, content_type, expected_status, changed_host = nil|
        uri = URI(url)
        unless [uri.scheme, uri.host, uri.port] == [origin.scheme, origin.host, origin.port]
          raise "refusing signed request outside the emulator origin"
        end
        http = Net::HTTP.new(uri.host, uri.port, nil)
        http.ipaddr = "127.0.0.1" if uri.hostname == "localhost"
        http.open_timeout = http.read_timeout = http.write_timeout = 10
        if uri.scheme == "https"
          http.use_ssl = true
          http.ca_file = ca
          http.verify_mode = OpenSSL::SSL::VERIFY_PEER
          http.verify_hostname = true
        end
        request = Net::HTTPGenericRequest.new(method, method == "PUT", true, uri.request_uri)
        request["Content-Type"] = content_type if content_type
        request["Host"] = changed_host if changed_host
        request.body = "Ruby signed private bytes" if method == "PUT"
        bytes = +""
        status = nil
        http.start do |client|
          client.request(request) do |response|
            status = response.code.to_i
            if expected_status == 200 && uri.query.include?("response-content-disposition=")
              raise "Ruby disposition override differs" unless response["Content-Disposition"] == 'attachment; filename="Ruby + file.csv"'
              raise "Ruby type override differs" unless response["Content-Type"] == "text/csv"
            end
            response.read_body do |chunk|
              bytes << chunk
              raise "signed response exceeded test limit" if bytes.bytesize > 64 * 1024
            end
          end
        end
        raise "signed #{method} expected #{expected_status}, got #{status}: #{bytes}" unless status == expected_status
        if expected_status == 403 && !bytes.include?("<Code>SignatureDoesNotMatch</Code>")
          raise "unexpected signature error: #{bytes}"
        end
        bytes
      end
      put_url = signed_url.call("PUT")
      get_url = signed_url.call("GET")
      raise "Ruby signer lost the port" unless URI(put_url).authority == authority
      raise "Ruby signer changed the path" unless URI(put_url).path == "/#{bucket.name}/signed.txt"
      call.call("PUT", put_url, "text/plain", 200)
      objects << bucket.file("signed.txt")
      normal_file = bucket.file("signed.txt", skip_lookup: true)
      default_url = normal_file.signed_url(version: :v4, method: "GET", issuer: issuer, signing_key: key,
        expires: 300, headers: { "Content-Type" => "text/plain" })
      raise "endpoint URL unexpectedly changed" unless URI(default_url).authority == authority
      default_uri = URI(default_url)
      parameters = URI.decode_www_form(default_uri.query).to_h
      canonical_query = default_uri.query.split("&").reject { |pair| pair.start_with?("X-Goog-Signature=") }.join("&")
      canonical = ["GET", default_uri.path, canonical_query,
        "content-type:text/plain\nhost:storage.googleapis.com\n", "content-type;host", "UNSIGNED-PAYLOAD"].join("\n")
      signing_input = lambda do |request|
        ["GOOG4-RSA-SHA256", parameters.fetch("X-Goog-Date"),
          parameters.fetch("X-Goog-Credential").split("/", 2).last, Digest::SHA256.hexdigest(request)].join("\n")
      end
      rsa = OpenSSL::PKey::RSA.new(key)
      signature = [parameters.fetch("X-Goog-Signature")].pack("H*")
      raise "Ruby canonical host assumption differs from the SDK" unless rsa.verify("SHA256", signature, signing_input.call(canonical))
      actual_host = canonical.sub("host:storage.googleapis.com\n", "host:#{authority}\n")
      raise "Ruby signer unexpectedly bound the endpoint host" if rsa.verify("SHA256", signature, signing_input.call(actual_host))
      call.call("GET", default_url, "text/plain", 403)
      call.call("PUT", put_url, "changed", 403)
      call.call("PUT", put_url, nil, 403)
      call.call("PUT", "#{put_url}&tampered=1", "text/plain", 403)
      call.call("PUT", put_url.sub("/signed.txt?", "/changed.txt?"), "text/plain", 403)
      call.call("PUT", put_url, "text/plain", 403, "changed.invalid")
      call.call("GET", put_url, "text/plain", 403)
      call.call("PUT", get_url, "text/plain", 403)
      raise "signed bytes differ" unless call.call("GET", get_url, "text/plain", 200) == "Ruby signed private bytes"
      overrides = { "response-content-disposition" => 'attachment; filename="Ruby + file.csv"', "response-content-type" => "text/csv" }
      override_url = signed_url.call("GET", overrides)
      raise "Ruby override bytes differ" unless call.call("GET", override_url, "text/plain", 200) == "Ruby signed private bytes"
      changed_override = override_url.sub("text%2Fcsv", "text%2Fhtml")
      raise "Ruby override tamper did not change URL" if changed_override == override_url
      call.call("GET", changed_override, "text/plain", 403)
      raise "Ruby override mutated metadata" unless bucket.file("signed.txt").content_type == "text/plain"
      puts "Ruby V4 response overrides passed: official query option, exact headers and bytes, value tamper rejection and unchanged metadata"
      private_url = "#{origin.to_s.delete_suffix('/')}/#{bucket.name}/signed.txt"
      call.call("GET", private_url, nil, 401)
      puts "Ruby V4 GET/PUT passed: default endpoint signer binds storage.googleapis.com and fails locally; bucket-bound host:port with an explicit bucket/name path succeeds without rewriting signed URLs"
      objects.pop.delete
    end
    raise "object size mismatch" unless object.size == source.size
    raise "object metadata mismatch" unless bucket.file(object.name).generation == object.generation
    raise "object absent from list" unless bucket.files.any? { |item| item.name == object.name }
    globbed = bucket.files(match_glob: "**/*.bin").all.map(&:name)
    raise "glob listing mismatch" unless globbed == [object.name]
    raise "glob empty listing mismatch" unless bucket.files(match_glob: "missing-*").all.to_a.empty?
    puts "ruby-match-glob passed"
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
    composed = bucket.compose([object, rewritten], "compose.bin")
    puts "ruby-default-compose passed"
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
