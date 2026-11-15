#include "runner.h"

#include <unistd.h>

#include <chrono>
#include <condition_variable>
#include <cstdio>
#include <exception>
#include <iostream>
#include <mutex>
#include <nlohmann/json.hpp>
#include <thread>

namespace scorecard {
namespace {

constexpr auto kCaseTimeout = std::chrono::seconds(60);
constexpr std::size_t kDetailLimit = 300;

// Cuts at a character boundary: a byte cut could split a UTF-8 sequence and
// make the JSON line invalid.
std::string TruncateChars(std::string const& text, std::size_t limit) {
  std::size_t chars = 0;
  for (std::size_t i = 0; i < text.size(); ++i) {
    if ((static_cast<unsigned char>(text[i]) & 0xC0) != 0x80 && chars++ == limit) {
      return text.substr(0, i);
    }
  }
  return text;
}

std::mutex& StdoutMutex() {
  static std::mutex mutex;
  return mutex;
}

void Emit(std::string const& line) {
  std::lock_guard<std::mutex> lock(StdoutMutex());
  std::cout << line << '\n';
  std::cout.flush();
}

// Ends the process when a case runs longer than its bound, as the Node probe
// does: the stuck SDK call cannot be cancelled, so the failing line is printed
// from this thread before exiting.
class Watchdog {
 public:
  Watchdog() : thread_([this] { Loop(); }) {}
  ~Watchdog() {
    {
      std::lock_guard<std::mutex> lock(mutex_);
      stop_ = true;
    }
    wake_.notify_all();
    thread_.join();
  }
  Watchdog(Watchdog const&) = delete;
  Watchdog& operator=(Watchdog const&) = delete;

  void Arm(std::string id) {
    {
      std::lock_guard<std::mutex> lock(mutex_);
      id_ = std::move(id);
      deadline_ = std::chrono::steady_clock::now() + kCaseTimeout;
      armed_ = true;
    }
    wake_.notify_all();
  }

  void Disarm() {
    std::lock_guard<std::mutex> lock(mutex_);
    armed_ = false;
  }

 private:
  void Loop() {
    std::unique_lock<std::mutex> lock(mutex_);
    while (!stop_) {
      if (!armed_) {
        wake_.wait(lock);
        continue;
      }
      if (wake_.wait_until(lock, deadline_) == std::cv_status::timeout && armed_ &&
          std::chrono::steady_clock::now() >= deadline_) {
        std::cerr << "case timed out: " << id_ << std::endl;
        Emit(ResultLine(id_, false, "case exceeded 60 seconds", false));
        ::_exit(2);
      }
    }
  }

  std::mutex mutex_;
  std::condition_variable wake_;
  std::string id_;
  std::chrono::steady_clock::time_point deadline_;
  bool armed_ = false;
  bool stop_ = false;
  std::thread thread_;
};

}  // namespace

std::string AreaOf(std::string const& id) {
  if (id.rfind("project.", 0) == 0) return "bucket";
  return id.substr(0, id.find('.'));
}

std::string ResultLine(std::string const& id, bool pass, std::string const& detail,
                       bool unsupported) {
  // ordered_json keeps the documented key order; the plain type sorts keys.
  nlohmann::ordered_json line = {{"sdk", "cpp"}, {"id", id}, {"area", AreaOf(id)}, {"pass", pass}};
  if (unsupported) line["unsupported"] = true;
  if (!detail.empty()) line["detail"] = TruncateChars(detail, kDetailLimit);
  return line.dump(-1, ' ', false, nlohmann::ordered_json::error_handler_t::replace);
}

int RunCases(Ctx& ctx, std::vector<Case> const& cases) {
  Watchdog watchdog;
  for (auto const& test : cases) {
    if (std::string(test.id).find(ctx.config.filter) == std::string::npos) continue;
    if (test.unsupported != nullptr) {
      Emit(ResultLine(test.id, false, test.unsupported, true));
      continue;
    }
    std::string failure;
    bool pass = true;
    watchdog.Arm(test.id);
    try {
      test.run(ctx);
    } catch (std::exception const& error) {
      pass = false;
      failure = error.what();
      if (dynamic_cast<CaseFailure const*>(&error) == nullptr) {
        failure = std::string(typeid(error).name()) + ": " + failure;
      }
    }
    watchdog.Disarm();
    Emit(ResultLine(test.id, pass, failure, false));
  }
  return 0;
}

}  // namespace scorecard
