# frozen_string_literal: true

# Emulator-neutral Ruby SDK probes. Prints one JSON line per case; every product
# receives the same cases, inputs and pass criteria as the Go probes.
require "google/cloud/storage"
require "json"
require "net/http"
require "securerandom"
require "stringio"
require "timeout"
require "uri"
require "zlib"

ORIGIN = URI(ENV.fetch("SCORECARD_ENDPOINT"))
PROJECT = ENV.fetch("SCORECARD_PROJECT", "local-project")
SIGNING_KEY = File.read(ENV.fetch("SCORECARD_SIGNING_KEY",
                                  File.expand_path("../../fixtures/signing/test-only-private.pem", __dir__)))
SIGNER = "local-admin@example.invalid"
%w[http_proxy https_proxy HTTP_PROXY HTTPS_PROXY all_proxy ALL_PROXY].each { |key| ENV.delete(key) }
# Keep stdout as pure JSONL; the generated client logs retry warnings there by default.
Google::Apis.logger = Logger.new($stderr, level: Logger::WARN)

# Anonymous construction prevents credential discovery and metadata-server access.
STORAGE = Google::Cloud::Storage.anonymous(endpoint: "#{ORIGIN.to_s.delete_suffix('/')}/",
                                           retries: 0, timeout: 20, upload_chunk_size: 256 * 1024)
STORAGE.service.project = PROJECT

class ProbeFailure < StandardError; end

def check(condition, message)
  raise ProbeFailure, message unless condition
end

def expect_error(klass)
  yield
  raise ProbeFailure, "expected #{klass.name.split('::').last}, got success"
rescue klass
  nil
end

def unique_name
  "sc-rb-#{SecureRandom.hex(6)}"
end

def new_bucket(**options, &block)
  STORAGE.create_bucket(unique_name, **options, &block)
end

def upload(bucket, name, data, **options)
  bucket.create_file(StringIO.new(data), name, **options)
end

def download(file, **options)
  io = file.download(**options)
  io.rewind
  io.read
end

def payload(size)
  ("a".."z").to_a.join * (size / 26 + 1)
end

def http(method, url, body: nil, headers: {})
  uri = URI(url)
  Net::HTTP.start(uri.host, uri.port, nil, use_ssl: uri.scheme == "https", open_timeout: 10, read_timeout: 20) do |client|
    request = Net::HTTPGenericRequest.new(method, !body.nil?, true, uri.request_uri)
    headers.each { |key, value| request[key] = value }
    request.body = body if body
    response = client.request(request)
    [response.code.to_i, response.body.to_s]
  end
end

def signed_v4(bucket, name, method)
  # The SDK's bucket-bound mode preserves host:port but omits the bucket path prefix.
  bucket.file("#{bucket.name}/#{name}", skip_lookup: true)
        .signed_url(version: :v4, method: method, issuer: SIGNER, signing_key: SIGNING_KEY, expires: 300,
                    scheme: ORIGIN.scheme, bucket_bound_hostname: ORIGIN.authority)
end

def seed(bucket, *names)
  names.each { |name| upload(bucket, name, name) }
end

def names_of(list)
  list.all.map(&:name).sort
end

CASES = []
def probe(id, area, &block)
  CASES << [id, area, block]
end

probe "bucket.create_get_delete", "bucket" do
  bucket = new_bucket
  check STORAGE.bucket(bucket.name), "bucket lookup failed"
  bucket.delete
  check STORAGE.bucket(bucket.name).nil?, "deleted bucket still visible"
end

probe "bucket.list_prefix_paging", "bucket" do
  prefix = unique_name
  3.times { |i| STORAGE.create_bucket("#{prefix}-#{i}") }
  page = STORAGE.buckets(prefix: prefix, max: 2)
  check page.size <= 2, "page size #{page.size} exceeds 2"
  names = page.all.map(&:name).sort
  check names == (0..2).map { |i| "#{prefix}-#{i}" }, "buckets #{names}"
end

probe "bucket.update_labels", "bucket" do
  bucket = new_bucket { |b| b.labels = { "a" => "1", "b" => "2" } }
  bucket.update { |b| b.labels = { "b" => "2", "c" => "3" } }
  labels = STORAGE.bucket(bucket.name).labels
  check labels == { "b" => "2", "c" => "3" }, "labels #{labels}"
end

