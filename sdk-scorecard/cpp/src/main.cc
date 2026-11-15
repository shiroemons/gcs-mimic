// Emulator-neutral probes for the official C++ Cloud Storage SDK
// (google-cloud-cpp); stdout is JSONL evidence, one line per case.
#include "config.h"
#include "harness.h"
#include "runner.h"
#include "transport_check.h"
#include <curl/curl.h>
#include <exception>
#include <iostream>
#include <string>
#include <vector>

namespace {

int Run(bool transport_check) {
  // Before any client or thread exists: the SDK reads these variables when a
  // client is built, and setenv is not thread safe.
  scorecard::ScrubEnvironment();
  auto config = scorecard::ConfigFromEnv();
  if (transport_check) {
    scorecard::CheckTransport(config);
    std::cout << "PASS: C++ SDK transport CA/SAN and signed-request exact-origin guard\n";
    return 0;
  }
  std::vector<scorecard::Case> cases;
  scorecard::AddBucketCases(cases);
  scorecard::AddObjectCases(cases);
  scorecard::AddSignedCases(cases);
  scorecard::AddSdkCases(cases);
  auto ctx = scorecard::MakeContext(std::move(config));
  return scorecard::RunCases(ctx, cases);
}

}  // namespace

int main(int argc, char** argv) {
  std::vector<std::string> args(argv + 1, argv + argc);
  bool transport_check = false;
  if (args.size() == 1 && args[0] == "--transport-check") {
    transport_check = true;
  } else if (!args.empty()) {
    std::cerr << "usage: gcs-mimic-scorecard-cpp [--transport-check]\n";
    return 2;
  }
  curl_global_init(CURL_GLOBAL_DEFAULT);
  try {
    return Run(transport_check);
  } catch (std::exception const& error) {
    std::cerr << "FAIL: " << error.what() << "\n";
    return 1;
  }
}
