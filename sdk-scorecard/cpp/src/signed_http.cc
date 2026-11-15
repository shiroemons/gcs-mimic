#include "signed_http.h"

#include <curl/curl.h>
#include <memory>

namespace scorecard {
namespace {

struct EasyDeleter {
  void operator()(CURL* easy) const { curl_easy_cleanup(easy); }
};
using Easy = std::unique_ptr<CURL, EasyDeleter>;

struct SlistDeleter {
  void operator()(curl_slist* list) const { curl_slist_free_all(list); }
};
using Slist = std::unique_ptr<curl_slist, SlistDeleter>;

struct MimeDeleter {
  void operator()(curl_mime* mime) const { curl_mime_free(mime); }
};
using Mime = std::unique_ptr<curl_mime, MimeDeleter>;

std::size_t Collect(char* data, std::size_t size, std::size_t count, void* sink) {
  static_cast<std::string*>(sink)->append(data, size * count);
  return size * count;
}

// Applies the guards every signed request needs. They are deliberately
// explicit instead of relying on libcurl defaults, so a future default change
// cannot widen where a signed URL may be sent.
Easy MakeGuardedHandle(Origin const& origin, std::string const& ca_cert, std::string* response,
                       char* error_buffer) {
  Easy easy(curl_easy_init());
  if (!easy) throw TransportError("curl_easy_init failed");
  auto* h = easy.get();
  curl_easy_setopt(h, CURLOPT_FOLLOWLOCATION, 0L);
  // An empty proxy plus NOPROXY "*" disables proxies even if libcurl read
  // an environment variable the probe did not scrub.
  curl_easy_setopt(h, CURLOPT_PROXY, "");
  curl_easy_setopt(h, CURLOPT_NOPROXY, "*");
  curl_easy_setopt(h, CURLOPT_PROTOCOLS_STR, origin.scheme.c_str());
  if (origin.scheme == "https" && !ca_cert.empty()) {
    curl_easy_setopt(h, CURLOPT_CAINFO, ca_cert.c_str());
  }
  curl_easy_setopt(h, CURLOPT_SSL_VERIFYPEER, 1L);
  curl_easy_setopt(h, CURLOPT_SSL_VERIFYHOST, 2L);
  curl_easy_setopt(h, CURLOPT_CONNECTTIMEOUT, 10L);
  curl_easy_setopt(h, CURLOPT_TIMEOUT, 20L);
  curl_easy_setopt(h, CURLOPT_NOSIGNAL, 1L);
  curl_easy_setopt(h, CURLOPT_WRITEFUNCTION, Collect);
  curl_easy_setopt(h, CURLOPT_WRITEDATA, response);
  curl_easy_setopt(h, CURLOPT_ERRORBUFFER, error_buffer);
  return easy;
}

HttpResponse Perform(CURL* easy, std::string&& body, char const* error_buffer) {
  auto const code = curl_easy_perform(easy);
  if (code != CURLE_OK) {
    std::string message = "libcurl: ";
    message += curl_easy_strerror(code);
    if (*error_buffer != '\0') message += ": " + std::string(error_buffer);
    throw TransportError(message);
  }
  HttpResponse response;
  curl_easy_getinfo(easy, CURLINFO_RESPONSE_CODE, &response.status);
  response.body = std::move(body);
  return response;
}

}  // namespace

SignedHttp::SignedHttp(Origin origin, std::string ca_cert)
    : origin_(std::move(origin)), ca_cert_(std::move(ca_cert)) {}

void SignedHttp::RequireOrigin(std::string const& url) const {
  Origin target;
  try {
    target = OriginOfUrl(url);
  } catch (ConfigError const& error) {
    throw TransportError(std::string("request escaped emulator: ") + error.what());
  }
  if (target.scheme != origin_.scheme || target.host != origin_.host ||
      target.port != origin_.port) {
    throw TransportError("request escaped emulator: " + target.ToString() + " is not " +
                         origin_.ToString());
  }
}

HttpResponse SignedHttp::Send(std::string const& method, std::string const& url,
                              std::string const& body) const {
  RequireOrigin(url);
  std::string response;
  char error_buffer[CURL_ERROR_SIZE] = {0};
  auto easy = MakeGuardedHandle(origin_, ca_cert_, &response, error_buffer);
  auto* h = easy.get();
  curl_easy_setopt(h, CURLOPT_URL, url.c_str());
  Slist headers;
  if (method == "GET") {
    curl_easy_setopt(h, CURLOPT_HTTPGET, 1L);
  } else {
    curl_easy_setopt(h, CURLOPT_CUSTOMREQUEST, method.c_str());
    curl_easy_setopt(h, CURLOPT_POSTFIELDSIZE_LARGE, static_cast<curl_off_t>(body.size()));
    curl_easy_setopt(h, CURLOPT_COPYPOSTFIELDS, body.c_str());
    // libcurl would add application/x-www-form-urlencoded; an empty value
    // removes the header, which keeps the request equal to the other probes'.
    headers.reset(curl_slist_append(nullptr, "Content-Type:"));
    curl_easy_setopt(h, CURLOPT_HTTPHEADER, headers.get());
  }
  return Perform(h, std::move(response), error_buffer);
}

HttpResponse SignedHttp::PostForm(std::string const& url,
                                  std::vector<std::pair<std::string, std::string>> const& fields,
                                  FilePart const& file) const {
  RequireOrigin(url);
  std::string response;
  char error_buffer[CURL_ERROR_SIZE] = {0};
  auto easy = MakeGuardedHandle(origin_, ca_cert_, &response, error_buffer);
  auto* h = easy.get();
  curl_easy_setopt(h, CURLOPT_URL, url.c_str());
  Mime mime(curl_mime_init(h));
  for (auto const& field : fields) {
    auto* part = curl_mime_addpart(mime.get());
    curl_mime_name(part, field.first.c_str());
    curl_mime_data(part, field.second.data(), field.second.size());
  }
  auto* part = curl_mime_addpart(mime.get());
  curl_mime_name(part, file.field.c_str());
  curl_mime_filename(part, file.filename.c_str());
  curl_mime_data(part, file.contents.data(), file.contents.size());
  curl_easy_setopt(h, CURLOPT_MIMEPOST, mime.get());
  return Perform(h, std::move(response), error_buffer);
}

}  // namespace scorecard