probe "bucket.metageneration_precondition", "bucket" do
  bucket = new_bucket
  expect_error(Google::Cloud::FailedPreconditionError) do
    bucket.update(if_metageneration_match: 999) { |b| b.storage_class = "NEARLINE" }
  end
end

probe "bucket.versioning_history", "bucket" do
  bucket = new_bucket(versioning: true)
  first = upload(bucket, "v.txt", "one")
  upload(bucket, "v.txt", "two")
  versions = bucket.files(versions: true).all.to_a
  check versions.size == 2, "versions listed #{versions.size}"
  old = bucket.file("v.txt", generation: first.generation)
  check download(old) == "one", "old generation bytes differ"
end

probe "bucket.lifecycle", "bucket" do
  bucket = new_bucket
  bucket.lifecycle { |l| l.add_delete_rule age: 30 }
  rules = STORAGE.bucket(bucket.name).lifecycle
  check rules.size == 1 && rules.first.age == 30, "lifecycle #{rules.to_a.inspect}"
end

probe "bucket.cors", "bucket" do
  bucket = new_bucket
  bucket.cors { |c| c.add_rule ["https://app.example"], "GET", max_age: 3600 }
  rules = STORAGE.bucket(bucket.name).cors
  check rules.size == 1 && rules.first.origin == ["https://app.example"], "cors #{rules.to_a.inspect}"
end

probe "bucket.retention_policy", "bucket" do
  bucket = new_bucket
  bucket.retention_period = 3600
  file = upload(bucket, "r", "r")
  expect_error(Google::Cloud::PermissionDeniedError) { file.delete }
end

probe "bucket.retention_lock", "bucket" do
  bucket = new_bucket
  bucket.retention_period = 3600
  bucket.reload!
  bucket.lock_retention_policy!
  check STORAGE.bucket(bucket.name).retention_policy_locked?, "policy not locked"
end

probe "bucket.uniform_bucket_level_access", "bucket" do
  bucket = new_bucket
  bucket.uniform_bucket_level_access = true
  check STORAGE.bucket(bucket.name).uniform_bucket_level_access?, "UBLA not enabled"
end

probe "bucket.public_access_prevention", "bucket" do
  bucket = new_bucket
  bucket.public_access_prevention = :enforced
  check STORAGE.bucket(bucket.name).public_access_prevention_enforced?, "PAP not enforced"
end

probe "bucket.iam_policy", "bucket" do
  bucket = new_bucket
  bucket.policy(requested_policy_version: 3) do |policy|
    policy.bindings.insert role: "roles/storage.objectViewer", members: ["user:someone@example.invalid"]
  end
  bindings = bucket.policy(requested_policy_version: 3).bindings.to_a
  found = bindings.any? { |b| b.role == "roles/storage.objectViewer" && b.members.include?("user:someone@example.invalid") }
  check found, "binding missing after update"
end

probe "bucket.iam_test_permissions", "bucket" do
  bucket = new_bucket
  permissions = bucket.test_permissions "storage.buckets.get", "storage.objects.list"
  check !permissions.empty?, "no permissions returned"
end

probe "bucket.soft_delete_policy", "bucket" do
  bucket = new_bucket
  bucket.soft_delete_policy = { retention_duration_seconds: 10 * 24 * 60 * 60 }
  policy = STORAGE.bucket(bucket.name).soft_delete_policy
  check policy&.retention_duration_seconds == 10 * 24 * 60 * 60, "soft delete policy #{policy.inspect}"
end

probe "bucket.autoclass", "bucket" do
  bucket = new_bucket(autoclass_enabled: true)
  check STORAGE.bucket(bucket.name).autoclass_enabled, "autoclass not enabled"
end

probe "bucket.acl", "bucket" do
  bucket = new_bucket
  bucket.acl.add_reader "allAuthenticatedUsers"
  bucket.acl.reload!
  check bucket.acl.readers.include?("allAuthenticatedUsers"), "ACL readers #{bucket.acl.readers}"
end

probe "bucket.default_object_acl", "bucket" do
  bucket = new_bucket
  bucket.default_acl.add_reader "allAuthenticatedUsers"
  bucket.default_acl.reload!
  check bucket.default_acl.readers.include?("allAuthenticatedUsers"), "default ACL readers #{bucket.default_acl.readers}"
end

probe "bucket.notifications", "bucket" do
  bucket = new_bucket
  notification = bucket.create_notification "projects/#{PROJECT}/topics/scorecard"
  check bucket.notifications.map(&:id).include?(notification.id), "notification not listed"
  notification.delete
