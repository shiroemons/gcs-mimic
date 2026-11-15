// Shared state and helpers for every probe case.
#ifndef SCORECARD_CPP_HARNESS_H
#define SCORECARD_CPP_HARNESS_H

#include "config.h"
#include "signed_http.h"
#include "google/cloud/storage/client.h"
#include <algorithm>
#include <functional>
#include <iterator>
#include <stdexcept>
#include <string>
#include <utility>
#include <vector>

namespace scorecard {

namespace gcs = ::google::cloud::storage;
namespace gc = ::google::cloud;

// An assertion inside a case failed.
class CaseFailure : public std::runtime_error {
 public:
  using std::runtime_error::runtime_error;
};

[[noreturn]] void Fail(std::string const& message);
void Check(bool condition, std::string const& message = "unexpected result");

// "NOT_FOUND (http 404): message". The HTTP status comes from the error
// metadata because the SDK maps 304, 308 and 412 to one StatusCode.
std::string Describe(gc::Status const& status);

// The HTTP status carried by a failed SDK call, or 0 when the failure did not
// come from an HTTP response (for example a TLS or connection error).
int HttpStatus(gc::Status const& status);

// Requires success, naming `what` in the failure.
void Require(gc::Status const& status, std::string const& what);

template <typename T>
T Require(gc::StatusOr<T> result, std::string const& what) {
  Require(result.status(), what);
  return *std::move(result);
}

// Requires a failure with exactly the given HTTP status.
void ExpectHttp(gc::Status const& status, int expected, std::string const& what);

template <typename T>
void ExpectHttp(gc::StatusOr<T> const& result, int expected, std::string const& what) {
  if (result.ok()) Fail(what + ": expected HTTP " + std::to_string(expected) + ", got success");
  ExpectHttp(result.status(), expected, what);
}

// "sc-cpp-" plus 12 random hex digits: a valid bucket name and an object prefix.
std::string UniqueName();

// `count` bytes from std::random_device, for payloads and customer-supplied keys.
std::string RandomBytes(std::size_t count);

// What every case receives.
struct Ctx {
  Config config;
  gcs::Client sdk;     // talks to the emulator
  gcs::Client signer;  // holds the signing key; never sends a request itself
  SignedHttp http;     // sends the signed URLs the signer produced
};

// Creates the SDK client against the emulator and the offline signing client.
Ctx MakeContext(Config config);

// Client options shared by both clients: endpoint, CA, retries and stalls.
gc::Options EndpointOptions(Config const& config);

// A case body; it throws (usually through Check or Require) to fail.
using CaseFn = void (*)(Ctx&);
struct Case {
  char const* id;
  CaseFn run;
  // Non-null only when the SDK has no public API for the case.
  char const* unsupported = nullptr;
};

// Registers each file's cases in the order of the shared case list, then the
// SDK cases in catalog order.
void AddBucketCases(std::vector<Case>& cases);
void AddObjectCases(std::vector<Case>& cases);
void AddSignedCases(std::vector<Case>& cases);
void AddSdkCases(std::vector<Case>& cases);

// Creates a bucket with the given name and returns the name.
template <typename... Options>
std::string NewBucketNamed(Ctx& c, std::string name, gcs::BucketMetadata metadata = {},
                           Options&&... options) {
  Require(c.sdk.CreateBucket(name, std::move(metadata), std::forward<Options>(options)...),
          "create bucket " + name);
  return name;
}

// Creates a bucket with a unique name and returns the name.
template <typename... Options>
std::string NewBucket(Ctx& c, gcs::BucketMetadata metadata = {}, Options&&... options) {
  return NewBucketNamed(c, UniqueName(), std::move(metadata), std::forward<Options>(options)...);
}

// Single-request upload.
template <typename... Options>
gcs::ObjectMetadata Upload(Ctx& c, std::string const& bucket, std::string const& name,
                           std::string const& contents, Options&&... options) {
  return Require(
      c.sdk.InsertObject(bucket, name, contents, std::forward<Options>(options)...),
      "insert " + name);
}

// Reads the whole object; fails on any stream error, including checksum
// mismatches the SDK detects at end of stream.
template <typename... Options>
std::string Download(Ctx& c, std::string const& bucket, std::string const& name,
                     Options&&... options) {
  auto stream = c.sdk.ReadObject(bucket, name, std::forward<Options>(options)...);
  std::string data{std::istreambuf_iterator<char>(stream), std::istreambuf_iterator<char>()};
  Require(stream.status(), "read " + name);
  return data;
}

// Metadata of an existing object.
inline gcs::ObjectMetadata Metadata(Ctx& c, std::string const& bucket, std::string const& name) {
  return Require(c.sdk.GetObjectMetadata(bucket, name), "metadata " + name);
}

inline gcs::BucketMetadata BucketInfo(Ctx& c, std::string const& bucket) {
  return Require(c.sdk.GetBucketMetadata(bucket), "bucket metadata " + bucket);
}

// Sorted names of the objects a listing returns.
template <typename... Options>
std::vector<std::string> ListNames(Ctx& c, std::string const& bucket, Options&&... options) {
  std::vector<std::string> names;
  for (auto& object : c.sdk.ListObjects(bucket, std::forward<Options>(options)...)) {
    names.push_back(Require(std::move(object), "list objects").name());
  }
  std::sort(names.begin(), names.end());
  return names;
}

// Uploads each name with its own name as contents.
inline void Seed(Ctx& c, std::string const& bucket, std::vector<std::string> const& names) {
  for (auto const& name : names) Upload(c, bucket, name, name);
}

// Equality with a readable failure message.
void CheckEqual(std::string const& actual, std::string const& expected, std::string const& what);
void CheckEqual(std::vector<std::string> const& actual, std::vector<std::string> const& expected,
                std::string const& what);

}  // namespace scorecard

#endif  // SCORECARD_CPP_HARNESS_H
