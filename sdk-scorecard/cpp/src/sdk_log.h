// Captures the SDK's own log for cases that observe what the SDK sends.
#ifndef SCORECARD_CPP_SDK_LOG_H
#define SCORECARD_CPP_SDK_LOG_H

#include "harness.h"
#include "google/cloud/log.h"
#include <cstdint>
#include <map>
#include <memory>
#include <mutex>
#include <set>
#include <string>
#include <vector>

namespace scorecard {

// Installs a log backend for one scope and restores the sink afterwards, even
// when the case throws.
class ScopedLogBackend {
 public:
  explicit ScopedLogBackend(std::shared_ptr<gc::LogBackend> backend);
  ~ScopedLogBackend();
  ScopedLogBackend(ScopedLogBackend const&) = delete;
  ScopedLogBackend& operator=(ScopedLogBackend const&) = delete;

 private:
  gc::LogSink& sink_;
  gc::LogSink::BackendId id_;
  gc::Severity severity_;
};

// A client like Ctx::sdk whose own log covers `components`: "rpc" logs each
// request and response object, "http" the libcurl trace of every request.
gcs::Client LoggingClient(Config const& config, std::set<std::string> components);

// One request as libcurl sent it.
struct SentRequest {
  std::string method;
  std::string target;                          // path and query
  std::map<std::string, std::string> headers;  // names lowercased
  std::uint64_t body_bytes = 0;

  // The header's value, or an empty string when it was not sent.
  std::string Header(std::string name) const;
};

// Rebuilds the requests of a LoggingClient with the "http" component from the
// libcurl trace it logs. The trace text is the SDK's, not an API: if it
// changes, no request is parsed and the cases relying on it fail.
class WireLog : public gc::LogBackend {
 public:
  void Process(gc::LogRecord const& record) override;
  void ProcessWithOwnership(gc::LogRecord record) override { Process(record); }

  std::vector<SentRequest> Requests() const;

 private:
  mutable std::mutex mutex_;
  std::vector<SentRequest> requests_;
};

}  // namespace scorecard

#endif  // SCORECARD_CPP_SDK_LOG_H
