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

// Header fields in order, as name and value pairs.
using HeaderFields = std::vector<std::pair<std::string, std::string>>;

struct HttpResponse {
  long status = 0;
  std::string body;
  // The final response's fields, names lowercased and values trimmed.
  HeaderFields headers;

  // Every value of the lowercase header `name`, in arrival order.
  std::vector<std::string> HeaderValues(std::string const& name) const;
};

struct FilePart {
  std::string field;
  std::string filename;
  std::string contents;
};

class SignedHttp {
 public:
  SignedHttp(Origin origin, std::string ca_cert);

  // Sends `url` unchanged with the extra request `headers`. `body` is sent as
  // the raw payload for PUT and POST and without a Content-Type or Expect
  // header, as the Python probe's requests session does.
  HttpResponse Send(std::string const& method, std::string const& url,
                    std::string const& body = {}, HeaderFields const& headers = {}) const;

  // Whether `url` is an absolute URL with exactly the scorecard origin, such
  // as a session URI a signed request returned.
  bool SameOrigin(std::string const& url) const;

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
