# Member 5 synthetic baseline

Status: pass. Profile: release.
Source commit: `9e4ced3c390467690c1ca5a0b577f2bda923995f`; local changes and source hashes are recorded in summary.json.

Medians in milliseconds. Body is nested within command wall time; do not add them.
Separate verify runs AFTER the experiment command and is not capture-time hashing.

| Workload | Mode | n | Copy | Persist | Command wall | Body (paced sleeps included) | Verify CLI |
|---|---|---:|---:|---:|---:|---:|---:|---:|
| small | native | 3 | 0.453 | 35.037 | 23.368 | 0.228 | N/A |
| small | teamfs | 3 | 4.282 | 11.732 | 51.041 | 19.842 | N/A |
| small | experiment | 3 | 3.785 | 8.733 | 137.363 | 15.131 | 6.245 |
| many | native | 3 | 9.290 | 2360.535 | 30.337 | 4.838 | N/A |
| many | teamfs | 3 | 382.369 | 24.867 | 409.814 | 382.722 | N/A |
| many | experiment | 3 | 400.058 | 21.004 | 620.145 | 444.764 | 207.982 |
| large | native | 3 | 6.922 | 30.014 | 65.096 | 35.715 | N/A |
| large | teamfs | 3 | 26.988 | 113.053 | 146.987 | 108.946 | N/A |
| large | experiment | 3 | 28.789 | 61.792 | 269.878 | 120.396 | 53.410 |
| paced_many | native | 3 | 10.020 | 1954.118 | 4123.326 | 4091.347 | N/A |
| paced_many | teamfs | 3 | 407.774 | 17.882 | 5332.243 | 5282.433 | N/A |
| paced_many | experiment | 3 | 445.192 | 19.488 | 5515.947 | 5356.443 | 204.924 |
| paced_large | native | 3 | 7.242 | 50.214 | 4405.494 | 4381.640 | N/A |
| paced_large | teamfs | 3 | 26.565 | 56.440 | 4554.397 | 4522.751 | N/A |
| paced_large | experiment | 3 | 23.574 | 63.578 | 4690.297 | 4548.154 | 46.196 |
| unused_large | native | 3 | 7.047 | 23.403 | 22.632 | 0.230 | N/A |
| unused_large | teamfs | 3 | 21.225 | 50.912 | 48.818 | 15.382 | N/A |
| unused_large | experiment | 3 | 27.005 | 51.202 | 129.588 | 11.790 | 5.430 |

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