end

probe "project.hmac_keys", "bucket" do
  key = STORAGE.create_hmac_key "sa@example.invalid"
  check !key.secret.to_s.empty? && !key.access_id.to_s.empty?, "empty HMAC key"
  key.inactive!
  key.delete!
end

probe "project.service_account", "bucket" do
  check STORAGE.service_account_email.to_s.include?("@"), "service account missing"
end

probe "object.write_read_small", "object" do
  bucket = new_bucket
  upload(bucket, "dir/日本語 +%?.txt", "hello")
  check download(bucket.file("dir/日本語 +%?.txt")) == "hello", "bytes differ"
end

probe "object.resumable_large", "object" do
  bucket = new_bucket
  data = payload(3 * 1024 * 1024 + 123)[0, 3 * 1024 * 1024 + 123]
  file = upload(bucket, "big.bin", data)
  check file.size == data.bytesize, "size #{file.size}"
  check download(bucket.file("big.bin")) == data, "bytes differ"
end

probe "object.metadata_on_write", "object" do
  bucket = new_bucket
  upload(bucket, "meta.txt", "x", content_type: "text/plain", cache_control: "no-cache",
                                  content_disposition: "attachment", content_language: "ja", metadata: { "k" => "v" })
  file = bucket.file("meta.txt")
  check file.content_type == "text/plain" && file.cache_control == "no-cache" &&
        file.content_disposition == "attachment" && file.content_language == "ja" &&
        file.metadata == { "k" => "v" }, "attributes #{file.gapi.to_h}"
end

probe "object.checksum_rejected", "object" do
  bucket = new_bucket
  begin
    upload(bucket, "crc.txt", "payload", crc32c: "AAAAAA==")
    raise ProbeFailure, "wrong CRC32C accepted"
  rescue Google::Cloud::Error
    nil
  end
  check bucket.file("crc.txt").nil?, "rejected upload left an object"
end

probe "object.md5_accepted", "object" do
  bucket = new_bucket
  md5 = Digest::MD5.base64digest("md5 payload")
  file = upload(bucket, "md5.txt", "md5 payload", md5: md5)
  check file.md5 == md5, "md5 #{file.md5}"
end

probe "object.range_reads", "object" do
  bucket = new_bucket
  file = upload(bucket, "range.txt", "0123456789")
  check download(file, range: 2..5) == "2345", "closed range"
  # The SDK rejects endless Ruby ranges, so open and suffix ranges use header strings.
  check download(file, range: "bytes=7-") == "789", "open range"
  check download(file, range: "bytes=-3") == "789", "suffix range"
end

probe "object.update_metadata", "object" do
  bucket = new_bucket
  file = upload(bucket, "u.txt", "x", metadata: { "a" => "1", "b" => "2" })
  file.update do |f|
    f.content_type = "text/csv"
    f.metadata["a"] = nil
    f.metadata["c"] = "3"
  end
  file = bucket.file("u.txt")
  check file.content_type == "text/csv" && file.metadata == { "b" => "2", "c" => "3" }, "metadata #{file.metadata}"
end

probe "object.delete_not_found", "object" do
  bucket = new_bucket
  file = upload(bucket, "d.txt", "x")
  file.delete
  check bucket.file("d.txt").nil?, "object visible after delete"
  expect_error(Google::Cloud::NotFoundError) { bucket.file("d.txt", skip_lookup: true).delete }
end

probe "object.does_not_exist_precondition", "object" do
  bucket = new_bucket
  upload(bucket, "once.txt", "first", if_generation_match: 0)
  expect_error(Google::Cloud::FailedPreconditionError) { upload(bucket, "once.txt", "second", if_generation_match: 0) }
  check download(bucket.file("once.txt")) == "first", "precondition failure changed bytes"
end

probe "object.generation_match_precondition", "object" do
  bucket = new_bucket
  file = upload(bucket, "g.txt", "x")
  expect_error(Google::Cloud::FailedPreconditionError) { file.delete if_generation_match: file.generation + 1 }
  file.delete if_generation_match: file.generation
end

probe "object.metageneration_match_precondition", "object" do
  bucket = new_bucket
  file = upload(bucket, "m.txt", "x")
  expect_error(Google::Cloud::FailedPreconditionError) do
    file.update(if_metageneration_match: file.metageneration + 5) { |f| f.content_type = "text/plain" }
  end
end

