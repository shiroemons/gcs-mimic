// Object cases: the 27 shared `object.*` IDs.
#include "harness.h"
#include "google/cloud/log.h"
#include <zlib.h>

#include <chrono>
#include <ctime>
#include <fstream>
#include <mutex>
#include <map>
#include <memory>
#include <numeric>
#include <random>
#include <variant>

namespace scorecard {
namespace {

std::string GzipCompress(std::string const& plain) {
  z_stream stream{};
  // windowBits 15 + 16 selects the gzip container instead of raw zlib.
  if (deflateInit2(&stream, Z_DEFAULT_COMPRESSION, Z_DEFLATED, 15 + 16, 8, Z_DEFAULT_STRATEGY) !=
      Z_OK) {
    Fail("deflateInit2 failed");
  }
  std::string out(deflateBound(&stream, plain.size()), '\0');
  stream.next_in = reinterpret_cast<Bytef*>(const_cast<char*>(plain.data()));
  stream.avail_in = static_cast<uInt>(plain.size());
  stream.next_out = reinterpret_cast<Bytef*>(out.data());
  stream.avail_out = static_cast<uInt>(out.size());
  auto const code = deflate(&stream, Z_FINISH);
  out.resize(stream.total_out);
  deflateEnd(&stream);
  if (code != Z_STREAM_END) Fail("gzip compression failed");
  return out;
}

std::string RandomBytes(std::size_t count) {
  std::random_device device;
  std::string bytes(count, '\0');
  for (auto& byte : bytes) byte = static_cast<char>(device() & 0xFF);
  return bytes;
}

std::string Repeat(std::string const& unit, std::size_t times) {
  std::string out;
  out.reserve(unit.size() * times);
  for (std::size_t i = 0; i < times; ++i) out += unit;
  return out;
}

void WriteSmall(Ctx& c) {
  auto const bucket = NewBucket(c);
  // Non-ASCII, space, plus, percent and question mark exercise path encoding.
  auto const name = std::string("dir/\xE6\x97\xA5\xE6\x9C\xAC\xE8\xAA\x9E +%?.txt");
  Upload(c, bucket, name, "hello");
  CheckEqual(Download(c, bucket, name), "hello", "bytes");
}

void ResumableLarge(Ctx& c) {
  auto const bucket = NewBucket(c);
  auto const data = std::string(3 * 1024 * 1024 + 123, 'a');
  // The default 8 MiB upload buffer would send this in one chunk; 256 KiB
  // forces a multi-chunk resumable session like the other probes.
  auto stream = c.sdk.WriteObject(bucket, "big.bin", gcs::UploadBufferSize(256 * 1024));
  stream.write(data.data(), static_cast<std::streamsize>(data.size()));
  stream.Close();
  auto const metadata = Require(stream.metadata(), "resumable upload");
  Check(metadata.size() == data.size(), "size differs");
  Check(Metadata(c, bucket, "big.bin").size() == data.size(), "stored size differs");
  CheckEqual(Download(c, bucket, "big.bin"), data, "bytes");
}

void MetadataOnWrite(Ctx& c) {
  auto const bucket = NewBucket(c);
  Upload(c, bucket, "meta.txt", "x", gcs::ContentType("text/plain"),
         gcs::WithObjectMetadata(gcs::ObjectMetadata()
                                     .set_cache_control("no-cache")
                                     .set_content_disposition("attachment")
                                     .set_content_language("ja")
                                     .upsert_metadata("k", "v")));
  auto const info = Metadata(c, bucket, "meta.txt");
  Check(info.content_type() == "text/plain" && info.cache_control() == "no-cache" &&
            info.content_disposition() == "attachment" && info.content_language() == "ja" &&
            info.metadata() == (std::map<std::string, std::string>{{"k", "v"}}),
        "metadata differs");
}

// Crc32cChecksumValue and DisableMD5Hash are the only public way to hand
// InsertObject a checksum; the replacement options are client-wide or apply to
// resumable uploads only. The deprecation is the SDK's, not a probe problem.
#pragma GCC diagnostic push
#pragma GCC diagnostic ignored "-Wdeprecated-declarations"
void ChecksumRejected(Ctx& c) {
  auto const bucket = NewBucket(c);
  auto const result = c.sdk.InsertObject(bucket, "crc.txt", "wrong checksum",
                                         gcs::Crc32cChecksumValue("AAAAAA=="),
                                         gcs::DisableMD5Hash(true));
  if (result.ok()) Fail("wrong CRC32C accepted");
  ExpectHttp(result.status(), 400, "wrong CRC32C");
  auto message = result.status().message();
  for (auto& ch : message) ch = static_cast<char>(std::tolower(static_cast<unsigned char>(ch)));
  Check(message.find("checksum") != std::string::npos || message.find("crc32c") != std::string::npos,
        "400 does not name a checksum: " + result.status().message());
  ExpectHttp(c.sdk.GetObjectMetadata(bucket, "crc.txt"), 404, "rejected upload left an object");
}
#pragma GCC diagnostic pop

void Md5Accepted(Ctx& c) {
  auto const bucket = NewBucket(c);
  std::string const data = "md5 payload";
  Upload(c, bucket, "md5.txt", data, gcs::EnableMD5Hash());
  Check(Metadata(c, bucket, "md5.txt").md5_hash() == gcs::ComputeMD5Hash(data), "MD5 differs");
}

void RangeReads(Ctx& c) {
  auto const bucket = NewBucket(c);
  Upload(c, bucket, "range.txt", "0123456789");
  // ReadRange is right-open, unlike the inclusive HTTP range.
  CheckEqual(Download(c, bucket, "range.txt", gcs::ReadRange(2, 6)), "2345", "closed range");
  CheckEqual(Download(c, bucket, "range.txt", gcs::ReadFromOffset(7)), "789", "open range");
  CheckEqual(Download(c, bucket, "range.txt", gcs::ReadLast(3)), "789", "suffix range");
}

void UpdateMetadata(Ctx& c) {
  auto const bucket = NewBucket(c);
  Upload(c, bucket, "u.txt", "x",
         gcs::WithObjectMetadata(
             gcs::ObjectMetadata().upsert_metadata("a", "1").upsert_metadata("b", "2")));
  Require(c.sdk.PatchObject(bucket, "u.txt",
                            gcs::ObjectMetadataPatchBuilder()
                                .SetContentType("text/csv")
                                .ResetMetadata("a")
                                .SetMetadata("c", "3")),
          "patch object");
  auto const info = Metadata(c, bucket, "u.txt");
  Check(info.content_type() == "text/csv" &&
            info.metadata() == (std::map<std::string, std::string>{{"b", "2"}, {"c", "3"}}),
        "metadata differs");
}

void DeleteNotFound(Ctx& c) {
  auto const bucket = NewBucket(c);
  Upload(c, bucket, "d.txt", "x");
  Require(c.sdk.DeleteObject(bucket, "d.txt"), "delete object");
  ExpectHttp(c.sdk.GetObjectMetadata(bucket, "d.txt"), 404, "deleted object visible");
  ExpectHttp(c.sdk.DeleteObject(bucket, "d.txt"), 404, "second delete");
}

void DoesNotExistPrecondition(Ctx& c) {
  auto const bucket = NewBucket(c);
  Upload(c, bucket, "once.txt", "first", gcs::IfGenerationMatch(0));
  ExpectHttp(c.sdk.InsertObject(bucket, "once.txt", "second", gcs::IfGenerationMatch(0)), 412,
             "second create-only write");
  CheckEqual(Download(c, bucket, "once.txt"), "first", "bytes after rejected write");
}

void GenerationMatchPrecondition(Ctx& c) {
  auto const bucket = NewBucket(c);
  auto const generation = Upload(c, bucket, "g.txt", "x").generation();
  ExpectHttp(c.sdk.DeleteObject(bucket, "g.txt", gcs::IfGenerationMatch(generation + 1)), 412,
             "delete with a wrong generation");
  Require(c.sdk.DeleteObject(bucket, "g.txt", gcs::IfGenerationMatch(generation)),
          "delete with the right generation");
}

void MetagenerationMatchPrecondition(Ctx& c) {
  auto const bucket = NewBucket(c);
  auto const metageneration = Upload(c, bucket, "m.txt", "x").metageneration();
  ExpectHttp(c.sdk.PatchObject(bucket, "m.txt",
                               gcs::ObjectMetadataPatchBuilder().SetContentType("text/plain"),
                               gcs::IfMetagenerationMatch(metageneration + 5)),
             412, "patch with a wrong metageneration");
}

void ListPrefixDelimiter(Ctx& c) {
  auto const bucket = NewBucket(c);
  Seed(c, bucket, {"a/1", "a/2", "b/1", "c"});
  std::vector<std::string> names;
  std::vector<std::string> prefixes;
  for (auto& item : c.sdk.ListObjectsAndPrefixes(bucket, gcs::Delimiter("/"))) {
    auto value = Require(std::move(item), "list with delimiter");
    if (auto* object = std::get_if<gcs::ObjectMetadata>(&value)) {
      names.push_back(object->name());
    } else {
      prefixes.push_back(std::get<std::string>(value));
    }
  }
  std::sort(names.begin(), names.end());
  std::sort(prefixes.begin(), prefixes.end());
  CheckEqual(names, {"c"}, "names");
  CheckEqual(prefixes, {"a/", "b/"}, "prefixes");
  CheckEqual(ListNames(c, bucket, gcs::Prefix("a/")), {"a/1", "a/2"}, "prefix listing");
}

// Collects the SDK's own "rpc" log of each list response. The object reader
// flattens pages and no public API exposes one, so the page sizes are counted
// from the logged ListObjects responses.
class ListResponseLog : public gc::LogBackend {
 public:
  void Process(gc::LogRecord const& record) override {
    constexpr char kResponse[] = "ListObjects() >> ";
    constexpr char kItem[] = "ObjectMetadata={name=";
    if (record.message.rfind(kResponse, 0) != 0) return;
    int items = 0;
    for (auto at = record.message.find(kItem); at != std::string::npos;
         at = record.message.find(kItem, at + 1)) {
      ++items;
    }
    std::lock_guard<std::mutex> lock(mutex_);
    page_sizes_.push_back(items);
  }
  void ProcessWithOwnership(gc::LogRecord record) override { Process(record); }

