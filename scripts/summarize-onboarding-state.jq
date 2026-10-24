def metrics_for($workload):
  ["wall_seconds", "server_cpu_seconds", "allocated_disk_KiB"] +
  if $workload | startswith("startup-") then ["ready_RSS_KiB"]
  else ["p50_seconds", "p95_seconds", "p99_seconds", "server_cpu_seconds_per_gib", "sampled_RSS_KiB"] end;

def calibrated($metric; $cpu_uncalibrated):
  sort as $values
  | $values[2] as $median
  | if any($values[]; . <= 0) or ($metric | startswith("server_cpu")) and $cpu_uncalibrated then
      {samples: $values, median: $median, calibration: "PENDING: a zero sample or missing/quantized raw server CPU repetition prevents calibration"}
    else
      (($values[4] - $values[0]) / $median) as $spread
      | ([0.10, 2 * $spread] | max) as $fraction
      | {samples: $values, median: $median, min: $values[0], max: $values[4],
          relative_spread: $spread, allowed_regression_fraction: $fraction,
          median_upper_bound: ($median * (1 + $fraction))}
    end;

[splits("\n") | fromjson? | select(type == "object") | select(has("workload"))]
| group_by(.workload)
| if map(.[0].workload) != ["download-1gib","small-overwrite","small-publish","startup-10000","startup-100000","upload-1gib"] or any(.[]; length != 5 or ([.[].round] | sort) != [1,2,3,4,5]) then
    error("expected all six available workloads with five distinct repetitions")
  elif any(.[][]; .errors != 0) then error("benchmark reported errors")
  elif any(.[][] | select(.workload == "small-publish" or .workload == "small-overwrite"); .requests != 10000 or .bytes != 10240000) then
    error("small-object workload count/size differs")
  elif any(.[][] | select(.workload == "upload-1gib" or .workload == "download-1gib"); .requests != 1 or .bytes != 1073741824) then
    error("transfer workload count/size differs")
  else . end
| {source: "60a4f03", profile: "native-macos-http-permissive-native-wal-full", repetitions: 5,
    formula: "upper = median * (1 + max(0.10, 2 * (max-min)/median))",
    pending: ["sync-full-rescan", "api-with-sync", "import-overlap", "import-resume", "cgroup-memory", "temporary-disk-high-water", "cold-cache"],
    workloads: map(. as $samples
      | (any($samples[]; (.server_cpu_seconds | type) != "number" or .server_cpu_seconds <= 0.1)) as $cpu_uncalibrated
      | metrics_for($samples[0].workload) as $metrics
      | {
      workload: $samples[0].workload,
      samples_by_round: ($samples | sort_by(.round)),
      not_applicable_metrics: (["p50_seconds", "p95_seconds", "p99_seconds", "server_cpu_seconds_per_gib", "sampled_RSS_KiB", "ready_RSS_KiB"] - $metrics),
      thresholds: ($metrics | map(. as $metric
        | [$samples[] | select((.[$metric] | type) != "number") | .round] as $missing
        | {key: $metric, value: (if $missing | length > 0 then
            {calibration: "PENDING: applicable metric is missing or nonnumeric", missing_rounds: $missing}
          else [$samples[] | .[$metric]] | calibrated($metric; $cpu_uncalibrated) end)})
        | from_entries)
    })}
