#include "signed_http.h"

#include <curl/curl.h>
#include <algorithm>
#include <cctype>
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

// curl_slist_append returns the list's head, or null without freeing the list.
void Append(Slist& list, std::string const& line) {
  auto* head = curl_slist_append(list.get(), line.c_str());
  if (head == nullptr) throw TransportError("curl_slist_append failed");
  static_cast<void>(list.release());
  list.reset(head);
}

std::size_t Collect(char* data, std::size_t size, std::size_t count, void* sink) {
  static_cast<std::string*>(sink)->append(data, size * count);
  return size * count;
}

std::size_t CollectHeader(char* data, std::size_t size, std::size_t count, void* sink) {
  auto& fields = *static_cast<HeaderFields*>(sink);
  std::string const line(data, size * count);
  // Every response, an interim 1xx one included, starts with a status line;
  // clearing there keeps only the final response's fields.
  if (line.rfind("HTTP/", 0) == 0) {
    fields.clear();
    return size * count;
  }
  auto const colon = line.find(':');
  if (colon == std::string::npos) return size * count;
  auto name = line.substr(0, colon);
  std::transform(name.begin(), name.end(), name.begin(),
                 [](unsigned char ch) { return static_cast<char>(std::tolower(ch)); });
  constexpr char kSpace[] = " \t\r\n";
  auto const first = line.find_first_not_of(kSpace, colon + 1);
  std::string value;
  if (first != std::string::npos) {
    value = line.substr(first, line.find_last_not_of(kSpace) - first + 1);
  }
  fields.emplace_back(std::move(name), std::move(value));
  return size * count;
}

// Applies the guards every signed request needs. They are deliberately
// explicit instead of relying on libcurl defaults, so a future default change
// cannot widen where a signed URL may be sent.
Easy MakeGuardedHandle(Origin const& origin, std::string const& ca_cert, HttpResponse* response,
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
  curl_easy_setopt(h, CURLOPT_WRITEDATA, &response->body);
  curl_easy_setopt(h, CURLOPT_HEADERFUNCTION, CollectHeader);
  curl_easy_setopt(h, CURLOPT_HEADERDATA, &response->headers);
  curl_easy_setopt(h, CURLOPT_ERRORBUFFER, error_buffer);
  return easy;
}

// Runs the transfer into the response the handle was made with.
void Perform(CURL* easy, HttpResponse& response, char const* error_buffer) {
  auto const code = curl_easy_perform(easy);
  if (code != CURLE_OK) {
    std::string message = "libcurl: ";
    message += curl_easy_strerror(code);
    if (*error_buffer != '\0') message += ": " + std::string(error_buffer);
    throw TransportError(message);
  }
  curl_easy_getinfo(easy, CURLINFO_RESPONSE_CODE, &response.status);
}

}  // namespace

std::vector<std::string> HttpResponse::HeaderValues(std::string const& name) const {
  std::vector<std::string> values;
  for (auto const& field : headers) {
    if (field.first == name) values.push_back(field.second);
  }
  return values;
}

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

bool SignedHttp::SameOrigin(std::string const& url) const {
  try {
    RequireOrigin(url);
    return true;
  } catch (TransportError const&) {
    return false;
  }
}

HttpResponse SignedHttp::Send(std::string const& method, std::string const& url,
                              std::string const& body, HeaderFields const& headers) const {
  RequireOrigin(url);
  HttpResponse response;
  char error_buffer[CURL_ERROR_SIZE] = {0};
  auto easy = MakeGuardedHandle(origin_, ca_cert_, &response, error_buffer);
  auto* h = easy.get();
  curl_easy_setopt(h, CURLOPT_URL, url.c_str());
  Slist list;
  for (auto const& header : headers) Append(list, header.first + ": " + header.second);
  if (method == "GET") {
    curl_easy_setopt(h, CURLOPT_HTTPGET, 1L);
  } else {
    curl_easy_setopt(h, CURLOPT_CUSTOMREQUEST, method.c_str());
    curl_easy_setopt(h, CURLOPT_POSTFIELDSIZE_LARGE, static_cast<curl_off_t>(body.size()));
    curl_easy_setopt(h, CURLOPT_COPYPOSTFIELDS, body.c_str());
    // libcurl would add application/x-www-form-urlencoded, and Expect:
    // 100-continue for a large body; an empty value removes each header, which
    // keeps the request equal to the other probes'.
    Append(list, "Content-Type:");
    Append(list, "Expect:");
  }
  if (list) curl_easy_setopt(h, CURLOPT_HTTPHEADER, list.get());
  Perform(h, response, error_buffer);
  return response;
}

HttpResponse SignedHttp::PostForm(std::string const& url,
                                  std::vector<std::pair<std::string, std::string>> const& fields,
                                  FilePart const& file) const {
  RequireOrigin(url);
  HttpResponse response;
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
  Perform(h, response, error_buffer);
  return response;
}

}  // namespace scorecard