probe "object.list_prefix_delimiter", "object" do
  bucket = new_bucket
  seed bucket, "a/1", "a/2", "b/1", "c"
  list = bucket.files(delimiter: "/")
  check list.prefixes.sort == ["a/", "b/"], "prefixes #{list.prefixes}"
  check list.map(&:name) == ["c"], "names #{list.map(&:name)}"
  check names_of(bucket.files(prefix: "a/")) == ["a/1", "a/2"], "prefix listing"
end

probe "object.list_paging", "object" do
  bucket = new_bucket
  seed bucket, "p1", "p2", "p3", "p4", "p5"
  page = bucket.files(max: 2)
  pages = 1
  names = page.map(&:name)
  check page.size <= 2, "page size #{page.size}"
  while page.next?
    page = page.next
    pages += 1
    check page.size <= 2, "page size #{page.size}"
    names.concat(page.map(&:name))
  end
  check pages >= 3 && names.sort == %w[p1 p2 p3 p4 p5], "pages=#{pages} names=#{names}"
end

probe "object.list_offsets", "object" do
  bucket = new_bucket
  seed bucket, "a", "b", "c", "d"
  # The Ruby SDK forwards offsets through the generated client's list options.
  gapi = STORAGE.service.service.list_objects(bucket.name, start_offset: "b", end_offset: "d")
  names = Array(gapi.items).map(&:name).sort
  check names == %w[b c], "names #{names}"
end

probe "object.list_match_glob", "object" do
  bucket = new_bucket
  seed bucket, "x/a.txt", "x/b.csv", "y/c.txt"
  check names_of(bucket.files(match_glob: "**/*.txt")) == ["x/a.txt", "y/c.txt"], "glob listing"
end

probe "object.copy_with_metadata", "object" do
  bucket = new_bucket
  destination = new_bucket
  source = upload(bucket, "src", "copy me")
  copied = source.copy(destination.name, "dst") do |f|
    f.content_type = "text/plain"
    f.metadata = { "copied" => "yes" }
  end
  check copied.content_type == "text/plain" && copied.metadata["copied"] == "yes", "copy attrs"
  check download(destination.file("dst")) == "copy me", "copy bytes"
end

probe "object.rewrite_storage_class", "object" do
  bucket = new_bucket
  data = payload(2 * 1024 * 1024 + 7)[0, 2 * 1024 * 1024 + 7]
  source = upload(bucket, "src", data)
  rewritten = source.rewrite("dst") { |f| f.storage_class = :nearline }
  check rewritten.storage_class == "NEARLINE", "storage class #{rewritten.storage_class}"
  check download(bucket.file("dst")) == data, "rewrite bytes"
end

probe "object.compose", "object" do
  bucket = new_bucket
  seed bucket, "p1", "p2", "p3"
  composed = bucket.compose(%w[p1 p2 p3], "all")
  check composed.gapi.component_count == 3, "componentCount #{composed.gapi.component_count}"
  check download(bucket.file("all")) == "p1p2p3", "compose bytes"
end

probe "object.move", "object" do
  bucket = new_bucket
  upload(bucket, "old", "moved")
  bucket.move_file "old", "new"
  check bucket.file("old").nil?, "source remains"
  check download(bucket.file("new")) == "moved", "move bytes"
end

probe "object.acl", "object" do
  bucket = new_bucket
  file = upload(bucket, "acl.txt", "x")
  file.acl.add_reader "allUsers"
  file.acl.reload!
  check file.acl.readers.include?("allUsers"), "object ACL readers #{file.acl.readers}"
end

probe "object.holds", "object" do
  bucket = new_bucket
  file = upload(bucket, "hold.txt", "x")
  file.set_temporary_hold!
  expect_error(Google::Cloud::PermissionDeniedError) { file.delete }
  file.release_temporary_hold!
  file.delete
end

probe "object.retention", "object" do
  bucket = new_bucket(enable_object_retention: true)
  file = upload(bucket, "ret.txt", "x")
  file.retention = { mode: "Unlocked", retain_until_time: (Time.now + 3600).to_datetime }
  check bucket.file("ret.txt").retention_mode == "Unlocked", "retention not stored"
  expect_error(Google::Cloud::PermissionDeniedError) { file.delete }
end

probe "object.custom_time", "object" do
  bucket = new_bucket
  file = upload(bucket, "ct.txt", "x")
  want = DateTime.new(2026, 1, 2, 3, 4, 5)
  file.custom_time = want
  check bucket.file("ct.txt").custom_time == want, "customTime #{bucket.file('ct.txt').custom_time}"
