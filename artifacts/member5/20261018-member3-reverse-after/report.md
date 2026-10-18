# Member 5 synthetic baseline

Status: pass. Profile: release.
Source commit: `9e4ced3c390467690c1ca5a0b577f2bda923995f`; local changes and source hashes are recorded in summary.json.

Medians in milliseconds. Body is nested within command wall time; do not add them.
Separate verify runs AFTER the experiment command and is not capture-time hashing.

| Workload | Mode | n | Copy | Persist | Command wall | Body (paced sleeps included) | Verify CLI |
|---|---|---:|---:|---:|---:|---:|---:|---:|
| small | native | 3 | 0.419 | 15.765 | 26.325 | 0.205 | N/A |
| small | teamfs | 3 | 4.334 | 10.121 | 43.271 | 11.950 | N/A |
| small | experiment | 3 | 3.873 | 10.056 | 126.541 | 13.931 | 6.365 |
| many | native | 3 | 7.357 | 1561.373 | 36.921 | 4.834 | N/A |
| many | teamfs | 3 | 360.248 | 16.839 | 373.804 | 342.177 | N/A |
| many | experiment | 3 | 354.231 | 14.747 | 482.666 | 385.287 | 193.638 |
| large | native | 3 | 7.167 | 48.631 | 51.730 | 28.927 | N/A |
| large | teamfs | 3 | 24.125 | 58.800 | 137.833 | 99.099 | N/A |
| large | experiment | 3 | 40.378 | 51.703 | 221.889 | 85.291 | 40.777 |
| paced_many | native | 3 | 8.301 | 1641.126 | 4279.399 | 4255.555 | N/A |
| paced_many | teamfs | 3 | 343.016 | 14.728 | 5162.702 | 5134.259 | N/A |
| paced_many | experiment | 3 | 317.372 | 14.982 | 5311.392 | 5197.590 | 157.097 |
| paced_large | native | 3 | 10.193 | 29.431 | 4414.769 | 4386.252 | N/A |
| paced_large | teamfs | 3 | 34.780 | 67.178 | 4625.457 | 4595.378 | N/A |
| paced_large | experiment | 3 | 25.500 | 59.009 | 4682.250 | 4546.783 | 42.714 |
| unused_large | native | 3 | 7.234 | 20.782 | 21.130 | 0.185 | N/A |
| unused_large | teamfs | 3 | 24.669 | 51.353 | 38.285 | 11.281 | N/A |
| unused_large | experiment | 3 | 21.496 | 49.777 | 133.471 | 11.135 | 5.120 |

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
