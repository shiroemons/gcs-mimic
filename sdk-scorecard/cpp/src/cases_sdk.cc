// SDK cases: the `sdk.*` IDs the catalog lists for C++, in its order.
#include "harness.h"
#include "sdk_log.h"
#include "google/cloud/storage/parallel_upload.h"

#include <charconv>
#include <cstdint>
#include <filesystem>
#include <fstream>
#include <map>
#include <memory>
#include <optional>
#include <sstream>
#include <string_view>
#include <system_error>

namespace scorecard {
namespace {

using StringMap = std::map<std::string, std::string>;

constexpr std::size_t kMiB = 1024 * 1024;
// The resumable upload quantum. A write of exactly one quantum into a buffer of
// that size is sent at once, which makes it the chunk an upload is interrupted
// after.
constexpr std::size_t kChunk = 256 * 1024;

std::string Format(StringMap const& values) {
  std::string text = "{";
  for (auto const& [key, value] : values) text += (text.size() > 1 ? "," : "") + key + ":" + value;
  return text + "}";
}

// A file under the OS temp directory, removed when it goes out of scope.
class TempFile {
 public:
  TempFile() : path_(std::filesystem::temp_directory_path() / UniqueName()) {}
  explicit TempFile(std::string const& contents) : TempFile() {
    std::ofstream file(path_, std::ios::binary);
    file.write(contents.data(), static_cast<std::streamsize>(contents.size()));
    file.close();
    Check(!file.fail(), "cannot write " + path());
  }
  ~TempFile() {
    std::error_code ignored;
    std::filesystem::remove(path_, ignored);
  }
  TempFile(TempFile const&) = delete;
  TempFile& operator=(TempFile const&) = delete;

  std::string path() const { return path_.string(); }

  std::string Contents() const {
    std::ifstream file(path_, std::ios::binary);
    Check(file.is_open(), "cannot read " + path());
    std::ostringstream contents;
    contents << file.rdbuf();
    return contents.str();
  }

 private:
  std::filesystem::path path_;
};

// "bytes 262144-1048575/*" starts at 262144; a status query ("bytes */*") has
// no start.
std::optional<std::uint64_t> RangeStart(std::string const& content_range) {
  constexpr std::string_view kUnit = "bytes ";
  if (content_range.rfind(kUnit.data(), 0, kUnit.size()) != 0) return std::nullopt;
  auto const* begin = content_range.data() + kUnit.size();
  std::uint64_t start = 0;
  auto const result = std::from_chars(begin, content_range.data() + content_range.size(), start);
  if (result.ec != std::errc() || result.ptr == begin) return std::nullopt;
  return start;
}

// What the requests of one resumable upload phase sent.
struct UploadTraffic {
  int sessions_started = 0;
  std::uint64_t bytes = 0;  // body bytes of the PUTs
  std::optional<std::uint64_t> first_offset;  // Content-Range start of the first data PUT