end

probe "object.storage_class_on_write", "object" do
  bucket = new_bucket
  file = upload(bucket, "cold", "x", storage_class: :coldline)
  check file.storage_class == "COLDLINE", "storage class #{file.storage_class}"
end

probe "object.gzip_transcoding", "object" do
  bucket = new_bucket
  plain = "compress me " * 50
  zipped = StringIO.new.tap do |io|
    gz = Zlib::GzipWriter.new(io)
    gz.write(plain)
    gz.finish
  end.string
  upload(bucket, "gz.txt", zipped, content_type: "text/plain", content_encoding: "gzip")
  file = bucket.file("gz.txt")
  check download(file) == plain, "decompressed read differs"
  check download(file, skip_decompress: true).b == zipped.b, "compressed read differs"
end

probe "object.customer_supplied_key", "object" do
  bucket = new_bucket
  key = SecureRandom.random_bytes(32)
  upload(bucket, "csek.txt", "secret", encryption_key: key)
  file = bucket.file("csek.txt")
  begin
    download(file)
    raise ProbeFailure, "read without key succeeded"
  rescue Google::Cloud::Error
    nil
  end
  check download(file, encryption_key: key) == "secret", "keyed read differs"
end

probe "object.soft_delete_restore", "object" do
  bucket = new_bucket
  bucket.soft_delete_policy = { retention_duration_seconds: 7 * 24 * 60 * 60 }
  file = upload(bucket, "sd.txt", "restore me")
  generation = file.generation
  file.delete
  check names_of(bucket.files(soft_deleted: true)) == ["sd.txt"], "soft-deleted listing"
  bucket.restore_file "sd.txt", generation
  check download(bucket.file("sd.txt")) == "restore me", "restored bytes"
end

probe "signed.v4_get", "signed" do
  bucket = new_bucket
  upload(bucket, "s.txt", "signed")
  status, body = http("GET", signed_v4(bucket, "s.txt", "GET"))
  check status == 200 && body == "signed", "status #{status} body #{body[0, 200]}"
end

probe "signed.v4_put", "signed" do
  bucket = new_bucket
  status, body = http("PUT", signed_v4(bucket, "put.txt", "PUT"), body: "uploaded")
  check status == 200, "status #{status} body #{body[0, 200]}"
  check download(bucket.file("put.txt")) == "uploaded", "uploaded bytes differ"
end

probe "signed.v2_get", "signed" do
  bucket = new_bucket
  upload(bucket, "s2.txt", "signed v2")
  url = bucket.file("s2.txt", skip_lookup: true).signed_url(method: "GET", issuer: SIGNER, signing_key: SIGNING_KEY)
  check URI(url).authority == ORIGIN.authority, "signer changed endpoint: #{url}"
  status, body = http("GET", url)
  check status == 200 && body == "signed v2", "status #{status} body #{body[0, 200]}"
end

probe "signed.post_policy_v4", "signed" do
  bucket = new_bucket
  policy = bucket.generate_signed_post_policy_v4("post.txt", issuer: SIGNER, signing_key: SIGNING_KEY, expires: 300)
  # POST policies sign the policy document rather than the host, so the path-style
  # emulator URL is used without invalidating the signature.
  uri = URI("#{ORIGIN.to_s.delete_suffix('/')}/#{bucket.name}")
  request = Net::HTTP::Post.new(uri)
  request.set_form(policy.fields.map { |k, v| [k, v] } + [["file", StringIO.new("posted"), { filename: "post.txt" }]],
                   "multipart/form-data")
  response = Net::HTTP.start(uri.host, uri.port, use_ssl: uri.scheme == "https") { |client| client.request(request) }
  check response.code.to_i.between?(200, 299), "status #{response.code} body #{response.body.to_s[0, 200]}"
  check download(bucket.file("post.txt")) == "posted", "posted bytes differ"
end

filter = ENV.fetch("SCORECARD_RUN", "")
CASES.each do |id, area, block|
  next unless filter.empty? || id.include?(filter)

  result = { sdk: "ruby", id: id, area: area, pass: true }
  begin
    Timeout.timeout(60) { block.call }
  rescue StandardError, Timeout::Error => e
    result[:pass] = false
    result[:detail] = "#{e.class}: #{e.message}"[0, 300]
  end
  puts JSON.generate(result)
  $stdout.flush
end
