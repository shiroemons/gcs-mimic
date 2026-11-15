// Signed URL and POST policy cases: the 4 shared `signed.*` IDs.
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
                                {"signed.v2_get", V2Get},
                                {"signed.post_policy_v4", PostPolicyV4},
                            });
}

}  // namespace scorecard