  std::string ToString() const {
    return std::to_string(sessions_started) + " session start(s), " + std::to_string(bytes) +
           " bytes from offset " + (first_offset ? std::to_string(*first_offset) : "none");
  }
};

UploadTraffic Uploads(std::vector<SentRequest> const& requests) {
  UploadTraffic traffic;
  for (auto const& request : requests) {
    if (request.method == "POST" && request.target.find("uploadType=resumable") != std::string::npos) {
      ++traffic.sessions_started;
    }
    if (request.method != "PUT") continue;
    traffic.bytes += request.body_bytes;
    if (!traffic.first_offset) traffic.first_offset = RangeStart(request.Header("Content-Range"));
  }
  return traffic;
}

std::string Methods(std::vector<SentRequest> const& requests) {
  std::string text = "[";
  for (auto const& request : requests) text += (text.size() > 1 ? "," : "") + request.method;
  return text + "]";
}

void ResumableResume(Ctx& c) {
  auto const bucket = NewBucket(c);
  auto const data = RandomBytes(kMiB + 17);
  auto client = LoggingClient(c.config, {"http"});
  auto const first_log = std::make_shared<WireLog>();
  std::string session;
  {
    ScopedLogBackend const scope(first_log);
    auto stream = client.WriteObject(bucket, "resume.bin", gcs::NewResumableUploadSession(),
                                     gcs::UploadBufferSize(kChunk));
    Require(stream.last_status(), "start resumable upload");
    stream.write(data.data(), static_cast<std::streamsize>(kChunk));
    Require(stream.last_status(), "first chunk");
    Check(stream.next_expected_byte() == kChunk,
          "next expected byte after the first chunk: " + std::to_string(stream.next_expected_byte()));
    session = stream.resumable_session_id();
    // Suspend leaves the session open, as an interrupted process would; the
    // destructor would finalize the upload with only the first chunk.
    std::move(stream).Suspend();
  }
  Check(!session.empty(), "no resumable session ID");
  ExpectHttp(c.sdk.GetObjectMetadata(bucket, "resume.bin"), 404, "interrupted upload created the object");

  auto const resumed_log = std::make_shared<WireLog>();
  gcs::ObjectMetadata stored;
  {
    ScopedLogBackend const scope(resumed_log);
    // The new writer queries the session for the bytes it already holds.
    auto stream = client.WriteObject(bucket, "resume.bin", gcs::RestoreResumableUploadSession(session),
                                     gcs::UploadBufferSize(kChunk));
    Require(stream.last_status(), "restore session");
    Check(stream.IsOpen(), "restored session is already closed");
    auto const offset = stream.next_expected_byte();
    Check(offset == kChunk, "restored next expected byte " + std::to_string(offset) + ", expected " +
                                std::to_string(kChunk));
    stream.write(data.data() + offset, static_cast<std::streamsize>(data.size() - offset));
    stream.Close();
    stored = Require(stream.metadata(), "resumed upload");
  }
  auto const first = Uploads(first_log->Requests());
  Check(first.sessions_started == 1 && first.bytes == kChunk && first.first_offset == std::uint64_t{0},
        "first upload sent " + first.ToString() + ", expected 1 session start and " +
            std::to_string(kChunk) + " bytes from offset 0");
  auto const resumed = Uploads(resumed_log->Requests());
  Check(resumed.sessions_started == 0 && resumed.bytes == data.size() - kChunk &&
            resumed.first_offset == std::uint64_t{kChunk},
        "resumed upload sent " + resumed.ToString() + ", expected no session start and " +
            std::to_string(data.size() - kChunk) + " bytes from offset " + std::to_string(kChunk));
  Check(stored.size() == data.size(), "stored size " + std::to_string(stored.size()));
  CheckEqual(Download(c, bucket, "resume.bin"), data, "bytes");
}

void ResumableCancel(Ctx& c) {
  auto const bucket = NewBucket(c);
  auto const data = RandomBytes(kChunk);
  auto stream = c.sdk.WriteObject(bucket, "cancel.bin", gcs::NewResumableUploadSession(),
                                  gcs::UploadBufferSize(kChunk));
  stream.write(data.data(), static_cast<std::streamsize>(kChunk));
  Require(stream.last_status(), "first chunk");
  auto const session = stream.resumable_session_id();
  std::move(stream).Suspend();
  Check(!session.empty(), "no resumable session ID");
  Require(c.sdk.DeleteResumableUpload(session), "DeleteResumableUpload");
  auto restored = c.sdk.WriteObject(bucket, "cancel.bin", gcs::RestoreResumableUploadSession(session));
  if (restored.IsOpen()) {
    // Without Suspend the destructor would finalize the upload it reopened.
    std::move(restored).Suspend();
    Fail("the cancelled session was restored");
  }
  // A stream that cannot be restored is born closed with the failure as its
  // metadata.
  auto const& result = restored.metadata();
  if (result.ok()) Fail("restoring the cancelled session returned object " + result->name());
  Check(HttpStatus(result.status()) >= 400,
        "restoring the cancelled session: " + Describe(result.status()));
  ExpectHttp(c.sdk.GetObjectMetadata(bucket, "cancel.bin"), 404, "cancelled upload left an object");
  CheckEqual(ListNames(c, bucket), {}, "objects after the cancel");
}

void ObjectUpdateReplace(Ctx& c) {
  auto const bucket = NewBucket(c);
  Upload(c, bucket, "replace.txt", "x",
         gcs::WithObjectMetadata(
             gcs::ObjectMetadata().upsert_metadata("a", "1").upsert_metadata("b", "2")));
  auto metadata = Metadata(c, bucket, "replace.txt");
  Check(metadata.metadata() == StringMap{{"a", "1"}, {"b", "2"}},
        "metadata before the update: " + Format(metadata.metadata()));
  // UpdateObject sends the whole writable resource, so this map replaces the
  // stored one.
  metadata.mutable_metadata() = {{"c", "3"}};
  auto client = LoggingClient(c.config, {"http"});
  auto const log = std::make_shared<WireLog>();
  gcs::ObjectMetadata updated;
  {
    ScopedLogBackend const scope(log);
    updated = Require(client.UpdateObject(bucket, "replace.txt", metadata), "update object");
  }
  auto const requests = log->Requests();
  Check(requests.size() == 1 && requests.front().method == "PUT",
        "UpdateObject sent " + Methods(requests) + ", expected one PUT");
  StringMap const want{{"c", "3"}};
  Check(updated.metadata() == want, "update response metadata: " + Format(updated.metadata()));
  auto const stored = Metadata(c, bucket, "replace.txt").metadata();
  Check(stored == want, "stored metadata: " + Format(stored));
}

void RewriteTokenLoop(Ctx& c) {
  auto const bucket = NewBucket(c);
  auto const data = RandomBytes(3 * kMiB + 9);
  Upload(c, bucket, "src", data);
  auto client = LoggingClient(c.config, {"http"});
  auto const log = std::make_shared<WireLog>();
  std::vector<gcs::RewriteProgress> steps;
  std::size_t calls_with_token = 0;
  gcs::ObjectMetadata result;
  {
    ScopedLogBackend const scope(log);
    // The per-call limit applies only when the copy changes location or
    // storage class, so the destination is NEARLINE.
    auto rewriter = client.RewriteObject(
        bucket, "src", bucket, "dst", gcs::MaxBytesRewrittenPerCall(kMiB),
        gcs::WithObjectMetadata(gcs::ObjectMetadata().set_storage_class("NEARLINE")));
    // Each Iterate() sends the token the previous response returned.
    while (steps.size() < 20 && (steps.empty() || !steps.back().done)) {
      if (!rewriter.token().empty()) ++calls_with_token;
      steps.push_back(Require(rewriter.Iterate(), "rewrite call " + std::to_string(steps.size() + 1)));
      Check(steps.back().done || !rewriter.token().empty(), "an unfinished rewrite returned no token");
    }
    Check(steps.back().done, "rewrite not done after 20 calls");
    result = Require(rewriter.Result(), "rewrite result");
  }
  Check(calls_with_token >= 2, std::to_string(steps.size()) + " rewrite calls, " +
                                   std::to_string(calls_with_token) + " with a token");
  std::uint64_t previous = 0;
  for (auto const& step : steps) {
    Check(step.object_size == data.size(), "reported object size " + std::to_string(step.object_size));
    Check(step.total_bytes_rewritten >= previous && step.total_bytes_rewritten - previous <= kMiB,
          "rewritten bytes went from " + std::to_string(previous) + " to " +
              std::to_string(step.total_bytes_rewritten) + " in one call");
    previous = step.total_bytes_rewritten;
  }
  Check(previous == data.size(), "total bytes rewritten " + std::to_string(previous));
  std::size_t sent = 0;
  std::size_t sent_with_token = 0;
  for (auto const& request : log->Requests()) {
    if (request.method != "POST" || request.target.find("/rewriteTo/") == std::string::npos) continue;
    ++sent;
    Check(request.target.find("maxBytesRewrittenPerCall=1048576") != std::string::npos,
          "rewrite call without maxBytesRewrittenPerCall=1048576");
    if (request.target.find("rewriteToken=") != std::string::npos) ++sent_with_token;
  }
  Check(sent == steps.size() && sent_with_token == calls_with_token,
        "sent " + std::to_string(sent) + " rewrite calls, " + std::to_string(sent_with_token) +
            " with rewriteToken, for " + std::to_string(steps.size()) + " iterations");
  CheckEqual(result.storage_class(), "NEARLINE", "rewrite result storage class");
  CheckEqual(Metadata(c, bucket, "dst").storage_class(), "NEARLINE", "stored storage class");
  CheckEqual(Download(c, bucket, "dst"), data, "rewrite bytes");
}

void ParallelCompositeUpload(Ctx& c) {
  auto const bucket = NewBucket(c);
  auto const data = RandomBytes(3 * kMiB + 1);
  TempFile const file(data);
  // The default 32 MiB minimum shard size would upload this file as one
  // shard; a 1 MiB minimum with at most three streams makes three.
  auto const composed = Require(
      gcs::ParallelUploadFile(c.sdk, file.path(), bucket, "composite.bin", "parts-", false,
                              gcs::MaxStreams(3), gcs::MinStreamSize(kMiB)),
      "parallel upload");
  Check(composed.component_count() == 3,
        "componentCount " + std::to_string(composed.component_count()) + ", expected 3 shards");
  auto const stored = Metadata(c, bucket, "composite.bin").component_count();
  Check(stored == 3, "stored componentCount " + std::to_string(stored));
  CheckEqual(Download(c, bucket, "composite.bin"), data, "composite bytes");
  CheckEqual(ListNames(c, bucket), {"composite.bin"}, "objects after the upload");
}

void FileUploadDownload(Ctx& c) {
  auto const bucket = NewBucket(c);
  auto const data = RandomBytes(kMiB + 3);
  TempFile const source(data);
  TempFile const target;
  auto const uploaded = Require(c.sdk.UploadFile(source.path(), bucket, "file.bin"), "UploadFile");
  Check(uploaded.size() == data.size(), "uploaded size " + std::to_string(uploaded.size()));
  Require(c.sdk.DownloadToFile(bucket, "file.bin", target.path()), "DownloadToFile");
  CheckEqual(target.Contents(), data, "downloaded file");
}

}  // namespace

void AddSdkCases(std::vector<Case>& cases) {
  cases.insert(cases.end(), {
                                {"sdk.resumable_resume", ResumableResume},
                                {"sdk.resumable_cancel", ResumableCancel},
                                {"sdk.object_update_replace", ObjectUpdateReplace},
                                {"sdk.rewrite_token_loop", RewriteTokenLoop},
                                {"sdk.parallel_composite_upload", ParallelCompositeUpload},
                                {"sdk.file_upload_download", FileUploadDownload},
                            });
}

}  // namespace scorecard
