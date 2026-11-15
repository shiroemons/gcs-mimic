#include "transport_check.h"

#include "harness.h"
#include "signed_http.h"
#include <initializer_list>
#include <iostream>
#include <stdexcept>
#include <string>

namespace scorecard {
namespace {

void CheckSdkTrust(Config const& config) {
  auto options = EndpointOptions(config);
  options.set<gc::UnifiedCredentialsOption>(gc::MakeInsecureCredentials());
  gcs::Client client(std::move(options));
  // `openssl s_server -www` answers every GET with a 200 HTML page. The SDK
  // cannot parse that as bucket metadata and reports INVALID_ARGUMENT without
  // an HTTP status; a real emulator answers with a status. Either proves the
  // TLS handshake and hostname check passed, because a response arrived. Any
  // other failure (certificate, connection refused, timeout) is a transport
  // failure, whose libcurl text names the cause.
  auto const status = client.GetBucketMetadata("scorecard-transport-check").status();
  std::cerr << "SDK answer: " << Describe(status) << "\n";
  bool const reached_http = status.ok() || HttpStatus(status) != 0 ||
                            status.code() == gc::StatusCode::kInvalidArgument;
  if (!reached_http) throw std::runtime_error("SDK client: " + Describe(status));
}

void CheckSignedHandle(Config const& config) {
  SignedHttp http(config.origin, config.ca_cert);
  auto const response = http.Send("GET", config.origin.ToString() + "/");
  if (response.status < 200 || response.status >= 300) {
    throw std::runtime_error("signed request handle: GET / answered HTTP " +
                             std::to_string(response.status));
  }
  try {
    http.Send("GET", "https://storage.googleapis.com/");
  } catch (TransportError const& error) {
    if (std::string(error.what()).find("escaped emulator") == std::string::npos) throw;
    return;
  }
  throw std::runtime_error("signed request handle accepted an external origin");
}

}  // namespace

void CheckTransport(Config const& config) {
  // Both checks always run, so a failure of one cannot hide the other's.
  std::string failures;
  for (auto check : {CheckSdkTrust, CheckSignedHandle}) {
    try {
      check(config);
    } catch (std::exception const& error) {
      failures += (failures.empty() ? "" : "; ") + std::string(error.what());
    }
  }
  if (!failures.empty()) throw std::runtime_error(failures);
}

}  // namespace scorecard
