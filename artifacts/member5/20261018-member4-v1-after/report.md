# Member 5 synthetic baseline

Status: pass. Profile: release.
Source commit: `c856f3f13c0f9b31cd316949fdf70ec42be833a7`; local changes and source hashes are recorded in summary.json.

Medians in milliseconds. Body is nested within command wall time; do not add them.
Separate verify runs AFTER the experiment command and is not capture-time hashing.

| Workload | Mode | n | Copy | Persist | Command wall | Body (paced sleeps included) | Verify CLI |
|---|---|---:|---:|---:|---:|---:|---:|---:|
| small | native | 3 | 0.317 | 18.553 | 23.260 | 0.194 | N/A |
| small | teamfs | 3 | 4.036 | 10.024 | 41.138 | 12.469 | N/A |
| small | experiment | 3 | 5.513 | 9.587 | 118.058 | 11.894 | 5.619 |
| many | native | 3 | 9.453 | 1564.359 | 26.453 | 4.133 | N/A |
| many | teamfs | 3 | 349.176 | 12.351 | 349.902 | 323.745 | N/A |
| many | experiment | 3 | 330.852 | 15.115 | 458.422 | 335.593 | 151.454 |
| large | native | 3 | 5.765 | 24.408 | 52.093 | 27.915 | N/A |
| large | teamfs | 3 | 23.823 | 47.397 | 103.297 | 78.740 | N/A |
| large | experiment | 3 | 27.678 | 47.281 | 202.211 | 80.353 | 35.225 |
| paced_many | native | 3 | 9.698 | 1531.651 | 4337.449 | 4315.463 | N/A |
| paced_many | teamfs | 3 | 316.916 | 16.100 | 5126.013 | 5099.295 | N/A |
| paced_many | experiment | 3 | 313.812 | 13.399 | 5339.548 | 5204.768 | 149.629 |
| paced_large | native | 3 | 5.828 | 27.138 | 4354.216 | 4332.867 | N/A |
| paced_large | teamfs | 3 | 23.143 | 57.696 | 4545.914 | 4519.085 | N/A |
| paced_large | experiment | 3 | 21.212 | 47.672 | 4664.754 | 4494.525 | 34.123 |
| unused_large | native | 3 | 9.148 | 25.352 | 21.318 | 0.193 | N/A |
| unused_large | teamfs | 3 | 23.944 | 47.920 | 39.041 | 12.230 | N/A |
| unused_large | experiment | 3 | 21.420 | 53.222 | 137.269 | 13.941 | 7.120 |

Scope and limitations:

- Synthetic sequential read plus SHA256 workloads, not a scientific case or universal filesystem ranking.
- Fresh mount/store per sample; rotating mode order. No OS cache drop. Prepared data and repeated reads include warm/uncontrolled caches.
- TeamFS uses default direct I/O, no file audit/trace/auto-sync; in-memory audit and runtime metrics remain enabled.
- Input copy and persistence are separate. Native persistence fsyncs copied files and directories; TeamFS uses sync CLI. Their durability mechanisms differ.
- Command wall includes subprocess startup. Experiment wall also includes snapshot commit, capture, complete-input hashing and report publication.
- Snapshot, capture hash and report export are NOT separately instrumented: null means unavailable, never zero. Verify CLI is a separate post-run recheck.
- Body timing includes read/hash loops and configured paced sleeps, and is contained in command wall. Active body subtracts observed sleeps, not FUSE blocking. Mount, archiving and cleanup are excluded.
- Paced workloads use fixed rounds and 0.5 second sleeps, not fixed-duration throughput. All modes have identical requested sleeps; sleep jitter remains measured.
- Checkpoint sequence includes the pre-start save; periodic count is sequence minus one. New reports must have at least two periodic saves for paced cases; old versions without checkpoints use null.
- Old/new comparisons include all member-3 changes, including snapshot fingerprint and stronger verify; they do not isolate a checkpoint on/off effect.
- Workload RSS is Linux ru_maxrss for the workload process only. Daemon peak is observed VmRSS sampled every 10 ms during command execution, may miss peaks; runner memory is not measured.
- Database/WAL/SHM values are logical file lengths, not physical device write traffic. Report bytes exclude workload outputs, which are recorded separately.
- Every successful output matches an independent deterministic oracle; experiment input hashes, classification, read bytes and unused-file exclusion are checked.
- No optimization claim or final integration acceptance: rerun after member 3/4 changes and with the final scientific case.
