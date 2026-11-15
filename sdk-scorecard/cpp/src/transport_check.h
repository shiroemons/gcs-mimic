// `--transport-check`: certificate, hostname and origin handling only.
#ifndef SCORECARD_CPP_TRANSPORT_CHECK_H
#define SCORECARD_CPP_TRANSPORT_CHECK_H

#include "config.h"

namespace scorecard {

// Proves two things without an emulator: the SDK client trusts the origin's
// certificate (a call reaches HTTP, whatever it answers), and the signed
// request handle reaches the origin but refuses an external one. Throws
// std::runtime_error with the SDK or libcurl text on failure; that text names
// the certificate problem for a wrong CA or SAN.
void CheckTransport(Config const& config);

}  // namespace scorecard

#endif  // SCORECARD_CPP_TRANSPORT_CHECK_H