  std::vector<int> PageSizes() const {
    std::lock_guard<std::mutex> lock(mutex_);
    return page_sizes_;
  }

 private:
  mutable std::mutex mutex_;
  std::vector<int> page_sizes_;
};

// Installs a log backend for one scope and restores the sink afterwards, even
// when the case throws.
class ScopedLogBackend {
 public:
  explicit ScopedLogBackend(std::shared_ptr<gc::LogBackend> backend)
      : sink_(gc::LogSink::Instance()),
        id_(sink_.AddBackend(std::move(backend))),
        severity_(sink_.minimum_severity()) {
    sink_.set_minimum_severity(gc::Severity::GCP_LS_DEBUG);
  }
  ~ScopedLogBackend() {
    sink_.set_minimum_severity(severity_);
    sink_.RemoveBackend(id_);
  }
  ScopedLogBackend(ScopedLogBackend const&) = delete;
  ScopedLogBackend& operator=(ScopedLogBackend const&) = delete;

 private:
  gc::LogSink& sink_;
  gc::LogSink::BackendId id_;
  gc::Severity severity_;
};

void ListPaging(Ctx& c) {
  auto const bucket = NewBucket(c);
  std::vector<std::string> const all = {"p1", "p2", "p3", "p4", "p5"};
  Seed(c, bucket, all);
  auto log = std::make_shared<ListResponseLog>();
  std::vector<std::string> names;
  {
    ScopedLogBackend const scope(log);
    auto options = EndpointOptions(c.config);
    options.set<gc::UnifiedCredentialsOption>(gc::MakeInsecureCredentials())
        .set<gc::LoggingComponentsOption>({"rpc"});
    gcs::Client traced(std::move(options));
    for (auto& object : traced.ListObjects(bucket, gcs::MaxResults(2))) {
      names.push_back(Require(std::move(object), "list objects").name());
    }
  }
  auto const sizes = log->PageSizes();
  Check(sizes.size() >= 3, "expected at least 3 list pages, saw " + std::to_string(sizes.size()));
  // If the SDK's log text changed, items would be counted as 0 and every page would look small;
  // requiring the counted items to add up to the listing keeps that from passing silently.
  Check(std::accumulate(sizes.begin(), sizes.end(), 0) == static_cast<int>(all.size()),
        "logged list pages do not account for every object");
  Check(std::all_of(sizes.begin(), sizes.end(), [](int size) { return size <= 2; }),
        "object page exceeds 2");
  std::sort(names.begin(), names.end());
  CheckEqual(names, all, "paged listing");
}

void ListOffsets(Ctx& c) {
  auto const bucket = NewBucket(c);
  Seed(c, bucket, {"a", "b", "c", "d"});
  CheckEqual(ListNames(c, bucket, gcs::StartOffset("b"), gcs::EndOffset("d")), {"b", "c"},
             "offset listing");
}

void ListMatchGlob(Ctx& c) {
  auto const bucket = NewBucket(c);
  Seed(c, bucket, {"x/a.txt", "x/b.csv", "y/c.txt"});
  CheckEqual(ListNames(c, bucket, gcs::MatchGlob("**/*.txt")), {"x/a.txt", "y/c.txt"},
             "glob listing");
}

void CopyWithMetadata(Ctx& c) {
  auto const source_bucket = NewBucket(c);
  auto const destination_bucket = NewBucket(c);
  Upload(c, source_bucket, "src", "copy me");
  Require(c.sdk.RewriteObjectBlocking(
              source_bucket, "src", destination_bucket, "dst",
              gcs::WithObjectMetadata(
                  gcs::ObjectMetadata().set_content_type("text/plain").upsert_metadata("copied", "yes"))),
          "rewrite with metadata");
  auto const info = Metadata(c, destination_bucket, "dst");
  Check(info.content_type() == "text/plain" &&
            info.metadata() == (std::map<std::string, std::string>{{"copied", "yes"}}),
        "copy metadata");
  CheckEqual(Download(c, destination_bucket, "dst"), "copy me", "copy bytes");
}

void RewriteStorageClass(Ctx& c) {
  auto const bucket = NewBucket(c);
  auto const data = std::string(2 * 1024 * 1024 + 7, 'a');
  Upload(c, bucket, "src", data);
  Require(c.sdk.RewriteObjectBlocking(
              bucket, "src", bucket, "dst",
              gcs::WithObjectMetadata(gcs::ObjectMetadata().set_storage_class("NEARLINE"))),
          "rewrite storage class");
  CheckEqual(Metadata(c, bucket, "dst").storage_class(), "NEARLINE", "storage class");
  CheckEqual(Download(c, bucket, "dst"), data, "rewrite bytes");
}

void Compose(Ctx& c) {
  auto const bucket = NewBucket(c);
  Seed(c, bucket, {"p1", "p2", "p3"});
  auto const composed = Require(
      c.sdk.ComposeObject(bucket,
                          {gcs::ComposeSourceObject{"p1", {}, {}},
                           gcs::ComposeSourceObject{"p2", {}, {}},
                           gcs::ComposeSourceObject{"p3", {}, {}}},
                          "all"),
      "compose");
  Check(composed.component_count() == 3, "component count");
  CheckEqual(Download(c, bucket, "all"), "p1p2p3", "compose bytes");
}

void Move(Ctx& c) {
  auto const bucket = NewBucket(c);
  Upload(c, bucket, "old", "moved");
  Require(c.sdk.MoveObject(bucket, "old", "new"), "move object");
  ExpectHttp(c.sdk.GetObjectMetadata(bucket, "old"), 404, "source remains");
  CheckEqual(Download(c, bucket, "new"), "moved", "move bytes");
}

void ObjectAcl(Ctx& c) {
  auto const bucket = NewBucket(c);
  Upload(c, bucket, "acl.txt", "x");
  Require(c.sdk.CreateObjectAcl(bucket, "acl.txt", "allUsers",
                                gcs::ObjectAccessControl::ROLE_READER()),
          "create object ACL");
  bool found = false;
  for (auto const& entry : Require(c.sdk.ListObjectAcl(bucket, "acl.txt"), "list object ACL")) {
    found = found || (entry.entity() == "allUsers" && entry.role() == "READER");
  }
  Check(found, "ACL missing");
}

void Holds(Ctx& c) {
  auto const bucket = NewBucket(c);
  Upload(c, bucket, "hold.txt", "x");
  Require(c.sdk.PatchObject(bucket, "hold.txt",
                            gcs::ObjectMetadataPatchBuilder().SetTemporaryHold(true)),
          "set hold");
  ExpectHttp(c.sdk.DeleteObject(bucket, "hold.txt"), 403, "delete under hold");
  Require(c.sdk.PatchObject(bucket, "hold.txt",
                            gcs::ObjectMetadataPatchBuilder().SetTemporaryHold(false)),
          "release hold");
  Require(c.sdk.DeleteObject(bucket, "hold.txt"), "delete after hold");
}

void Retention(Ctx& c) {
  auto const bucket = NewBucket(c, {}, gcs::EnableObjectRetention(true));
  Upload(c, bucket, "ret.txt", "x");
  gcs::ObjectRetention retention;
  retention.mode = gcs::ObjectRetentionUnlocked();
  retention.retain_until_time = std::chrono::system_clock::now() + std::chrono::hours(1);
  Require(c.sdk.PatchObject(bucket, "ret.txt",
                            gcs::ObjectMetadataPatchBuilder().SetRetention(retention)),
          "set retention");
  auto const info = Metadata(c, bucket, "ret.txt");
  Check(info.has_retention() && info.retention().mode == "Unlocked", "retention not stored");
  ExpectHttp(c.sdk.DeleteObject(bucket, "ret.txt"), 403, "delete under retention");
}

void CustomTime(Ctx& c) {
  auto const bucket = NewBucket(c);
  Upload(c, bucket, "ct.txt", "x");
  std::tm tm{};
  tm.tm_year = 2026 - 1900;
  tm.tm_mon = 0;
  tm.tm_mday = 2;
  tm.tm_hour = 3;
  tm.tm_min = 4;
  tm.tm_sec = 5;
  auto const want = std::chrono::system_clock::from_time_t(timegm(&tm));
  Require(c.sdk.PatchObject(bucket, "ct.txt",
                            gcs::ObjectMetadataPatchBuilder().SetCustomTime(want)),
          "set customTime");
  auto const info = Metadata(c, bucket, "ct.txt");
  Check(info.has_custom_time() && info.custom_time() == want, "customTime differs");
}

void StorageClassOnWrite(Ctx& c) {
  auto const bucket = NewBucket(c);
  Upload(c, bucket, "cold", "x",
         gcs::WithObjectMetadata(gcs::ObjectMetadata().set_storage_class("COLDLINE")));
  CheckEqual(Metadata(c, bucket, "cold").storage_class(), "COLDLINE", "storage class");
}

void GzipTranscoding(Ctx& c) {
  auto const bucket = NewBucket(c);
  auto const plain = Repeat("compress me ", 50);
  auto const zipped = GzipCompress(plain);
  Upload(c, bucket, "gz.txt", zipped, gcs::ContentType("text/plain"), gcs::ContentEncoding("gzip"));
  // The SDK skips hash validation of a decompressed body only when the
  // response says x-guploader-response-body-transformations: gunzipped.
  CheckEqual(Download(c, bucket, "gz.txt"), plain, "decompressed bytes");
  CheckEqual(Download(c, bucket, "gz.txt", gcs::AcceptEncodingGzip()), zipped, "compressed bytes");
}

void CustomerSuppliedKey(Ctx& c) {
  auto const bucket = NewBucket(c);
  auto const key = gcs::EncryptionKey::FromBinaryKey(RandomBytes(32));
  Upload(c, bucket, "csek.txt", "secret", key);
  auto stream = c.sdk.ReadObject(bucket, "csek.txt");
  std::string const ignored{std::istreambuf_iterator<char>(stream), std::istreambuf_iterator<char>()};
  Check(!stream.status().ok(), "keyless read of an encrypted object succeeded");
  CheckEqual(Download(c, bucket, "csek.txt", key), "secret", "keyed bytes");
}

void SoftDeleteRestore(Ctx& c) {
  auto const bucket = NewBucket(c);
  Require(c.sdk.PatchBucket(bucket, gcs::BucketMetadataPatchBuilder().SetSoftDeletePolicy(
                                        std::chrono::seconds(604800))),
          "set soft delete policy");
  auto const generation = Upload(c, bucket, "sd.txt", "restore me").generation();
  Require(c.sdk.DeleteObject(bucket, "sd.txt"), "delete object");
  CheckEqual(ListNames(c, bucket, gcs::SoftDeleted(true)), {"sd.txt"}, "soft-deleted listing");
  Require(c.sdk.RestoreObject(bucket, "sd.txt", generation), "restore object");
  CheckEqual(Download(c, bucket, "sd.txt"), "restore me", "restored bytes");
}

}  // namespace

void AddObjectCases(std::vector<Case>& cases) {
  cases.insert(cases.end(),
               {
                   {"object.write_read_small", WriteSmall},
                   {"object.resumable_large", ResumableLarge},
                   {"object.metadata_on_write", MetadataOnWrite},
                   {"object.checksum_rejected", ChecksumRejected},
                   {"object.md5_accepted", Md5Accepted},
                   {"object.range_reads", RangeReads},
                   {"object.update_metadata", UpdateMetadata},
                   {"object.delete_not_found", DeleteNotFound},
                   {"object.does_not_exist_precondition", DoesNotExistPrecondition},
                   {"object.generation_match_precondition", GenerationMatchPrecondition},
                   {"object.metageneration_match_precondition", MetagenerationMatchPrecondition},
                   {"object.list_prefix_delimiter", ListPrefixDelimiter},
                   {"object.list_paging", ListPaging},
                   {"object.list_offsets", ListOffsets},
                   {"object.list_match_glob", ListMatchGlob},
                   {"object.copy_with_metadata", CopyWithMetadata},
                   {"object.rewrite_storage_class", RewriteStorageClass},
                   {"object.compose", Compose},
                   {"object.move", Move},
                   {"object.acl", ObjectAcl},
                   {"object.holds", Holds},
                   {"object.retention", Retention},
                   {"object.custom_time", CustomTime},
                   {"object.storage_class_on_write", StorageClassOnWrite},
                   {"object.gzip_transcoding", GzipTranscoding},
                   {"object.customer_supplied_key", CustomerSuppliedKey},
                   {"object.soft_delete_restore", SoftDeleteRestore},
               });
}

}  // namespace scorecard
