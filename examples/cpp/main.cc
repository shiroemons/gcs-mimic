// Uses the official C++ client against a local gcs-mimic.

#include "google/cloud/storage/client.h"
#include <chrono>
#include <cstdlib>
#include <iostream>
#include <iterator>
#include <sstream>
#include <stdexcept>
#include <string>
#include <utility>

namespace gc = ::google::cloud;
namespace gcs = ::google::cloud::storage;

namespace {

char const* Env(char const* name) { return std::getenv(name); }

void Check(gc::Status const& status) {
  if (!status.ok()) throw std::runtime_error(status.message());
}

template <typename T>
T Check(gc::StatusOr<T> result) {
  Check(result.status());
  return *std::move(result);
}

}  // namespace

int main() try {
  if (Env("GCSM_ENDPOINT") == nullptr && Env("GCS_MIMIC_ENDPOINT") != nullptr) {
    throw std::runtime_error("GCS_MIMIC_ENDPOINT was renamed to GCSM_ENDPOINT");
  }
  std::string const endpoint =
      Env("GCSM_ENDPOINT") != nullptr ? Env("GCSM_ENDPOINT") : "http://localhost:4443";
  // The SDK sends every request to CLOUD_STORAGE_EMULATOR_ENDPOINT when it is set, ignoring
  // RestEndpointOption, so a stale value would quietly point the client elsewhere.
  if (auto const* emulator = Env("CLOUD_STORAGE_EMULATOR_ENDPOINT");
      emulator != nullptr && endpoint != emulator) {
    throw std::runtime_error(
        "CLOUD_STORAGE_EMULATOR_ENDPOINT overrides the endpoint; unset it or set it to " +
        endpoint);
  }

  // Insecure credentials send no Authorization header, so the client never contacts Google.
  // The default retry policy keeps retrying for 15 minutes, so stop after 30 seconds instead
  // when the emulator is not running.
  auto client = gcs::Client(
      gc::Options{}
          .set<gcs::RestEndpointOption>(endpoint)
          .set<gcs::ProjectIdOption>("local-project")
          .set<gc::UnifiedCredentialsOption>(gc::MakeInsecureCredentials())
          .set<gcs::RetryPolicyOption>(
              gcs::LimitedTimeRetryPolicy(std::chrono::seconds(30)).clone()));

  std::ostringstream name;
  name << "example-cpp-" << std::hex
       << std::chrono::system_clock::now().time_since_epoch().count();
  auto const bucket = name.str();
  Check(client.CreateBucket(bucket, gcs::BucketMetadata()));
  std::cout << "created bucket " << bucket << "\n";

  // InsertObject sends one request; WriteObject would always start a resumable upload.
  Check(client.InsertObject(bucket, "hello.txt", "Hello from gcs-mimic!"));
  std::cout << "uploaded hello.txt\n";

  for (auto& listed : client.ListObjects(bucket)) {
    auto const object = Check(std::move(listed));
    std::cout << "listed " << object.name() << " (" << object.size() << " bytes)\n";
  }

  auto reader = client.ReadObject(bucket, "hello.txt");
  std::string const content{std::istreambuf_iterator<char>{reader}, {}};
  Check(reader.status());
  std::cout << "downloaded: " << content << "\n";

  Check(client.DeleteObject(bucket, "hello.txt"));
  std::cout << "deleted hello.txt\n";
  Check(client.DeleteBucket(bucket));
  std::cout << "deleted bucket " << bucket << "\n";
  return 0;
} catch (std::exception const& error) {
  std::cerr << error.what() << "\n";
  return 1;
}
