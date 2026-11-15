// Runs the selected cases and prints one JSONL line per case.
#ifndef GCS_MIMIC_SCORECARD_CPP_RUNNER_H
#define GCS_MIMIC_SCORECARD_CPP_RUNNER_H

#include "harness.h"
#include <string>
#include <vector>

namespace scorecard {

// The `area` of a case ID: its prefix, except `project.*` which scores as
// `bucket`.
std::string AreaOf(std::string const& id);

// One JSONL line without the trailing newline. `detail` is cut to 300
// characters and omitted when empty; `unsupported` adds the flag.
std::string ResultLine(std::string const& id, bool pass, std::string const& detail,
                       bool unsupported);

// Runs every case whose ID contains `config.filter`, each bounded to 60
// seconds. A case that exceeds its bound prints its failing line and ends the
// process with status 2. Returns 0 after running all selected cases, even if
// some failed.
int RunCases(Ctx& ctx, std::vector<Case> const& cases);

}  // namespace scorecard

#endif  // GCS_MIMIC_SCORECARD_CPP_RUNNER_H
