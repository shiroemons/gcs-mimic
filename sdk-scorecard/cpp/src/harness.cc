#include "harness.h"

#include <chrono>
#include <cstdint>
#include <fstream>
#include <nlohmann/json.hpp>
#include <random>
#include <sstream>

namespace scorecard {

[[noreturn]] void Fail(std::string const& message) { throw CaseFailure(message); }

void Check(bool condition, std::string const& message) {
  if (!condition) Fail(message);
}

int HttpStatus(gc::Status const& status) {
  auto const& metadata = status.error_info().metadata();
  auto const it = metadata.find("http_status_code");
  if (it == metadata.end()) return 0;
  try {
    return std::stoi(it->second);
  } catch (std::exception const&) {
    return 0;
  }
}

std::string Describe(gc::Status const& status) {
  std::ostringstream out;
  out << gc::StatusCodeToString(status.code());
  if (auto const http = HttpStatus(status); http != 0) out << " (http " << http << ")";
  out << ": " << status.message();
  return out.str();
}

void Require(gc::Status const& status, std::string const& what) {
  if (!status.ok()) Fail(what + ": " + Describe(status));
}

void ExpectHttp(gc::Status const& status, int expected, std::string const& what) {
  if (status.ok()) Fail(what + ": expected HTTP " + std::to_string(expected) + ", got success");
  auto const actual = HttpStatus(status);
  if (actual != expected) {
    Fail(what + ": expected HTTP " + std::to_string(expected) + ", got " + Describe(status));
  }
}

std::string UniqueName() {
  // A fresh engine per call keeps the helper free of shared mutable state;
  // random_device supplies the entropy so parallel runs cannot collide.
  std::random_device device;
  std::mt19937_64 engine((static_cast<std::uint64_t>(device()) << 32) ^ device());
  constexpr char kHex[] = "0123456789abcdef";
  std::string name = "sc-cpp-";
  for (int i = 0; i < 12; ++i) name += kHex[engine() % 16];
  return name;
}

std::string RandomBytes(std::size_t count) {
  std::random_device device;
  std::string bytes(count, '\0');
  for (auto& byte : bytes) byte = static_cast<char>(device() & 0xFF);
  return bytes;
}

gc::Options EndpointOptions(Config const& config) {
  auto const origin = config.origin.ToString();
  // Zero retries make one failed attempt one failure. Without it a 503 would
  // be retried for minutes and hide which request failed.
  gc::Options options;
  options.set<gcs::RestEndpointOption>(origin)
      .set<gcs::IamEndpointOption>(origin + "/iamapi")
      .set<gcs::ProjectIdOption>(config.project)
      .set<gcs::RetryPolicyOption>(gcs::LimitedErrorCountRetryPolicy(0).clone())
      .set<gcs::BackoffPolicyOption>(
          gcs::ExponentialBackoffPolicy(std::chrono::milliseconds(10),
                                        std::chrono::milliseconds(100), 2.0)
              .clone())
      // Bound how long a hung emulator can block a call; the per-case
      // watchdog is the last resort.
      .set<gcs::TransferStallTimeoutOption>(std::chrono::seconds(30))
      .set<gcs::DownloadStallTimeoutOption>(std::chrono::seconds(30))
      .set<gc::storage_experimental::HttpConnectTimeoutOption>(std::chrono::milliseconds(10000));
  if (!config.ca_cert.empty()) options.set<gc::CARootsFilePathOption>(config.ca_cert);
  return options;
}

namespace {

std::string ReadFile(std::string const& path) {
  std::ifstream file(path, std::ios::binary);
  if (!file) throw ConfigError("cannot read signing key " + path);
  std::ostringstream contents;
  contents << file.rdbuf();
  return contents.str();
}

// The built-in test signer of gcs-mimic; see fixtures/signing/README.md.
std::string SignerJson(Config const& config) {
  nlohmann::json key = {
      {"type", "service_account"},
      {"project_id", "gcs-mimic-scorecard"},
      {"private_key_id", "test-only"},
      {"private_key", ReadFile(config.signing_key)},
      {"client_email", "test-signer@gcs-mimic.invalid"},
      {"client_id", "1"},
      // Never contacted: signing is local, so no token is requested.
      {"token_uri", config.origin.ToString() + "/token"},
  };
  return key.dump();
}

}  // namespace

Ctx MakeContext(Config config) {
  auto sdk_options = EndpointOptions(config);
  sdk_options.set<gc::UnifiedCredentialsOption>(gc::MakeInsecureCredentials());
  // The signer is a separate client so the emulator client keeps insecure
  // credentials. The SDK signs locally with the key and only falls back to the
  // IAM signBlob endpoint, which is also local, so nothing can reach Google.
  auto signer_options = EndpointOptions(config);
  signer_options.set<gc::UnifiedCredentialsOption>(
      gc::MakeServiceAccountCredentials(SignerJson(config)));
  SignedHttp http(config.origin, config.ca_cert);
  auto sdk = gcs::Client(std::move(sdk_options));
  auto signer = gcs::Client(std::move(signer_options));
  return Ctx{std::move(config), std::move(sdk), std::move(signer), std::move(http)};
}

void CheckEqual(std::string const& actual, std::string const& expected, std::string const& what) {
  if (actual == expected) return;
  // Bodies can be megabytes; report sizes rather than the bytes.
  Fail(what + " differs (" + std::to_string(actual.size()) + " bytes, expected " +
       std::to_string(expected.size()) + ")" +
       (actual.size() <= 80 ? ": " + actual : std::string()));
}

void CheckEqual(std::vector<std::string> const& actual, std::vector<std::string> const& expected,
                std::string const& what) {
  if (actual == expected) return;
  auto join = [](std::vector<std::string> const& values) {
    std::string text = "[";
    for (auto const& value : values) text += (text.size() > 1 ? "," : "") + value;
    return text + "]";
  };
  Fail(what + " differs: " + join(actual) + ", expected " + join(expected));
}

}  // namespace scorecard
