// Environment handling for the C++ scorecard probe.
#ifndef GCS_MIMIC_SCORECARD_CPP_CONFIG_H
#define GCS_MIMIC_SCORECARD_CPP_CONFIG_H

#include <stdexcept>
#include <string>

namespace scorecard {

// Thrown for a process or configuration failure, as opposed to a failing case.
class ConfigError : public std::runtime_error {
 public:
  using std::runtime_error::runtime_error;
};

// A parsed loopback origin such as "https://127.0.0.1:4443".
struct Origin {
  std::string scheme;  // "http" or "https"
  std::string host;    // as written, with brackets kept for IPv6
  int port = 0;        // explicit or the scheme default

  // "scheme://host:port": the form used for exact comparisons.
  std::string Authority() const;
  // The origin as the SDK and signed URLs expect it: no trailing slash.
  std::string ToString() const;
};

// Parses an origin on 127.0.0.1, localhost or ::1 with no path, query,
// fragment or userinfo. Throws ConfigError for anything else.
Origin ParseLoopbackOrigin(std::string const& text);

// Returns the origin of an absolute http(s) URL (everything before the path),
// or throws ConfigError when the URL has no such prefix. Unlike
// ParseLoopbackOrigin it accepts any host; the caller compares it.
Origin OriginOfUrl(std::string const& url);

struct Config {
  Origin origin;
  std::string ca_cert;      // empty when SCORECARD_CA_CERT is unset
  std::string signing_key;  // path to the PKCS#8 PEM
  std::string project;
  std::string filter;  // substring filter on case IDs
};

// Removes every variable that could redirect the SDK, select ambient
// credentials, reach a metadata server or route through a proxy. It must run
// before any client or thread exists, because setenv is not thread safe.
void ScrubEnvironment();

// Reads SCORECARD_* variables. Throws ConfigError when SCORECARD_ENDPOINT is
// missing or is not a loopback origin.
Config ConfigFromEnv();

}  // namespace scorecard

#endif  // GCS_MIMIC_SCORECARD_CPP_CONFIG_H
