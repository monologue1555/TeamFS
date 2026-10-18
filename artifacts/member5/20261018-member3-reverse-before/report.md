# Member 5 synthetic baseline

Status: pass. Profile: release.
Source commit: `276e68e37381a2c56c853ca7243859c340431213`; local changes and source hashes are recorded in summary.json.

Medians in milliseconds. Body is nested within command wall time; do not add them.
Separate verify runs AFTER the experiment command and is not capture-time hashing.

| Workload | Mode | n | Copy | Persist | Command wall | Body (paced sleeps included) | Verify CLI |
|---|---|---:|---:|---:|---:|---:|---:|---:|
| small | native | 3 | 0.383 | 18.390 | 21.589 | 0.189 | N/A |
| small | teamfs | 3 | 3.448 | 10.361 | 48.054 | 11.959 | N/A |
| small | experiment | 3 | 3.750 | 8.443 | 111.245 | 12.327 | 3.678 |
| many | native | 3 | 8.319 | 1499.037 | 26.314 | 4.251 | N/A |
| many | teamfs | 3 | 326.853 | 11.694 | 371.793 | 344.515 | N/A |
| many | experiment | 3 | 313.294 | 15.148 | 461.394 | 363.529 | 178.768 |
| large | native | 3 | 6.082 | 24.933 | 51.184 | 29.391 | N/A |
| large | teamfs | 3 | 21.376 | 50.682 | 112.190 | 84.811 | N/A |
| large | experiment | 3 | 21.697 | 48.001 | 194.248 | 82.968 | 31.023 |
| paced_many | native | 3 | 9.605 | 1499.883 | 4278.987 | 4257.084 | N/A |
| paced_many | teamfs | 3 | 308.343 | 13.115 | 5130.187 | 5103.529 | N/A |
| paced_many | experiment | 3 | 337.866 | 15.135 | 5348.917 | 5239.769 | 206.824 |
| paced_large | native | 3 | 7.186 | 33.997 | 4436.968 | 4411.143 | N/A |
| paced_large | teamfs | 3 | 24.264 | 72.223 | 4545.200 | 4507.754 | N/A |
| paced_large | experiment | 3 | 25.421 | 54.602 | 4668.054 | 4553.270 | 38.969 |
| unused_large | native | 3 | 8.139 | 24.883 | 21.796 | 0.189 | N/A |
| unused_large | teamfs | 3 | 21.988 | 51.631 | 36.007 | 11.729 | N/A |
| unused_large | experiment | 3 | 21.513 | 53.988 | 116.760 | 12.036 | 4.377 |

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
