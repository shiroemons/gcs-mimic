// A libcurl handle that sends pre-signed requests to the scorecard origin only.
#ifndef SCORECARD_CPP_SIGNED_HTTP_H
#define SCORECARD_CPP_SIGNED_HTTP_H

#include "config.h"
#include <stdexcept>
#include <string>
#include <utility>
#include <vector>

namespace scorecard {

// A request was refused locally or libcurl failed. The message carries the
// libcurl error text, which names a certificate problem for TLS failures.
class TransportError : public std::runtime_error {
 public:
  using std::runtime_error::runtime_error;
};

struct HttpResponse {
  long status = 0;
  std::string body;
};

struct FilePart {
  std::string field;
  std::string filename;
  std::string contents;
};

class SignedHttp {
 public:
  SignedHttp(Origin origin, std::string ca_cert);

  // Sends `url` unchanged. `body` is sent as the raw payload for PUT and POST
  // and without a Content-Type, as the Python probe's requests session does.
  HttpResponse Send(std::string const& method, std::string const& url,
                    std::string const& body = {}) const;

  // Sends a multipart/form-data POST with the text fields first and the file
  // part last, the order a signed POST policy requires.
  HttpResponse PostForm(std::string const& url,
                        std::vector<std::pair<std::string, std::string>> const& fields,
                        FilePart const& file) const;

 private:
  // Throws TransportError unless `url` has exactly the scorecard origin.
  void RequireOrigin(std::string const& url) const;

  Origin origin_;
  std::string ca_cert_;
};

}  // namespace scorecard

#endif  // SCORECARD_CPP_SIGNED_HTTP_H
