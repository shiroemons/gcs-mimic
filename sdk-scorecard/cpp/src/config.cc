#include "config.h"

#include <algorithm>
#include <cctype>
#include <cstdlib>
#include <initializer_list>
#include <utility>

namespace scorecard {
namespace {

std::string Lower(std::string text) {
  std::transform(text.begin(), text.end(), text.begin(), [](unsigned char c) {
    return static_cast<char>(std::tolower(c));
  });
  return text;
}

// Parses "scheme://host[:port]" followed by an optional remainder that starts
// at the first '/', '?' or '#'. Returns the remainder through `rest`.
Origin ParseAuthority(std::string const& text, std::string* rest) {
  auto const scheme_end = text.find("://");
  if (scheme_end == std::string::npos) {
    throw ConfigError("origin has no scheme: " + text);
  }
  Origin origin;
  origin.scheme = Lower(text.substr(0, scheme_end));
  if (origin.scheme != "http" && origin.scheme != "https") {
    throw ConfigError("origin scheme must be http or https: " + text);
  }
  auto const authority_start = scheme_end + 3;
  auto const authority_end = text.find_first_of("/?#", authority_start);
  auto const authority = text.substr(authority_start, authority_end == std::string::npos
                                                          ? std::string::npos
                                                          : authority_end - authority_start);
  if (rest != nullptr) {
    *rest = authority_end == std::string::npos ? "" : text.substr(authority_end);
  }
  // Userinfo would let "127.0.0.1@elsewhere" pass a prefix check.
  if (authority.empty() || authority.find('@') != std::string::npos) {
    throw ConfigError("origin has an empty authority or userinfo: " + text);
  }
  std::string port_text;
  if (authority.front() == '[') {
    auto const close = authority.find(']');
    if (close == std::string::npos) throw ConfigError("unterminated IPv6 host: " + text);
    origin.host = Lower(authority.substr(0, close + 1));
    if (close + 1 < authority.size()) {
      if (authority[close + 1] != ':') throw ConfigError("malformed origin: " + text);
      port_text = authority.substr(close + 2);
    }
  } else {
    auto const colon = authority.find(':');
    origin.host = Lower(authority.substr(0, colon));
    if (colon != std::string::npos) port_text = authority.substr(colon + 1);
  }
  if (origin.host.empty()) throw ConfigError("origin has no host: " + text);
  if (port_text.empty()) {
    origin.port = origin.scheme == "https" ? 443 : 80;
  } else {
    if (port_text.size() > 5 ||
        !std::all_of(port_text.begin(), port_text.end(),
                     [](unsigned char c) { return std::isdigit(c) != 0; })) {
      throw ConfigError("origin port is not a number: " + text);
    }
    origin.port = std::stoi(port_text);
    if (origin.port < 1 || origin.port > 65535) {
      throw ConfigError("origin port is out of range: " + text);
    }
  }
  return origin;
}

std::string RequiredEnv(char const* name) {
  char const* value = std::getenv(name);
  if (value == nullptr || *value == '\0') {
    throw ConfigError(std::string(name) + " is required");
  }
  return value;
}

std::string EnvOr(char const* name, std::string fallback) {
  char const* value = std::getenv(name);
  return value == nullptr || *value == '\0' ? std::move(fallback) : std::string(value);
}

}  // namespace

std::string Origin::Authority() const { return host + ":" + std::to_string(port); }

std::string Origin::ToString() const { return scheme + "://" + Authority(); }

Origin ParseLoopbackOrigin(std::string const& text) {
  std::string rest;
  auto origin = ParseAuthority(text, &rest);
  if (!rest.empty() && rest != "/") {
    throw ConfigError("origin must not carry a path, query or fragment: " + text);
  }
  if (origin.host != "127.0.0.1" && origin.host != "localhost" && origin.host != "[::1]") {
    throw ConfigError("origin host must be 127.0.0.1, localhost or ::1: " + text);
  }
  return origin;
}

Origin OriginOfUrl(std::string const& url) { return ParseAuthority(url, nullptr); }

void ScrubEnvironment() {
  // CLOUD_STORAGE_*_ENDPOINT would silently replace an explicit
  // RestEndpointOption, for requests and for signed URLs. The rest select
  // ambient credentials or a metadata server, enable request tracing that
  // could print secrets, or route libcurl through a proxy.
  for (char const* name :
       {"CLOUD_STORAGE_EMULATOR_ENDPOINT", "CLOUD_STORAGE_TESTBENCH_ENDPOINT",
        "GOOGLE_APPLICATION_CREDENTIALS", "GOOGLE_CLOUD_UNIVERSE_DOMAIN",
        "GOOGLE_CLOUD_PROJECT", "GOOGLE_CLOUD_QUOTA_PROJECT", "CLOUD_STORAGE_ENABLE_TRACING",
        "GOOGLE_CLOUD_CPP_ENABLE_TRACING", "GOOGLE_CLOUD_CPP_ENABLE_CLOG", "CLOUDSDK_CONFIG", "GCE_METADATA_HOST",
        "GCE_METADATA_ROOT", "GCE_METADATA_IP", "STORAGE_EMULATOR_HOST", "SSL_CERT_DIR",
        "SSL_CERT_FILE", "CURL_CA_BUNDLE", "http_proxy", "https_proxy", "HTTP_PROXY",
        "HTTPS_PROXY", "all_proxy", "ALL_PROXY", "no_proxy", "NO_PROXY", "REQUEST_METHOD"}) {
    ::unsetenv(name);
  }
}

Config ConfigFromEnv() {
  Config config;
  config.origin = ParseLoopbackOrigin(RequiredEnv("SCORECARD_ENDPOINT"));
  config.ca_cert = EnvOr("SCORECARD_CA_CERT", "");
  config.signing_key =
      EnvOr("SCORECARD_SIGNING_KEY", "/app/fixtures/signing/test-only-private.pem");
  config.project = EnvOr("SCORECARD_PROJECT", "local-project");
  config.filter = EnvOr("SCORECARD_RUN", "");
  return config;
}

}  // namespace scorecard
