// Signed URL and POST policy cases: the 6 shared `signed.*` IDs.
//
// They run in the permissive profile and establish routing, parameters and
// bytes, not emulator-side signature verification.
#include "harness.h"
#include <chrono>

namespace scorecard {
namespace {

enum class Version { kV2, kV4 };

constexpr auto kLifetime = std::chrono::minutes(5);

// V4 URLs and POST policies default to https whatever the endpoint, so the origin's scheme is
// passed to the signer; the URL is then sent exactly as signed. V2 URLs take the endpoint as is.
std::string SignedUrl(Ctx& c, Version version, std::string const& method,
                      std::string const& bucket) {
  auto url = version == Version::kV4
                 ? c.signer.CreateV4SignedUrl(method, bucket, "signed.txt",
                                              gcs::SignedUrlDuration(kLifetime),
                                              gcs::Scheme(c.config.origin.scheme))
                 : c.signer.CreateV2SignedUrl(
                       method, bucket, "signed.txt",
                       gcs::ExpirationTime(std::chrono::system_clock::now() + kLifetime));
  return Require(std::move(url), "sign URL");
}

void CheckStatus(HttpResponse const& response, std::string const& what) {
  Check(response.status == 200,
        what + ": status " + std::to_string(response.status) + ": " + response.body.substr(0, 250));
}

void SignedRoundTrip(Ctx& c, Version version, std::string const& method) {
  auto const bucket = NewBucket(c);
  if (method == "GET") Upload(c, bucket, "signed.txt", "signed");
  auto const url = SignedUrl(c, version, method, bucket);
  auto const response = c.http.Send(method, url, method == "PUT" ? "uploaded" : "");
  CheckStatus(response, method + " " + (version == Version::kV4 ? "V4" : "V2"));
  if (method == "GET") {
    CheckEqual(response.body, "signed", "signed bytes");
  } else {
    CheckEqual(Download(c, bucket, "signed.txt"), "uploaded", "signed bytes");
  }
}

void V4Get(Ctx& c) { SignedRoundTrip(c, Version::kV4, "GET"); }
void V4Put(Ctx& c) { SignedRoundTrip(c, Version::kV4, "PUT"); }
void V2Get(Ctx& c) { SignedRoundTrip(c, Version::kV2, "GET"); }

void V4ResumableUpload(Ctx& c) {
  auto const bucket = NewBucket(c);
  auto const data = RandomBytes(256 * 1024 + 5);
  // The start header is signed, so the request must send it unchanged.
  auto const url = Require(
      c.signer.CreateV4SignedUrl("POST", bucket, "resumable.bin", gcs::SignedUrlDuration(kLifetime),
                                 gcs::Scheme(c.config.origin.scheme),
                                 gcs::AddExtensionHeader("x-goog-resumable", "start")),
      "sign resumable start");
  auto const start = c.http.Send("POST", url, "", {{"x-goog-resumable", "start"}});
  Check(start.status == 201,
        "start: status " + std::to_string(start.status) + ": " + start.body.substr(0, 250));
  auto const locations = start.HeaderValues("location");
  Check(locations.size() == 1,
        "start returned " + std::to_string(locations.size()) + " Location headers");
  auto const& session = locations.front();
  Check(c.http.SameOrigin(session), "session URI " + session + " leaves the origin");
  auto const finish = c.http.Send("PUT", session, data);
  Check(finish.status == 200 || finish.status == 201,
        "upload: status " + std::to_string(finish.status) + ": " + finish.body.substr(0, 250));
  CheckEqual(Download(c, bucket, "resumable.bin"), data, "signed resumable bytes");
}

void V4ResponseOverrides(Ctx& c) {
  auto const bucket = NewBucket(c);
  std::string const disposition = "attachment; filename=\"report.csv\"";
  Upload(c, bucket, "report.txt", "a,b\n", gcs::ContentType("text/plain"));
  auto const url = Require(
      c.signer.CreateV4SignedUrl("GET", bucket, "report.txt", gcs::SignedUrlDuration(kLifetime),
                                 gcs::Scheme(c.config.origin.scheme),
                                 gcs::WithResponseContentDisposition(disposition),
                                 gcs::WithResponseContentType("text/csv")),
      "sign response overrides");
  auto const response = c.http.Send("GET", url);
  CheckStatus(response, "GET with response overrides");
  CheckEqual(response.body, "a,b\n", "signed bytes");
  CheckEqual(response.HeaderValues("content-disposition"), {disposition}, "Content-Disposition");
  CheckEqual(response.HeaderValues("content-type"), {"text/csv"}, "Content-Type");
  CheckEqual(Metadata(c, bucket, "report.txt").content_type(), "text/plain", "stored content type");
}

void PostPolicyV4(Ctx& c) {
  auto const bucket = NewBucket(c);
  auto const policy =
      Require(c.signer.GenerateSignedPostPolicyV4(gcs::PolicyDocumentV4(bucket, "post.txt", kLifetime),
                                                  gcs::Scheme(c.config.origin.scheme)),
              "sign POST policy");
  std::vector<std::pair<std::string, std::string>> fields(policy.required_form_fields.begin(),
                                                          policy.required_form_fields.end());
  // The form goes to the bucket path of the scorecard origin, as every probe
  // does, so a bug in the SDK's reported URL cannot redirect the request.
  auto const response = c.http.PostForm(c.config.origin.ToString() + "/" + bucket, fields,
                                        FilePart{"file", "post.txt", "posted"});
  Check(response.status >= 200 && response.status < 300,
        "status " + std::to_string(response.status) + ": " + response.body.substr(0, 250));
  CheckEqual(Download(c, bucket, "post.txt"), "posted", "posted bytes");
}

}  // namespace

void AddSignedCases(std::vector<Case>& cases) {
  cases.insert(cases.end(), {
                                {"signed.v4_get", V4Get},
                                {"signed.v4_put", V4Put},
                                {"signed.v4_resumable_upload", V4ResumableUpload},
                                {"signed.v4_response_overrides", V4ResponseOverrides},
                                {"signed.v2_get", V2Get},
                                {"signed.post_policy_v4", PostPolicyV4},
                            });
}

}  // namespace scorecard
