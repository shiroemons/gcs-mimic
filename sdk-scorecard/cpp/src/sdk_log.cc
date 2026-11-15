#include "sdk_log.h"

#include <cctype>
#include <charconv>
#include <sstream>
#include <system_error>
#include <string_view>

namespace scorecard {
namespace {

std::string Lowercase(std::string text) {
  for (auto& ch : text) ch = static_cast<char>(std::tolower(static_cast<unsigned char>(ch)));
  return text;
}

// 0 when the text does not start with a number, so a changed trace undercounts
// and fails the size checks instead of passing them.
std::uint64_t LeadingNumber(std::string_view text) {
  std::uint64_t value = 0;
  auto const result = std::from_chars(text.data(), text.data() + text.size(), value);
  return result.ec == std::errc() ? value : 0;
}

}  // namespace

ScopedLogBackend::ScopedLogBackend(std::shared_ptr<gc::LogBackend> backend)
    : sink_(gc::LogSink::Instance()),
      id_(sink_.AddBackend(std::move(backend))),
      severity_(sink_.minimum_severity()) {
  sink_.set_minimum_severity(gc::Severity::GCP_LS_DEBUG);
}

ScopedLogBackend::~ScopedLogBackend() {
  sink_.set_minimum_severity(severity_);
  sink_.RemoveBackend(id_);
}

gcs::Client LoggingClient(Config const& config, std::set<std::string> components) {
  auto options = EndpointOptions(config);
  options.set<gc::UnifiedCredentialsOption>(gc::MakeInsecureCredentials())
      .set<gc::LoggingComponentsOption>(std::move(components));
  return gcs::Client(std::move(options));
}

std::string SentRequest::Header(std::string name) const {
  auto const it = headers.find(Lowercase(std::move(name)));
  return it == headers.end() ? std::string() : it->second;
}

void WireLog::Process(gc::LogRecord const& record) {
  // libcurl hands a whole request head to one debug callback, so a head never
  // spans records; its body writes can be flushed in later records and belong
  // to the last head seen.
  constexpr std::string_view kHead = ">> curl(Send Header): ";
  constexpr std::string_view kBody = ">> curl(Send Data): size=";
  std::lock_guard<std::mutex> lock(mutex_);
  bool in_head = false;
  std::istringstream lines(record.message);
  for (std::string line; std::getline(lines, line);) {
    if (!line.empty() && line.back() == '\r') line.pop_back();
    auto const head = line.find(kHead);
    auto const body = line.find(kBody);
    if (head != std::string::npos && head < body) {
      SentRequest request;
      std::istringstream(line.substr(head + kHead.size())) >> request.method >> request.target;
      requests_.push_back(std::move(request));
      in_head = true;
    } else if (body != std::string::npos) {
      in_head = false;
      if (!requests_.empty()) {
        requests_.back().body_bytes += LeadingNumber(std::string_view(line).substr(body + kBody.size()));
      }
    } else if (line.empty() || line.find("curl(") != std::string::npos) {
      in_head = false;
    } else if (in_head && !requests_.empty()) {
      auto const colon = line.find(':');
      if (colon == std::string::npos) continue;
      auto const value = line.find_first_not_of(' ', colon + 1);
      requests_.back().headers[Lowercase(line.substr(0, colon))] =
          value == std::string::npos ? std::string() : line.substr(value);
    }
  }
}

std::vector<SentRequest> WireLog::Requests() const {
  std::lock_guard<std::mutex> lock(mutex_);
  return requests_;
}

}  // namespace scorecard
